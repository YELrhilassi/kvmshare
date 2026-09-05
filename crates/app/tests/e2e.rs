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
use kvmshare_core::server::{Control, Engine, Server};
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
    let server = Arc::new(Server::with_control(session, 0, Some(control_rx)).unwrap());
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
        move || server.run(input_rx, engine, clipboard).unwrap()
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

#[test]
fn cursor_enters_moves_and_crosses_back_over_tcp() {
    let h = start_server();

    let (client, injector, client_calls, out_rx) = connect_client(h.port);
    thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx).unwrap());
    h.wait_for_clients(1);

    // -- Cross from pc onto hp (left screen). --
    // The real cursor parks at the shared edge (beacon), then an outward
    // push crosses. (Deltas alone never cross — see core::session.)
    feed(&h, Message::MouseMoveAbs { x: 0, y: 540 });
    feed(&h, Message::MouseMoveRel { dx: -5, dy: 0 });

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

    // -- Roam around on hp: forwarded as *relative* motion. --
    // The client's OS applies its own pointer transform to relative
    // input, so the shared cursor feels native there. The server must
    // never send absolute positions in the motion stream (the hidden
    // local cursor never moves while we are away).
    feed(&h, Message::MouseMoveRel { dx: -100, dy: 0 });
    let cc = calls(&client_calls);
    // Under the closed-loop model the -100 frame is fed forward
    // immediately (half a frame) and the damped corrections deliver the
    // rest against the read-back cursor — what must hold is that the
    // command trajectory is honored exactly: the recorded relative
    // stream totals -100 px on x, nothing on y.
    let (rel_x, rel_y): (i64, i64) = cc
        .iter()
        .filter_map(|c| c.strip_prefix("rel "))
        .map(|r| {
            let (x, y) = r.split_once(',').unwrap();
            (x.parse::<i64>().unwrap(), y.parse::<i64>().unwrap())
        })
        .fold((0, 0), |(ax, ay), (x, y)| (ax + x, ay + y));
    assert_eq!(rel_x, -100, "motion must deliver the full -100 px command, got {cc:?}");
    assert_eq!(rel_y, 0, "no motion outside the command axis, got {cc:?}");
    assert!(
        cc.iter().all(|c| !c.starts_with("move ") || c == "move 1919,540"),
        "only the entry move may be absolute, got {cc:?}"
    );

    // -- Buttons and keys forward while on the client. --
    feed(&h, Message::MouseButton { button: 0, pressed: true });
    let cc = calls(&client_calls);
    assert!(cc.contains(&"button 0 true".to_string()), "button should forward, got {cc:?}");

    feed(&h, Message::Key { kind: KeyKind::Down, key: 0x04 }); // canonical HID usage: 'a'
    let cc = calls(&client_calls);
    assert!(cc.contains(&"key Down 4".to_string()), "key should forward, got {cc:?}");

    // -- Cross back to pc. --
    // The client's real cursor must be pinned on the shared edge (its
    // right edge) for a crossing; first push moves it there, then the
    // next outward push (a frame later, as in real use) crosses.
    feed(&h, Message::MouseMoveRel { dx: 120, dy: 0 }); // roam back to the right wall
    feed(&h, Message::MouseMoveRel { dx: 10, dy: 0 }); // keep pushing: cross home

    let cc = calls(&client_calls);
    assert!(cc.contains(&"leave".to_string()), "client should leave, got {cc:?}");
    let ec = calls(&h.engine_calls);
    assert!(ec.iter().any(|c| c == "cursor true"), "server should restore its cursor, got {ec:?}");
}

/// A raw peer that speaks just enough of the protocol to register as a
/// client, flood the server's UDP beacon stream with `n` cursor
/// beacons, then vanish. Used to simulate a previous client session whose
/// UDP sequence counter reached `n` before it disconnected — the
/// reconnect must not inherit that state (stale beacons must never
/// deafen a fresh peer).
fn raw_beacon_client(port: u16, n: u32) {
    let mut tcp = Transport::new(TcpStream::connect(("127.0.0.1", port)).unwrap()).unwrap();
    let info = ScreenInfo { width: 1920, height: 1080, scale: 1.0 };
    tcp.send(&Message::Hello { version: VERSION, name: "hp".into(), info }).unwrap();
    let id = match tcp.recv().unwrap() {
        RecvResult::Msg(Message::Welcome { own_screen_id, .. }) => own_screen_id,
        other => panic!("expected welcome, got {other:?}"),
    };
    // A UDP stream to the same server port, like the real client's.
    let udp_sock = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
    udp_sock.connect(("127.0.0.1", port)).unwrap();
    for seq in 1..=n {
        udp_sock
            .send(&udp::pack(id, seq, &Message::CursorPos { x: 500, y: 540 }))
            .unwrap();
    }
    // Give the server a moment to drain the datagrams, then vanish.
    thread::sleep(Duration::from_millis(50));
}

#[test]
fn client_reconnect_is_not_deafened_by_stale_udp_sequences() {
    let h = start_server();

    // A previous "hp" session ran long enough that the server's UDP
    // sequence tracker for its screen id climbed high, then it
    // disconnected. (The tracker must be cleared on disconnect — a fresh
    // session starts its own sequence at 1.)
    raw_beacon_client(h.port, 500);
    for _ in 0..100 {
        if h.server.client_count() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(h.server.client_count(), 0, "old session should have disconnected");

    // The real client reconnects (same screen id) and must work normally:
    // beacons from sequence 1 on are fresh and drive the crossing back.
    let (client, injector, client_calls, out_rx) = connect_client(h.port);
    thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx).unwrap());
    h.wait_for_clients(1);

    // Cross onto hp.
    feed(&h, Message::MouseMoveAbs { x: 0, y: 540 });
    feed(&h, Message::MouseMoveRel { dx: -5, dy: 0 });
    let cc = calls(&client_calls);
    assert!(cc.contains(&"enter".to_string()), "client should enter, got {cc:?}");
    assert!(calls(&h.engine_calls).iter().any(|c| c == "cursor false"));

    // Cross back: the client's real cursor is parked on the shared edge;
    // its (fresh) beacons arm it and the outward push fires the crossing.
    feed(&h, Message::MouseMoveRel { dx: 10, dy: 0 });

    let mut cc = calls(&client_calls);
    for _ in 0..50 {
        if cc.contains(&"leave".to_string()) {
            break;
        }
        thread::sleep(Duration::from_millis(20));
        cc = calls(&client_calls);
    }
    assert!(cc.contains(&"leave".to_string()), "reconnected client should cross back, got {cc:?}");
    assert!(
        calls(&h.engine_calls).iter().any(|c| c == "cursor true"),
        "server cursor should be restored after the return crossing"
    );
}

#[test]
fn client_disconnect_returns_cursor_home() {
    let h = start_server();

    // Connect but never run the loop: the socket stays open.
    let (client, _injector, _client_calls, _out_rx) = connect_client(h.port);
    h.wait_for_clients(1);
    feed(&h, Message::MouseMoveAbs { x: 0, y: 540 }); // beacon at the shared edge
    feed(&h, Message::MouseMoveRel { dx: -5, dy: 0 }); // outward push: on hp now

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
    thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx).unwrap());
    h.wait_for_clients(1);

    // Move onto hp, then reload a layout that no longer has hp: the
    // cursor must come home and the client must be told to leave.
    feed(&h, Message::MouseMoveAbs { x: 0, y: 540 }); // beacon at the shared edge
    feed(&h, Message::MouseMoveRel { dx: -5, dy: 0 }); // outward push
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