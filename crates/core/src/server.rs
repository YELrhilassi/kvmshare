//! The server side.
//!
//! Owns the [`Session`] and the set of connected clients. Each client has
//! **two links**:
//!
//! * **TCP** — the reliable control channel: handshake, layout,
//!   Enter/Leave, buttons, keys, wheel, clipboard, keepalive. Ordered and
//!   lossless by nature, and quiet enough that backpressure is never a
//!   concern.
//! * **UDP** — the cursor stream: relative mouse motion out, real-cursor
//!   beacons in. Both are *additive and loss-tolerant*, so they never
//!   need retransmission — and never subject the cursor's latency to the
//!   reliable stream's buffering or a busy peer's TCP backpressure
//!   (which is what turned smooth motion into clumps and stalls under
//!   load in earlier designs). See [`crate::udp`] for the datagram
//!   envelope.
//!
//! Everything the session says goes into a per-client **outbound queue**
//! drained by that client's writer thread, which owns the TCP socket and
//! the UDP address. The main input loop therefore never blocks on the
//! network: a wedged client can delay its own frames, never the input
//! path.
//!
//! One thread per client reads the TCP control channel (blocking IO is
//! plenty for a KVM — a handful of control messages per second). A single
//! receiver thread owns the UDP socket: it learns each client's UDP
//! address from its first datagram (sent right after the handshake),
//! routes beacons to the session, and executes any crossing the beacon
//! fires (a beacon that parks the cursor on a wall mid-push crosses on
//! the park itself — the client's position stream is the only input that
//! may not be followed by another motion frame).

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use kvmshare_log::{log_debug, log_info, log_trace, log_warn};
use kvmshare_protocol::id::errors;
use kvmshare_protocol::message::{Layout, Message};

use crate::layout::Layout as Desktop;
use crate::session::{Action, Session};
use crate::transport::{RecvResult, Transport};
use crate::udp;

/// The platform hook the server calls to control the *local* machine.
pub trait Engine: Send {
    /// Warp the local cursor to a local-screen position.
    fn warp_local(&mut self, x: i32, y: i32);
    /// Take (or release) exclusive ownership of the local keyboard and
    /// pointer while the cursor is on a client screen. Without it, the
    /// same physical input would act on the local desktop *and* be
    /// forwarded — clicks and typing would land on both machines at
    /// once. A best-effort call: platforms that cannot grab (yet) simply
    /// do nothing.
    fn grab_input(&mut self, grabbed: bool);
    /// Isolate (or release) the physical input devices from the local
    /// desktop entirely while the cursor is on a client — stronger than
    /// [`Engine::grab_input`], because it also stops *raw* event
    /// delivery to apps that read it directly (browsers, smooth-scroll
    /// terminals), which no grab can suppress. Best-effort: platforms
    /// without kernel device isolation do nothing.
    fn isolate_input(&mut self, _isolated: bool) {}

    /// Hide/show the local cursor while away / at home.
    fn show_local_cursor(&mut self, visible: bool);
    /// Put `data` into the local clipboard (copied on a client).
    fn clipboard_set(&mut self, mime: &str, data: &[u8]);
    /// Read the current local clipboard, if any.
    fn clipboard_get(&mut self) -> Option<(String, Vec<u8>)>;
    /// The last clipboard content applied from a *remote* source (set via
    /// [`Engine::clipboard_set`]). Clipboard pollers compare against this
    /// so content that arrived from a peer is never echoed back to it.
    fn clipboard_last_injected(&mut self) -> Option<(String, Vec<u8>)>;
}

/// The server's view of one connected client.
struct Client {
    id: u8,
    name: String,
    /// Everything destined for this client: reliable control frames
    /// (TCP) and cursor-stream frames (UDP), in enqueue order. Drained
    /// by the writer thread.
    out: Sender<Outbound>,
}

/// One outbound item for a client.
enum Outbound {
    /// Reliable control frame (TCP).
    Tcp(Message),
    /// Loss-tolerant cursor frame (UDP) — only relative motion.
    Udp(Message),
}

/// Control messages from the app layer (never travel over the wire).
#[derive(Debug)]
pub enum Control {
    /// The config changed on disk — adopt this new desktop layout now.
    Reload(Desktop),
}

/// How often the main loop polls for control messages while idle.
const CONTROL_POLL: Duration = Duration::from_millis(100);

/// A running server.
pub struct Server {
    listener: TcpListener,
    udp: Arc<UdpSocket>,
    session: Arc<Mutex<Session>>,
    clients: Arc<Mutex<HashMap<u8, Arc<Client>>>>,
    /// Id of the client the cursor is currently on (`None` = local).
    active: Arc<Mutex<Option<u8>>>,
    /// Client id → UDP address, learned from each client's first
    /// datagram. The writers need it to route cursor-stream frames.
    udp_addrs: Arc<Mutex<HashMap<u8, SocketAddr>>>,
    /// Client id → last applied beacon sequence (stale/duplicate UDP
    /// datagrams are dropped, so an out-of-order "at the wall" report can
    /// never arm a crossing the user did not push for).
    udp_seqs: Arc<Mutex<HashMap<u8, u32>>>,
    /// App-layer control messages (hot reload). `None` disables them.
    /// In a `Mutex` so `Server` stays `Sync` (the channel itself is not).
    control: Mutex<Option<Receiver<Control>>>,
    /// Measures the server's pointer transform (px per device count)
    /// from the capture stream; the session scales forwarded motion by
    /// it so the client's cursor mirrors the server's (see [`GainTracker`]).
    /// In an `Arc<Mutex>` so `run(&self)` can feed it (the main loop is
    /// the only writer).
    gain: Arc<std::sync::Mutex<crate::motion::GainTracker>>,
}

impl Server {
    /// Bind without a control channel (no hot reload).
    pub fn bind(session: Session, port: u16) -> io::Result<Self> {
        Self::with_control(session, port, None)
    }

    /// Bind with an optional app-layer control channel. The TCP listener
    /// and the UDP cursor socket share one port (they are independent
    /// protocol namespaces).
    pub fn with_control(
        session: Session,
        port: u16,
        control: Option<Receiver<Control>>,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        let udp_port = listener.local_addr()?.port();
        let udp = Arc::new(UdpSocket::bind(("0.0.0.0", udp_port))?);
        // Non-blocking sends: the writer thread must never stall the
        // cursor stream on a full socket buffer (congestion, a slow
        // peer). Motion is loss-tolerant — dropping a frame is always
        // better than delaying the next hundred.
        udp.set_nonblocking(true)?;
        Ok(Self {
            listener,
            udp,
            session: Arc::new(Mutex::new(session)),
            clients: Arc::new(Mutex::new(HashMap::new())),
            active: Arc::new(Mutex::new(None)),
            udp_addrs: Arc::new(Mutex::new(HashMap::new())),
            udp_seqs: Arc::new(Mutex::new(HashMap::new())),
            control: Mutex::new(control),
            gain: Arc::new(std::sync::Mutex::new(crate::motion::GainTracker::new())),
        })
    }

    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// How many clients are currently connected (useful for the GUI's
    /// connection status and for tests to wait for registration).
    pub fn client_count(&self) -> usize {
        self.clients.lock().unwrap().len()
    }

    /// Run the server forever. `input` delivers local input events from
    /// the platform; `engine` lets us control the local cursor and is
    /// shared so other threads (client handlers, the UDP receiver, the
    /// app's clipboard poller) can reach it too.
    pub fn run(&self, input: Receiver<Message>, engine: Arc<Mutex<Box<dyn Engine>>>) -> io::Result<()> {
        // Accept clients on a background thread.
        let (listener, session, clients, active, engine_accept) = (
            self.listener.try_clone()?,
            self.session.clone(),
            self.clients.clone(),
            self.active.clone(),
            engine.clone(),
        );
        let udp_accept = self.udp.clone();
        let addrs_accept = self.udp_addrs.clone();
        let seqs_accept = self.udp_seqs.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        let addr = s.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| "?".into());
                        if let Err(e) = Client::spawn(
                            s,
                            session.clone(),
                            clients.clone(),
                            active.clone(),
                            engine_accept.clone(),
                            udp_accept.clone(),
                            addrs_accept.clone(),
                            seqs_accept.clone(),
                        ) {
                            log_warn!("client {addr}: {e}");
                        }
                    }
                    Err(e) => log_warn!("accept error: {e}"),
                }
            }
        });

        // Route the UDP cursor stream on its own thread.
        let (session_udp, clients_udp, active_udp, engine_udp) = (
            self.session.clone(),
            self.clients.clone(),
            self.active.clone(),
            engine.clone(),
        );
        let addrs_udp = self.udp_addrs.clone();
        let seqs_udp = self.udp_seqs.clone();
        let udp_sock = self.udp.clone();
        thread::spawn(move || udp_receiver(udp_sock, session_udp, clients_udp, active_udp, engine_udp, addrs_udp, seqs_udp));

        // Main loop: process local input. The engine lock is taken per
        // event (not held for the whole loop) so other threads — client
        // threads applying remote clipboard content, the UDP receiver
        // executing beacon crossings, and the app's clipboard poller —
        // can reach the engine between events. Idle timeouts also drain
        // the app-layer control channel (hot reload).
        loop {
            match input.recv_timeout(CONTROL_POLL) {
                Ok(msg) => {
                    // Every message (motion deltas + position beacons from
                    // the capture) goes straight to the session. Nothing
                    // here touches the engine: the platform feeds the
                    // real pointer position through its own beacons, and
                    // per-event X round-trips would stall the cursor the
                    // moment the local desktop gets busy.
                    //
                    // First, feed the pointer-gain measurement: raw
                    // deltas vs the real-position beacons give the
                    // server's own px-per-count, which the session
                    // applies to forwarded motion so the client's cursor
                    // mirrors the server's.
                    match &msg {
                        Message::MouseMoveRel { dx, dy } => self.gain.lock().unwrap().on_delta(*dx, *dy),
                        Message::MouseMoveAbs { x, y } => {
                            let g = self.gain.lock().unwrap().on_beacon(*x, *y);
                            self.session.lock().unwrap().set_gain(g);
                        }
                        _ => {}
                    }
                    let actions = { self.session.lock().unwrap().on_local_event(msg) };
                    let mut engine = engine.lock().unwrap();
                    for action in actions {
                        self.execute(action, &mut engine)?;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Drain the app-layer control channel, then apply each
                    // command on this (serialized) thread.
                    let cmds: Vec<Control> = {
                        let mut control = self.control.lock().unwrap();
                        let mut v = Vec::new();
                        if let Some(rx) = control.as_mut() {
                            while let Ok(cmd) = rx.try_recv() {
                                v.push(cmd);
                            }
                        }
                        v
                    };
                    for cmd in cmds {
                        self.apply_control(cmd, &engine)?;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(())
    }

    /// Apply an app-layer control message. Runs on the main loop thread
    /// so it is serialized with input processing.
    fn apply_control(
        &self,
        cmd: Control,
        engine: &Arc<Mutex<Box<dyn Engine>>>,
    ) -> io::Result<()> {
        let Control::Reload(layout) = cmd;
        let screen_count = layout.screens.len();
        log_info!("layout reloaded: {screen_count} screens");

        // 1. Let the session adopt the new layout; it may ask us to bring
        //    the cursor home (it was on a client).
        let actions = { self.session.lock().unwrap().swap_layout(layout) };
        {
            let mut engine = engine.lock().unwrap();
            for action in actions {
                self.execute(action, &mut engine)?;
            }
        }

        // 2. Drop clients that no longer exist in the layout (their name
        //    or id changed), so no ghost connections linger.
        self.drop_stale_clients();

        // 3. Tell every remaining client about the new layout.
        let layout = {
            let s = self.session.lock().unwrap();
            Layout { screens: s.layout().screens.clone() }
        };
        self.broadcast(&Message::Layout { layout })
    }

    /// Disconnect clients whose screen disappeared from the new layout
    /// (their id no longer maps to a screen with the same name), so no
    /// ghost connections linger after a reload.
    fn drop_stale_clients(&self) {
        let gone: Vec<u8> = {
            let session = self.session.lock().unwrap();
            let clients = self.clients.lock().unwrap();
            clients
                .iter()
                .filter(|(_, c)| {
                    !session
                        .layout()
                        .screens
                        .iter()
                        .any(|s| s.id == c.id && s.name == c.name)
                })
                .map(|(id, _)| *id)
                .collect()
        };
        for id in &gone {
            enqueue(&self.clients, *id, Message::Leave { screen_id: *id });
            self.clients.lock().unwrap().remove(id);
            self.udp_addrs.lock().unwrap().remove(id);
            self.udp_seqs.lock().unwrap().remove(id);
            let mut act = self.active.lock().unwrap();
            if *act == Some(*id) {
                *act = None;
            }
        }
        if !gone.is_empty() {
            log_info!("dropped {} stale client(s) after reload", gone.len());
        }
    }

    /// Send a message to every connected client (e.g. layout or
    /// clipboard broadcasts from the app layer).
    pub fn broadcast(&self, msg: &Message) -> io::Result<()> {
        let clients = self.clients.lock().unwrap();
        for c in clients.values() {
            let item = route(msg.clone());
            let _ = c.out.send(item);
        }
        Ok(())
    }

    /// Apply a session [`Action`] to the world.
    fn execute(&self, action: Action, engine: &mut MutexGuard<'_, Box<dyn Engine>>) -> io::Result<()> {
        apply_action(action, &self.active, &self.clients, engine)
    }
}

/// Which link a message travels on: everything is a reliable control
/// frame except the additive cursor motion.
fn route(msg: Message) -> Outbound {
    if matches!(msg, Message::MouseMoveRel { .. }) {
        Outbound::Udp(msg)
    } else {
        Outbound::Tcp(msg)
    }
}

/// Push a message onto a client's outbound queue (never blocks — the
/// queue is unbounded; the writer drains it). Unknown client = gone.
fn enqueue(clients: &Arc<Mutex<HashMap<u8, Arc<Client>>>>, id: u8, msg: Message) {
    let Some(client) = clients.lock().unwrap().get(&id).cloned() else { return };
    let _ = client.out.send(route(msg));
}

/// Apply a session [`Action`] to the world. A free function so it can
/// run from the main loop and from the client/UDP threads (which hold
/// only the shared state, not the whole `Server`).
fn apply_action(
    action: Action,
    active: &Arc<Mutex<Option<u8>>>,
    clients: &Arc<Mutex<HashMap<u8, Arc<Client>>>>,
    engine: &mut MutexGuard<'_, Box<dyn Engine>>,
) -> io::Result<()> {
    match action {
        Action::Nothing => {}
        Action::Send(msg) => {
            if let Some(id) = *active.lock().unwrap() {
                // Relative mouse motion is the hot path (up to hundreds
                // per second) — not even trace logs those.
                if !matches!(msg, Message::MouseMoveRel { .. }) {
                    log_trace!("forward {msg:?} -> client {id}");
                }
                enqueue(clients, id, msg);
            }
        }
        Action::SwitchTo { to, x, y } => {
            // Leave whatever screen we are on.
            if let Some(old) = active.lock().unwrap().take() {
                enqueue(clients, old, Message::Leave { screen_id: old });
            }
            *active.lock().unwrap() = Some(to);
            log_debug!("cursor switched to client {to} at ({x},{y})");
            // `Enter` carries the entry point; the client places its
            // cursor there itself (absolute placement is reserved for
            // entry — the motion stream that follows is relative).
            enqueue(clients, to, Message::Enter { screen_id: to, x, y });
            // From here on, local input must only reach the client:
            // grab the pointer+keyboard so the local desktop does not
            // act on the same physical events being forwarded, and
            // isolate the physical devices at the kernel so even
            // raw-reading apps (browsers, terminals) see nothing.
            engine.grab_input(true);
            engine.isolate_input(true);
            // Hide the local cursor **in place** — it is already at
            // the shared edge where it crossed. Warping it (even
            // hidden) would sweep hover/enter effects across local
            // windows; warping it *before* hiding, as an earlier
            // design did, visibly dashed the cursor to the screen
            // center on every crossing.
            engine.show_local_cursor(false);
        }
        Action::SwitchToLocal { x, y } => {
            if let Some(old) = active.lock().unwrap().take() {
                enqueue(clients, old, Message::Leave { screen_id: old });
            }
            log_debug!("cursor back to local screen at ({x},{y})");
            // Control is home: local input belongs to the local
            // desktop again. Release the kernel isolation first so
            // the warp below reaches the desktop.
            engine.isolate_input(false);
            engine.grab_input(false);
            engine.warp_local(x, y);
            engine.show_local_cursor(true);
        }
    }
    Ok(())
}

/// The UDP receiver: learns each client's address from its first
/// datagram, routes real-cursor beacons to the session (dropping stale
/// or duplicate frames by sequence number), and executes any crossing a
/// beacon fires — a beacon that parks the real cursor on a wall mid-push
/// must not wait for the next motion frame, which may never come (the
/// user stopped exactly at the wall).
fn udp_receiver(
    udp: Arc<UdpSocket>,
    session: Arc<Mutex<Session>>,
    clients: Arc<Mutex<HashMap<u8, Arc<Client>>>>,
    active: Arc<Mutex<Option<u8>>>,
    engine: Arc<Mutex<Box<dyn Engine>>>,
    addrs: Arc<Mutex<HashMap<u8, SocketAddr>>>,
    seqs: Arc<Mutex<HashMap<u8, u32>>>,
) {
    let mut buf = [0u8; 1500];
    loop {
        match udp.recv_from(&mut buf) {
            Ok((n, from)) => {
                let Some(d) = udp::unpack(&buf[..n]) else { continue };
                // Only datagrams from a known client count. Learning the
                // address happens here too — the first datagram is the
                // registration the client sends right after the
                // handshake.
                {
                    let clients = clients.lock().unwrap();
                    if !clients.contains_key(&d.id) {
                        continue;
                    }
                }
                // Learn or verify the datagram's source. The first
                // datagram from a client teaches us its address; a
                // datagram from a *different* address is either a stale
                // frame from a previous session (still draining the
                // socket buffer after a disconnect) or a fresh
                // registration from a reconnect. Either way the old
                // sequence space belongs to the old address — reset it
                // and adopt the new source. Without this, a late frame
                // from a dead session re-creates the seq tracker at its
                // high value and every fresh beacon (starting at 1) is
                // judged stale: the live session is deafened.
                {
                    let mut addrs = addrs.lock().unwrap();
                    match addrs.get(&d.id) {
                        None => {
                            addrs.insert(d.id, from);
                            log_debug!("client {} registered UDP stream from {from}", d.id);
                        }
                        Some(addr) if *addr != from => {
                            seqs.lock().unwrap().remove(&d.id);
                            addrs.insert(d.id, from);
                            log_debug!("client {} re-registered UDP stream from {from}", d.id);
                        }
                        Some(_) => {}
                    }
                }
                match d.msg {
                    Message::CursorPos { x, y } => {
                        // Stale or duplicate beacons are dropped: a late
                        // "at the wall" report must never arm a crossing.
                        {
                            let mut seqs = seqs.lock().unwrap();
                            let last = seqs.entry(d.id).or_default();
                            if !udp::is_newer(d.seq, *last) {
                                continue;
                            }
                            *last = d.seq;
                        }
                        // The client's *real* cursor position drives
                        // remote edge crossings. Session state is updated
                        // here; a crossing fires either on the next
                        // outward delta in the main loop or — when the
                        // beacon parks the cursor on a wall mid-push —
                        // right here, on the park itself.
                        let actions = { session.lock().unwrap().on_remote_beacon(d.id, x, y) };
                        if !actions.is_empty() {
                            if let Ok(mut engine) = engine.lock() {
                                for a in actions {
                                    if let Err(e) = apply_action(a, &active, &clients, &mut engine) {
                                        log_warn!("beacon crossing for client {}: {e}", d.id);
                                    }
                                }
                            }
                        }
                    }
                    // Registration frames and anything else that happens
                    // to ride UDP are acknowledged by existing; nothing
                    // to do here.
                    _ => {}
                }
            }
            // Non-blocking socket: WouldBlock is the normal idle state,
            // not an error — yield briefly and check again. The sleep is
            // kept short so a beacon parked at a wall is answered within
            // ~1 ms (crossing latency is invisible at that scale) while
            // the thread still yields the CPU when nothing is flowing.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                log_trace!("udp receiver idle");
                thread::sleep(Duration::from_millis(1));
            }
            Err(e) => {
                log_warn!("udp receiver: {e}");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

impl Client {
    fn spawn(
        stream: TcpStream,
        session: Arc<Mutex<Session>>,
        clients: Arc<Mutex<HashMap<u8, Arc<Client>>>>,
        active: Arc<Mutex<Option<u8>>>,
        engine: Arc<Mutex<Box<dyn Engine>>>,
        udp: Arc<UdpSocket>,
        addrs: Arc<Mutex<HashMap<u8, SocketAddr>>>,
        seqs: Arc<Mutex<HashMap<u8, u32>>>,
    ) -> io::Result<()> {
        let mut transport = Transport::new(stream)?;

        // 1. Handshake: expect Hello with a matching protocol version.
        let (name, info) = match transport.recv()? {
            RecvResult::Msg(Message::Hello { version, name, info }) => {
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
                (name, info)
            }
            RecvResult::Msg(_) => return Err(io::Error::other("expected hello")),
            RecvResult::Eof | RecvResult::NoData => return Err(io::Error::other("client closed before hello")),
        };

        // 2. The client's screen id comes from the layout, matched by name.
        let id = match session.lock().unwrap().assign_screen_id(&name) {
            Some(id) => id,
            None => {
                let _ = transport.send(&Message::Error {
                    code: errors::NAME_CONFLICT,
                    text: format!("no screen named \"{name}\" in the layout"),
                });
                return Err(io::Error::other(format!("unknown client name {name}")));
            }
        };
        // The client's real screen shape (may differ from the config).
        session.lock().unwrap().update_screen_info(id, info);

        // 3. Send Welcome + current layout, then hand both sockets to a
        //    writer thread. The transport stays the writer's; the reader
        //    gets its own lock-free socket clone (TCP is full-duplex), so
        //    it can block on recv while the writer sends freely.
        let layout = {
            let s = session.lock().unwrap();
            Layout { screens: s.layout().screens.clone() }
        };
        transport.send(&Message::Welcome {
            server_version: kvmshare_protocol::VERSION,
            layout: layout.clone(),
            own_screen_id: id,
        })?;
        let mut reader = transport.reader()?;
        let (out_tx, out_rx) = mpsc::channel::<Outbound>();
        // The reader thread owns teardown, so it needs the UDP routing
        // state too (the writer gets the originals).
        let addrs2 = addrs.clone();
        let seqs2 = seqs.clone();
        spawn_writer(id, transport, udp, addrs, out_rx);
        let client = Arc::new(Client { id, name, out: out_tx });
        clients.lock().unwrap().insert(id, client.clone());
        // Stable marker for the GUI's notification watcher (kept in sync
        // with the "disconnected" line below): "client X connected".
        log_info!("client {} connected", client.name);

        // 4. Service the client's TCP control channel until it goes away.
        //    On EOF (or error), unregister and give the session a chance
        //    to return home if this was the active client.
        let c2 = client.clone();
        let clients2 = clients.clone();
        let active2 = active.clone();
        let session2 = session.clone();
        let engine2 = engine.clone();
        thread::spawn(move || {
            loop {
                let msg = match reader.recv() {
                    Ok(RecvResult::Msg(msg)) => msg,
                    Ok(RecvResult::NoData) => continue,
                    Ok(RecvResult::Eof) | Err(_) => break,
                };
                match msg {
                    Message::KeepAlive => {}
                    Message::ScreenInfo { info } => {
                        // The client's resolution changed: rebuild the
                        // layout so edge math stays correct, then reply.
                        let layout = {
                            let mut s = session2.lock().unwrap();
                            s.update_screen_info(c2.id, info);
                            Layout { screens: s.layout().screens.clone() }
                        };
                        enqueue(&clients2, c2.id, Message::Layout { layout });
                    }
                    Message::Clipboard { mime, data } => {
                        // Content copied on the client reaches the
                        // server's local clipboard.
                        log_debug!("clipboard from {}: {} ({} bytes)", c2.name, mime, data.len());
                        if let Ok(mut engine) = engine2.lock() {
                            engine.clipboard_set(&mime, &data);
                        }
                    }
                    // Defensive: current clients send beacons over UDP;
                    // keep the TCP arm for robustness (mixed or older
                    // peers, transport fallbacks).
                    Message::CursorPos { x, y } => {
                        session2.lock().unwrap().on_remote_beacon(c2.id, x, y);
                    }
                    _ => {}
                }
            }

            log_info!("client {} disconnected", c2.name);
            // Unregister and give the session a chance to return home if
            // this was the active client. Dropping the last `Sender` of
            // the outbound queue ends the writer thread (its channel
            // closes and the socket goes with it). The UDP routing state
            // goes too — a later reconnect re-registers its (fresh)
            // address and sequence space.
            clients2.lock().unwrap().remove(&c2.id);
            addrs2.lock().unwrap().remove(&c2.id);
            seqs2.lock().unwrap().remove(&c2.id);

            {
                let mut act = active2.lock().unwrap();
                if *act == Some(c2.id) {
                    *act = None;
                }
            }
            let action = session2.lock().unwrap().on_client_disconnected(c2.id);
            if let Action::SwitchToLocal { .. } = action {
                if let Ok(mut engine) = engine2.lock() {
                    let _ = apply_action(action, &active2, &clients2, &mut engine);
                }
            }
        });

        Ok(())
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