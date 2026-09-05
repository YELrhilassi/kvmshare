use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::sync::mpsc;
use std::time::Duration;
use kvmshare_protocol::message::KeyKind;

/// A fake server on an already-bound listener (no race with the
/// client connecting). Replies with `reply`, then plays `script`, then
/// hangs up so the client sees a clean EOF.
fn fake_server(listener: TcpListener, reply: Message, script: &[Message]) {
    let script: Vec<Message> = script.to_vec();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).unwrap(); // hello
        stream.write_all(&reply.encode()).unwrap();
        stream.flush().unwrap();
        for msg in &script {
            stream.write_all(&msg.encode()).unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_millis(50));
    });
}

/// A clipboard that never has anything on it.
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

/// Records calls into a shared vec so the test can inspect them after
/// the (consuming) run loop has finished. `info` is shared too, so a
/// test can simulate a resolution change while the loop is running.
/// `pos` is the simulated real cursor position: absolute moves set
/// it, relative moves shift it (a fake OS without acceleration).
/// `abs` simulates an absolute-placement backend (SetCursorPos-style):
/// each relative move lands the whole delta exactly.
struct RecordingInjector {
    calls: Arc<Mutex<Vec<String>>>,
    info: Arc<Mutex<ScreenInfo>>,
    pos: Arc<Mutex<(i32, i32)>>,
    abs: bool,
}

impl Default for RecordingInjector {
    fn default() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            info: Arc::new(Mutex::new(ScreenInfo::default())),
            pos: Arc::new(Mutex::new((0, 0))),
            abs: false,
        }
    }
}

impl RecordingInjector {
    fn new(info: ScreenInfo) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            info: Arc::new(Mutex::new(info)),
            pos: Arc::new(Mutex::new((0, 0))),
            abs: false,
        }
    }
    fn absolute(mut self) -> Self {
        self.abs = true;
        self
    }
}

impl Injector for RecordingInjector {
    fn absolute_motion(&self) -> bool {
        self.abs
    }
    fn screen_info(&mut self) -> ScreenInfo {
        *self.info.lock().unwrap()
    }
    fn move_cursor(&mut self, x: i32, y: i32) {
        *self.pos.lock().unwrap() = (x, y);
        self.calls.lock().unwrap().push(format!("move {x},{y}"));
    }
    fn move_rel(&mut self, dx: i32, dy: i32) {
        let mut pos = self.pos.lock().unwrap();
        pos.0 += dx;
        pos.1 += dy;
        drop(pos);
        self.calls.lock().unwrap().push(format!("rel {dx},{dy}"));
    }
    fn cursor_position(&mut self) -> (i32, i32) {
        *self.pos.lock().unwrap()
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

#[test]
fn client_handshakes_and_applies_messages() {
    let port = 39001;
    let welcome = Message::Welcome {
        server_version: kvmshare_protocol::VERSION,
        layout: Layout { screens: vec![] },
        own_screen_id: 7,
    };
    let script = [Message::MouseMoveAbs { x: 100, y: 200 }, Message::Leave { screen_id: 1 }];
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    fake_server(listener, welcome, &script);

    let mut injector = RecordingInjector::new(ScreenInfo { width: 1920, height: 1080, scale: 1.0 });
    let calls_handle = injector.calls.clone();
    let client = Client::connect(&format!("127.0.0.1:{port}"), "test", injector.screen_info()).unwrap();
    assert_eq!(client.own_id(), 7);
    let (_tx, rx) = mpsc::channel::<Message>();
    client.run(Box::new(injector), Box::new(NoClipboard), &rx).unwrap();

    let calls = calls_handle.lock().unwrap().clone();
    assert!(calls.contains(&"move 100,200".to_string()));
    assert!(calls.contains(&"leave".to_string()));
}

#[test]
fn client_rejects_version_mismatch() {
    let port = 39002;
    let welcome = Message::Welcome {
        server_version: 999,
        layout: Layout { screens: vec![] },
        own_screen_id: 7,
    };
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    fake_server(listener, welcome, &[]);
    let info = ScreenInfo { width: 1920, height: 1080, scale: 1.0 };
    let err = Client::connect(&format!("127.0.0.1:{port}"), "test", info).unwrap_err();
    assert!(err.to_string().contains("v999"));
}

#[test]
fn resolution_change_is_reported() {
    let port = 39003;
    let welcome = Message::Welcome {
        server_version: kvmshare_protocol::VERSION,
        layout: Layout { screens: vec![] },
        own_screen_id: 1,
    };
    let (tx, rx) = mpsc::channel::<String>();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf).unwrap(); // hello
        stream.write_all(&welcome.encode()).unwrap();
        // Read until the client's ScreenInfo frame arrives, then hang
        // up so the client loop sees EOF.
        loop {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            // SCREEN_INFO (0x03) in a type byte at offset 4 of a frame.
            if buf[..n].windows(5).any(|w| w[0..4] == *b"KVM1" && w[4] == 0x03) {
                let _ = tx.send("screeninfo seen".into());
                break;
            }
        }
    });

    let info = ScreenInfo { width: 1920, height: 1080, scale: 1.0 };
    let injector = RecordingInjector::new(info);
    let info_handle = injector.info.clone();
    let client = Client::connect(&format!("127.0.0.1:{port}"), "test", info).unwrap();
    let (_out_tx, out_rx) = mpsc::channel::<Message>();

    // Run the client on its own thread and simulate a display scale
    // change while it is running.
    let client_thread =
        thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx).unwrap());
    thread::sleep(Duration::from_millis(200));
    *info_handle.lock().unwrap() = ScreenInfo { width: 3840, height: 2160, scale: 2.0 };

    client_thread.join().unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok("screeninfo seen".to_string()));
}

/// The full UDP cursor path, exactly as the real server provides it:
/// TCP handshake + control channel, UDP socket on the same port, the
/// client's registration datagram teaching us where to send motion.
///
/// Under the closed-loop model the frames are *not* injected verbatim:
/// half is fed forward immediately and the damped corrections deliver
/// the rest against the read-back real cursor. What must hold is that
/// the command trajectory is honored **exactly** — total injected
/// motion equals the sum of the received frames, in order, with the
/// stale duplicate never re-applied.
#[test]
fn udp_motion_stream_reaches_the_command_exactly_and_dedupes() {
    let port = 39004;
    let welcome = Message::Welcome {
        server_version: kvmshare_protocol::VERSION,
        layout: Layout { screens: vec![] },
        own_screen_id: 7,
    };
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let udp = UdpSocket::bind(("127.0.0.1", port)).unwrap();
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    thread::spawn(move || {
        // Control channel: handshake.
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).unwrap(); // hello
        stream.write_all(&welcome.encode()).unwrap();
        stream.flush().unwrap();
        // The client registers its UDP stream; learn its address.
        let mut reg = [0u8; 512];
        let (n, from) = udp.recv_from(&mut reg).unwrap();
        let d = crate::udp::unpack(&reg[..n]).expect("registration datagram");
        assert_eq!(d.id, 7);
        assert_eq!(d.msg, Message::KeepAlive);
        // Take control, then feed the cursor stream.
        thread::sleep(Duration::from_millis(30));
        stream.write_all(&Message::Enter { screen_id: 7, x: 100, y: 100 }.encode()).unwrap();
        stream.flush().unwrap();
        thread::sleep(Duration::from_millis(80));
        // A burst of three motion frames, then a stale duplicate.
        for (seq, dx) in [(1u32, 10), (2, 20), (3, 30)] {
            let bytes = crate::udp::pack(7, seq, &Message::MouseMoveRel { dx, dy: 0 });
            udp.send_to(&bytes, from).unwrap();
        }
        let dup = crate::udp::pack(7, 2, &Message::MouseMoveRel { dx: 99, dy: 0 });
        udp.send_to(&dup, from).unwrap();
        thread::sleep(Duration::from_millis(200));
        // Tell the recorder we are done; the client keeps running
        // until we close the TCP side.
        drop(stream);
    });

    let mut injector = RecordingInjector::new(ScreenInfo { width: 1920, height: 1080, scale: 1.0 });
    let calls_handle = injector.calls.clone();
    let pos_handle = injector.pos.clone();
    let client = Client::connect(&format!("127.0.0.1:{port}"), "test", injector.screen_info()).unwrap();
    let (_tx, rx) = mpsc::channel::<Message>();
    // The fake server closes the TCP side after ~310ms; run until EOF.
    let _ = client.run(Box::new(injector), Box::new(NoClipboard), &rx);
    let _ = calls;

    let calls = calls_handle.lock().unwrap().clone();
    // The command trajectory is honored exactly: entry at (100,100)
    // plus the received frames (10+20+30, 0) — no frame lost to the
    // wire, none duplicated by the stale re-send, and the closed-loop
    // corrections settle on the command rather than overshooting it.
    assert_eq!(*pos_handle.lock().unwrap(), (160, 100), "command trajectory honored exactly");
    let rels: Vec<&String> = calls.iter().filter(|c| c.starts_with("rel ")).collect();
    let total_x: i64 = rels
        .iter()
        .filter_map(|c| c.split_once(' ').and_then(|(_, r)| r.split_once(',')))
        .map(|(x, _)| x.parse::<i64>().unwrap())
        .sum();
    assert_eq!(total_x, 60, "total injected motion equals the command sum");
    assert!(
        rels.iter().all(|c| !c.contains("99")),
        "stale duplicate (seq 2, dx 99) must never be applied, got {rels:?}"
    );
    assert!(
        rels.iter().all(|c| c.ends_with(",0")),
        "no motion outside the command axis, got {rels:?}"
    );
    assert!(calls.contains(&"enter".to_string()));
    assert!(calls.iter().any(|c| c == "move 100,100"));
}

/// An absolute-placement backend (SetCursorPos-style) must land the
/// cursor **exactly on the command trajectory**: every received frame
/// advances the command (the stale duplicate never re-applies), and
/// the per-tick placement puts the cursor at the command — once,
/// evenly, never one clump per datagram burst.
#[test]
fn absolute_backends_land_exactly_on_the_command_via_tick_placement() {
    let port = 39005;
    let welcome = Message::Welcome {
        server_version: kvmshare_protocol::VERSION,
        layout: Layout { screens: vec![] },
        own_screen_id: 7,
    };
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let udp = UdpSocket::bind(("127.0.0.1", port)).unwrap();

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).unwrap(); // hello
        stream.write_all(&welcome.encode()).unwrap();
        stream.flush().unwrap();
        let mut reg = [0u8; 512];
        let (n, from) = udp.recv_from(&mut reg).unwrap();
        let d = crate::udp::unpack(&reg[..n]).expect("registration datagram");
        assert_eq!(d.id, 7);
        thread::sleep(Duration::from_millis(30));
        stream.write_all(&Message::Enter { screen_id: 7, x: 100, y: 100 }.encode()).unwrap();
        stream.flush().unwrap();
        thread::sleep(Duration::from_millis(80));
        for (seq, dx) in [(1u32, 10), (2, 20), (3, 30)] {
            let bytes = crate::udp::pack(7, seq, &Message::MouseMoveRel { dx, dy: 0 });
            udp.send_to(&bytes, from).unwrap();
        }
        let dup = crate::udp::pack(7, 2, &Message::MouseMoveRel { dx: 99, dy: 0 });
        udp.send_to(&dup, from).unwrap();
        thread::sleep(Duration::from_millis(200));
        drop(stream);
    });

    let mut injector = RecordingInjector::new(ScreenInfo { width: 1920, height: 1080, scale: 1.0 }).absolute();
    let calls_handle = injector.calls.clone();
    let pos_handle = injector.pos.clone();
    let client = Client::connect(&format!("127.0.0.1:{port}"), "test", injector.screen_info()).unwrap();
    let (_tx, rx) = mpsc::channel::<Message>();
    let _ = client.run(Box::new(injector), Box::new(NoClipboard), &rx);

    let calls = calls_handle.lock().unwrap().clone();
    // Nothing is injected per datagram — the tick places the whole
    // command. The cursor lands exactly on the command trajectory.
    assert!(
        calls.iter().all(|c| !c.starts_with("rel ")),
        "no per-datagram injection for absolute backends, got {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c == "move 160,100"),
        "tick placement lands on the command, got {calls:?}"
    );
    assert_eq!(*pos_handle.lock().unwrap(), (160, 100), "cursor lands exactly on the command");
    assert!(
        calls.iter().all(|c| !c.contains("99")),
        "stale duplicate (seq 2, dx 99) must never be applied, got {calls:?}"
    );
    assert!(calls.contains(&"enter".to_string()));
    assert!(calls.iter().any(|c| c == "move 100,100"));
}

#[test]
fn button_wheel_and_key_are_injected_in_order_on_the_motion_thread() {
    let port = 39007;
    let welcome = Message::Welcome {
        server_version: kvmshare_protocol::VERSION,
        layout: Layout { screens: vec![] },
        own_screen_id: 7,
    };
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let udp = UdpSocket::bind(("127.0.0.1", port)).unwrap();

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).unwrap(); // hello
        stream.write_all(&welcome.encode()).unwrap();
        stream.flush().unwrap();
        let mut reg = [0u8; 512];
        let (_, from) = udp.recv_from(&mut reg).unwrap();
        stream
            .write_all(&Message::Enter { screen_id: 7, x: 50, y: 50 }.encode())
            .unwrap();
        stream.flush().unwrap();
        thread::sleep(Duration::from_millis(80));
        // Three ordering-critical events in one burst: they must land
        // in wire order, on the motion thread's cadence.
        stream.write_all(&Message::MouseButton { button: 1, pressed: true }.encode()).unwrap();
        stream.write_all(&Message::MouseButton { button: 1, pressed: false }.encode()).unwrap();
        stream.write_all(&Message::Key { kind: KeyKind::Down, key: 65 }.encode()).unwrap();
        stream.flush().unwrap();
        thread::sleep(Duration::from_millis(300));
        drop(stream);
    });

    let mut injector = RecordingInjector::new(ScreenInfo { width: 1920, height: 1080, scale: 1.0 }).absolute();
    let calls_handle = injector.calls.clone();
    let client = Client::connect(&format!("127.0.0.1:{port}"), "test", injector.screen_info()).unwrap();
    let (_tx, rx) = mpsc::channel::<Message>();
    let _ = client.run(Box::new(injector), Box::new(NoClipboard), &rx);

    let calls = calls_handle.lock().unwrap().clone();
    // The button/key events execute in wire order, interleaved with the
    // motion thread's own placement of the command point.
    let events: Vec<String> = calls
        .iter()
        .filter(|c| c.starts_with("button ") || c.starts_with("key "))
        .cloned()
        .collect();
    assert_eq!(
        events,
        vec![
            "button 1 true".to_string(),
            "button 1 false".to_string(),
            "key Down 65".to_string(),
        ],
        "injection events execute in wire order, got {calls:?}"
    );
}
