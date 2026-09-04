//! The client side.
//!
//! Connects to a server over **two links** (see [`crate::server`]):
//!
//! * **TCP** — the reliable control channel: handshake, Enter/Leave,
//!   buttons, keys, wheel, clipboard, layout, keepalive.
//! * **UDP** — the cursor stream: relative motion in, real-cursor
//!   beacons out. Additive and loss-tolerant, so the cursor's latency is
//!   never coupled to the reliable stream's buffering.
//!
//! Motion frames arrive over UDP in clumps (bursts, scheduling jitter).
//! Injecting each as it lands makes the visible cursor jump in
//! syncopation with the wire, so frames are replayed through
//! [`PacedFrames`] at the fixed cadence — smooth under load, honest to
//! the client OS's pointer acceleration. All OS-specific work (moving the
//! real cursor, injecting keys, touching the clipboard) lives behind the
//! [`Injector`] trait; this module is plain message dispatch and can be
//! tested with a fake injector.

use std::io;
use std::net::{TcpStream, UdpSocket};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use kvmshare_log::{log_debug, log_trace, log_warn};
use kvmshare_protocol::message::{KeyKind, Layout, Message, ScreenInfo};

use crate::motion::{PacedFrames, MOTION_PERIOD};
use crate::transport::{RecvResult, Transport};
use crate::udp;

/// The platform hook the client calls to affect the local machine.
///
/// The server controls this machine, so every call here is "make the
/// local machine do what the server asked".
pub trait Injector: Send {
    /// The client's screen shape (resolution and scale). Re-queried on
    /// every loop iteration so resolution changes are noticed and
    /// reported back to the server.
    fn screen_info(&mut self) -> ScreenInfo;
    /// Move the local cursor to local screen pixels `(x, y)` (absolute
    /// placement: used when control *enters* and for any explicit
    /// positioning, never in the motion stream).
    fn move_cursor(&mut self, x: i32, y: i32);
    /// Apply relative cursor motion. The client OS applies its own
    /// pointer transform (acceleration / speed settings) to relative
    /// input, so the shared cursor feels exactly like a physical mouse on
    /// this machine — the model every mature KVM uses. The motion stream
    /// is relative; absolute positioning is reserved for entry points.
    fn move_rel(&mut self, dx: i32, dy: i32);
    /// The cursor's *real* current position in local screen pixels (what
    /// the OS reports, after applying its transform to the relative
    /// motion we injected). Reported to the server on a cadence while
    /// being controlled, so the server knows exactly where the shared
    /// cursor sits for edge crossings.
    fn cursor_position(&mut self) -> (i32, i32);
    fn button(&mut self, button: u8, pressed: bool);
    fn wheel(&mut self, dx: i32, dy: i32);
    /// Press/release/repeat a key, addressed by its canonical USB HID
    /// usage id (the platform backend maps it to the local key identity).
    fn key(&mut self, kind: KeyKind, key: u32);
    /// Control has entered this machine: hide the local cursor so the
    /// server's stream is the only visible one.
    fn enter(&mut self);
    /// Control has left this machine: show the local cursor again.
    fn leave(&mut self);
    /// Put `data` into the local clipboard (received from the server).
    fn clipboard(&mut self, mime: &str, data: &[u8]);
    /// Read the current local clipboard, if any.
    fn clipboard_get(&mut self) -> Option<(String, Vec<u8>)>;
    /// The last clipboard content applied from a *remote* source (set via
    /// [`Injector::clipboard`]). Clipboard pollers compare against this
    /// so content that arrived from the server is never echoed back.
    fn clipboard_last_injected(&mut self) -> Option<(String, Vec<u8>)>;
    /// Whether the local OS is dropping injected input right now (e.g. an
    /// elevated or input-isolated window on Windows swallows SendInput).
    /// The client loop reports this to the server, which brings control
    /// home so the user is never trapped on a screen that cannot move.
    /// Defaults to `false`; platforms that cannot detect this simply
    /// never report it.
    fn input_blocked(&mut self) -> bool {
        false
    }
}

/// How often the client sends a keepalive when idle.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(2);
/// How long the TCP read can block while idle (nothing being paced, not
/// controlled). While the client is pacing motion or being controlled the
/// timeout drops to one motion period, so buffered motion and periodic
/// duties are never delayed by a long block.
const READ_TIMEOUT: Duration = Duration::from_millis(100);
/// How often the client polls its local clipboard to push changes up.
const CLIPBOARD_INTERVAL: Duration = Duration::from_millis(500);
/// How often the client re-checks whether local input injection is being
/// dropped by the OS (cheap; guards against a continuous message stream
/// starving the check).
const BLOCK_CHECK_INTERVAL: Duration = Duration::from_millis(150);
/// How often the client reports its real cursor position to the server
/// while being controlled. The server anchors its virtual cursor and
/// edge decisions on these (the client's OS applies its own pointer
/// acceleration to relative motion, so the reported position is the only
/// ground truth) — a tight cadence keeps crossings exact without
/// flooding the wire. Beacons ride UDP; a lost one is replaced by the
/// next, which is fine — the stream is a continuous sample, not a
/// command.
const CURSOR_BEACON_INTERVAL: Duration = Duration::from_millis(8);

/// A connected client.
#[derive(Debug)]
pub struct Client {
    transport: Transport,
    /// The UDP cursor stream: relative motion in, beacons out. Connected
    /// to the server's address; the first datagram sent is the
    /// registration that teaches the server where to route motion.
    udp: UdpSocket,
    /// Sequence for outgoing beacon datagrams (registration used 0).
    udp_seq: u32,
    /// The id this machine has in the server's layout.
    own_id: u8,
    /// The full layout as last sent by the server (includes the server's
    /// own screen). Clients mostly ignore it today; it exists so future
    /// features (relative position awareness, on-screen indicators) have
    /// the data they need.
    layout: Layout,
}

impl Client {
    /// Connect to `addr`, say hello with `name`, and wait for the server's
    /// welcome. Opens the UDP cursor stream and registers it with the
    /// server. Returns the client ready to run.
    pub fn connect(addr: &str, name: &str, info: ScreenInfo) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        let mut transport = Transport::with_read_timeout(stream, Some(READ_TIMEOUT))?;
        transport.send(&Message::Hello { version: kvmshare_protocol::VERSION, name: name.to_owned(), info })?;

        let (own_id, layout) = match transport.recv()? {
            RecvResult::Msg(Message::Welcome { server_version, layout, own_screen_id }) => {
                if server_version != kvmshare_protocol::VERSION {
                    return Err(io::Error::other(format!(
                        "server speaks v{server_version}, client speaks v{}",
                        kvmshare_protocol::VERSION
                    )));
                }
                (own_screen_id, layout)
            }
            RecvResult::Msg(Message::Error { code, text }) => Err(io::Error::other(format!(
                "server rejected the connection ({code}): {text}"
            )))?,
            other => return Err(io::Error::other(format!("unexpected first message: {other:?}"))),
        };

        // The cursor stream: one UDP socket, connected to the server's
        // address, non-blocking so the loop can drain it every iteration.
        let udp = UdpSocket::bind(("0.0.0.0", 0))?;
        udp.set_nonblocking(true)?;
        udp.connect(addr)?;
        // First datagram = registration: it carries the client id, so the
        // server learns both who we are and where to send motion.
        udp.send(&udp::pack(own_id, 0, &Message::KeepAlive))?;

        Ok(Self { transport, udp, udp_seq: 1, own_id, layout })
    }

    /// Run the message loop until the connection closes.
    ///
    /// `outbox` lets the app layer push messages to the server (future
    /// control messages). Drained every loop iteration.
    pub fn run(mut self, mut injector: Box<dyn Injector>, outbox: &Receiver<Message>) -> io::Result<()> {
        let mut last_info = injector.screen_info();
        let mut last_keepalive = Instant::now();
        let mut last_clip_check = Instant::now();
        let mut last_clip_seen: Option<(String, Vec<u8>)> = None;
        let mut last_block_check = Instant::now();
        let mut last_cursor_beacon = Instant::now();
        let mut blocked_reported = false;
        let mut controlled = false;
        let mut beacon_failed = false;
        // Motion pacing: incoming frames are replayed at the fixed
        // cadence (see [`PacedFrames`]), so network clumps never turn
        // into cursor jumps.
        let mut paced = PacedFrames::default();
        // Start as if a frame went out one period ago, so the first queued
        // frame emits immediately.
        let mut paced_since = Instant::now() - MOTION_PERIOD;
        // Sequence of the newest applied cursor-stream frame (stale and
        // duplicate datagrams are dropped).
        let mut motion_seq: u32 = 0;
        // Current TCP read timeout, so it is only reconfigured on change.
        let mut timeout: Option<Duration> = Some(READ_TIMEOUT);
        loop {
            // 1. Drain the UDP cursor stream. Only relative motion rides
            //    it; beacons are sent below. Stale/duplicate frames are
            //    dropped by sequence number (additive motion loses
            //    nothing — a reordered frame is older traffic the cursor
            //    already moved past).
            loop {
                let mut buf = [0u8; 512];
                match self.udp.recv(&mut buf) {
                    Ok(n) => {
                        let Some(d) = udp::unpack(&buf[..n]) else { continue };
                        if d.id != self.own_id || !udp::is_newer(d.seq, motion_seq) {
                            continue;
                        }
                        motion_seq = d.seq;
                        if let Message::MouseMoveRel { dx, dy } = d.msg {
                            // Motion outside Enter/Leave is dropped: it can
                            // beat the TCP Enter on the wire (different
                            // transports), and at most a frame or two at
                            // the seam is lost — self-correcting.
                            if controlled {
                                paced.push(dx, dy);
                            }
                        }
                    }
                    Err(_) => break, // WouldBlock or link error: drained
                }
            }

            // 2. Replay paced motion at the cadence (no-op when nothing
            //    is queued or nothing is due yet).
            paced.flush(&mut paced_since, &mut |dx, dy| injector.move_rel(dx, dy));

            // 3. Control channel: one TCP message, or a timeout. While
            //    pacing motion or being controlled, wake at motion
            //    cadence so clumps keep draining and beacons stay fresh
            //    even on a quiet wire.
            let want = if controlled || paced.has_pending() {
                Some(MOTION_PERIOD)
            } else {
                Some(READ_TIMEOUT)
            };
            if want != timeout {
                self.transport.set_read_timeout(want)?;
                timeout = want;
            }
            match self.transport.recv()? {
                RecvResult::Msg(msg) => {
                    let entered = matches!(msg, Message::Enter { .. });
                    let left = matches!(msg, Message::Leave { .. });
                    self.dispatch(msg, &mut *injector, &mut paced);
                    if entered {
                        controlled = true;
                        // Control just landed: report where we are right
                        // away so the server's edge state is fresh from
                        // the first moment.
                        let (x, y) = injector.cursor_position();
                        self.send_beacon(x, y, &mut beacon_failed)?;
                        last_cursor_beacon = Instant::now();
                    } else if left {
                        controlled = false;
                    }
                }
                RecvResult::Eof => break,
                RecvResult::NoData => {
                    // Nothing from the server: service the outbox, notice
                    // resolution changes, push clipboard changes up, and
                    // keep the link warm.
                    while let Ok(msg) = outbox.try_recv() {
                        self.transport.send(&msg)?;
                    }
                    let info = injector.screen_info();
                    if info != last_info {
                        self.transport.send(&Message::ScreenInfo { info })?;
                        last_info = info;
                    }
                    if last_clip_check.elapsed() >= CLIPBOARD_INTERVAL {
                        last_clip_check = Instant::now();
                        let cur = injector.clipboard_get();
                        // Skip content we just applied from the server, and
                        // content we have already sent.
                        if let Some(cur) = cur {
                            if last_clip_seen.as_ref() != Some(&cur)
                                && injector.clipboard_last_injected().as_ref() != Some(&cur)
                            {
                                let (mime, data) = cur.clone();
                                self.transport.send(&Message::Clipboard { mime, data })?;
                                last_clip_seen = Some(cur);
                            }
                        }
                    }
                    if last_keepalive.elapsed() >= KEEPALIVE_INTERVAL {
                        self.transport.send(&Message::KeepAlive)?;
                        last_keepalive = Instant::now();
                    }
                }
            }

            // 4. While being controlled, keep the server fed with our
            //    real cursor position over the UDP stream (see
            //    [`CURSOR_BEACON_INTERVAL`]). Runs after every wakeup so a
            //    continuous message stream never starves it.
            if controlled && last_cursor_beacon.elapsed() >= CURSOR_BEACON_INTERVAL {
                last_cursor_beacon = Instant::now();
                let (x, y) = injector.cursor_position();
                self.send_beacon(x, y, &mut beacon_failed)?;
            }

            // 5. After every wakeup (message or timeout): if the local OS
            //    is dropping our injected input (elevated /
            //    input-isolated window), tell the server so it brings
            //    control home. Rate-limited; a latch avoids spamming the
            //    server while blocked.
            if last_block_check.elapsed() >= BLOCK_CHECK_INTERVAL {
                last_block_check = Instant::now();
                let blocked = injector.input_blocked();
                if blocked && !blocked_reported {
                    blocked_reported = true;
                    log_warn!("local input injection blocked — asking the server to return control");
                    self.transport.send(&Message::InputBlocked)?;
                } else if !blocked {
                    blocked_reported = false;
                }
            }
        }
        Ok(())
    }

    /// Send one real-cursor beacon over the UDP stream. The stream is
    /// loss-tolerant, but a dead link is worth knowing about — log the
    /// first failure, then stay quiet (the TCP keepalives will surface a
    /// genuinely dead connection).
    fn send_beacon(&mut self, x: i32, y: i32, failed: &mut bool) -> io::Result<()> {
        let bytes = udp::pack(self.own_id, self.udp_seq, &Message::CursorPos { x, y });
        self.udp_seq = self.udp_seq.wrapping_add(1);
        match self.udp.send(&bytes) {
            Ok(_) => Ok(()),
            Err(e) => {
                if !*failed {
                    log_warn!("cursor beacon send failed (first): {e}");
                    *failed = true;
                }
                Ok(())
            }
        }
    }

    /// Apply one server message to the local machine. Ordering-critical
    /// events flush any paced motion first, so a click, key or control
    /// transition lands after the motion that preceded it.
    fn dispatch(&mut self, msg: Message, injector: &mut dyn Injector, paced: &mut PacedFrames) {
        match msg {
            Message::MouseMoveRel { dx, dy } => {
                // Cursor-stream frames normally arrive over UDP (drained
                // in run()); a frame that came over the control channel
                // takes the same paced path.
                paced.push(dx, dy);
            }
            Message::MouseMoveAbs { x, y } => {
                // Absolute placement (defensive — entry placement travels
                // in the Enter message; the session never emits absolute
                // moves in the motion stream).
                paced.drain_now(&mut |dx, dy| injector.move_rel(dx, dy));
                injector.move_cursor(x, y);
            }
            Message::Enter { screen_id: _, x, y } => {
                paced.drain_now(&mut |dx, dy| injector.move_rel(dx, dy));
                log_trace!("control entered at ({x},{y})");
                injector.enter();
                // Absolute placement at the entry point only — from here
                // on the motion stream is relative (see [`Injector::move_rel`]).
                injector.move_cursor(x, y);
            }
            Message::Leave { screen_id: _ } => {
                paced.drain_now(&mut |dx, dy| injector.move_rel(dx, dy));
                log_trace!("control left");
                injector.leave();
            }
            // Buttons, keys and wheel are ordering-critical: they land
            // after the motion that positioned the cursor.
            Message::MouseButton { button, pressed } => {
                paced.drain_now(&mut |dx, dy| injector.move_rel(dx, dy));
                log_trace!("button {button} {}", if pressed { "down" } else { "up" });
                injector.button(button, pressed);
            }
            Message::MouseWheel { dx, dy } => {
                paced.drain_now(&mut |dx, dy| injector.move_rel(dx, dy));
                log_trace!("wheel {dx},{dy}");
                injector.wheel(dx, dy);
            }
            Message::Key { kind, key } => {
                paced.drain_now(&mut |dx, dy| injector.move_rel(dx, dy));
                log_trace!("key {kind:?} {key}");
                injector.key(kind, key);
            }
            Message::Clipboard { mime, data } => {
                log_debug!("clipboard from server: {} ({} bytes)", mime, data.len());
                injector.clipboard(&mime, &data);
            }
            Message::Layout { layout } => {
                log_debug!("layout updated: {} screens", layout.screens.len());
                self.layout = layout;
            }
            Message::KeepAlive => {}
            Message::Error { code, text } => log_warn!("server error ({code}): {text}"),
            // Not valid client-side traffic; ignore defensively.
            Message::Hello { .. }
            | Message::Welcome { .. }
            | Message::ScreenInfo { .. }
            | Message::CursorPos { .. }
            | Message::InputBlocked
            | Message::Escape => {}
        }
    }

    pub fn own_id(&self) -> u8 {
        self.own_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, UdpSocket};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;
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

    /// Records calls into a shared vec so the test can inspect them after
    /// the (consuming) run loop has finished. `info` is shared too, so a
    /// test can simulate a resolution change while the loop is running.
    /// `pos` is the simulated real cursor position: absolute moves set
    /// it, relative moves shift it (a fake OS without acceleration).
    struct RecordingInjector {
        calls: Arc<Mutex<Vec<String>>>,
        info: Arc<Mutex<ScreenInfo>>,
        pos: Arc<Mutex<(i32, i32)>>,
    }

    impl Default for RecordingInjector {
        fn default() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                info: Arc::new(Mutex::new(ScreenInfo::default())),
                pos: Arc::new(Mutex::new((0, 0))),
            }
        }
    }

    impl RecordingInjector {
        fn new(info: ScreenInfo) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                info: Arc::new(Mutex::new(info)),
                pos: Arc::new(Mutex::new((0, 0))),
            }
        }
    }

    impl Injector for RecordingInjector {
        fn screen_info(&mut self) -> ScreenInfo {
            *self.info.lock().unwrap()
        }
        fn clipboard_get(&mut self) -> Option<(String, Vec<u8>)> {
            None
        }
        fn clipboard_last_injected(&mut self) -> Option<(String, Vec<u8>)> {
            None
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
        fn clipboard(&mut self, mime: &str, data: &[u8]) {
            self.calls.lock().unwrap().push(format!("clipboard {mime}: {}", String::from_utf8_lossy(data)));
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
        client.run(Box::new(injector), &rx).unwrap();

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
        let client_thread = thread::spawn(move || client.run(Box::new(injector), &out_rx).unwrap());
        thread::sleep(Duration::from_millis(200));
        *info_handle.lock().unwrap() = ScreenInfo { width: 3840, height: 2160, scale: 2.0 };

        client_thread.join().unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok("screeninfo seen".to_string()));
    }

    /// The full UDP cursor path, exactly as the real server provides it:
    /// TCP handshake + control channel, UDP socket on the same port, the
    /// client's registration datagram teaching us where to send motion.
    #[test]
    fn udp_motion_stream_is_paced_and_deduped() {
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
        let client = Client::connect(&format!("127.0.0.1:{port}"), "test", injector.screen_info()).unwrap();
        let (_tx, rx) = mpsc::channel::<Message>();
        // The fake server closes the TCP side after ~310ms; run until EOF.
        let _ = client.run(Box::new(injector), &rx);
        let _ = calls;

        let calls = calls_handle.lock().unwrap().clone();
        // The burst is replayed one frame per motion period, in order —
        // the cursor tracks the hand smoothly instead of jumping 60px at
        // once — and the stale duplicate never re-applies.
        let rels: Vec<&String> = calls.iter().filter(|c| c.starts_with("rel ")).collect();
        assert_eq!(
            rels,
            vec![&"rel 10,0".to_string(), &"rel 20,0".to_string(), &"rel 30,0".to_string()],
            "motion must be paced per frame with no duplicates, got {rels:?}"
        );
        assert!(calls.contains(&"enter".to_string()));
        assert!(calls.iter().any(|c| c == "move 100,100"));
    }
}