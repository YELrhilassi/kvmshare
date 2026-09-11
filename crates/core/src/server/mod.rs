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
//!
//! ## Layout
//!
//! * [`engine`] — the local-machine control hook and the clipboard
//!   handle.
//! * [`liveness`] — heartbeats and the supervisor watchdog.
//! * [`client`] — one connected client: outbound queue, shared context,
//!   and the accept → service → teardown lifecycle.
//! * [`actions`] — the executor that turns session actions into reality.
//! * [`udp`] — the cursor-stream receiver and its beacon watchdog.

mod actions;
mod client;
mod engine;
mod liveness;
mod udp;

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use kvmshare_log::{log_info, log_warn};
use kvmshare_protocol::message::{Layout, Message, ScreenInfo};

use crate::layout::Layout as Desktop;
use crate::session::{Action, Session};
use crate::time::now_ms;

use actions::apply_action;
use client::{Client, ClientCtx};
use liveness::{supervisor_loop, CONTROL_POLL};

pub use engine::{Engine, ServerClipboard};
pub use liveness::{EXIT_RESTART, Liveness};

/// Control messages from the app layer (never travel over the wire).
#[derive(Debug)]
pub enum Control {
    /// The config changed on disk — adopt this new desktop layout,
    /// shortcut bindings and input preferences now.
    Reload(Desktop, crate::actions::BindSection, crate::input::InputPrefs),
    /// The `[network]` policy changed on disk — adopt it now. Separate
    /// from [`Control::Reload`] because it has a side effect a layout edit
    /// must never have: any *connected* client whose machine id is in the
    /// new `revoked_ids` is disconnected on the spot. Without this the
    /// policy was only read at startup, so trusting or revoking a machine
    /// silently did nothing until the server restarted.
    SetPolicy(Policy),
    /// Send an operational command to one connected client, looked up by
    /// its screen name. `command` is a [`kvmshare_protocol::id::control`]
    /// constant. The GUI writes these via the `server.cmd` control file.
    ClientCommand { name: String, command: u8 },
}

/// The server's connection policy, loaded from the `[network]` config
/// section. Decides which peers may connect at all.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Only accept clients whose **exact** screen name appears in the
    /// layout, plus trusted machine ids. When false, any name is
    /// admitted dynamically (the legacy plug-and-play behavior).
    pub allowlist: bool,
    /// Only accept connections from the local network (RFC1918 private
    /// ranges, loopback and link-local). Blocks WAN/bridged peers.
    pub local_only: bool,
    /// Machine ids that may connect even when their name is not in the
    /// layout (they are admitted dynamically, like a fresh client).
    pub trusted_ids: Vec<String>,
    /// Machine ids that may **never** connect. Checked before everything
    /// else, including the layout and `trusted_ids`: revoking a machine is
    /// a hard deny, so it is the one policy that cannot be bypassed by a
    /// pinned layout screen. Both lists may hold the same id; revoke wins.
    pub revoked_ids: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            allowlist: true,
            local_only: true,
            trusted_ids: Vec::new(),
            revoked_ids: Vec::new(),
        }
    }
}

impl Policy {
    /// Is `machine_id` explicitly revoked? Same prefix matching as trust
    /// (short or full ids work, in both directions), so revoking the short
    /// id shown in the GUI also refuses the full one. Revocation is the
    /// strongest rule: callers check this before anything else.
    pub fn is_revoked(&self, machine_id: &str) -> bool {
        self.revoked_ids.iter().any(|r| id_matches(machine_id, r))
    }

    /// Is `machine_id` trusted (admitted even without a layout screen)?
    pub fn is_trusted(&self, machine_id: &str) -> bool {
        self.trusted_ids.iter().any(|t| id_matches(machine_id, t))
    }
}

/// Does a machine id match an id-list entry? An entry may be the full id
/// or its 8-char short form (prefix match). Guards: empty entries never
/// match; a short form must be at least 4 chars so a typo'd one-char
/// "trust" cannot silently admit everything starting with it.
pub(crate) fn id_matches(id: &str, entry: &str) -> bool {
    if entry.is_empty() || entry.len() < 4 {
        return false;
    }
    id == entry || id.starts_with(entry)
}

/// Lifecycle events the server emits for its app layer (and GUI). One
/// [`ClientConnected`] per accepted client, one [`ClientDisconnected`]
/// when it goes away. The app layer persists them (the GUI's connected-
/// client list) and applies auto-config.
#[derive(Debug)]
pub enum ServerEvent {
    ClientConnected {
        name: String,
        id: String,
        addr: String,
        since_ms: u64,
        /// The screen shape the client reported in its `Hello`, so the
        /// app layer can auto-configure the layout entry to the real
        /// size.
        info: ScreenInfo,
    },
    ClientDisconnected { name: String },
}

/// Everything the app layer can configure when binding a server.
#[derive(Default)]
pub struct Options {
    /// App-layer control channel (hot reload + client commands).
    pub control: Option<Receiver<Control>>,
    /// Connection policy (allowlist / local-only / trusted ids).
    pub policy: Policy,
    /// Lifecycle events out to the app layer (client list, auto-config).
    pub events: Option<Sender<ServerEvent>>,
    /// This machine's stable id, sent to clients in `Welcome` so they can
    /// trust the server (discovery, auto-connect).
    pub server_id: String,
}

pub struct Server {
    listener: TcpListener,
    udp: Arc<UdpSocket>,
    session: Arc<Mutex<Session>>,
    clients: Arc<Mutex<HashMap<u8, Arc<Client>>>>,
    /// Id of the client the cursor is currently on (`None` = local).
    active: Arc<Mutex<Option<u8>>>,
    /// The local engine, shared with the accept path's `ClientCtx`. Held
    /// so a server-initiated disconnect (revoke, stale layout) can bring
    /// the cursor home without waiting for the client's socket to die —
    /// a revoked machine's socket may outlive the cleanup by seconds.
    /// Set by `run`; `disconnect_client` treats it as best-effort.
    engine: Arc<Mutex<Option<Arc<Mutex<Box<dyn Engine>>>>>>,
    /// Client id → UDP address, learned from each client's first
    /// datagram. The writers need it to route cursor-stream frames.
    udp_addrs: Arc<Mutex<HashMap<u8, SocketAddr>>>,
    /// Client id → last applied beacon sequence (stale/duplicate UDP
    /// datagrams are dropped, so an out-of-order "at the wall" report can
    /// never arm a crossing the user did not push for).
    udp_seqs: Arc<Mutex<HashMap<u8, u32>>>,
    /// Client id → when its cursor stream was last heard (monotonic ms).
    /// The active client beacons every few ms; a stream gone silent is
    /// the signature of a wedged client — see the beacon watchdog in
    /// [`udp::udp_receiver`]. Reset to "now" at the moment the session
    /// activates a client, so the client's first beacons (which only
    /// flow while it is active) can never be judged late.
    last_heard: Arc<Mutex<HashMap<u8, u64>>>,
    /// App-layer control messages (hot reload). `None` disables them.
    /// In a `Mutex` so `Server` stays `Sync` (the channel itself is not).
    control: Mutex<Option<Receiver<Control>>>,
    /// Connection policy (allowlist / local-only / trusted + revoked
    /// ids). Shared with every cloned [`ClientCtx`] behind one lock so a
    /// hot policy change ([`Control::SetPolicy`]) takes effect on the
    /// very next handshake instead of at the next restart.
    policy: Arc<Mutex<Policy>>,
    /// Lifecycle events out to the app layer (connected client list,
    /// auto-config). `None` disables them.
    events: Mutex<Option<Sender<ServerEvent>>>,
    /// This machine's stable id, sent to clients in `Welcome` so they can
    /// trust the server (discovery, auto-connect).
    server_id: String,
    /// Measures the server's pointer transform (px per device count)
    /// from the capture stream; the session scales forwarded motion by
    /// it so the client's cursor mirrors the server's (see
    /// `GainTracker`). In an `Arc<Mutex>` so `run(&self)` can feed it
    /// (the main loop is the only writer).
    gain: Arc<std::sync::Mutex<crate::motion::GainTracker>>,
}

impl Server {
    /// Bind without a control channel or events (default policy).
    pub fn bind(session: Session, port: u16) -> io::Result<Self> {
        Self::with_options(session, port, Options::default())
    }

    /// Bind with an optional app-layer control channel, a connection
    /// policy and an optional event sink. The TCP listener and the UDP
    /// cursor socket share one port (they are independent protocol
    /// namespaces).
    pub fn with_control(
        session: Session,
        port: u16,
        control: Option<Receiver<Control>>,
    ) -> io::Result<Self> {
        Self::with_options(
            session,
            port,
            Options { control, policy: Policy::default(), events: None, server_id: String::new() },
        )
    }

    /// Bind with full options (control channel, policy, event sink).
    pub fn with_options(session: Session, port: u16, opts: Options) -> io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        let udp_port = listener.local_addr()?.port();
        let udp = Arc::new(UdpSocket::bind(("0.0.0.0", udp_port))?);
        // Blocking receive with a short read timeout: the receiver
        // sleeps in the kernel and datagrams wake it immediately (no
        // busy polling); the timeout only bounds idle wakes so the
        // beacon staleness watchdog runs. The same socket carries the
        // writers' sends — matching the client's proven pattern. Motion
        // frames are tiny, the send buffer is huge, and a full buffer
        // (local NIC backpressure) would stall any protocol; the client
        // has shipped with exactly this setup.
        udp.set_read_timeout(Some(udp::IDLE_TIMEOUT))?;
        Ok(Self {
            listener,
            udp,
            session: Arc::new(Mutex::new(session)),
            clients: Arc::new(Mutex::new(HashMap::new())),
            active: Arc::new(Mutex::new(None)),
            engine: Arc::new(Mutex::new(None)),
            udp_addrs: Arc::new(Mutex::new(HashMap::new())),
            udp_seqs: Arc::new(Mutex::new(HashMap::new())),
            last_heard: Arc::new(Mutex::new(HashMap::new())),
            control: Mutex::new(opts.control),
            policy: Arc::new(Mutex::new(opts.policy)),
            events: Mutex::new(opts.events),
            server_id: opts.server_id,
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
        // From here on, server-initiated disconnects can bring the cursor
        // home themselves (see `disconnect_client`).
        *self.engine.lock().unwrap() = Some(engine.clone());
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
            last_heard: self.last_heard.clone(),
            policy: self.policy.clone(), // shared: hot policy changes apply
            events: self.events.lock().unwrap().clone(),
            server_id: self.server_id.clone(),
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
        thread::spawn(move || udp::udp_receiver(udp_sock, ctx));

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
        let layout = match cmd {
            Control::ClientCommand { name, command } => {
                self.apply_client_command(&name, command);
                return Ok(());
            }
            Control::SetPolicy(policy) => {
                self.apply_policy(policy);
                return Ok(());
            }
            Control::Reload(layout, bindings, prefs) => {
                let mut session = self.session.lock().unwrap();
                session.set_bindings(bindings);
                session.set_prefs(prefs);
                drop(session);
                layout
            }
        };
        log_info!("layout reloaded: {} screens", layout.screens.len());

        // 1. Let the session adopt the new layout; it may ask us to bring
        //    the cursor home (it was on a client). The session keeps any
        //    dynamically admitted client the config still does not name
        //    (see [`Session::swap_layout`]), so an unrelated edit never
        //    kicks a live client off.
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

    /// Adopt a hot `[network]` policy change and enforce its one immediate
    /// consequence: a client whose machine id is now revoked is
    /// disconnected on the spot. A revoked machine must not keep a session
    /// it may no longer open — otherwise "revoke" would only apply to the
    /// *next* connection, leaving a live cursor-sharing session running.
    fn apply_policy(&self, policy: Policy) {
        let revoked: Vec<u8> = {
            let mut current = self.policy.lock().unwrap();
            *current = policy;
            let clients = self.clients.lock().unwrap();
            clients
                .values()
                .filter(|c| current.is_revoked(&c.machine_id))
                .map(|c| c.id)
                .collect()
        };
        for id in revoked {
            self.disconnect_client(id, "its machine id was revoked");
        }
    }

    /// Disconnect one client and clean up everything its session held:
    /// the wire commands (disconnect + leave), the registration maps, the
    /// active-cursor state and the session (which may bring the cursor
    /// home), plus the [`ServerEvent::ClientDisconnected`] that keeps the
    /// app layer's `clients.json` — and therefore the GUI's connected
    /// list — truthful.
    ///
    /// This is *the* disconnect path. Every server-initiated drop goes
    /// through it; the three call sites used to each reimplement a subset,
    /// and the subsets that skipped the event left the GUI showing
    /// "connected to you" for a machine that was long gone.
    ///
    /// `remove_screen`: policy drops (revoke, stale layout) also remove
    /// the client's screen from the running desktop (see
    /// [`Session::on_client_removed`]) — the operator refused the machine,
    /// not just its session.
    fn disconnect_client(&self, id: u8, why: &str) {
        self.disconnect_client_inner(id, why, true)
    }

    /// Plain disconnect: the session keeps the screen (a client that
    /// merely dropped may reconnect into its slot).
    fn disconnect_client_keep_screen(&self, id: u8, why: &str) {
        self.disconnect_client_inner(id, why, false)
    }

    fn disconnect_client_inner(&self, id: u8, why: &str, remove_screen: bool) {
        let name = {
            let clients = self.clients.lock().unwrap();
            match clients.get(&id) {
                Some(c) => c.name.clone(),
                None => return, // already gone; nothing to clean up
            }
        };
        log_info!("disconnecting client {name}: {why}");

        // Tell the client to end its session (stays stopped) and leave
        // the desktop. Either command may hit a dead socket — the writer
        // drops those.
        client::enqueue(
            &self.clients,
            id,
            Message::Control { command: kvmshare_protocol::id::control::DISCONNECT },
        );
        client::enqueue(&self.clients, id, Message::Leave { screen_id: id });

        // The reader thread for this connection ends when its socket
        // closes (or on these commands); teardown is identity-checked and
        // idempotent, so its later cleanup is a no-op from here on.
        self.clients.lock().unwrap().remove(&id);
        self.udp_addrs.lock().unwrap().remove(&id);
        self.udp_seqs.lock().unwrap().remove(&id);
        self.last_heard.lock().unwrap().remove(&id);

        // Session first: it decides whether the cursor must come home.
        let action = if remove_screen {
            self.session.lock().unwrap().on_client_removed(id)
        } else {
            self.session.lock().unwrap().on_client_disconnected(id)
        };
        {
            let mut act = self.active.lock().unwrap();
            if *act == Some(id) {
                *act = None;
            }
        }
        if let Action::SwitchToLocal { .. } = action {
            let engine = self.engine.lock().unwrap().clone();
            if let Some(engine) = engine {
                if let Ok(mut engine) = engine.lock() {
                    let _ = apply_action(action, &self.active, &self.clients, &self.last_heard, &mut engine);
                }
            }
        }

        // App layer last, so clients.json is rewritten after the maps
        // settled (the event sink persists on every event).
        if let Some(tx) = self.events.lock().unwrap().as_ref() {
            let _ = tx.send(ServerEvent::ClientDisconnected { name });
        }
    }

    /// Disconnect clients whose screen disappeared from the new layout
    /// (their id no longer maps to a screen with the same name), so no
    /// ghost connections linger after a reload. The session already
    /// dropped those screens in `swap_layout`, so the disconnect keeps
    /// whatever screen state the session decided.
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
        for id in gone {
            self.disconnect_client_keep_screen(id, "its screen left the layout");
        }
    }

    /// Send an operational command to one connected client by screen
    /// name (see [`Control::ClientCommand`]).
    ///
    /// `disconnect` is routed through [`Server::disconnect_client`] so it
    /// cleans up every map, the session and the app-layer event — the
    /// old inline send left `clients.json` claiming the client was still
    /// connected until the socket happened to die.
    fn apply_client_command(&self, name: &str, command: u8) {
        let id = {
            let clients = self.clients.lock().unwrap();
            match clients.values().find(|c| c.name == name) {
                Some(c) => c.id,
                None => {
                    log_warn!("client command: no connected client named {name:?}");
                    return;
                }
            }
        };
        match command {
            // The operator dismissed the machine; the screen goes too so a
            // reconnect (reconnect/restart commands) is admitted fresh.
            kvmshare_protocol::id::control::DISCONNECT => self.disconnect_client(id, "the operator asked it to disconnect"),
            _ => {
                log_info!("client command to {name}: control code {command}");
                client::enqueue(&self.clients, id, Message::Control { command });
            }
        }
    }

    /// Send a message to every connected client (e.g. layout or
    /// clipboard broadcasts from the app layer).
    pub fn broadcast(&self, msg: &Message) -> io::Result<()> {
        let clients = self.clients.lock().unwrap();
        for c in clients.values() {
            let item = client::route(msg.clone());
            let _ = c.out.send(item);
        }
        Ok(())
    }

    /// Apply a session [`Action`] to the world.
    fn execute(&self, action: Action, engine: &mut MutexGuard<'_, Box<dyn Engine>>) -> io::Result<()> {
        apply_action(action, &self.active, &self.clients, &self.last_heard, engine)
    }
}