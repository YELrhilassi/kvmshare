//! The capture thread: owns the X connection, drains raw events,
//! executes engine commands, and runs the beacon thread.
//!
//! The loop is **event-driven**: it blocks in `poll(2)` on the X
//! connection fd plus a command wake pipe, and only wakes for a reason —
//! an X event (raw motion, keys, position), an engine command, or a
//! pending cadence duty (a beacon to send, a held key to repeat). Fully
//! idle it sleeps in the kernel instead of ticking.

use std::collections::HashMap;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xinput;
use x11rb::protocol::xproto::{self};
use x11rb::protocol::Event as XEvent;
use x11rb::rust_connection::RustConnection;

use kvmshare_log::{log_error, log_info, log_trace, log_warn};
use kvmshare_protocol::message::{KeyKind, Message};

use kvmshare_core::motion::PendingMotion;

use super::beacon::spawn_beacon_thread;
use super::events::{
    is_escape, raw_xy, select_input_events, CaptureCommand, Held, REPEAT_DELAY, REPEAT_INTERVAL,
};
use crate::evdev::EvdevReader;
use crate::x11::buttons::{self, XButton};
use crate::x11::geometry::{visible_desktop, VisibleDesktop};

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
pub(crate) const BEACON_PERIOD: Duration = Duration::from_millis(6);
/// The beacon thread's idle interval: once the pointer has sat still for
/// a few consecutive queries, the round-trips back off to this rate —
/// nothing changed, nothing to report. The first query after motion
/// resumes sees a new position and drops back to [`BEACON_PERIOD`]
/// immediately. Crossing latency is untouched: crossings are driven by
/// raw motion deltas and the *client's* beacons, never by this thread's
/// position stream.
pub(crate) const BEACON_IDLE_PERIOD: Duration = Duration::from_millis(25);
/// Consecutive identical positions before the beacon thread backs off.
pub(crate) const BEACON_IDLE_AFTER: u32 = 3;
/// How often the capture loop wakes while the cursor is on a client and
/// nothing else is pending. The supervisor watches the capture thread's
/// heartbeat to detect a wedge, and the heartbeat must keep advancing
/// even when the user is idle away from home — this tick keeps it fresh
/// at ~10 Hz, far above the supervisor's 3 s stall bound, for a rounding
/// error of CPU.
const REMOTE_IDLE_TICK: Duration = Duration::from_millis(100);
/// Pause after a `poll(2)` error other than EINTR (rare and usually
/// permanent — a wedged X transport); prevents a spin.
const POLL_ERROR_PAUSE: Duration = Duration::from_millis(50);

/// Captures local input and controls the local cursor on a background
/// thread. Owns its X connection exclusively — the engine talks to it
/// only through [`CaptureCommand`]s, and the clipboard/position queries
/// use the engine's own separate connection.
pub(crate) struct InputCapture {
    pub(crate) conn: RustConnection,
    pub(crate) root: xproto::Window,
    /// The visible desktop (see [`crate::x11::geometry`]): the session
    /// works in visible-local pixels, so position beacons are translated
    /// out of root pixels here and warps are translated back in.
    pub(crate) visible: VisibleDesktop,
    pub(crate) tx: Sender<Message>,
    pub(crate) cmd_rx: Receiver<CaptureCommand>,
    /// Read end of the command wake pipe: the engine writes a byte on
    /// every command, which releases this loop from poll immediately —
    /// commands are never delayed by an idle wait.
    pub(crate) wake_rx: UnixStream,
    pub(crate) motion: PendingMotion,
    /// The newest real pointer position seen but not yet forwarded as a
    /// beacon (coalesced to [`BEACON_PERIOD`]; see the const docs).
    pub(crate) beacon: Option<(i32, i32)>,
    /// When the last beacon was sent (rate limiter).
    pub(crate) last_beacon: Option<Instant>,
    /// Motion telemetry (trace): forwarded raw counts vs pc's own real
    /// (post-acceleration) pointer travel — the px-per-count reference
    /// the client's feel should match. Fed at the send points and
    /// sampled on the poll cadence (see `MotionProbe`).
    pub(crate) probe: kvmshare_core::motion::MotionProbe,
    /// Keys the device has pressed but not yet released (HID usage → state).
    pub(crate) held: HashMap<u32, Held>,
    /// Whether *we* currently hold the pointer+keyboard grab. Shared
    /// with the beacon thread: while the local pointer is grabbed and
    /// parked, position beacons are meaningless and must not be sent.
    pub(crate) grabbed: Arc<AtomicBool>,
    /// The latest real pointer position, polled by the beacon thread on
    /// its own X connection — so the event hot path never waits on a
    /// round-trip reply. Fed to the probe and to the coalesced beacon
    /// when the local pointer is free.
    pub(crate) real_pos: Arc<Mutex<Option<(i32, i32)>>>,
    /// Kernel-level device isolation while the cursor is on a client.
    /// Always present; it engages the moment `/dev/input` becomes
    /// readable (grab-only until then).
    pub(crate) evdev: EvdevReader,
    /// Heartbeat for the server supervisor (`Liveness::capture_tick_ms`):
    /// bumped every capture-loop iteration. This thread owns the local
    /// input grab while remote, so its wedge is exactly what traps the
    /// machine's input — the supervisor must see it.
    pub(crate) capture_tick: Arc<AtomicU64>,
    /// The chord set currently installed as passive grabs (sorted,
    /// deduplicated, packed mods) — the diff baseline so a repeated
    /// publish (an unrelated config reload) changes nothing.
    pub(crate) grabbed_chords: Vec<(u8, u32)>,
}

/// Open the X display (`None` = `$DISPLAY`), select XI2 raw events on the
/// root window, and start the capture thread.
///
/// Returns the channel the server's main loop reads local input from,
/// the capture thread's heartbeat for the server supervisor, and the
/// **write end of the command wake pipe** — the engine keeps it and
/// writes a byte on every command so the capture loop's poll returns
/// immediately instead of sleeping through a command.
pub fn start(
    display: Option<&str>,
    cmd_rx: Receiver<CaptureCommand>,
) -> Result<(Receiver<Message>, Arc<AtomicU64>, UnixStream), String> {
    let (conn, screen_num) =
        RustConnection::connect(display).map_err(|e| format!("X11 connect: {e}"))?;
    let root = conn.setup().roots[screen_num].root;

    // XI2 handshake (raw events need server XI >= 2.0).
    let version = xinput::xi_query_version(&conn, 2, 0)
        .map_err(|e| format!("XI2 query: {e}"))?
        .reply()
        .map_err(|e| format!("XI2 reply: {e}"))?;
    if version.major_version < 2 {
        return Err(format!(
            "XI2 required, server has XI {}.{}",
            version.major_version, version.minor_version
        ));
    }

    select_input_events(&conn, root)?;
    // XFixes is needed for cursor hide/show (also used by the engine).
    if conn
        .extension_information(xfixes::X11_EXTENSION_NAME)
        .map_err(|e| format!("XFixes query: {e}"))?
        .is_none()
    {
        return Err("XFixes extension not available".into());
    }
    conn.xfixes_query_version(5, 0)
        .map_err(|e| format!("XFixes version: {e}"))?;
    log_info!("input capture started (XI2 raw events)");
    // The visible desktop, with the whole root as the fallback. Computed
    // once here on the capture connection and shared with the beacon
    // thread, so both translate positions identically.
    let visible = visible_desktop(&conn, screen_num).unwrap_or_else(|| {
        let s = &conn.setup().roots[screen_num];
        VisibleDesktop::whole_root(s.width_in_pixels as u32, s.height_in_pixels as u32)
    });

    // The command wake pipe: nonblocking both ways, so an engine write
    // never stalls on a full pipe (the byte is a nudge — the command
    // itself travels over the channel).
    let (wake_rx, wake_tx) = UnixStream::pair().map_err(|e| format!("wake pipe: {e}"))?;
    wake_rx
        .set_nonblocking(true)
        .map_err(|e| format!("wake pipe nonblocking: {e}"))?;
    wake_tx
        .set_nonblocking(true)
        .map_err(|e| format!("wake pipe nonblocking: {e}"))?;

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
        visible,
        tx: tx.clone(),
        cmd_rx,
        wake_rx,
        motion: PendingMotion::default(),
        beacon: None,
        last_beacon: None,
        probe: kvmshare_core::motion::MotionProbe::default(),
        held: HashMap::new(),
        grabbed: grabbed.clone(),
        real_pos: real_pos.clone(),
        evdev,
        capture_tick: capture_tick.clone(),
        grabbed_chords: Vec::new(),
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
    spawn_beacon_thread(display.map(str::to_owned), visible, grabbed, real_pos, tx);
    Ok((rx, capture_tick, wake_tx))
}

impl InputCapture {
    /// The capture loop: drain X events, apply engine commands, forward
    /// coalesced motion and synthesized key repeats at their cadences,
    /// then block in `poll(2)` until there is a reason to wake. Runs
    /// forever; returns only on a fatal X error.
    fn run_forever(mut self) -> Result<(), String> {
        // The X connection's fd: polled together with the command wake
        // pipe, so a fully idle loop sleeps in the kernel instead of
        // ticking.
        let x_fd = self.conn.stream().as_raw_fd();
        let wake_fd = self.wake_rx.as_raw_fd();
        loop {
            self.capture_tick.store(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
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

            // Block until there is something to do. The wait is computed,
            // not fixed: a pending cadence duty (beacon, held key)
            // bounds it, a remote cursor keeps the heartbeat alive, and
            // a fully idle local loop sleeps indefinitely (an X event or
            // an engine command wakes it).
            let timeout = self.wait_timeout();
            let ms = timeout
                .map(|t| t.as_millis().min(i32::MAX as u128) as i32)
                .unwrap_or(-1);
            let mut pfd = [
                libc::pollfd {
                    fd: x_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: wake_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: pfd is two valid pollfds backed by the X connection
            // fd and the wake pipe fd, both alive for the call.
            let ready = unsafe { libc::poll(pfd.as_mut_ptr(), 2, ms) };
            if ready < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                log_warn!("input capture: poll: {err}");
                thread::sleep(POLL_ERROR_PAUSE);
                continue;
            }
            // A command arrived: drain the nudge bytes; the commands
            // themselves are picked up at the top of the next iteration.
            if pfd[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                let mut buf = [0u8; 64];
                let mut r = &self.wake_rx;
                loop {
                    match r.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            }
        }
    }

    /// How long the next poll may sleep. `None` = block until an X event
    /// or command (the fully idle local state).
    fn wait_timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let mut due: Option<Duration> = None;
        // A beacon is set but rate-limited: it must go out within
        // [`BEACON_PERIOD`] of the last send.
        if self.beacon.is_some() {
            if let Some(last) = self.last_beacon {
                due = Some(BEACON_PERIOD.saturating_sub(now.duration_since(last)));
            }
        }
        // Held keys need the repeat cadence: the earliest of the first
        // repeat delay and each key's next repeat interval.
        if !self.held.is_empty() {
            let mut earliest: Option<Duration> = None;
            for h in self.held.values() {
                let wait = if now.duration_since(h.down_at) < REPEAT_DELAY {
                    REPEAT_DELAY.saturating_sub(now.duration_since(h.down_at))
                } else {
                    REPEAT_INTERVAL.saturating_sub(now.duration_since(h.last_repeat))
                };
                earliest = Some(earliest.map_or(wait, |e: Duration| e.min(wait)));
            }
            due = match (due, earliest) {
                (Some(d), Some(e)) => Some(d.min(e)),
                (d, e) => d.or(e),
            };
        }
        match due {
            // Cadence duty pending: wake at the duty's cadence.
            Some(d) => Some(d),
            // Nothing pending. While the cursor is on a client the
            // heartbeat must keep advancing (the supervisor watches it),
            // so wake on a slow tick; fully idle and local, sleep until
            // an X event or command.
            None if self.grabbed.load(Ordering::Relaxed) => Some(REMOTE_IDLE_TICK),
            None => None,
        }
    }

    pub(crate) fn on_event(&mut self, ev: XEvent) {
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
                // session applies deltas then re-anchors, in order. Root
                // pixels -> visible-local, matching every other position
                // this thread reports.
                self.flush_motion();
                let x = (e.root_x >> 16) as i32;
                let y = (e.root_y >> 16) as i32;
                if !self.grabbed.load(Ordering::Relaxed) {
                    let (x, y) = self.visible.from_root(x, y);
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
                        log_info!(
                            "escape (Scroll Lock) pressed while remote — returning control home"
                        );
                        self.send(Message::Escape);
                        return;
                    }
                    self.held.insert(
                        key,
                        Held {
                            down_at: Instant::now(),
                            last_repeat: Instant::now(),
                        },
                    );
                    self.send(Message::Key {
                        kind: KeyKind::Down,
                        key,
                    });
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
                    self.send(Message::Key {
                        kind: KeyKind::Up,
                        key,
                    });
                }
            }
            // Anything else (including core events redirected to us by
            // our own grabs) is deliberately ignored: raw events plus the
            // motion beacons are the only streams we act on.
            _ => {}
        }
    }

    pub(crate) fn on_button(&mut self, x11_button: u32, pressed: bool) {
        match buttons::from_x11(x11_button) {
            XButton::Button(canon) => self.send(Message::MouseButton {
                button: canon,
                pressed,
            }),
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
    pub(crate) fn flush_motion(&mut self) {
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
    pub(crate) fn sample_probe(&mut self) {
        // Local only: while the cursor is on a client the local pointer
        // is grabbed and pinned, and comparing against it would report a
        // bogus zero travel.
        if self.grabbed.load(Ordering::Relaxed) || !self.probe.due() {
            return;
        }
        let Some(real) = *self.real_pos.lock().unwrap() else {
            return;
        };
        self.probe
            .sample(real, &mut |rx, ry, ax, ay, ex, ey, gx, gy| {
                log_trace!(
                    "motion req=({rx},{ry}) act=({ax},{ay}) exp=({ex},{ey}) real=({gx},{gy})"
                );
            });
    }

    /// XI2 raw key events carry X keycodes. The standard X11 evdev
    /// mapping is `keycode = evdev + 8`, so the canonical HID usage is
    /// looked up from `keycode - 8`. Unknown keys are dropped (with a
    /// debug log at the caller) rather than sent with a wrong identity.
    pub(crate) fn canonical_key(&self, keycode: u32) -> Option<u32> {
        let evdev = keycode.checked_sub(8)? as u16;
        crate::keys::hid_from_evdev(evdev)
    }
}
