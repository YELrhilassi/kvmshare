//! End-to-end wiring test: a real [`Server`] and a real [`Client`] over
//! TCP, with mock local input (the channel the platform would feed) and a
//! recording injector (the platform's other half).
//!
//! This proves the whole path works without an X display: session logic →
//! server → wire → client → injector, plus the engine actions the server
//! takes on its own machine.

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use kvmshare_core::client::{Client, Injector};
use kvmshare_core::layout::Layout;
use kvmshare_core::server::{Control, Engine, Server};
use kvmshare_core::session::Session;
use kvmshare_protocol::message::{KeyKind, Message, Rect, Screen, ScreenInfo};

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
    fn clipboard_set(&mut self, _mime: &str, _data: &[u8]) {}
    fn clipboard_get(&mut self) -> Option<(String, Vec<u8>)> {
        None
    }
    fn clipboard_last_injected(&mut self) -> Option<(String, Vec<u8>)> {
        None
    }
}

/// A client-side injector that records what the client was told to do.
struct RecordingInjector {
    calls: Arc<Mutex<Vec<String>>>,
    info: ScreenInfo,
}

impl RecordingInjector {
    fn new(info: ScreenInfo) -> Self {
        Self { calls: Arc::new(Mutex::new(Vec::new())), info }
    }
}

impl Injector for RecordingInjector {
    fn screen_info(&mut self) -> ScreenInfo {
        self.info
    }
    fn move_cursor(&mut self, x: i32, y: i32) {
        self.calls.lock().unwrap().push(format!("move {x},{y}"));
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
    fn clipboard(&mut self, _mime: &str, _data: &[u8]) {}
    fn clipboard_get(&mut self) -> Option<(String, Vec<u8>)> {
        None
    }
    fn clipboard_last_injected(&mut self) -> Option<(String, Vec<u8>)> {
        None
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
    let server = Arc::new(Server::with_control(session, 0, Some(control_rx)).unwrap());
    let port = server.local_addr().unwrap().port();

    let (input_tx, input_rx) = mpsc::channel::<Message>();
    let engine_calls = Arc::new(Mutex::new(Vec::new()));
    let engine = Arc::new(Mutex::new(Box::new(MockEngine { calls: engine_calls.clone() }) as Box<dyn Engine>));

    // Dropping the JoinHandle detaches the thread; it keeps running for
    // the whole test on its own Arc clones.
    thread::spawn({
        let server = server.clone();
        let engine = engine.clone();
        move || server.run(input_rx, engine).unwrap()
    });

    Harness { server, input_tx, control_tx, engine_calls, port }
}

fn connect_client(port: u16) -> (Client, RecordingInjector, Arc<Mutex<Vec<String>>>, Receiver<Message>) {
    let info = ScreenInfo { width: 1920, height: 1080, scale: 1.0 };
    let injector = RecordingInjector::new(info);
    let calls = injector.calls.clone();
    let client = Client::connect(&format!("127.0.0.1:{port}"), "hp", info).unwrap();
    assert_eq!(client.own_id(), 1);
    let (_out_tx, out_rx) = mpsc::channel::<Message>();
    (client, injector, calls, out_rx)
}

/// Feed one input event and let the pipeline settle.
fn feed(h: &Harness, msg: Message) {
    h.input_tx.send(msg).unwrap();
    thread::sleep(Duration::from_millis(80));
}

fn calls(c: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    c.lock().unwrap().clone()
}

#[test]
fn cursor_enters_moves_and_crosses_back_over_tcp() {
    let h = start_server();

    let (client, injector, client_calls, out_rx) = connect_client(h.port);
    thread::spawn(move || client.run(Box::new(injector), &out_rx).unwrap());
    h.wait_for_clients(1);

    // -- Cross from pc onto hp (left screen). --
    feed(&h, Message::MouseMoveRel { dx: -1000, dy: 0 }); // pc center (960,540) - 1000 → past left edge

    let cc = calls(&client_calls);
    assert!(cc.contains(&"enter".to_string()), "client should enter, got {cc:?}");
    // Entry point is hp's right edge (1919, 540); the server also sends
    // an absolute move for the entry position.
    assert!(cc.iter().any(|c| c == "move 1919,540"), "client should move to entry point, got {cc:?}");

    let ec = calls(&h.engine_calls);
    assert!(ec.iter().any(|c| c == "cursor false"), "server should hide its cursor, got {ec:?}");
    // The hidden cursor must stay exactly where it crossed — the server
    // must NOT warp it (a warp would sweep hover/enter effects across
    // the local desktop, and a visible one would dash the cursor to the
    // screen center on every crossing).
    assert!(
        !ec.iter().any(|c| c.starts_with("warp ")),
        "server must not warp its cursor when switching away, got {ec:?}"
    );

    // -- Move around on hp: forwarded as absolute positions. --
    feed(&h, Message::MouseMoveRel { dx: -100, dy: 0 }); // virtual -101 → hp-local 1819
    let cc = calls(&client_calls);
    assert!(cc.iter().any(|c| c == "move 1819,540"), "expected forwarded move, got {cc:?}");

    // -- Buttons and keys forward while on the client. --
    feed(&h, Message::MouseButton { button: 0, pressed: true });
    let cc = calls(&client_calls);
    assert!(cc.contains(&"button 0 true".to_string()), "button should forward, got {cc:?}");

    feed(&h, Message::Key { kind: KeyKind::Down, key: 0x04 }); // canonical HID usage: 'a'
    let cc = calls(&client_calls);
    assert!(cc.contains(&"key Down 4".to_string()), "key should forward, got {cc:?}");

    // -- Cross back to pc: the client leaves and the server's cursor returns. --
    feed(&h, Message::MouseMoveRel { dx: 102, dy: 0 }); // virtual 1 → past hp's right edge

    let cc = calls(&client_calls);
    assert!(cc.contains(&"leave".to_string()), "client should leave, got {cc:?}");
    let ec = calls(&h.engine_calls);
    assert!(ec.iter().any(|c| c == "cursor true"), "server should restore its cursor, got {ec:?}");
}

#[test]
fn client_disconnect_returns_cursor_home() {
    let h = start_server();

    // Connect but never run the loop: the socket stays open.
    let (client, _injector, _client_calls, _out_rx) = connect_client(h.port);
    h.wait_for_clients(1);
    feed(&h, Message::MouseMoveRel { dx: -1000, dy: 0 }); // on hp now

    // Dropping the client closes the TCP connection; the server notices
    // and returns the cursor to the local screen.
    drop(client);
    thread::sleep(Duration::from_millis(150));

    let ec = calls(&h.engine_calls);
    assert!(ec.iter().any(|c| c == "cursor true"), "cursor should be restored after disconnect, got {ec:?}");
}

#[test]
fn unknown_client_name_is_rejected() {
    let h = start_server();
    let info = ScreenInfo { width: 1920, height: 1080, scale: 1.0 };
    let err = Client::connect(&format!("127.0.0.1:{}", h.port), "not-in-layout", info).unwrap_err();
    assert!(err.to_string().contains("no screen named"), "got: {err}");
}

#[test]
fn config_hot_reload_returns_cursor_home_and_broadcasts() {
    let h = start_server();

    let (client, injector, client_calls, out_rx) = connect_client(h.port);
    thread::spawn(move || client.run(Box::new(injector), &out_rx).unwrap());
    h.wait_for_clients(1);

    // Move onto hp, then reload a layout that no longer has hp: the
    // cursor must come home and the client must be told to leave.
    feed(&h, Message::MouseMoveRel { dx: -1000, dy: 0 });
    assert!(calls(&h.engine_calls).iter().any(|c| c == "cursor false"));

    let new_layout = Layout::new(vec![Screen {
        id: 0,
        name: "pc".into(),
        rect: Rect { x: 0, y: 0, w: 1920, h: 1080 },
    }]);
    h.control_tx.send(Control::Reload(new_layout)).unwrap();
    thread::sleep(Duration::from_millis(200));

    let ec = calls(&h.engine_calls);
    assert!(ec.iter().any(|c| c == "cursor true"), "cursor should return home, got {ec:?}");
    assert!(ec.iter().any(|c| c == "warp 960,540"), "cursor should warp to local center, got {ec:?}");

    let cc = calls(&client_calls);
    assert!(cc.contains(&"leave".to_string()), "client should be told to leave, got {cc:?}");

    // hp was dropped from the layout, so it must be unregistered.
    assert_eq!(h.server.client_count(), 0, "stale client should be dropped after reload");
}