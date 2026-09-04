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
//! The cursor on the client is steered by a **closed loop**
//! ([`PositionFollower`]): every received motion frame advances a
//! commanded position and is injected verbatim (zero added latency on a
//! healthy wire), and every loop tick the real cursor is compared
//! against the command — any residual (the OS's pointer acceleration
//! over-moving, a lost frame under-moving, a stall) is corrected with a
//! damped injection. There is no replay queue, so no backlog can ever
//! form. All OS-specific work (moving the real cursor, injecting keys,
//! touching the clipboard) lives behind the [`Injector`] trait; this
//! module is plain message dispatch and can be tested with a fake
//! injector.

use std::io;
use std::net::{TcpStream, UdpSocket};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use kvmshare_log::{log_debug, log_trace, log_warn};
use kvmshare_protocol::message::{KeyKind, Layout, Message, ScreenInfo};

use crate::motion::{MotionProbe, PositionFollower, MOTION_PERIOD};
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
    /// Whether this backend places the cursor **absolutely** for motion
    /// (each `move_rel` delta accumulated and the cursor set exactly)
    /// rather than forwarding relative input for the OS to transform.
    ///
    /// Absolute placement bypasses the client OS's pointer acceleration
    /// entirely: the shared cursor lands exactly where commanded, the
    /// OS can never over-run the hand, and a lost frame self-heals (the
    /// next set lands the whole command). Backends that return `true`
    /// skip the closed-loop follower — the placement *is* the loop. The
    /// server compensates for its own pointer transform by scaling the
    /// counts it sends (see `GainTracker`), so the client cursor mirrors
    /// the server cursor pixel-for-pixel.
    fn absolute_motion(&self) -> bool {
        false
    }
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
/// How long the TCP read can block while idle (not controlled). While
/// being controlled the timeout drops to one motion period, so the
/// closed-loop tick and beacons stay fresh even on a quiet wire.
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
        // The cursor is steered by a closed loop (see
        // [`PositionFollower`]): received frames advance the command and
        // are injected verbatim; each tick the real cursor is corrected
        // toward the command. No queue — no backlog can form.
        let mut follower = PositionFollower::default();
        // Sequence of the newest applied cursor-stream frame (stale and
        // duplicate datagrams are dropped).
        let mut motion_seq: u32 = 0;
        // Motion telemetry (trace): the commanded trajectory (entry point
        // plus every received frame) vs where the real cursor actually
        // is — the residual gap is exactly the lag a windowed magnitude
        // probe cannot see. See [`MotionProbe`].
        let mut probe = MotionProbe::default();
        // Current TCP read timeout, so it is only reconfigured on change.
        let mut timeout: Option<Duration> = Some(READ_TIMEOUT);
        // Periodic duties (resolution check, clipboard, keepalive) only
        // run at this cadence while controlled — they must never crowd
        // the motion tick.
        let mut last_duty = Instant::now();
        let duty_interval = CLIPBOARD_INTERVAL;
        loop {
            // 1. Drain the UDP cursor stream. Only relative motion rides
            //    it; beacons are sent below. Stale/duplicate frames are
            //    dropped by sequence number (a reordered frame is older
            //    traffic the cursor already moved past). Each accepted
            //    frame advances the commanded position and is injected
            //    verbatim (feedforward — the wire cadence *is* the
            //    cursor cadence).
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
                                Self::apply_motion(dx, dy, &mut *injector, &mut follower, &mut probe);
                            }
                        }
                    }
                    Err(_) => break, // WouldBlock or link error: drained
                }
            }

            // 2. Control channel: one TCP message, or a timeout. While
            //    being controlled, wake at motion cadence so corrections
            //    and beacons stay fresh even on a quiet wire.
            let want = if controlled { Some(MOTION_PERIOD) } else { Some(READ_TIMEOUT) };
            if want != timeout {
                self.transport.set_read_timeout(want)?;
                timeout = want;
            }
            match self.transport.recv()? {
                RecvResult::Msg(msg) => {
                    let entered = matches!(msg, Message::Enter { .. });
                    let left = matches!(msg, Message::Leave { .. });
                    self.dispatch(msg, &mut *injector, &mut follower, &mut probe);
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
                    // Periodic duties run at a cadence, never on every
                    // wakeup: a slow clipboard read or a monitor query
                    // must not crowd the motion tick while controlled.
                    if last_duty.elapsed() >= duty_interval {
                        last_duty = Instant::now();
                        // Service the outbox, notice resolution changes,
                        // push clipboard changes up, keep the link warm.
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
                            // Skip content we just applied from the
                            // server, and content we have already sent.
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
            }

            // 3. The closed-loop tick while being controlled: steer the
            //    cursor, read the real position once, beacon it to the
            //    server (edge crossings depend on it), and sample
            //    telemetry — all from the same fresh position.
            if controlled {
                if injector.absolute_motion() {
                    // Absolute backends place the cursor exactly at the
                    // commanded position once per tick. Every frame that
                    // arrived since the last tick is represented, so the
                    // cursor moves once — evenly, at the loop's cadence
                    // — and a burst of UDP datagrams can never clump it
                    // into visible steps. (The placement is skipped when
                    // the cursor is already there: an idle cursor costs
                    // nothing, and a stray native move is re-placed —
                    // self-healing.)
                    let (rx, ry) = injector.cursor_position();
                    let (cx, cy) = follower.command();
                    if (cx, cy) != (rx, ry) {
                        injector.move_cursor(cx, cy);
                    }
                } else {
                    let (rx, ry) = injector.cursor_position();
                    if let Some((cx, cy)) = follower.correct((rx, ry)) {
                        injector.move_rel(cx, cy);
                    }
                }
                let (rx, ry) = injector.cursor_position();
                if last_cursor_beacon.elapsed() >= CURSOR_BEACON_INTERVAL {
                    last_cursor_beacon = Instant::now();
                    self.send_beacon(rx, ry, &mut beacon_failed)?;
                }
                if probe.due() {
                    let (ex, ey) = follower.error((rx, ry));
                    probe.sample((rx, ry), &mut |rx, ry, ax, ay, _ex, _ey, gx, gy| {
                        log_trace!(
                            "motion req=({rx},{ry}) act=({ax},{ay}) err=({ex},{ey}) real=({gx},{gy})"
                        );
                    });
                }
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

    /// Apply one motion frame to the local machine. Relative backends
    /// get the follower's feedforward portion (the closed loop delivers
    /// the rest). Absolute backends only advance the command — the
    /// cursor itself is placed at the command on the loop tick (see the
    /// run loop), so the wire cadence never reaches the cursor directly
    /// and a burst of datagrams cannot clump it. Either way the command
    /// (and hence the telemetry) tracks the full frame.
    fn apply_motion(
        dx: i32,
        dy: i32,
        injector: &mut dyn Injector,
        follower: &mut PositionFollower,
        probe: &mut MotionProbe,
    ) {
        if injector.absolute_motion() {
            follower.advance(dx, dy);
        } else {
            let (dx, dy) = follower.push(dx, dy);
            injector.move_rel(dx, dy);
        }
        probe.requested(dx, dy);
    }

    /// Before an ordering-critical event (button, key, wheel) the cursor
    /// must sit on the command point. Absolute backends place it there
    /// exactly — the placement is the loop, and the command is the only
    /// truth. Relative backends flush the follower's residual as one
    /// capped move, so a click lands where the motion pointed without a
    /// wedged cursor dragging it across the screen.
    fn flush_before_event(injector: &mut dyn Injector, follower: &mut PositionFollower) {
        if injector.absolute_motion() {
            let (cx, cy) = follower.command();
            injector.move_cursor(cx, cy);
            return;
        }
        let (rx, ry) = injector.cursor_position();
        if let Some((cx, cy)) = follower.flush((rx, ry)) {
            injector.move_rel(cx, cy);
        }
    }

    /// Apply one server message to the local machine. Motion advances
    /// the [`PositionFollower`] command (and is injected verbatim);
    /// ordering-critical events first flush the follower's residual so a
    /// click, key or wheel lands where the motion pointed.
    fn dispatch(
        &mut self,
        msg: Message,
        injector: &mut dyn Injector,
        follower: &mut PositionFollower,
        probe: &mut MotionProbe,
    ) {
        match msg {
            Message::MouseMoveRel { dx, dy } => {
                // Defensive: motion normally arrives over UDP (drained in
                // run()); a frame on the control channel follows the same
                // path.
                Self::apply_motion(dx, dy, injector, follower, probe);
            }
            Message::MouseMoveAbs { x, y } => {
                // Absolute placement (defensive — entry placement travels
                // in the Enter message; the session never emits absolute
                // moves in the motion stream). The command follows the
                // placed point.
                injector.move_cursor(x, y);
                follower.reanchor(x, y);
            }
            Message::Enter { screen_id: _, x, y } => {
                log_trace!("control entered at ({x},{y})");
                follower.enter(x, y);
                injector.enter();
                // Absolute placement at the entry point only — from here
                // on the motion stream is relative (see
                // [`Injector::move_rel`]). Anchor the command and the
                // telemetry at where the cursor actually ended up (read
                // back, so placement rounding never shows up as drift).
                injector.move_cursor(x, y);
                let (rx, ry) = injector.cursor_position();
                follower.reanchor(rx, ry);
                probe.enter((rx, ry));
            }
            Message::Leave { screen_id: _ } => {
                log_trace!("control left");
                follower.leave();
                probe.leave();
                injector.leave();
            }
            // Buttons, keys and wheel are ordering-critical: the cursor
            // must sit on the command point before the event fires.
            Message::MouseButton { button, pressed } => {
                Self::flush_before_event(injector, follower);
                log_trace!("button {button} {}", if pressed { "down" } else { "up" });
                injector.button(button, pressed);
            }
            Message::MouseWheel { dx, dy } => {
                Self::flush_before_event(injector, follower);
                log_trace!("wheel {dx},{dy}");
                injector.wheel(dx, dy);
            }
            Message::Key { kind, key } => {
                Self::flush_before_event(injector, follower);
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
        let _ = client.run(Box::new(injector), &rx);
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
        let _ = client.run(Box::new(injector), &rx);

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
}