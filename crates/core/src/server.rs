//! The server side.
//!
//! Owns the [`Session`] and the set of connected clients. One thread per
//! client (blocking IO is plenty for a KVM: tens of messages per second
//! per client, tiny frames). The main loop drains local input events from
//! the platform and executes whatever [`Action`]s the session produces.

use std::collections::HashMap;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use kvmshare_protocol::message::{Layout, Message};
use kvmshare_protocol::id::errors;

use crate::session::{Action, Session};
use crate::transport::{RecvResult, Transport};

/// The platform hook the server calls to control the *local* machine.
pub trait Engine: Send {
    /// Warp the local cursor to a local-screen position.
    fn warp_local(&mut self, x: i32, y: i32);
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

/// A running server.
pub struct Server {
    listener: TcpListener,
    session: Arc<Mutex<Session>>,
    clients: Arc<Mutex<HashMap<u8, Arc<Client>>>>,
    /// Id of the client the cursor is currently on (`None` = local).
    active: Arc<Mutex<Option<u8>>>,
}

impl Server {
    pub fn bind(session: Session, port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        Ok(Self {
            listener,
            session: Arc::new(Mutex::new(session)),
            clients: Arc::new(Mutex::new(HashMap::new())),
            active: Arc::new(Mutex::new(None)),
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
                        if let Err(e) =
                            Client::spawn(s, session.clone(), clients.clone(), active.clone(), engine_accept.clone())
                        {
                            eprintln!("client error: {e}");
                        }
                    }
                    Err(e) => eprintln!("accept error: {e}"),
                }
            }
        });

        // Main loop: process local input. The engine lock is taken per
        // event (not held for the whole loop) so other threads — client
        // threads applying remote clipboard content, and the app's
        // clipboard poller — can reach the engine between events.
        while let Ok(msg) = input.recv() {
            let actions = { self.session.lock().unwrap().on_local_event(msg) };
            let mut engine = engine.lock().unwrap();
            for action in actions {
                self.execute(action, &mut engine)?;
            }
        }
        Ok(())
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
                    self.send_to(id, &msg)?;
                }
            }
            Action::SwitchTo { to, x, y, park } => {
                // Leave whatever screen we are on.
                if let Some(old) = self.active.lock().unwrap().take() {
                    self.send_to(old, &Message::Leave { screen_id: old })?;
                }
                *self.active.lock().unwrap() = Some(to);
                self.send_to(to, &Message::Enter { screen_id: to, x, y })?;
                self.send_to(to, &Message::MouseMoveAbs { x, y })?;
                // Park the hidden local cursor at its center so it has
                // room to roam.
                engine.warp_local(park.0, park.1);
                engine.show_local_cursor(false);
            }
            Action::SwitchToLocal { x, y } => {
                if let Some(old) = self.active.lock().unwrap().take() {
                    self.send_to(old, &Message::Leave { screen_id: old })?;
                }
                engine.warp_local(x, y);
                engine.show_local_cursor(true);
            }
            Action::RecenterLocal { park } => {
                // Edge guard for the hidden physical cursor. The virtual
                // cursor is unaffected (raw input has no warp feedback).
                engine.warp_local(park.0, park.1);
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
                        if let Ok(mut engine) = engine2.lock() {
                            engine.clipboard_set(&mime, &data);
                        }
                    }
                    _ => {}
                }
            }

            eprintln!("client {} disconnected", c2.name);
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
            // screen and apply the engine side (warp + show cursor). This
            // mirrors `Action::SwitchToLocal` in `execute`.
            let action = session2.lock().unwrap().on_client_disconnected(c2.id);
            if let Action::SwitchToLocal { x, y } = action {
                if let Ok(mut engine) = engine2.lock() {
                    engine.warp_local(x, y);
                    engine.show_local_cursor(true);
                }
            }
        });

        Ok(())
    }
}

