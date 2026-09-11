//! End-to-end wiring test: a real [`Server`] and a real [`Client`] over
//! TCP, with mock local input (the channel the platform would feed) and a
//! recording injector (the platform's other half).
//!
//! This proves the whole path works without an X display: session logic →
//! server → wire → client → injector, plus the engine actions the server
//! takes on its own machine.

use std::net::{TcpStream, UdpSocket};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use kvmshare_core::client::{Clipboard, Client, Injector};
use kvmshare_core::layout::Layout;
use kvmshare_core::server::{Control, Engine, Options, Policy, Server};
use kvmshare_core::session::Session;
use kvmshare_core::transport::{RecvResult, Transport};
use kvmshare_core::udp;
use kvmshare_protocol::message::{KeyKind, Message, Rect, Screen, ScreenInfo};
use kvmshare_protocol::VERSION;

/// The classic layout from the deskflow debugging sessions: pc (server)
/// on the right, hp (client) to its left.
fn two_screen_layout() -> Layout {
    Layout::new(vec![
        Screen { id: 0, name: "pc".into(), rect: Rect { x: 0, y: 0, w: 1920, h: 1080 } },
        Screen { id: 1, name: "hp".into(), rect: Rect { x: -1920, y: 0, w: 1920, h: 1080 } },
    ])
}

/// A no-op engine that records what the server asked it to do.
struct MockEngine {
    calls: Arc<Mutex<Vec<String>>>,
}

impl Engine for MockEngine {
    fn warp_local(&mut self, x: i32, y: i32) {
        self.calls.lock().unwrap().push(format!("warp {x},{y}"));
    }
    fn grab_input(&mut self, grabbed: bool) {
        self.calls.lock().unwrap().push(format!("grab {grabbed}"));
    }
    fn show_local_cursor(&mut self, visible: bool) {
        self.calls.lock().unwrap().push(format!("cursor {visible}"));
    }
}

/// A client-side injector that records what the client was told to do.
struct RecordingInjector {
    calls: Arc<Mutex<Vec<String>>>,
    info: ScreenInfo,
    /// Tracked local cursor position, so `cursor_position` reflects the
    /// effect of absolute moves and relative motion.
    cursor: (i32, i32),
}

impl RecordingInjector {
    fn new(info: ScreenInfo) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            info,
            cursor: ((info.width / 2) as i32, (info.height / 2) as i32),
        }
    }
}

impl Injector for RecordingInjector {
    fn screen_info(&mut self) -> ScreenInfo {
        self.info
    }
    fn move_cursor(&mut self, x: i32, y: i32) {
        self.cursor = (x, y);
        self.calls.lock().unwrap().push(format!("move {x},{y}"));
    }
    fn move_rel(&mut self, dx: i32, dy: i32) {
        self.cursor.0 += dx;
        self.cursor.1 += dy;
        self.calls.lock().unwrap().push(format!("rel {dx},{dy}"));
    }
    fn cursor_position(&mut self) -> (i32, i32) {
        self.cursor
    }
    fn button(&mut self, button: u8, pressed: bool) {
        self.calls.lock().unwrap().push(format!("button {button} {pressed}"));
    }
    fn wheel(&mut self, dx: i32, dy: i32) {
        self.calls.lock().unwrap().push(format!("wheel {dx},{dy}"));
    }
    fn key(&mut self, kind: KeyKind, key: u32) {
        self.calls.lock().unwrap().push(format!("key {kind:?} {key}"));
    }
    fn enter(&mut self) {
        self.calls.lock().unwrap().push("enter".into());
    }
    fn leave(&mut self) {
        self.calls.lock().unwrap().push("leave".into());
    }
}

/// A running server with a channel for feeding it local input, a control
/// channel for hot reloads, plus handles to both recorders.
struct Harness {
    server: Arc<Server>,
    input_tx: mpsc::Sender<Message>,
    control_tx: mpsc::Sender<Control>,
    engine_calls: Arc<Mutex<Vec<String>>>,
    port: u16,
}

impl Harness {
    /// Wait until `n` clients are registered with the server.
    fn wait_for_clients(&self, n: usize) {
        for _ in 0..100 {
            if self.server.client_count() >= n {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {n} client(s)");
    }
}

fn start_server() -> Harness {
    let session = Session::new(two_screen_layout(), 0);
    let (control_tx, control_rx) = mpsc::channel::<Control>();
    // The e2e harness keeps the legacy open behavior (allowlist off) so
    // every pre-existing scenario works unchanged; the allowlist itself
    // is exercised by its own dedicated test.
    let server = Arc::new(
        Server::with_options(
            session,
            0,
            Options { control: Some(control_rx), policy: Policy { allowlist: false, ..Policy::default() }, events: None, server_id: "server-pc".into() },
        )
        .unwrap(),
    );
    let port = server.local_addr().unwrap().port();

    let (input_tx, input_rx) = mpsc::channel::<Message>();
    let engine_calls = Arc::new(Mutex::new(Vec::new()));
    let engine = Arc::new(Mutex::new(Box::new(MockEngine { calls: engine_calls.clone() }) as Box<dyn Engine>));

    // Dropping the JoinHandle detaches the thread; it keeps running for
    // the whole test on its own Arc clones.
    let clipboard: kvmshare_core::server::ServerClipboard =
        Arc::new(Mutex::new(Box::new(NoClipboard) as Box<dyn Clipboard>));
    thread::spawn({
        let server = server.clone();
        let engine = engine.clone();
        let clipboard = clipboard.clone();
        move || {
            server
                .run(input_rx, engine, clipboard, Arc::new(kvmshare_core::server::Liveness::default()))
                .unwrap()
        }
    });

    Harness { server, input_tx, control_tx, engine_calls, port }
}

fn connect_client(port: u16) -> (Client, RecordingInjector, Arc<Mutex<Vec<String>>>, Receiver<Message>) {
    let info = ScreenInfo { width: 1920, height: 1080, scale: 1.0 };
    let injector = RecordingInjector::new(info);
    let calls = injector.calls.clone();
    let client = Client::connect(&format!("127.0.0.1:{port}"), "hp", "machine-hp", info).unwrap();
    assert_eq!(client.own_id(), 1);
    let (_out_tx, out_rx) = mpsc::channel::<Message>();
    (client, injector, calls, out_rx)
}

/// The e2e tests never touch a real clipboard; the split [`Clipboard`]
/// service is satisfied with a no-op (the sync thread's poll simply
/// reports nothing).
struct NoClipboard;

impl Clipboard for NoClipboard {
    fn set(&mut self, _mime: &str, _data: &[u8]) {}
    fn get(&mut self) -> Option<(String, Vec<u8>)> {
        None
    }
    fn last_injected(&mut self) -> Option<(String, Vec<u8>)> {
        None
    }
}

/// Feed one input event and let the pipeline settle.
fn feed(h: &Harness, msg: Message) {
    h.input_tx.send(msg).unwrap();
    thread::sleep(Duration::from_millis(80));
}

fn calls(c: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    c.lock().unwrap().clone()
}

mod core_tests;
