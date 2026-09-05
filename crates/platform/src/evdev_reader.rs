//! Linux input capture straight from `/dev/input` (evdev) with **kernel
//! device isolation**.
//!
//! Why a second capture path? Apps that read XI2 *raw* events — browsers,
//! smooth-scroll terminals (kitty), TUIs with mouse support — receive
//! them **regardless of any X grab**: core pointer grabs only suppress
//! core delivery, and XI2 device grabs do not suppress raw delivery
//! either (verified empirically on a live X server). The only way to
//! make the local desktop *literally see nothing* while the cursor is on
//! a client is to stop the devices at the kernel with `EVIOCGRAB`.
//!
//! While the cursor is on a client this module:
//!
//! 1. grabs every physical pointer and keyboard (`EVIOCGRAB`), so X
//!    receives zero events — raw included — and no local app can scroll,
//!    hover, or react to the very input being forwarded;
//! 2. reads the devices directly and forwards the same protocol
//!    [`Message`]s the X capture would (motion coalesced at the shared
//!    cadence, wheel one notch per event, buttons, keys, and the Scroll
//!    Lock escape).
//!
//! On return home the grab is released and the X capture resumes. Kernel
//! grabs are released automatically when the process dies, so a crash
//! can never leave the desktop input-dead (unlike an X-side device
//! disable, which persists until re-enabled).
//!
//! ## Permissions and hot-plug
//!
//! Reading `/dev/input` needs the `input` group (or a udev rule). The
//! reader is **always running** and re-enumerates on a cadence in both
//! modes, so access granted later (installer udev rule, group change,
//! device plugged in) is picked up live — no restart, no user steps.
//! Until devices are readable the server runs grab-only: degraded but
//! functional, and logged once on the state change.
//!
//! ## No event replay across a boundary
//!
//! A key or button physically held when the cursor crosses a boundary
//! was pressed on the *other* capture path — the client must never see
//! its stream replayed here. The reader therefore tracks which presses
//! it forwarded and suppresses kernel auto-repeats and releases for
//! anything it did not press. The client releases whatever it does hold
//! when control leaves it (its own `leave` path).
//!
//! ## Portability
//!
//! The module is Linux-only but **X-free**: it speaks only `/dev/input`
//! and the protocol channel, so the same reader slots into a future
//! Wayland capture unchanged.

use std::collections::HashSet;
use std::io;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use evdev::{Device, EventType, InputEvent, KeyCode, RelativeAxisCode};

use kvmshare_log::{log_debug, log_info, log_warn};
use kvmshare_protocol::id::buttons;
use kvmshare_protocol::message::{KeyKind, Message};

use crate::keys::{hid_from_evdev, ESCAPE_KEY_HID};
use crate::motion::PendingMotion;

/// How often the enumerator thread re-opens `/dev/input`, so a device
/// plugged in (or access granted) mid-session is picked up and grabbed
/// too. Runs on its own thread, so the cadence serves both modes; a
/// slow open (wedged driver) delays new devices, never the cursor
/// stream.
const HOTPLUG_PERIOD: Duration = Duration::from_millis(2000);
/// Idle poll pause while remote (no events flowing). Nonblocking reads
/// are polled in a loop; 1 ms keeps latency negligible.
const REMOTE_POLL_PAUSE: Duration = Duration::from_millis(1);
/// Pause while the cursor is local (nothing to do — the X capture owns
/// input then). Longer is fine: the only duty is watching the flag and
/// the slow re-enumeration cadence.
const LOCAL_PAUSE: Duration = Duration::from_millis(20);

/// One opened input device.
struct Opened {
    path: PathBuf,
    name: String,
    dev: Device,
}

/// Whether a device is one we should read and isolate: a pointer with
/// relative X/Y axes or a keyboard with letter/whitespace keys. Devices
/// without either (power buttons, lid switches, sensors) are left alone.
fn classify(dev: &Device) -> bool {
    let pointer = dev
        .supported_relative_axes()
        .is_some_and(|a| a.contains(RelativeAxisCode::REL_X));
    let keyboard = dev.supported_keys().is_some_and(|k| {
        k.contains(KeyCode::KEY_A) || k.contains(KeyCode::KEY_ENTER) || k.contains(KeyCode::KEY_SPACE)
    });
    pointer || keyboard
}

/// Open every pointer/keyboard in `/dev/input`, with the fd opened
/// **nonblocking at the syscall level** (`O_NONBLOCK` on `open(2)`
/// itself). A wedged device driver that blocks in `open` must fail
/// fast, never hang the caller. The bool reports whether any open
/// failed on permissions (the actionable case — vs. no input devices at
/// all).
///
/// # Safety
///
/// The raw fd is opened with `O_NONBLOCK | O_CLOEXEC` and ownership
/// moves into `OwnedFd` immediately; `Device::from_fd` takes it from
/// there.
fn open_devices(known: &HashSet<PathBuf>) -> (Vec<Opened>, bool) {
    let mut out = Vec::new();
    let mut denied = false;
    if let Ok(dir) = std::fs::read_dir("/dev/input") {
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("event") {
                continue;
            }
            let path = entry.path();
            // Skip devices the reader already watches. Re-opening a
            // watched device would hand us a duplicate handle whose
            // `Drop` (an EVIOCGRAB(0) ungrab ioctl) would run on the
            // caller's thread — eight of those per pass are exactly the
            // periodic stall this reader used to see.
            if known.contains(&path) {
                continue;
            }
            // /dev/input/eventN is plain ASCII; a failure here is
            // unrepresentable, so skip rather than panic.
            let Ok(cpath) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
                continue;
            };
            // SAFETY: open(2) with a NUL-terminated path; the result is
            // checked before it is wrapped.
            let fd = unsafe {
                libc::open(cpath.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC)
            };
            if fd < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::PermissionDenied {
                    denied = true;
                }
                continue;
            }
            // SAFETY: fd is a valid, owned, nonblocking event-device fd.
            let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
            let dev = match Device::from_fd(owned) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if !classify(&dev) {
                continue;
            }
            let name = dev.name().unwrap_or(&name).to_owned();
            out.push(Opened { path, name, dev });
        }
    }
    (out, denied)
}

/// Merge freshly opened devices into the watch set, isolating any new
/// ones immediately when remote (hot-plugged mid-session), and record
/// their paths so the enumerator never re-opens (and later drops) a
/// device the reader already watches.
fn absorb(devices: &mut Vec<Opened>, fresh: Vec<Opened>, remote: &AtomicBool, known: &Mutex<HashSet<PathBuf>>) {
    for d in fresh {
        if devices.iter().any(|w| w.path == d.path) {
            continue;
        }
        let (mut dev, path, name) = (d.dev, d.path, d.name);
        if remote.load(Ordering::Relaxed) {
            if let Err(e) = dev.grab() {
                log_warn!("evdev: {name}: cannot grab: {e}");
            }
        }
        known.lock().unwrap().insert(path.clone());
        log_info!("evdev: watching {name} ({path:?})");
        devices.push(Opened { path, name, dev });
    }
}

/// Which physical presses this reader has forwarded a Down for. A key or
/// button held across a boundary crossing was pressed on the *other*
/// capture path; forwarding its kernel repeats or release here would
/// replay a press the client never saw (or duplicate one it did).
struct PressState {
    pressed: HashSet<u32>,
}

impl PressState {
    fn new() -> Self {
        Self { pressed: HashSet::new() }
    }

    /// Classify one EV_KEY value against what this reader pressed.
    /// Returns the key action to forward, or `None` when the event
    /// belongs to a press that started elsewhere (drop it).
    fn key_action(&mut self, hid: u32, value: i32) -> Option<KeyKind> {
        match value {
            1 => {
                self.pressed.insert(hid);
                Some(KeyKind::Down)
            }
            0 => {
                let known = self.pressed.remove(&hid);
                known.then_some(KeyKind::Up)
            }
            // Kernel auto-repeat (held key): only ours to relay if we
            // saw the press.
            _ => self.pressed.contains(&hid).then_some(KeyKind::Repeat),
        }
    }

    /// A button down/up, same rule as [`Self::key_action`] (buttons share
    /// the EV_KEY event type). Returns whether to forward.
    fn button(&mut self, id: u8, pressed: bool) -> bool {
        if pressed {
            self.pressed.insert(id as u32);
            true
        } else {
            self.pressed.remove(&(id as u32))
        }
    }
}

/// Translate one kernel event into protocol messages.
///
/// `motion` accumulates relative X/Y and applies the shared
/// [`crate::motion::MOTION_PERIOD`] rate limit. Wheel, buttons and keys
/// are sent immediately. Scroll Lock is the escape: while remote it
/// becomes [`Message::Escape`] and is never forwarded.
fn handle_event(ev: &InputEvent, motion: &mut PendingMotion, press: &mut PressState, send: &mut dyn FnMut(Message)) {
    match (ev.event_type(), ev.code()) {
        (EventType::RELATIVE, code) => match RelativeAxisCode(code) {
            RelativeAxisCode::REL_X => motion.push(ev.value() as f64, 0.0),
            RelativeAxisCode::REL_Y => motion.push(0.0, ev.value() as f64),
            // One notch per kernel event — never doubled.
            RelativeAxisCode::REL_WHEEL => send(Message::MouseWheel { dx: 0, dy: ev.value() }),
            RelativeAxisCode::REL_HWHEEL => send(Message::MouseWheel { dx: ev.value(), dy: 0 }),
            _ => {}
        },
        (EventType::KEY, code) => {
            let code = KeyCode(code);
            // Mouse buttons first (they share the EV_KEY type).
            let button = match code {
                KeyCode::BTN_LEFT => Some(buttons::LEFT),
                KeyCode::BTN_RIGHT => Some(buttons::RIGHT),
                KeyCode::BTN_MIDDLE => Some(buttons::MIDDLE),
                KeyCode::BTN_SIDE => Some(buttons::EXTRA_1),
                KeyCode::BTN_EXTRA => Some(buttons::EXTRA_2),
                _ => None,
            };
            if let Some(id) = button {
                if press.button(id, ev.value() != 0) {
                    send(Message::MouseButton { button: id, pressed: ev.value() != 0 });
                }
                return;
            }
            // Keyboard keys travel as canonical HID usages, exactly like
            // the X capture path.
            let Some(hid) = hid_from_evdev(code.0) else { return };
            if hid == ESCAPE_KEY_HID {
                // Swallow press and release: control comes home, the key
                // itself never reaches the client.
                if ev.value() == 1 {
                    log_info!("escape (Scroll Lock) pressed while remote — returning control home");
                    send(Message::Escape);
                }
                return;
            }
            if let Some(kind) = press.key_action(hid, ev.value()) {
                send(Message::Key { kind, key: hid });
            }
        }
        _ => {}
    }
}

/// A handle to the evdev reader thread.
pub struct EvdevReader {
    remote: Arc<AtomicBool>,
    /// Bumped by the reader thread every time it completes a grab/release
    /// transition. A caller that needs the devices in a *known* state
    /// (the crossing-back warp must land on a live stream) waits for the
    /// counter to advance — bounded, so nothing can ever hang.
    ack: Arc<AtomicU64>,
    _thread: thread::JoinHandle<()>,
}

impl EvdevReader {
    /// Start the reader thread. Always succeeds: input isolation engages
    /// the moment devices become readable (see the module docs) and the
    /// server otherwise runs grab-only.
    pub fn start(tx: Sender<Message>) -> Self {
        let remote = Arc::new(AtomicBool::new(false));
        let remote2 = Arc::clone(&remote);
        let ack = Arc::new(AtomicU64::new(0));
        let ack2 = Arc::clone(&ack);
        // Device re-enumeration runs on its own thread: `open(2)` on a
        // wedged or slow device can block for hundreds of milliseconds,
        // and while the cursor is on a client this reader *is* the whole
        // cursor stream — a stall here is a client cursor freeze. The
        // reader only ever applies freshly opened lists (fast grabs), so
        // a slow hot-plug pass delays new devices, never motion.
        let (enum_tx, enum_rx) = mpsc::channel();
        // Devices the reader already watches; the enumerator skips them
        // so a pass never hands back duplicate handles (whose `Drop` —
        // an EVIOCGRAB(0) ungrab ioctl — would stall the reader).
        let known_paths = Arc::new(Mutex::new(HashSet::new()));
        spawn_enumerator(enum_tx, Arc::clone(&known_paths));
        let thread = thread::Builder::new()
            .name("kvmshare-evdev".into())
            .spawn(move || reader_main(tx, remote2, ack2, enum_rx, known_paths))
            .expect("cannot spawn evdev reader");
        Self { remote, ack, _thread: thread }
    }

    /// Switch the reader between forwarding (remote) and silent (local)
    /// mode. Called by the capture thread when control crosses a
    /// boundary. Async: the reader picks the flag up on its next
    /// iteration (≤ ~2 ms) and grabs/releases then.
    pub fn set_remote(&self, remote: bool) {
        self.remote.store(remote, Ordering::Relaxed);
    }

    /// Release the kernel grabs **and wait until the reader has actually
    /// done it** (bounded — never blocks more than [`RELEASE_WAIT`]).
    /// The crossing-back path uses this so the entry warp lands on a
    /// live input stream instead of racing an async release that can
    /// swallow the first moments of physical motion. Safe to call when
    /// already local (the flag is already false; the wait just times out
    /// quietly).
    pub fn release_and_wait(&self) {
        let before = self.ack.load(Ordering::Acquire);
        self.remote.store(false, Ordering::Release);
        let deadline = Instant::now() + RELEASE_WAIT;
        while self.ack.load(Ordering::Acquire) == before && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
    }
}

/// How long [`EvdevReader::release_and_wait`] waits for the reader
/// thread to complete the release. A healthy reader processes a
/// transition within one iteration (~1 ms idle sleep + the ungrab
/// ioctls); this bound just keeps the capture thread from ever waiting
/// on a wedged reader.
const RELEASE_WAIT: Duration = Duration::from_millis(30);

impl Drop for EvdevReader {
    fn drop(&mut self) {
        // Let the thread release the kernel grabs before the devices are
        // dropped — never leave the desktop input-dead.
        self.remote.store(false, Ordering::Relaxed);
    }
}

/// The re-enumeration thread: opens `/dev/input` on a cadence and hands
/// the fresh device list to the reader over a channel. Everything that
/// can block (the device opens themselves) happens here — the reader
/// never waits on it. The thread is detached: if a device open wedges
/// for a long time, the reader simply keeps its current devices until
/// the next pass completes.
fn spawn_enumerator(enum_tx: Sender<(Vec<Opened>, bool)>, known_paths: Arc<Mutex<HashSet<PathBuf>>>) {
    thread::Builder::new()
        .name("kvmshare-evdev-enum".into())
        .spawn(move || loop {
            thread::sleep(HOTPLUG_PERIOD);
            // Diagnostic: a slow pass is now harmless to the cursor
            // stream, but still worth knowing about.
            let t0 = Instant::now();
            let known = known_paths.lock().unwrap().clone();
            let (fresh, denied) = open_devices(&known);
            let took = t0.elapsed();
            // A pass is normally 100-200 ms (the device opens) and runs
            // off the motion thread, so it is not worth a WARN on every
            // cadence — that was log spam. Only a genuinely pathological
            // pass (a wedged device open) rises to WARN.
            if took > Duration::from_millis(400) {
                log_warn!("evdev: re-enumeration took {took:?} (off the motion thread)");
            } else if took > Duration::from_millis(20) {
                log_debug!("evdev: re-enumeration took {took:?}");
            }
            if enum_tx.send((fresh, denied)).is_err() {
                return; // reader gone
            }
        })
        .ok();
}

/// The reader loop. Waits for the remote flag; on each transition grabs
/// (or releases) every device. **Devices are drained in both modes** —
/// the kernel ring buffer would otherwise replay events that happened
/// while the cursor was local (a mute tap, a Win+E, a click) to the
/// client the moment forwarding starts. Drain-and-discard while local,
/// drain-and-forward while remote, so a boundary crossing never carries
/// stale events across it. Freshly opened device lists arrive from the
/// enumerator thread over the channel (see [`spawn_enumerator`]).
fn reader_main(
    tx: Sender<Message>,
    remote: Arc<AtomicBool>,
    ack: Arc<AtomicU64>,
    enum_rx: Receiver<(Vec<Opened>, bool)>,
    known_paths: Arc<Mutex<HashSet<PathBuf>>>,
) {
    let (mut devices, mut denied) = open_devices(&HashSet::new());
    known_paths.lock().unwrap().extend(devices.iter().map(|d| d.path.clone()));
    let mut was_remote = false;
    let mut press = PressState::new();
    let mut motion = PendingMotion::default();
    let mut logged = u8::MAX; // never-logged sentinel
    let mut last_remote_drain: Option<Instant> = None;
    log_presence(&devices, denied, &mut logged);
    loop {
        let iter_start = Instant::now();
        let is_remote = remote.load(Ordering::Relaxed);
        if is_remote != was_remote {
            set_grabbed(&mut devices, is_remote);
            // Acknowledge the transition so a caller waiting on
            // [`EvdevReader::release_and_wait`] can proceed.
            ack.fetch_add(1, Ordering::Release);
            if is_remote {
                // Events that landed in the ring buffers in the moments
                // before the grab belong to the local side (the buffer is
                // normally drained continuously, but the last instant can
                // slip through): purge them so the crossing never
                // replays them on the client.
                purge(&mut devices);
                last_remote_drain = None;
            } else {
                // Local again: the X capture owns input; any press state
                // this reader accumulated belongs to the past.
                press = PressState::new();
            }
            was_remote = is_remote;
        }
        // Apply any fresh device list the enumerator produced (fast:
        // just merges and grabs new devices — all the slow opens already
        // happened on the enumerator thread).
        let enum_start = Instant::now();
        while let Ok((fresh, d)) = enum_rx.try_recv() {
            denied = d;
            absorb(&mut devices, fresh, &remote, &known_paths);
            log_presence(&devices, denied, &mut logged);
        }
        let enum_took = enum_start.elapsed();
        // Drain every device. Forward only while remote; while local the
        // events belong to the X capture and are discarded here — but
        // they must be *read* so they never replay later.
        let drain_start = Instant::now();
        let mut saw_event = false;
        let mut dead: Vec<usize> = Vec::new();
        for (i, d) in devices.iter_mut().enumerate() {
            match d.dev.fetch_events() {
                Ok(events) => {
                    for ev in events {
                        saw_event = true;
                        if is_remote {
                            handle_event(&ev, &mut motion, &mut press, &mut |m| {
                                let _ = tx.send(m);
                            });
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    log_warn!("evdev: {}: {e} — removed", d.name);
                    dead.push(i);
                }
            }
        }
        for i in dead.into_iter().rev() {
            if let Some(removed) = devices.get(i) {
                known_paths.lock().unwrap().remove(&removed.path);
            }
            devices.remove(i);
        }
        if is_remote {
            motion.flush(&mut |dx, dy| {
                let _ = tx.send(Message::MouseMoveRel { dx, dy });
            });
        }
        // Diagnostic: an iteration that took longer than ~15 ms while
        // remote means this thread itself was blocked or starved for the
        // whole stream (everything the client needs flows through here).
        // The phase split shows whether the block was the device drain
        // or the (off-thread-open) enumerator apply.
        let iter = iter_start.elapsed();
        if is_remote && iter > Duration::from_millis(15) {
            let drain_took = drain_start.elapsed();
            log_warn!("evdev: reader iteration took {iter:?} (enum apply {enum_took:?}, drain {drain_took:?})");
        }
        if !saw_event {
            // Idle: pause a tick. While local the pause is longer (the
            // X capture owns input; we only keep the buffers drained).
            thread::sleep(if is_remote { REMOTE_POLL_PAUSE } else { LOCAL_PAUSE });
        } else if is_remote {
            // Diagnostic: while the cursor is on a client this thread is
            // the whole cursor stream. A long gap between event-bearing
            // drains means the stream stalled *here* (this thread blocked
            // on a device open/grab/read) — the client would see exactly
            // that gap as a frozen cursor.
            if let Some(last) = last_remote_drain {
                let gap = last.elapsed();
                if gap > std::time::Duration::from_millis(100) {
                    log_warn!("evdev: remote motion stream gap of {gap:?}");
                }
            }
            last_remote_drain = Some(Instant::now());
        }
    }
}

/// Drain and discard everything currently buffered on every device
/// (nonblocking). Used at the moment forwarding starts so events from
/// just before the crossing are never replayed on the client.
fn purge(devices: &mut [Opened]) {
    for d in devices.iter_mut() {
        loop {
            match d.dev.fetch_events() {
                Ok(events) => {
                    for _ in events {}
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }
}

/// Grab or release every device with `EVIOCGRAB`. While grabbed, X (and
/// every other reader) receives nothing from these devices — the local
/// desktop is inert no matter what the app reads.
fn set_grabbed(devices: &mut [Opened], grab: bool) {
    let mut ok = 0;
    for d in devices.iter_mut() {
        let res = if grab { d.dev.grab() } else { d.dev.ungrab() };
        match res {
            Ok(()) => ok += 1,
            Err(e) => log_warn!("evdev: {}: cannot {}: {e}", d.name, if grab { "grab" } else { "release" }),
        }
    }
    if grab {
        log_debug!("evdev: {ok}/{} device(s) isolated from X", devices.len());
    } else {
        log_debug!("evdev: {ok}/{} device(s) released", devices.len());
    }
}

/// Log whether isolation is live — but only when the state changes, since
/// re-enumeration runs on a cadence forever.
fn log_presence(devices: &[Opened], denied: bool, logged: &mut u8) {
    let state = match (devices.is_empty(), denied) {
        (false, _) => 1,     // live
        (true, true) => 2,   // permission denied
        (true, false) => 3,  // no devices
    };
    if *logged == state {
        return;
    }
    *logged = state;
    match state {
        1 => log_info!("input isolation available (evdev, {} device(s))", devices.len()),
        2 => log_warn!("input isolation unavailable: permission denied reading /dev/input (granting input access fixes it)"),
        _ => log_warn!("input isolation unavailable: no pointer/keyboard devices in /dev/input"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(events: &[(u16, u16, i32)]) -> Vec<Message> {
        let mut motion = PendingMotion::default();
        let mut press = PressState::new();
        let mut out = Vec::new();
        for (ty, code, value) in events {
            handle_event(&InputEvent::new(*ty, *code, *value), &mut motion, &mut press, &mut |m| out.push(m));
        }
        motion.flush(&mut |dx, dy| out.push(Message::MouseMoveRel { dx, dy }));
        out
    }

    fn key(code: u16, value: i32) -> (u16, u16, i32) {
        (1, code, value) // EV_KEY
    }

    fn rel(code: u16, value: i32) -> (u16, u16, i32) {
        (2, code, value) // EV_REL
    }

    #[test]
    fn motion_is_accumulated_and_rate_limited() {
        let msgs = collect(&[rel(RelativeAxisCode::REL_X.0, 5), rel(RelativeAxisCode::REL_Y.0, -3)]);
        assert_eq!(msgs, vec![Message::MouseMoveRel { dx: 5, dy: -3 }]);
    }

    #[test]
    fn wheel_is_one_message_per_notch() {
        let msgs = collect(&[
            rel(RelativeAxisCode::REL_WHEEL.0, 1),
            rel(RelativeAxisCode::REL_WHEEL.0, -1),
            rel(RelativeAxisCode::REL_HWHEEL.0, 1),
        ]);
        assert_eq!(
            msgs,
            vec![
                Message::MouseWheel { dx: 0, dy: 1 },
                Message::MouseWheel { dx: 0, dy: -1 },
                Message::MouseWheel { dx: 1, dy: 0 },
            ]
        );
    }

    #[test]
    fn buttons_map_to_canonical_ids() {
        let msgs = collect(&[
            key(KeyCode::BTN_LEFT.0, 1),
            key(KeyCode::BTN_LEFT.0, 0),
            key(KeyCode::BTN_RIGHT.0, 1),
            key(KeyCode::BTN_SIDE.0, 1),
            key(KeyCode::BTN_EXTRA.0, 1),
        ]);
        assert_eq!(
            msgs,
            vec![
                Message::MouseButton { button: buttons::LEFT, pressed: true },
                Message::MouseButton { button: buttons::LEFT, pressed: false },
                Message::MouseButton { button: buttons::RIGHT, pressed: true },
                Message::MouseButton { button: buttons::EXTRA_1, pressed: true },
                Message::MouseButton { button: buttons::EXTRA_2, pressed: true },
            ]
        );
    }

    #[test]
    fn keys_travel_as_hid_usages_with_native_repeat() {
        let msgs = collect(&[key(KeyCode::KEY_A.0, 1), key(KeyCode::KEY_A.0, 2), key(KeyCode::KEY_A.0, 0)]);
        assert_eq!(
            msgs,
            vec![
                Message::Key { kind: KeyKind::Down, key: 0x04 },
                Message::Key { kind: KeyKind::Repeat, key: 0x04 },
                Message::Key { kind: KeyKind::Up, key: 0x04 },
            ]
        );
    }

    #[test]
    fn media_keys_travel_as_hid_usages() {
        // Play/Pause and volume are declared by the Ergodox consumer
        // node and must reach the client as canonical usages.
        let msgs = collect(&[
            key(KeyCode::KEY_PLAYPAUSE.0, 1),
            key(KeyCode::KEY_PLAYPAUSE.0, 0),
            key(KeyCode::KEY_NEXTSONG.0, 1),
            key(KeyCode::KEY_VOLUMEUP.0, 1),
        ]);
        assert_eq!(
            msgs,
            vec![
                Message::Key { kind: KeyKind::Down, key: 0xcd },
                Message::Key { kind: KeyKind::Up, key: 0xcd },
                Message::Key { kind: KeyKind::Down, key: 0xb5 },
                Message::Key { kind: KeyKind::Down, key: 0xe9 },
            ]
        );
    }

    #[test]
    fn repeats_and_releases_of_foreign_presses_are_suppressed() {
        // A key physically held when the cursor crossed onto the client
        // was pressed on the *other* capture path: its kernel repeats and
        // release must not replay a press the client never saw.
        let msgs = collect(&[
            key(KeyCode::KEY_A.0, 2), // repeat, no down seen
            key(KeyCode::KEY_A.0, 0), // release, no down seen
            key(KeyCode::KEY_A.0, 1), // a real press: from here on it is ours
            key(KeyCode::KEY_A.0, 2),
            key(KeyCode::KEY_A.0, 0),
        ]);
        assert_eq!(
            msgs,
            vec![
                Message::Key { kind: KeyKind::Down, key: 0x04 },
                Message::Key { kind: KeyKind::Repeat, key: 0x04 },
                Message::Key { kind: KeyKind::Up, key: 0x04 },
            ]
        );
    }

    #[test]
    fn foreign_button_release_is_suppressed() {
        // Drag started on the local screen, crossing to the client while
        // held: the client never saw the press, so the release must not
        // reach it either.
        let msgs = collect(&[
            key(KeyCode::BTN_LEFT.0, 0), // release without a press
            key(KeyCode::BTN_LEFT.0, 1), // a real click: forwards both
            key(KeyCode::BTN_LEFT.0, 0),
        ]);
        assert_eq!(
            msgs,
            vec![
                Message::MouseButton { button: buttons::LEFT, pressed: true },
                Message::MouseButton { button: buttons::LEFT, pressed: false },
            ]
        );
    }

    #[test]
    fn scroll_lock_is_the_escape_not_a_key() {
        let msgs = collect(&[key(KeyCode::KEY_SCROLLLOCK.0, 1), key(KeyCode::KEY_SCROLLLOCK.0, 0)]);
        assert_eq!(msgs, vec![Message::Escape]);
    }

    #[test]
    fn unknown_codes_are_ignored() {
        let msgs = collect(&[key(200, 1), rel(0x7f, 1), (0, 0, 0)]);
        assert_eq!(msgs, Vec::<Message>::new());
    }

    #[test]
    fn classify_constants_are_stable() {
        // The classification and translation rely on these raw codes.
        assert_eq!(RelativeAxisCode::REL_X.0, 0x00);
        assert_eq!(KeyCode::KEY_A.0, 30);
        assert_eq!(KeyCode::KEY_SCROLLLOCK.0, 70);
        assert_eq!(KeyCode::KEY_PLAYPAUSE.0, 164);
        assert_eq!(ESCAPE_KEY_HID, 0x47);
    }
}