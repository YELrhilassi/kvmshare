//! One connected client: its outbound queue, its shared context, and
//! the accept → handshake → service → teardown lifecycle.

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use kvmshare_log::{log_debug, log_info, log_warn};
use kvmshare_protocol::id::errors;
use kvmshare_protocol::message::{Layout, Message, ScreenInfo};

use crate::server::actions::apply_action;
use crate::server::engine::{Engine, ServerClipboard};
use crate::server::{Policy, ServerEvent};
use crate::session::{Action, Session};
use crate::transport::{RecvResult, Transport};
use crate::udp;

/// Read timeout on each client's TCP control channel. The client sends
/// a keepalive every 2 s, so this window only paces the reader — it does
/// not drop anyone.
const CLIENT_READ_TIMEOUT: Duration = Duration::from_secs(2);
/// A client silent for longer than this is gone (asleep, wedged, or
/// dead — TCP alone often survives sleep and crashes, so without a
/// liveness timeout the session would believe a dead machine still has
/// the cursor and keep this machine's input isolated and its cursor
/// hidden after the client's next resume). The keepalive cadence is
/// 2 s; this is five missed keepalives.
const CLIENT_SILENT_TIMEOUT: Duration = Duration::from_secs(10);

/// The server's view of one connected client.
pub struct Client {
    pub id: u8,
    pub name: String,
    /// The client's stable machine id, from its `Hello`. Kept so a hot
    /// policy change can tell whether *this* connected client has just
    /// been revoked and must be disconnected.
    pub machine_id: String,
    /// Monotonic ms when the client connected (for the client list).
    pub since_ms: u64,
    /// Everything destined for this client: reliable control frames
    /// (TCP) and cursor-stream frames (UDP), in enqueue order. Drained
    /// by the writer thread.
    pub out: Sender<Outbound>,
}

/// One outbound item for a client.
pub enum Outbound {
    /// Reliable control frame (TCP).
    Tcp(Message),
    /// Loss-tolerant cursor frame (UDP) — only relative motion.
    Udp(Message),
}

/// Which link a message travels on: everything is a reliable control
/// frame except the additive cursor motion.
pub fn route(msg: Message) -> Outbound {
    if matches!(msg, Message::MouseMoveRel { .. }) {
        Outbound::Udp(msg)
    } else {
        Outbound::Tcp(msg)
    }
}

/// Push a message onto a client's outbound queue (never blocks — the
/// queue is unbounded; the writer drains it). Unknown client = gone.
pub fn enqueue(clients: &Arc<Mutex<HashMap<u8, Arc<Client>>>>, id: u8, msg: Message) {
    let Some(client) = clients.lock().unwrap().get(&id).cloned() else { return };
    let _ = client.out.send(route(msg));
}

/// Shared state one connected client's threads need. Bundled once at
/// accept time so neither the handshake nor the service loop carries a
/// nine-parameter signature, and so adding per-client state is a one-
/// place change.
pub struct ClientCtx {
    pub session: Arc<Mutex<Session>>,
    pub clients: Arc<Mutex<HashMap<u8, Arc<Client>>>>,
    pub active: Arc<Mutex<Option<u8>>>,
    pub engine: Arc<Mutex<Box<dyn Engine>>>,
    pub clipboard: ServerClipboard,
    /// UDP routing state: per-client datagram addresses and the
    /// anti-replay sequence counters. Owned jointly by the writer
    /// (address) and the UDP receiver (address + sequence).
    pub addrs: Arc<Mutex<HashMap<u8, SocketAddr>>>,
    pub seqs: Arc<Mutex<HashMap<u8, u32>>>,
    /// When each client's cursor stream was last heard (monotonic ms).
    /// The active client beacons every few ms; a stream gone silent is
    /// the signature of a wedged client — see the beacon watchdog in
    /// [`crate::server::udp::udp_receiver`].
    pub last_heard: Arc<Mutex<HashMap<u8, u64>>>,
    /// Connection policy (allowlist / local-only / trusted + revoked
    /// ids). Shared with the `Server` so a hot policy change applies to
    /// the next handshake without a restart.
    pub policy: Arc<Mutex<Policy>>,
    /// Lifecycle events out to the app layer (client list, auto-config).
    pub events: Option<Sender<ServerEvent>>,
    /// This machine's stable id, sent to clients in `Welcome`.
    pub server_id: String,
}

impl ClientCtx {
    /// The current layout snapshot in wire form (for Welcome /
    /// ScreenInfo replies).
    pub fn layout_snapshot(&self) -> Layout {
        let s = self.session.lock().unwrap();
        Layout { screens: s.layout().screens.clone() }
    }

    /// This machine's stable id, sent to clients in `Welcome` so they
    /// can trust the server (discovery, auto-connect).
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// Unregister the client everywhere and, if it had the cursor,
    /// bring the session home. Called by the reader thread exactly once
    /// when the control channel dies (EOF, error, or silence timeout).
    /// Dropping the last `Sender` of the outbound queue ends the writer
    /// thread, and the socket with it.
    ///
    /// Identity-safe: only the client that is *still registered* under
    /// its id is torn down. A newer connection that took over the same
    /// id (same name — the machine reconnected before the old socket's
    /// death was noticed, or a second instance bypassed the role lock)
    /// must never be unregistered by the old connection's reader. The
    /// old reader simply finishes and drops its Arc; the fresh
    /// registration is untouched.
    pub fn teardown(&self, client: &Arc<Client>) {
        let id = client.id;
        let registered = {
            let clients = self.clients.lock().unwrap();
            match clients.get(&id) {
                Some(c) if Arc::ptr_eq(c, client) => true,
                _ => false,
            }
        };
        if !registered {
            // Superseded by a newer connection with the same id — this
            // reader is the stale one. Its outbound sender is no longer
            // in the map, so dropping this Arc ends the old writer and
            // socket; nothing else must be touched.
            log_debug!("client {}: stale connection superseded — skipping teardown", client.name);
            return;
        }
        log_info!("client {} disconnected", client.name);
        self.clients.lock().unwrap().remove(&id);
        self.addrs.lock().unwrap().remove(&id);
        self.seqs.lock().unwrap().remove(&id);
        self.last_heard.lock().unwrap().remove(&id);
        if let Some(tx) = &self.events {
            let _ = tx.send(ServerEvent::ClientDisconnected { name: client.name.clone() });
        }

        {
            let mut act = self.active.lock().unwrap();
            if *act == Some(id) {
                *act = None;
            }
        }
        let action = self.session.lock().unwrap().on_client_disconnected(id);
        if let Action::SwitchToLocal { .. } = action {
            if let Ok(mut engine) = self.engine.lock() {
                let _ = apply_action(action, &self.active, &self.clients, &self.last_heard, &mut engine);
            }
        }
    }
}

impl Client {
    /// Full accept path for one connection: handshake, registration and
    /// the reader thread. Each stage is its own function below.
    pub fn spawn(stream: TcpStream, ctx: Arc<ClientCtx>, udp: Arc<UdpSocket>) -> io::Result<()> {
        // Timed reads: a client that stops sending (sleep, wedge, crash)
        // must be noticed and dropped so the session returns home — see
        // [`CLIENT_SILENT_TIMEOUT`].
        let addr = stream.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| "?".into());
        let mut transport = Transport::with_read_timeout(stream, Some(CLIENT_READ_TIMEOUT))?;
        let (id, machine_id, name, info, admitted) = exchange_hello(&mut transport, &ctx, &addr)?;
        ctx.session.lock().unwrap().update_screen_info(id, info.clone());

        // Send Welcome + current layout, then split the transport: the
        // writer keeps the sending half; the reader gets its own lock-
        // free socket clone (TCP is full-duplex), so it can block on
        // recv while the writer sends freely.
        transport.send(&Message::Welcome {
            server_version: kvmshare_protocol::VERSION,
            server_id: ctx.server_id().to_owned(),
            layout: ctx.layout_snapshot(),
            own_screen_id: id,
        })?;
        let reader = transport.reader()?;
        let (out_tx, out_rx) = mpsc::channel::<Outbound>();
        spawn_writer(id, transport, udp, ctx.addrs.clone(), out_rx);

        let client = Arc::new(Client {
            id,
            name: name.clone(),
            machine_id: machine_id.clone(),
            since_ms: crate::time::now_ms(),
            out: out_tx,
        });
        // A client with this id is already registered (same name): the
        // machine reconnected before the old socket's death was noticed,
        // or a second instance started with a different state dir
        // bypassed the role lock. The fresh connection is authoritative
        // — replace the map entry and end the stale one cleanly. Its
        // reader will finish on the old socket's EOF and its teardown is
        // identity-checked (see [`ClientCtx::teardown`]), so it can
        // never unregister this new client. The Leave/Control pair tells
        // the old peer to end its session; if the old socket is already
        // dead, the writer drops it on the next send.
        {
            let mut clients = ctx.clients.lock().unwrap();
            if let Some(old) = clients.insert(id, client.clone()) {
                log_info!("client {}: replacing stale connection with the same id", old.name);
                let _ = old.out.send(route(Message::Leave { screen_id: id }));
                let _ = old.out.send(route(Message::Control {
                    command: kvmshare_protocol::id::control::DISCONNECT,
                }));
            }
        }
        // The client is now fully registered: crossings may enter its
        // screen. Marking it here — after the map insert — guarantees a
        // crossing can never fire into a screen whose `Enter` would be
        // dropped (the client must exist before the cursor can go there).
        ctx.session.lock().unwrap().on_client_connected(id);
        // Stable marker for the GUI's notification watcher (kept in sync
        // with the "disconnected" line in `teardown`): "client X connected".
        log_info!("client {} connected", client.name);
        if let Some(tx) = &ctx.events {
            let _ = tx.send(ServerEvent::ClientConnected {
                name: name.clone(),
                id: machine_id,
                addr,
                since_ms: client.since_ms,
                info,
            });
        }
        if admitted {
            // The client was not in the layout and was admitted
            // dynamically. Every already-connected client must learn the
            // new screen map; the newcomer's Welcome already carries it.
            log_info!(
                "client {} was not in the layout — admitted dynamically; pin it in the Layout page to make the position permanent",
                client.name
            );
            let layout = ctx.layout_snapshot();
            for c in ctx.clients.lock().unwrap().values() {
                let _ = c.out.send(route(Message::Layout { layout: layout.clone() }));
            }
        }
        service_client(client, reader, ctx);
        Ok(())
    }
}

/// Handshake: expect Hello with a matching protocol version, then apply
/// the connection policy (local-only, allowlist / trusted ids) and find
/// the client a screen.
///
/// Refusals: protocol version mismatch, a revoked machine id (a hard
/// deny, checked first), a name colliding with the server's own screen, a
/// peer outside the local network (when `local_only`), and a name absent
/// from the layout whose machine id is not trusted (when `allowlist`).
fn exchange_hello(
    transport: &mut Transport,
    ctx: &ClientCtx,
    addr: &str,
) -> io::Result<(u8, String, String, ScreenInfo, bool)> {
    let (machine_id, name, info) = match transport.recv()? {
        RecvResult::Msg(Message::Hello { version, id, name, info }) => {
            if version != kvmshare_protocol::VERSION {
                let _ = transport.send(&Message::Error {
                    code: errors::VERSION_MISMATCH,
                    text: format!(
                        "server speaks v{}, client speaks v{version}",
                        kvmshare_protocol::VERSION
                    ),
                });
                return Err(io::Error::other("version mismatch"));
            }
            (id, name, info)
        }
        RecvResult::Msg(_) => return Err(io::Error::other("expected hello")),
        RecvResult::Eof | RecvResult::NoData => {
            return Err(io::Error::other("client closed before hello"))
        }
    };

    // Revocation first: it is the strongest rule and the only one that
    // beats a pinned layout screen. A machine the operator explicitly
    // refused must never connect, whatever else would admit it.
    if ctx.policy.lock().unwrap().is_revoked(&machine_id) {
        let _ = transport.send(&Message::Error {
            code: errors::REVOKED,
            text: format!(
                "this machine's id is revoked on this server — ask the server's operator to re-trust it"
            ),
        });
        return Err(io::Error::other(format!(
            "client {name} ({machine_id}) refused: machine id is revoked"
        )));
    }

    // Only accept connections from the local network (RFC1918 private
    // ranges, loopback, link-local). A bridged/WAN peer is refused
    // before any layout state is touched.
    if ctx.policy.lock().unwrap().local_only && !is_local_addr(addr) {
        let _ = transport.send(&Message::Error {
            code: errors::NOT_LOCAL,
            text: format!("connection from {addr} refused — only local-network peers are accepted"),
        });
        return Err(io::Error::other(format!("client {addr} is not on the local network")));
    }

    // Allowlist: the name must be in the layout, or the machine id must
    // be trusted (then it is admitted dynamically). Anything else is
    // refused — a client has to be named in the layout first.
    // Trusted ids may be full 32-char hex or the 8-char short form
    // (matching by prefix), so a user can paste the short id shown in
    // the GUI instead of the whole string.
    if ctx.policy.lock().unwrap().allowlist {
        let named = ctx.session.lock().unwrap().assign_screen_id(&name).is_some();
        let trusted = ctx.policy.lock().unwrap().is_trusted(&machine_id);
        if !named && !trusted {
            let _ = transport.send(&Message::Error {
                code: errors::NOT_ALLOWED,
                text: format!(
                    "\"{name}\" is not in this server's layout and its machine id is not trusted — add it to the layout or trust its id to connect"
                ),
            });
            return Err(io::Error::other(format!(
                "client {name} ({machine_id}) refused: not in the layout and not trusted"
            )));
        }
    }

    let (id, admitted) = match ctx.session.lock().unwrap().admit_client(&name, info.clone()) {
        Some(admitted) => admitted,
        None => {
            let _ = transport.send(&Message::Error {
                code: errors::NAME_CONFLICT,
                text: format!("a machine cannot connect as \"{name}\" — that is this server's own screen"),
            });
            return Err(io::Error::other(format!("client name {name} conflicts with the local screen")));
        }
    };
    Ok((id, machine_id, name, info, admitted))
}

/// Is `addr` (a `peer_addr()` string, possibly with port) on the local
/// network? Accepts IPv4 RFC1918 private ranges (10/8, 172.16/12,
/// 192.168/16), loopback and link-local, plus IPv6 loopback, ULA
/// (fc00::/7) and link-local (fe80::/10). Anything else is "remote".
fn is_local_addr(addr: &str) -> bool {
    let ip = addr.split(':').next().unwrap_or(addr);
    if let Ok(v4) = ip.parse::<std::net::Ipv4Addr>() {
        return v4.is_private() || v4.is_loopback() || v4.is_link_local();
    }
    if let Ok(v6) = ip.parse::<std::net::Ipv6Addr>() {
        let octets = v6.octets();
        return v6.is_loopback()
            || v6.is_unique_local()
            || (octets[0] == 0xfe && (octets[1] & 0xc0) == 0x80);
    }
    false
}

/// The reader thread: services one client's TCP control channel until it
/// goes away, then tears the client down. Blocking IO is plenty for a
/// KVM — a handful of control messages per second.
fn service_client(client: Arc<Client>, mut reader: Transport, ctx: Arc<ClientCtx>) {
    thread::spawn(move || {
        let mut last_seen = Instant::now();
        loop {
            let msg = match reader.recv() {
                Ok(RecvResult::Msg(msg)) => {
                    last_seen = Instant::now();
                    msg
                }
                Ok(RecvResult::NoData) => {
                    // Silence within the read window. Keepalives land
                    // every 2 s; a silence far beyond that means the
                    // client is not coming back (it slept through its
                    // keepalives, wedged, or died) — drop it so the
                    // session returns home instead of believing a
                    // dead machine still has the cursor.
                    if last_seen.elapsed() > CLIENT_SILENT_TIMEOUT {
                        log_debug!("client {}: silent for {CLIENT_SILENT_TIMEOUT:?} — dropping", client.name);
                        break;
                    }
                    continue;
                }
                Ok(RecvResult::Eof) | Err(_) => break,
            };
            handle_client_message(&client, msg, &ctx);
        }
        ctx.teardown(&client);
    });
}

/// Dispatch one inbound control message from a client.
fn handle_client_message(client: &Client, msg: Message, ctx: &ClientCtx) {
    match msg {
        Message::KeepAlive => {}
        Message::ScreenInfo { info } => {
            // The client's resolution changed: rebuild the
            // layout so edge math stays correct, then reply.
            {
                let mut s = ctx.session.lock().unwrap();
                s.update_screen_info(client.id, info);
            }
            enqueue(&ctx.clients, client.id, Message::Layout { layout: ctx.layout_snapshot() });
        }
        Message::Clipboard { mime, data } => {
            // Content copied on the client reaches the
            // server's local clipboard. Applied through the
            // clipboard service's own lock: `set` can block
            // (selection ownership handshakes), and it must
            // never hold the engine lock that serializes
            // every cursor motion.
            log_debug!("clipboard from {}: {} ({} bytes)", client.name, mime, data.len());
            if let Ok(mut cb) = ctx.clipboard.lock() {
                cb.set(&mime, &data);
            }
        }
        // Defensive: current clients send beacons over UDP;
        // keep the TCP arm for robustness (mixed or older
        // peers, transport fallbacks).
        Message::CursorPos { x, y } => {
            ctx.session.lock().unwrap().on_remote_beacon(client.id, x, y);
        }
        _ => {}
    }
}

/// One writer thread per client: drains the outbound queue in order and
/// owns both sockets (the TCP transport and, through the shared UDP
/// socket, this client's datagram address). Reliable frames go over TCP;
/// cursor motion goes over UDP stamped with a per-client sequence number.
/// The thread never takes a session lock, so it can block on a wedged
/// peer without ever stalling the input path.
fn spawn_writer(
    id: u8,
    mut tcp: Transport,
    udp: Arc<UdpSocket>,
    addrs: Arc<Mutex<HashMap<u8, SocketAddr>>>,
    rx: Receiver<Outbound>,
) {
    thread::Builder::new()
        .name(format!("kvmshare-writer-{id}"))
        .spawn(move || {
            // Sequence starts at 1: the peer's receiver initializes to 0,
            // so the very first frame must count as newer, not duplicate.
            let mut seq: u32 = 1;
            let mut unregistered = false;
            while let Ok(item) = rx.recv() {
                let res = match item {
                    Outbound::Tcp(msg) => tcp.send(&msg),
                    Outbound::Udp(msg) => {
                        let addr = addrs.lock().unwrap().get(&id).copied();
                        match addr {
                            Some(addr) => {
                                let bytes = udp::pack(id, seq, &msg);
                                seq = seq.wrapping_add(1);
                                udp.send_to(&bytes, addr).map(|_| ())
                            }
                            None => {
                                // The client registers its address with its
                                // first datagram right after the handshake;
                                // only a race can deliver motion before
                                // that, and motion is loss-tolerant.
                                if !unregistered {
                                    log_debug!("client {id}: no UDP address yet, dropping cursor frame");
                                    unregistered = true;
                                }
                                Ok(())
                            }
                        }
                    }
                };
                if let Err(e) = res {
                    log_warn!("client {id}: send failed: {e}");
                    break;
                }
            }
        })
        .expect("cannot spawn client writer");
}