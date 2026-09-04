//! The server side.
//!
//! Owns the [`Session`] and the set of connected clients. One thread per
//! client (blocking IO is plenty for a KVM: tens of messages per second
//! per client, tiny frames). The main loop drains local input events from
//! the platform and executes whatever [`Action`]s the session produces.

use std::collections::HashMap;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use kvmshare_log::{log_debug, log_info, log_trace, log_warn};
use kvmshare_protocol::id::errors;
use kvmshare_protocol::message::{Layout, Message};

use crate::layout::Layout as Desktop;
use crate::session::{Action, Session};
use crate::transport::{RecvResult, Transport};

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
    transport: Mutex<Transport>,
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
    session: Arc<Mutex<Session>>,
    clients: Arc<Mutex<HashMap<u8, Arc<Client>>>>,
    /// Id of the client the cursor is currently on (`None` = local).
    active: Arc<Mutex<Option<u8>>>,
    /// App-layer control messages (hot reload). `None` disables them.
    /// In a `Mutex` so `Server` stays `Sync` (the channel itself is not).
    control: Mutex<Option<Receiver<Control>>>,
}

impl Server {
    /// Bind without a control channel (no hot reload).
    pub fn bind(session: Session, port: u16) -> io::Result<Self> {
        Self::with_control(session, port, None)
    }

    /// Bind with an optional app-layer control channel.
    pub fn with_control(
        session: Session,
        port: u16,
        control: Option<Receiver<Control>>,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        Ok(Self {
            listener,
            session: Arc::new(Mutex::new(session)),
            clients: Arc::new(Mutex::new(HashMap::new())),
            active: Arc::new(Mutex::new(None)),
            control: Mutex::new(control),
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
    /// shared so other threads (client handlers, the app's clipboard
    /// poller) can reach it too.
    pub fn run(&self, input: Receiver<Message>, engine: Arc<Mutex<Box<dyn Engine>>>) -> io::Result<()> {
        // Accept clients on a background thread.
        let (listener, session, clients) =
            (self.listener.try_clone()?, self.session.clone(), self.clients.clone());
        let active = self.active.clone();
        let engine_accept = engine.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        let addr = s.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| "?".into());
                        if let Err(e) =
                            Client::spawn(s, session.clone(), clients.clone(), active.clone(), engine_accept.clone())
                        {
                            log_warn!("client {addr}: {e}");
                        }
                    }
                    Err(e) => log_warn!("accept error: {e}"),
                }
            }
        });

        // Main loop: process local input. The engine lock is taken per
        // event (not held for the whole loop) so other threads — client
        // threads applying remote clipboard content, and the app's
        // clipboard poller — can reach the engine between events. Idle
        // timeouts also drain the app-layer control channel (hot reload).
        loop {
            match input.recv_timeout(CONTROL_POLL) {
                Ok(msg) => {
                    // Every message (motion deltas + position beacons from
                    // the capture) goes straight to the session. Nothing
                    // here touches the engine: the platform feeds the
                    // real pointer position through its own beacons, and
                    // per-event X round-trips would stall the cursor the
                    // moment the local desktop gets busy.
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
            let _ = self.send_to(*id, &Message::Leave { screen_id: *id });
            self.clients.lock().unwrap().remove(id);
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
            c.transport.lock().unwrap().send(msg)?;
        }
        Ok(())
    }

    /// Apply a session [`Action`] to the world.
    fn execute(&self, action: Action, engine: &mut MutexGuard<'_, Box<dyn Engine>>) -> io::Result<()> {
        match action {
            Action::Nothing => {}
            Action::Send(msg) => {
                if let Some(id) = *self.active.lock().unwrap() {
                    // Relative mouse motion is the hot path (up to
                    // hundreds per second) — not even trace logs those.
                    if !matches!(msg, Message::MouseMoveRel { .. }) {
                        log_trace!("forward {msg:?} -> client {id}");
                    }
                    self.send_to(id, &msg)?;
                }
            }
            Action::SwitchTo { to, x, y } => {
                // Leave whatever screen we are on.
                if let Some(old) = self.active.lock().unwrap().take() {
                    self.send_to(old, &Message::Leave { screen_id: old })?;
                }
                *self.active.lock().unwrap() = Some(to);
                log_debug!("cursor switched to client {to} at ({x},{y})");
                // `Enter` carries the entry point; the client places its
                // cursor there itself (absolute placement is reserved for
                // entry — the motion stream that follows is relative).
                self.send_to(to, &Message::Enter { screen_id: to, x, y })?;
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
                if let Some(old) = self.active.lock().unwrap().take() {
                    self.send_to(old, &Message::Leave { screen_id: old })?;
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

    fn send_to(&self, id: u8, msg: &Message) -> io::Result<()> {
        let client = self.clients.lock().unwrap().get(&id).cloned();
        match client {
            Some(c) => c.transport.lock().unwrap().send(msg),
            None => Ok(()), // client vanished; its thread handles cleanup
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

        // 3. Register, then send Welcome + current layout. The transport
        // stays on the writer side (mutex-protected, used by the main
        // thread); the service thread below gets its own lock-free reader.
        let mut reader = transport.reader()?;
        let client = Arc::new(Client { id, name, transport: Mutex::new(transport) });
        let layout = {
            let s = session.lock().unwrap();
            Layout { screens: s.layout().screens.clone() }
        };
        client.transport.lock().unwrap().send(&Message::Welcome {
            server_version: kvmshare_protocol::VERSION,
            layout: layout.clone(),
            own_screen_id: id,
        })?;
        clients.lock().unwrap().insert(id, client.clone());
        // Stable marker for the GUI's notification watcher (kept in sync
        // with the "disconnected" line below): "client X connected".
        log_info!("client {} connected", client.name);

        // 4. Service the client until it goes away. The reader owns a
        // socket clone, so it can block on recv without ever holding the
        // transport lock — the main thread keeps writing freely. On EOF
        // (or error), unregister and give the session a chance to return
        // home if this was the active client.
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
                        let _ = c2.transport.lock().unwrap().send(&Message::Layout { layout });
                    }
                    Message::Clipboard { mime, data } => {
                        // Content copied on the client reaches the
                        // server's local clipboard.
                        log_debug!("clipboard from {}: {} ({} bytes)", c2.name, mime, data.len());
                        if let Ok(mut engine) = engine2.lock() {
                            engine.clipboard_set(&mime, &data);
                        }
                    }
                    Message::CursorPos { x, y } => {
                        // The client's *real* cursor position (its OS
                        // reports it after applying its own pointer
                        // transform to the relative motion we inject).
                        // Drives remote edge crossings: only when the
                        // beacon confirms the cursor is parked on a shared
                        // edge do outward deltas cross. Session state is
                        // updated here; the crossing itself fires on the
                        // next outward delta in the main loop.
                        session2.lock().unwrap().on_remote_beacon(c2.id, x, y);
                    }
                    Message::InputBlocked => {
                        // The client's OS is dropping our injected input
                        // (on Windows: an elevated or input-isolated
                        // window — UAC prompt, on-screen keyboard, an
                        // admin tool — swallows SendInput). The cursor is
                        // frozen there and the local grab would keep the
                        // keyboard from the user, so bring control home.
                        // Mirrors `Action::SwitchToLocal` in `execute`;
                        // the client thread has no `Server` handle, only
                        // the shared state.
                        let was_active = *active2.lock().unwrap() == Some(c2.id);
                        if !was_active {
                            continue;
                        }
                        log_warn!(
                            "{}: local input blocked (elevated window?) — returning control home",
                            c2.name
                        );
                        let action = session2.lock().unwrap().force_local();
                        if let Action::SwitchToLocal { x, y } = action {
                            if let Ok(mut engine) = engine2.lock() {
                                *active2.lock().unwrap() = None;
                                let _ = c2.transport.lock().unwrap().send(&Message::Leave { screen_id: c2.id });
                                engine.isolate_input(false);
                                engine.grab_input(false);
                                engine.warp_local(x, y);
                                engine.show_local_cursor(true);
                            }
                        }
                    }
                    _ => {}
                }
            }

            log_info!("client {} disconnected", c2.name);
            // Unregister and give the session a chance to return home if
            // this was the active client.
            clients2.lock().unwrap().remove(&c2.id);
            {
                let mut act = active2.lock().unwrap();
                if *act == Some(c2.id) {
                    *act = None;
                }
            }
            // If the cursor was on this client, drop back to the local
            // screen and apply the engine side (ungrab + warp + show
            // cursor). This mirrors `Action::SwitchToLocal` in `execute`.
            let action = session2.lock().unwrap().on_client_disconnected(c2.id);
            if let Action::SwitchToLocal { x, y } = action {
                if let Ok(mut engine) = engine2.lock() {
                    engine.grab_input(false);
                    engine.warp_local(x, y);
                    engine.show_local_cursor(true);
                }
            }
        });

        Ok(())
    }
}

