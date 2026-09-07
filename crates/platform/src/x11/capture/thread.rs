//! The capture thread: owns the X connection, drains raw events,
//! executes engine commands, and runs the beacon thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xinput;
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::Event as XEvent;
use x11rb::rust_connection::RustConnection;

use kvmshare_log::{log_debug, log_error, log_info, log_trace, log_warn};
use kvmshare_protocol::message::{KeyKind, Message};

use kvmshare_core::motion::PendingMotion;

use super::events::{is_escape, raw_xy, select_input_events, CaptureCommand, Held, REPEAT_DELAY, REPEAT_INTERVAL};
use crate::evdev::EvdevReader;
use crate::x11::buttons::{self, XButton};

/// Idle poll pause. The capture loop polls for X events instead of
/// blocking, so it can also apply engine commands, forward motion at the
/// capped cadence, and synthesize key repeats while no input is flowing.
/// 2 ms is far below human perception and keeps idle CPU negligible.
const POLL_PAUSE: Duration = Duration::from_millis(2);

/// Minimum gap between forwarded position beacons. The real pointer
/// position is sampled at device rate (up to 1000 Hz on a modern mouse);
/// forwarding every sample floods the wire and the session channel with
/// near-identical positions, and under load the backlog makes every
/// beacon *stale* — the session then re-anchors to old positions, which
/// delays the edge-park confirmation and makes crossings hesitate.
/// Beacons are therefore coalesced like motion: only the newest position
/// is kept and sent at this cadence, so a beacon is never older than one
/// period plus the poll pause, and the stream costs a few frames per
/// second instead of a thousand.
const BEACON_PERIOD: Duration = Duration::from_millis(6);

/// Captures local input and controls the local cursor on a background
/// thread. Owns its X connection exclusively — the engine talks to it
/// only through [`CaptureCommand`]s, and the clipboard/position queries
/// use the engine's own separate connection.
struct InputCapture {
    conn: RustConnection,
    root: xproto::Window,
    tx: Sender<Message>,
    cmd_rx: Receiver<CaptureCommand>,
    motion: PendingMotion,
    /// The newest real pointer position seen but not yet forwarded as a
    /// beacon (coalesced to [`BEACON_PERIOD`]; see the const docs).
    beacon: Option<(i32, i32)>,
    /// When the last beacon was sent (rate limiter).
    last_beacon: Option<Instant>,
    /// Motion telemetry (trace): forwarded raw counts vs pc's own real
    /// (post-acceleration) pointer travel — the px-per-count reference
    /// the client's feel should match. Fed at the send points and
    /// sampled on the poll cadence (see `MotionProbe`).
    probe: kvmshare_core::motion::MotionProbe,
    /// Keys the device has pressed but not yet released (HID usage → state).
    held: HashMap<u32, Held>,
    /// Whether *we* currently hold the pointer+keyboard grab. Shared
    /// with the beacon thread: while the local pointer is grabbed and
    /// parked, position beacons are meaningless and must not be sent.
    grabbed: Arc<AtomicBool>,
    /// The latest real pointer position, polled by the beacon thread on
    /// its own X connection — so the event hot path never waits on a
    /// round-trip reply. Fed to the probe and to the coalesced beacon
    /// when the local pointer is free.
    real_pos: Arc<Mutex<Option<(i32, i32)>>>,
    /// Kernel-level device isolation while the cursor is on a client.
    /// Always present; it engages the moment `/dev/input` becomes
    /// readable (grab-only until then).
    evdev: EvdevReader,
    /// Heartbeat for the server supervisor (`Liveness::capture_tick_ms`):
    /// bumped every capture-loop iteration. This thread owns the local
    /// input grab while remote, so its wedge is exactly what traps the
    /// machine's input — the supervisor must see it.
    capture_tick: Arc<AtomicU64>,
}

/// Open the X display (`None` = `$DISPLAY`), select XI2 raw events on the
/// root window, and start the capture thread.
///
/// Returns the channel the server's main loop reads local input from,
/// plus the capture thread's heartbeat for the server supervisor.
/// `cmd_rx` delivers the engine's cursor-control commands.
pub fn start(
    display: Option<&str>,
    cmd_rx: Receiver<CaptureCommand>,
) -> Result<(Receiver<Message>, Arc<AtomicU64>), String> {
    let (conn, screen_num) = RustConnection::connect(display).map_err(|e| format!("X11 connect: {e}"))?;
    let root = conn.setup().roots[screen_num].root;

    // XI2 handshake (raw events need server XI >= 2.0).
    let version = xinput::xi_query_version(&conn, 2, 0).map_err(|e| format!("XI2 query: {e}"))?.reply().map_err(|e| format!("XI2 reply: {e}"))?;
    if version.major_version < 2 {
        return Err(format!("XI2 required, server has XI {}.{}", version.major_version, version.minor_version));
    }

    select_input_events(&conn, root)?;
    // XFixes is needed for cursor hide/show (also used by the engine).
    if conn.extension_information(xfixes::X11_EXTENSION_NAME).map_err(|e| format!("XFixes query: {e}"))?.is_none() {
        return Err("XFixes extension not available".into());
    }
    conn.xfixes_query_version(5, 0).map_err(|e| format!("XFixes version: {e}"))?;
    log_info!("input capture started (XI2 raw events)");

    let (tx, rx) = mpsc::channel();
    // The evdev reader is always started; it isolates the devices at the
    // kernel the moment they become readable. Until then the server runs
    // grab-only (raw-reading apps may react to forwarded input).
    let evdev = EvdevReader::start(tx.clone());
    // Shared between the event thread and the beacon thread: the grab
    // state (beacons must stop while the pointer is grabbed) and the
    // latest polled pointer position (the event thread feeds it to the
    // probe without ever doing a round-trip itself).
    let grabbed = Arc::new(AtomicBool::new(false));
    let real_pos = Arc::new(Mutex::new(None));
    let capture_tick = Arc::new(AtomicU64::new(0));
    let capture = InputCapture {
        conn,
        root,
        tx: tx.clone(),
        cmd_rx,
        motion: PendingMotion::default(),
        beacon: None,
        last_beacon: None,
        probe: kvmshare_core::motion::MotionProbe::default(),
        held: HashMap::new(),
        grabbed: grabbed.clone(),
        real_pos: real_pos.clone(),
        evdev,
        capture_tick: capture_tick.clone(),
    };
    thread::spawn(move || {
        if let Err(e) = capture.run_forever() {
            log_error!("input capture stopped: {e}");
        }
    });
    // The real pointer position is polled on its own thread with its own
    // X connection: a synchronous `QueryPointer` round-trip on the event
    // thread stalled motion flushing whenever the X server was busy — a
    // busy server (compositor, a game, heavy load) delayed the reply, and
    // with it the cursor stream, which showed up as the cursor stopping
    // for a beat. Polling is also strictly unnecessary while the pointer
    // is grabbed (beacons are suppressed then anyway), so the beacon
    // thread skips the round-trip entirely in that state.
    spawn_beacon_thread(display.map(str::to_owned), grabbed, real_pos, tx);
    Ok((rx, capture_tick))
}

/// Poll the real pointer position on a dedicated thread with its own X
/// connection, feeding the shared [`InputCapture::real_pos`] and — while
/// the local pointer is free — sending position beacons at
/// [`BEACON_PERIOD`] cadence.
///
/// This is the *only* thread that does a `QueryPointer` round-trip. The
/// event thread forwards raw motion and processes X events without ever
/// waiting on a reply, so a busy X server can delay the beacon thread's
/// round-trips without stalling the cursor stream.
pub fn spawn_beacon_thread(
    display: Option<String>,
    grabbed: Arc<AtomicBool>,
    real_pos: Arc<Mutex<Option<(i32, i32)>>>,
    tx: Sender<Message>,
) {
    thread::spawn(move || {
        let Ok((conn, screen_num)) = RustConnection::connect(display.as_deref()) else { return };
        let root = conn.setup().roots[screen_num].root;
        let mut last: Option<(i32, i32)> = None;
        loop {
            // While the local pointer is grabbed (cursor on a client),
            // beacons are meaningless — skip the round-trip entirely.
            if !grabbed.load(Ordering::Relaxed) {
                if let Ok(cookie) = conn.query_pointer(root) {
                    if let Ok(reply) = cookie.reply() {
                        let (x, y) = (reply.root_x as i32, reply.root_y as i32);
                        *real_pos.lock().unwrap() = Some((x, y));
                        if last != Some((x, y)) {
                            last = Some((x, y));
                            // The session re-anchors on every beacon; the
                            // ordering guarantee (motion first, then the
                            // position) is the event thread's job — the
                            // poll path is a resync, never a command.
                            if tx.send(Message::MouseMoveAbs { x, y }).is_err() {
                                return; // server gone
                            }
                        }
                    }
                }
            }
            thread::sleep(BEACON_PERIOD);
        }
    });
}

impl InputCapture {
    /// The capture loop: drain X events, apply engine commands, forward
    /// coalesced motion and synthesized key repeats at their cadences,
    /// and pause briefly so the thread stays responsive to all of it even
    /// when nothing else is happening. Runs forever; returns only on a
    /// fatal X error.
    fn run_forever(mut self) -> Result<(), String> {
        loop {
            self.capture_tick.store(
                SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
                Ordering::Relaxed,
            );
            loop {
                match self.conn.poll_for_event() {
                    Ok(Some(ev)) => self.on_event(ev),
                    Ok(None) => break,
                    Err(e) => return Err(format!("X event: {e}")),
                }
            }
            while let Ok(cmd) = self.cmd_rx.try_recv() {
                self.apply(cmd);
            }
            self.flush_motion();
            self.flush_beacon();
            self.tick_repeats();
            self.sample_probe();
            thread::sleep(POLL_PAUSE);
        }
    }

    fn on_event(&mut self, ev: XEvent) {
        match ev {
            XEvent::XinputRawMotion(e) => {
                let (dx, dy) = raw_xy(&e.valuator_mask, &e.axisvalues_raw);
                self.motion.push(dx, dy);
            }
            XEvent::XinputMotion(e) => {
                // Real (post-acceleration) pointer position: forward any
                // motion accrued since the last send first, then keep
                // only the newest position for the coalesced beacon (see
                // [`BEACON_PERIOD`]) — motion first, resync after, so the
                // session applies deltas then re-anchors, in order.
                self.flush_motion();
                let x = (e.root_x >> 16) as i32;
                let y = (e.root_y >> 16) as i32;
                if !self.grabbed.load(Ordering::Relaxed) {
                    self.beacon = Some((x, y));
                }
            }
            XEvent::XinputRawButtonPress(e) => self.on_button(e.detail, true),
            XEvent::XinputRawButtonRelease(e) => self.on_button(e.detail, false),
            XEvent::XinputRawKeyPress(e) => {
                if let Some(key) = self.canonical_key(e.detail) {
                    // While the cursor is on a client, Scroll Lock is the
                    // escape hatch: it is consumed here and turned into a
                    // session-level "come home" signal instead of being
                    // forwarded.
                    if self.grabbed.load(Ordering::Relaxed) && is_escape(key) {
                        log_info!("escape (Scroll Lock) pressed while remote — returning control home");
                        self.send(Message::Escape);
                        return;
                    }
                    self.held.insert(key, Held { down_at: Instant::now(), last_repeat: Instant::now() });
                    self.send(Message::Key { kind: KeyKind::Down, key });
                }
            }
            XEvent::XinputRawKeyRelease(e) => {
                if let Some(key) = self.canonical_key(e.detail) {
                    // Swallow the release of a consumed escape press too
                    // (see the press arm above). If the grab already
                    // released, the stray key-up goes nowhere.
                    if self.grabbed.load(Ordering::Relaxed) && is_escape(key) {
                        return;
                    }
                    self.held.remove(&key);
                    self.send(Message::Key { kind: KeyKind::Up, key });
                }
            }
            // Anything else (including core events redirected to us by
            // our own grabs) is deliberately ignored: raw events plus the
            // motion beacons are the only streams we act on.
            _ => {}
        }
    }

    fn on_button(&mut self, x11_button: u32, pressed: bool) {
        match buttons::from_x11(x11_button) {
            XButton::Button(canon) => self.send(Message::MouseButton { button: canon, pressed }),
            // A wheel notch is one press/release pair — only the press is
            // a scroll. Forwarding the release as well would make every
            // notch scroll twice on the client.
            XButton::Wheel(dx, dy) => {
                if pressed {
                    self.send(Message::MouseWheel { dx, dy });
                }
            }
            XButton::Ignore => {}
        }
    }

    /// Forward accumulated motion at the shared [`PendingMotion`] cadence.
    /// Called from the poll loop and before every position beacon, so
    /// motion is never held back longer than one period even when only
    /// raw events arrive (the OS pins the pointer at the screen edge, so
    /// raw motion keeps flowing while position events stop — that
    /// continuous flow is what pushes the virtual cursor across the
    /// boundary).
    fn flush_motion(&mut self) {
        let tx = &self.tx;
        self.motion.flush(&mut |dx, dy| {
            self.probe.requested(dx, dy);
            let _ = tx.send(Message::MouseMoveRel { dx, dy });
        });
    }

    /// Sample pc's own motion telemetry at the probe cadence (trace):
    /// raw counts forwarded vs pc's real post-acceleration pointer
    /// travel over the same window. The ratio is pc's px-per-count — the
    /// reference the client's feel must match. Skipped while the pointer
    /// is remote (grabbed and isolated: XI motion events stop, and
    /// comparing against a pinned cursor would report a bogus zero).
    fn sample_probe(&mut self) {
        // Local only: while the cursor is on a client the local pointer
        // is grabbed and pinned, and comparing against it would report a
        // bogus zero travel.
        if self.grabbed.load(Ordering::Relaxed) || !self.probe.due() {
            return;
        }
        let Some(real) = *self.real_pos.lock().unwrap() else { return };
        self.probe.sample(real, &mut |rx, ry, ax, ay, ex, ey, gx, gy| {
            log_trace!("motion req=({rx},{ry}) act=({ax},{ay}) exp=({ex},{ey}) real=({gx},{gy})");
        });
    }

    /// XI2 raw key events carry X keycodes. The standard X11 evdev
    /// mapping is `keycode = evdev + 8`, so the canonical HID usage is
    /// looked up from `keycode - 8`. Unknown keys are dropped (with a
    /// debug log at the caller) rather than sent with a wrong identity.
    fn canonical_key(&self, keycode: u32) -> Option<u32> {
        let evdev = keycode.checked_sub(8)? as u16;
        crate::keys::hid_from_evdev(evdev)
    }

    /// Execute one engine command on this connection.
    fn apply(&mut self, cmd: CaptureCommand) {
        match cmd {
            CaptureCommand::Warp(x, y) => {
                // src_win = NONE warps from the current position.
                let _ = self.conn.warp_pointer(x11rb::NONE, self.root, 0, 0, 0, 0, x as i16, y as i16);
                let _ = self.conn.flush();
            }
            CaptureCommand::CursorVisible(visible) => {
                let res = if visible {
                    xfixes::show_cursor(&self.conn, self.root)
                } else {
                    xfixes::hide_cursor(&self.conn, self.root)
                };
                if res.is_ok() {
                    let _ = self.conn.flush();
                }
            }
            CaptureCommand::Grab(grab) => self.set_grabbed(grab),
            CaptureCommand::IsolateRemote(remote) => {
                // The evdev reader grabs the physical devices at the
                // kernel (X goes fully silent) and starts forwarding;
                // releasing does the reverse and X capture resumes.
                // Also clear the held-key state: once the devices are
                // kernel-grabbed, X never sees the releases of keys that
                // were pressed before (or during) the isolation, so
                // synthesizing repeats for them here would replay stale
                // presses on the client later — the "media keys saved
                // and applied on the client" bug. The evdev reader
                // tracks its own presses instead.
                self.held.clear();
                if remote {
                    // Grab is async: the kernel grab engages within a
                    // couple of ms, before any forwarded event can leak.
                    self.evdev.set_remote(true);
                } else {
                    // Release is SYNCHRONOUS: the next command in this
                    // queue is the entry warp, and it must land on a
                    // live input stream. Waiting here (bounded) means
                    // the physical mouse is already ungrab'd before the
                    // warp — no swallowed motion at the seam.
                    self.evdev.release_and_wait();
                }
            }
        }
    }

    /// Grab (or release) the pointer and keyboard on this connection.
    ///
    /// While grabbed, physical input is redirected to us and the local
    /// desktop sees nothing of it; the raw stream — which the session
    /// actually consumes — is unaffected by grabs, so forwarding keeps
    /// working. A failed grab (another client holds one, e.g. a window
    /// manager popup) is logged and tolerated: input still forwards, only
    /// the local-echo suppression is lost until the grab succeeds.
    fn set_grabbed(&mut self, grab: bool) {
        if grab == self.grabbed.load(Ordering::Relaxed) {
            return;
        }
        let ok = if grab {
            let mask = xproto::EventMask::BUTTON_PRESS
                | xproto::EventMask::BUTTON_RELEASE
                | xproto::EventMask::POINTER_MOTION;
            // Each step fails as `None` on transport or reply errors.
            let pointer = self
                .conn
                .grab_pointer(
                    false, self.root, mask,
                    xproto::GrabMode::ASYNC, xproto::GrabMode::ASYNC,
                    x11rb::NONE, x11rb::NONE, x11rb::CURRENT_TIME,
                )
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| r.status == xproto::GrabStatus::SUCCESS);
            let keyboard = self
                .conn
                .grab_keyboard(false, self.root, x11rb::CURRENT_TIME, xproto::GrabMode::ASYNC, xproto::GrabMode::ASYNC)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| r.status == xproto::GrabStatus::SUCCESS);
            match (pointer, keyboard) {
                (Some(true), Some(true)) => true,
                (p, k) => {
                    log_warn!("input grab not acquired (pointer: {p:?}, keyboard: {k:?})");
                    false
                }
            }
        } else {
            let _ = self.conn.ungrab_pointer(x11rb::CURRENT_TIME);
            let _ = self.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
            let _ = self.conn.flush();
            false
        };
        self.grabbed.store(if grab { ok } else { false }, Ordering::Relaxed);
        if self.grabbed.load(Ordering::Relaxed) {
            log_debug!("local input grabbed (cursor is on a client)");
        } else if !grab {
            log_debug!("local input released");
        }
    }

    /// Forward the coalesced position beacon at [`BEACON_PERIOD`]
    /// cadence, if one is pending. Called from the poll loop, so a beacon
    /// is never delayed longer than one period after the pointer stops
    /// (the loop wakes every [`POLL_PAUSE`]) — edge parks are confirmed
    /// to the session within ~8 ms even under load.
    fn flush_beacon(&mut self) {
        let Some((x, y)) = self.beacon else { return };
        let now = Instant::now();
        let due = match self.last_beacon {
            Some(t) => now.duration_since(t) >= BEACON_PERIOD,
            None => true,
        };
        if !due {
            return;
        }
        self.last_beacon = Some(now);
        self.beacon = None;
        self.send(Message::MouseMoveAbs { x, y });
    }

    /// Synthesize auto-repeat for physically held keys. Raw events carry
    /// no repeats (they are device transitions only), so clients would
    /// otherwise see a single press for a held key.
    fn tick_repeats(&mut self) {
        if self.held.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut due: Vec<u32> = Vec::new();
        for (key, h) in self.held.iter_mut() {
            if now.duration_since(h.down_at) >= REPEAT_DELAY && now.duration_since(h.last_repeat) >= REPEAT_INTERVAL {
                h.last_repeat = now;
                due.push(*key);
            }
        }
        for key in due {
            self.send(Message::Key { kind: KeyKind::Repeat, key });
        }
    }

    fn send(&self, msg: Message) {
        // The channel is unbounded; the server main loop drains it at its
        // own pace. If the receiver is gone (shutdown), drop the message.
        let _ = self.tx.send(msg);
    }
}