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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use kvmshare_log::{log_debug, log_error, log_info, log_trace, log_warn};
use kvmshare_protocol::id::errors;
use kvmshare_protocol::message::{Layout, Message, ScreenInfo};

use crate::layout::Layout as Desktop;
use crate::session::{Action, Session};
use crate::time::now_ms;
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
}

/// A shared handle to the server's clipboard service.
///
/// The clipboard is deliberately **not** part of [`Engine`]: reading or
/// writing the system clipboard can block for a long time (an X11
/// selection owner busy or gone, another process holding the clipboard
/// open), and the engine lock serializes the *entire input path* — a
/// clipboard call under that lock would freeze every cursor motion on
/// every client for as long as the call blocks. The clipboard lives
/// behind its own lock instead, serviced by its own thread (the app's
/// poller) and only ever touched by clipboard work.
pub type ServerClipboard = Arc<Mutex<Box<dyn crate::clipboard::Clipboard>>>;

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
/// Heartbeats the server supervisor watches. Each field is a millisecond
/// timestamp (see [`now_ms`]) updated by its owning thread every loop
/// iteration. A tick that goes stale while the cursor is on a client
/// means that thread is wedged — and a wedged input-path thread while
/// remote can leave this machine's keyboard and mouse trapped (the
/// engine lock blocks crossings, or the capture thread holds the local
/// input grab forever), so the supervisor recovers by exiting cleanly
/// (see [`supervisor_loop`]).
#[derive(Default)]
pub struct Liveness {

    /// The main input loop (wakes every ≤ [`CONTROL_POLL`]).
    pub loop_tick_ms: AtomicU64,
    /// The platform's capture thread (wakes every ~2 ms). This is the
    /// thread that owns the local input grab while remote — the one
    /// whose wedge traps the machine. Shared (`Arc`) because the
    /// platform creates and owns the thread that writes it.
    pub capture_tick_ms: Arc<AtomicU64>,
}

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

/// How often the supervisor wakes to check the heartbeats.
const SUPERVISOR_POLL: Duration = Duration::from_millis(500);
/// A heartbeat older than this (ms) while remote is a genuine wedge: a
/// healthy main loop wakes every [`CONTROL_POLL`] and a healthy capture
/// loop every ~2 ms, so 3 s means the thread has missed hundreds of
/// wakes (never a false positive from load).
const SUPERVISOR_STALL_MS: u64 = 3000;
/// Exit code the supervisor uses to ask the process manager (the GUI)
/// for a restart. Distinct from a crash (1) so the manager can tell a
/// deliberate recovery from a failure.
pub const EXIT_RESTART: i32 = 66;

/// The watchdog for the server's input path. While the cursor is on a
/// client, the local machine must be able to bring it home: a wedged
/// main loop holds the engine lock that crossings need, and a wedged
/// capture thread holds the local input grab forever — either way the
/// local keyboard and mouse stay trapped until the process dies. The
/// supervisor shares nothing with those threads but the liveness
/// atomics, so whatever wedges them cannot block it. On a stall it
/// exits with [`EXIT_RESTART`]; process exit closes every fd and X
/// connection, which releases every kernel and X grab — the machine is
/// never left input-dead, and the process manager (the GUI) restarts a
/// clean server. Disabled when no real capture is present (test
/// harnesses): there is nothing to trap.
fn supervisor_loop(active: Arc<Mutex<Option<u8>>>, liveness: Arc<Liveness>) -> ! {
    loop {
        thread::sleep(SUPERVISOR_POLL);
        if liveness.capture_tick_ms.load(Ordering::Relaxed) == 0 {
            continue; // no real capture thread: nothing to guard
        }
        // `try_lock`: the supervisor must never block on a lock a wedged
        // thread holds — that would kill the watchdog itself. If the
        // active-state lock is held, assume the worst (remote) and let
        // the tick ages decide.
        let remote = match active.try_lock() {
            Ok(guard) => guard.is_some(),
            Err(_) => true,
        };
        if !remote {
            continue; // local idle is not a stall
        }
        let now = now_ms();
        let loop_age = now.saturating_sub(liveness.loop_tick_ms.load(Ordering::Relaxed));
        let capture_age = now.saturating_sub(liveness.capture_tick_ms.load(Ordering::Relaxed));
        if loop_age > SUPERVISOR_STALL_MS || capture_age > SUPERVISOR_STALL_MS {
            log_error!(
                "SUPERVISOR: input path stalled while the cursor is on a client (main loop {loop_age:?}, capture {capture_age:?}) — exiting with code {EXIT_RESTART} so the manager restarts a clean server; local input is never left trapped"
            );
            // Give the log writer a moment to drain the line, then exit
            // (which releases every grab via fd/connection close).
            thread::sleep(Duration::from_millis(120));
            std::process::exit(EXIT_RESTART);
        }
    }
}

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
    /// shared so other threads (client handlers, the UDP receiver) can
    /// reach it too. `clipboard` is the local clipboard service, on its
    /// own lock: inbound client clipboard is applied through it, and the
    /// app's poller reads through it — a stalled clipboard read can
    /// never hold the engine lock (which serializes every cursor
    /// motion).
    pub fn run(
        &self,
        input: Receiver<Message>,
        engine: Arc<Mutex<Box<dyn Engine>>>,
        clipboard: ServerClipboard,
        liveness: Arc<Liveness>,
    ) -> io::Result<()> {
        // The watchdog: shares nothing with the input threads but the
        // liveness atomics, so whatever wedges them cannot block it.
        // While the cursor is on a client it verifies the input path is
        // still alive; a wedged path exits with [`EXIT_RESTART`] so the
        // manager (GUI) restarts a clean server — process exit releases
        // every kernel and X grab, so the local machine is never left
        // input-dead.
        let supervisor = {
            let active = self.active.clone();
            let liveness = liveness.clone();
            thread::Builder::new()
                .name("kvmshare-server-supervisor".into())
                .spawn(move || supervisor_loop(active, liveness))
                .expect("cannot spawn server supervisor")
        };
        // Accept clients on a background thread.
        let listener = self.listener.try_clone()?;
        let ctx = Arc::new(ClientCtx {
            session: self.session.clone(),
            clients: self.clients.clone(),
            active: self.active.clone(),
            engine: engine.clone(),
            clipboard,
            addrs: self.udp_addrs.clone(),
            seqs: self.udp_seqs.clone(),
            last_heard: Arc::new(Mutex::new(HashMap::new())),
        });
        let udp_accept = self.udp.clone();
        let ctx_accept = ctx.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        let addr = s.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| "?".into());
                        if let Err(e) = Client::spawn(s, ctx_accept.clone(), udp_accept.clone()) {
                            log_warn!("client {addr}: {e}");
                        }
                    }
                    Err(e) => log_warn!("accept error: {e}"),
                }
            }
        });

        // Route the UDP cursor stream on its own thread.
        let udp_sock = self.udp.clone();
        thread::spawn(move || udp_receiver(udp_sock, ctx));

        // Main loop: process local input. The engine lock is taken per
        // event (not held for the whole loop) so other threads — client
        // threads applying remote clipboard content, the UDP receiver
        // executing beacon crossings, and the app's clipboard poller —
        // can reach the engine between events. Idle timeouts also drain
        // the app-layer control channel (hot reload).
        loop {
            liveness.loop_tick_ms.store(now_ms(), Ordering::Relaxed);
            match input.recv_timeout(CONTROL_POLL) {
                Ok(msg) => self.handle_local_input(msg, &engine)?,
                Err(RecvTimeoutError::Timeout) => self.drain_controls(&engine)?,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        // Session over: the supervisor exits only via `process::exit`
        // (its whole point is recovering a wedged thread), so this join
        // is just a graceful-session cleanup.
        let _ = supervisor.join();
        Ok(())
    }

    /// One local input message: feed the pointer-gain measurement, run
    /// it through the session, and apply whatever the session decided.
    ///
    /// Every message (motion deltas + position beacons from the capture)
    /// goes straight to the session. Nothing here touches the engine
    /// beyond the apply step: the platform feeds the real pointer
    /// position through its own beacons, and per-event X round-trips
    /// would stall the cursor the moment the local desktop gets busy.
    /// The engine lock is held only for the apply step, so other threads
    /// (clipboard, beacon crossings) can reach the engine between events.
    fn handle_local_input(
        &self,
        msg: Message,
        engine: &Mutex<Box<dyn Engine>>,
    ) -> io::Result<()> {
        // First, feed the pointer-gain measurement: raw deltas vs the
        // real-position beacons give the server's own px-per-count,
        // which the session applies to forwarded motion so the client's
        // cursor mirrors the server's.
        match &msg {
            Message::MouseMoveRel { dx, dy } => self.gain.lock().unwrap().on_delta(*dx, *dy),
            Message::MouseMoveAbs { x, y } => {
                let g = self.gain.lock().unwrap().on_beacon(*x, *y);
                self.session.lock().unwrap().set_gain(g);
            }
            _ => {}
        }
        let actions = { self.session.lock().unwrap().on_local_event(msg) };
        // Diagnostic: the engine lock serializes the whole input path;
        // if another thread holds it for a long time (a slow platform
        // call under the lock), every motion event queues behind it —
        // the exact shape of a client cursor freeze. Flag any long wait.
        let started = std::time::Instant::now();
        let mut engine = engine.lock().unwrap();
        let waited = started.elapsed();
        if waited > std::time::Duration::from_millis(10) {
            log_warn!("input loop: engine lock held {waited:?} by another thread");
        }
        for action in actions {
            self.execute(action, &mut engine)?;
        }
        let took = started.elapsed();
        if took > std::time::Duration::from_millis(15) {
            log_warn!("input loop: message processing took {took:?}");
        }
        Ok(())
    }

    /// Drain the app-layer control channel (hot reload) and apply each
    /// command on this (serialized) thread.
    fn drain_controls(&self, engine: &Arc<Mutex<Box<dyn Engine>>>) -> io::Result<()> {
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
            self.apply_control(cmd, engine)?;
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
/// How long the active client's cursor stream may go silent before the
/// server drops it. The client beacons every ~8 ms while active, so this
/// is generous — a healthy stream can never trip it, and a genuinely
/// wedged client (its motion loop stuck, even though TCP keepalives
/// still flow) is caught in about a second. The drop returns control
/// home instead of leaving the cursor stranded on a client that cannot
/// move it.
const ACTIVE_BEACON_TIMEOUT: Duration = Duration::from_millis(1500);

/// Drop the active client when its cursor stream has been silent for
/// [`ACTIVE_BEACON_TIMEOUT`]. Called from the UDP receiver whenever the
/// stream is quiet. Mirrors the TCP silence drop in the reader thread —
/// but catches what that cannot: a client whose motion loop is wedged
/// while its control thread (and keepalives) are still alive.
fn check_active_beacon_staleness(ctx: &ClientCtx) {
    let active = *ctx.active.lock().unwrap();
    let Some(id) = active else { return };
    let now = now_ms();
    let last = ctx.last_heard.lock().unwrap().get(&id).copied();
    let Some(last) = last else { return };
    if now.saturating_sub(last) <= ACTIVE_BEACON_TIMEOUT.as_millis() as u64 {
        return;
    }
    log_warn!(
        "client {id}: cursor stream silent for {ACTIVE_BEACON_TIMEOUT:?} while active — dropping so control returns home"
    );
    // The reader thread's normal teardown path does the unregister +
    // return-home; triggering it from here (a forced disconnect) is the
    // same idempotent cleanup.
    let name = {
        let clients = ctx.clients.lock().unwrap();
        clients.get(&id).map(|c| c.name.clone()).unwrap_or_default()
    };
    ctx.teardown(id, &name);
}

fn udp_receiver(udp: Arc<UdpSocket>, ctx: Arc<ClientCtx>) {
    let mut buf = [0u8; 1500];
    loop {
        match udp.recv_from(&mut buf) {
            Ok((n, from)) => {
                let Some(d) = udp::unpack(&buf[..n]) else { continue };
                // Only datagrams from a known client count. Learning the
                // address happens here too — the first datagram is the
                // registration the client sends right after the
                // handshake.
                if !ctx.clients.lock().unwrap().contains_key(&d.id) {
                    continue;
                }
                // Any datagram from a known client proves its cursor
                // stream is alive (beacons flow continuously while it is
                // active). Tracked for the staleness watchdog above.
                ctx.last_heard.lock().unwrap().insert(d.id, now_ms());
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
                    let mut addrs = ctx.addrs.lock().unwrap();
                    match addrs.get(&d.id) {
                        None => {
                            addrs.insert(d.id, from);
                            log_debug!("client {} registered UDP stream from {from}", d.id);
                        }
                        Some(addr) if *addr != from => {
                            ctx.seqs.lock().unwrap().remove(&d.id);
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
                            let mut seqs = ctx.seqs.lock().unwrap();
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
                        let actions = { ctx.session.lock().unwrap().on_remote_beacon(d.id, x, y) };
                        if !actions.is_empty() {
                            if let Ok(mut engine) = ctx.engine.lock() {
                                for a in actions {
                                    if let Err(e) = apply_action(a, &ctx.active, &ctx.clients, &mut engine) {
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
                check_active_beacon_staleness(&ctx);
                thread::sleep(Duration::from_millis(1));
            }
            Err(e) => {
                log_warn!("udp receiver: {e}");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// Shared state one connected client's threads need. Bundled once at
/// accept time so neither the handshake nor the service loop carries a
/// nine-parameter signature, and so adding per-client state is a one-
/// place change.
struct ClientCtx {
    session: Arc<Mutex<Session>>,
    clients: Arc<Mutex<HashMap<u8, Arc<Client>>>>,
    active: Arc<Mutex<Option<u8>>>,
    engine: Arc<Mutex<Box<dyn Engine>>>,
    clipboard: ServerClipboard,
    /// UDP routing state: per-client datagram addresses and the
    /// anti-replay sequence counters. Owned jointly by the writer
    /// (address) and the UDP receiver (address + sequence).
    addrs: Arc<Mutex<HashMap<u8, SocketAddr>>>,
    seqs: Arc<Mutex<HashMap<u8, u32>>>,
    /// When each client's cursor stream was last heard (monotonic ms).
    /// The active client beacons every few ms; a stream gone silent is
    /// the signature of a wedged client — see the beacon watchdog in
    /// [`udp_receiver`].
    last_heard: Arc<Mutex<HashMap<u8, u64>>>,
}

impl ClientCtx {
    /// The current layout snapshot in wire form (for Welcome /
    /// ScreenInfo replies).
    fn layout_snapshot(&self) -> Layout {
        let s = self.session.lock().unwrap();
        Layout { screens: s.layout().screens.clone() }
    }

    /// Unregister the client everywhere and, if it had the cursor,
    /// bring the session home. Called by the reader thread exactly once
    /// when the control channel dies (EOF, error, or silence timeout).
    /// Dropping the last `Sender` of the outbound queue ends the writer
    /// thread, and the socket with it.
    fn teardown(&self, id: u8, name: &str) {
        log_info!("client {name} disconnected");
        self.clients.lock().unwrap().remove(&id);
        self.addrs.lock().unwrap().remove(&id);
        self.seqs.lock().unwrap().remove(&id);
        self.last_heard.lock().unwrap().remove(&id);

        {
            let mut act = self.active.lock().unwrap();
            if *act == Some(id) {
                *act = None;
            }
        }
        let action = self.session.lock().unwrap().on_client_disconnected(id);
        if let Action::SwitchToLocal { .. } = action {
            if let Ok(mut engine) = self.engine.lock() {
                let _ = apply_action(action, &self.active, &self.clients, &mut engine);
            }
        }
    }
}

impl Client {
    /// Full accept path for one connection: handshake, registration and
    /// the reader thread. Each stage is its own function below.
    fn spawn(
        stream: TcpStream,
        ctx: Arc<ClientCtx>,
        udp: Arc<UdpSocket>,
    ) -> io::Result<()> {
        // Timed reads: a client that stops sending (sleep, wedge, crash)
        // must be noticed and dropped so the session returns home — see
        // [`CLIENT_SILENT_TIMEOUT`].
        let mut transport = Transport::with_read_timeout(stream, Some(CLIENT_READ_TIMEOUT))?;
        let (id, name, info) = exchange_hello(&mut transport, &ctx)?;
        ctx.session.lock().unwrap().update_screen_info(id, info);

        // Send Welcome + current layout, then split the transport: the
        // writer keeps the sending half; the reader gets its own lock-
        // free socket clone (TCP is full-duplex), so it can block on
        // recv while the writer sends freely.
        transport.send(&Message::Welcome {
            server_version: kvmshare_protocol::VERSION,
            layout: ctx.layout_snapshot(),
            own_screen_id: id,
        })?;
        let reader = transport.reader()?;
        let (out_tx, out_rx) = mpsc::channel::<Outbound>();
        spawn_writer(id, transport, udp, ctx.addrs.clone(), out_rx);

        let client = Arc::new(Client { id, name, out: out_tx });
        ctx.clients.lock().unwrap().insert(id, client.clone());
        // Stable marker for the GUI's notification watcher (kept in sync
        // with the "disconnected" line in `teardown`): "client X connected".
        log_info!("client {} connected", client.name);
        service_client(client, reader, ctx);
        Ok(())
    }
}

/// Handshake: expect Hello with a matching protocol version, then
/// assign the screen id from the layout, matched by name.
fn exchange_hello(
    transport: &mut Transport,
    ctx: &ClientCtx,
) -> io::Result<(u8, String, ScreenInfo)> {
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
        RecvResult::Eof | RecvResult::NoData => {
            return Err(io::Error::other("client closed before hello"))
        }
    };
    let id = match ctx.session.lock().unwrap().assign_screen_id(&name) {
        Some(id) => id,
        None => {
            let _ = transport.send(&Message::Error {
                code: errors::NAME_CONFLICT,
                text: format!("no screen named \"{name}\" in the layout"),
            });
            return Err(io::Error::other(format!("unknown client name {name}")));
        }
    };
    Ok((id, name, info))
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
        ctx.teardown(client.id, &client.name);
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