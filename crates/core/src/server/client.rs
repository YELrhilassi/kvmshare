//! One connected client: its outbound queue, its shared context, and
//! the accept → handshake → service → teardown lifecycle.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
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

/// How many outbound items may queue per client before sends start
/// dropping. Generous — healthy control traffic is a handful of frames
/// per second. The bound exists so a wedged writer (a stalled TCP peer,
/// a vanished UDP target) can never grow memory without limit: the old
/// unbounded channel turned a stuck writer into unbounded memory growth./// Cursor motion (UDP) is loss-tolerant by design and simply drops when
/// the queue is full; a *reliable* frame dropped here means the writer
/// is this many frames behind — a dead connection in practice — and the
/// existing silence timeouts reap the client shortly after.
const OUT_QUEUE_CAP: usize = 1024;

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
    /// (TCP) and cursor-stream frames (UDP), in enqueue order. Bounded
    /// ([`OUT_QUEUE_CAP`]) and drained by the writer thread.
    pub out: SyncSender<Outbound>,
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
/// queue is bounded and full queues drop; see [`OUT_QUEUE_CAP`]).
/// Unknown client = gone.
pub fn enqueue(clients: &Arc<Mutex<HashMap<u8, Arc<Client>>>>, id: u8, msg: Message) {
    let Some(client) = clients.lock().unwrap().get(&id).cloned() else { return };
    match route(msg) {
        Outbound::Udp(m) => {
            // Motion is loss-tolerant by design: a full queue drops the
            // frame, the next one self-heals.
            let _ = client.out.try_send(Outbound::Udp(m));
        }
        Outbound::Tcp(m) => {
            if client.out.try_send(Outbound::Tcp(m)).is_err() {
                // A full *reliable* queue means the writer is wedged
                // far behind. Dropping keeps the input path live — it
                // must never block on a stuck client — and the silence
                // timeouts end the session shortly after.
                log_warn!("client {id}: outbound queue full — dropping a control frame");
            }
        }
    }
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
    /// The **authenticated** transport peers: client id → the IP its TCP
    /// handshake came from, recorded at handshake time. The UDP cursor
    /// stream is accepted only from these: the id inside a UDP datagram
    /// is client-supplied (anyone on the network can put a connected
    /// client's id in a packet and forge beacons that drive edge
    /// crossings), but the source *IP* of a datagram cannot be chosen by
    /// the sender — so binding datagrams to the handshake's IP turns the
    /// id from a claim into a verified identity. Cleaned up wherever the
    /// client itself is (teardown, operator disconnect).
    pub tcp_ips: Arc<Mutex<HashMap<u8, IpAddr>>>,
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
        // Last words on the way out, in this order:
        //
        // 1. `Leave` — restores this client's own input the moment the
        //    message lands, instead of waiting for its process to end
        //    (the fresh injector a reconnect builds would do it too,
        //    but a person sitting at the dropped machine should not
        //    wait even a second for their keyboard back).
        // 2. `Control{RECONNECT}` — the client's app loop returns
        //    SessionEnd::Reconnect for it and re-handshakes immediately
        //    instead of after its full retry delay (and, on Wi-Fi, ~3 s
        //    later is often already a different AP). An unattended drop
        //    must heal itself without the operator watching a stuck
        //    "connecting…" state. The socket is often dead by now, so
        //    these sends usually land nowhere; they matter exactly in
        //    the window where the link is half-alive (a silence timeout
        //    on a dozing Wi-Fi NIC) — precisely the case where the
        //    client is *not* already reconnecting on its own.
        //
        // The reader thread for this connection ends right after this
        // (teardown is its tail), so nothing can enqueue behind these.
        let _ = client.out.try_send(route(Message::Leave { screen_id: id }));
        let _ = client.out.try_send(route(Message::Control {
            command: kvmshare_protocol::id::control::RECONNECT,
        }));
        // Unregister atomically: the identity check and the removals must
        // share one `clients` lock acquisition. The removals used to be
        // separate by-id deletes after a dropped-lock identity check — a
        // replacement connection that registered in that window was
        // wiped out by the stale connection's teardown (its fresh
        // `tcp_ips` entry included), leaving a "connected" client the
        // cursor stream could never reach.
        {
            let mut clients = self.clients.lock().unwrap();
            match clients.get(&id) {
                Some(c) if Arc::ptr_eq(c, client) => {
                    clients.remove(&id);
                    self.tcp_ips.lock().unwrap().remove(&id);
                    self.addrs.lock().unwrap().remove(&id);
                    self.seqs.lock().unwrap().remove(&id);
                    self.last_heard.lock().unwrap().remove(&id);
                }
                _ => {
                    // Superseded while the farewell messages were being
                    // queued — the fresh registration owns the id now.
                    log_debug!("client {}: superseded during teardown — fresh registration wins", client.name);
                    return;
                }
            }
        }
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
                let _ = apply_action(
                    action,
                    &self.active,
                    &self.clients,
                    &self.last_heard,
                    &mut engine,
                    self.events.as_ref(),
                );
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
        let tcp_peer = stream.peer_addr()?;
        let addr = tcp_peer.to_string();
        let mut transport = Transport::with_read_timeout(stream, Some(CLIENT_READ_TIMEOUT))?;
        let (id, machine_id, name, info, admitted) = exchange_hello(&mut transport, &ctx, tcp_peer)?;
        ctx.session.lock().unwrap().update_screen_info(id, info.clone());
        // The transport identity is now proven: from here the UDP stream
        // for this id is accepted only from this peer's IP (see
        // [`ClientCtx::tcp_ips`]). The mapping is recorded *with* the
        // registration below (one lock scope), so a stale connection's
        // teardown can never delete it mid-handshake.

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
        let (out_tx, out_rx) = mpsc::sync_channel::<Outbound>(OUT_QUEUE_CAP);
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
            // Registration and the UDP-identity mapping land together:
            // a teardown that runs before this sees no registered client
            // under this id (its by-id cleanup is harmless), and one that
            // runs after sees a different Arc and touches nothing.
            ctx.tcp_ips.lock().unwrap().insert(id, tcp_peer.ip());
            if let Some(old) = clients.insert(id, client.clone()) {
                log_info!("client {}: replacing stale connection with the same id", old.name);
                let _ = old.out.try_send(route(Message::Leave { screen_id: id }));
                let _ = old.out.try_send(route(Message::Control {
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
                // try_send: a full queue must never block a handshake —
                // a dropped Layout is refreshed by the next reload or
                // ScreenInfo exchange.
                let _ = c.out.try_send(route(Message::Layout { layout: layout.clone() }));
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
    peer: SocketAddr,
) -> io::Result<(u8, String, String, ScreenInfo, bool)> {
    let (machine_id, name, info) = match transport.recv()? {
        RecvResult::Msg(Message::Hello { version, id, name, info }) => {
            if !kvmshare_protocol::compatible(version) {
                let _ = transport.send(&Message::Error {
                    code: errors::VERSION_MISMATCH,
                    text: format!(
                        "this server speaks protocol v{} (accepts v{}–v{}) and the client speaks v{version} — update the older machine",
                        kvmshare_protocol::VERSION,
                        kvmshare_protocol::MIN_PROTOCOL,
                        kvmshare_protocol::MAX_PROTOCOL,
                    ),
                });
                return Err(io::Error::other(format!(
                    "client refused: protocol version mismatch (server v{}, client v{version})",
                    kvmshare_protocol::VERSION
                )));
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
    // before any layout state is touched. The peer's SocketAddr is used
    // as-is — parsing "host:port" strings by hand breaks on IPv6
    // ("[::1]:24800" splits at the first ':'), the typed value cannot.
    if ctx.policy.lock().unwrap().local_only && !is_local_ip(peer.ip()) {
        let _ = transport.send(&Message::Error {
            code: errors::NOT_LOCAL,
            text: format!("connection from {peer} refused — only local-network peers are accepted"),
        });
        return Err(io::Error::other(format!("client {peer} is not on the local network")));
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

/// Is this IP on the local network? Accepts IPv4 RFC1918 private ranges
/// (10/8, 172.16/12, 192.168/16), loopback and link-local, plus IPv6
/// loopback, ULA (fc00::/7) and link-local (fe80::/10). Anything else is
/// "remote". Note the honest meaning: **private-address**, not provably
/// same-subnet — a VPN or a routed private network also presents these
/// ranges (see the trust-model notes in the wire-protocol docs).
fn is_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let o = v6.octets();
            v6.is_loopback()
                || v6.is_unique_local()
                || (o[0] == 0xfe && (o[1] & 0xc0) == 0x80)
        }
    }
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