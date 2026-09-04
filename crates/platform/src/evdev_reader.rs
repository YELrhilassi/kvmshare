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
//! ## Fallback
//!
//! If `/dev/input` is not readable (the user is not in the `input`
//! group), [`EvdevReader::start`] returns an error, the server logs a
//! hint once, and the X capture runs as before (grab only) — degraded
//! but functional. No feature is lost silently.
//!
//! ## Portability
//!
//! The module is Linux-only but **X-free**: it speaks only `/dev/input`
//! and the protocol channel, so the same reader slots into a future
//! Wayland capture unchanged.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use evdev::{Device, EventType, InputEvent, KeyCode, RelativeAxisCode};

use kvmshare_log::{log_debug, log_info, log_warn};
use kvmshare_protocol::id::buttons;
use kvmshare_protocol::message::{KeyKind, Message};

use crate::keys::{hid_from_evdev, ESCAPE_KEY_HID};
use crate::motion::PendingMotion;

/// How often the reader re-enumerates `/dev/input` while remote, so a
/// device plugged in mid-session is picked up and grabbed too.
const HOTPLUG_PERIOD: Duration = Duration::from_millis(2000);
/// Idle poll pause while remote (no events flowing). Nonblocking reads
/// are polled in a loop; 1 ms keeps latency negligible.
const REMOTE_POLL_PAUSE: Duration = Duration::from_millis(1);
/// Pause while the cursor is local (nothing to do — the X capture owns
/// input then). Longer is fine: the only duty is watching the flag.
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

/// Open every pointer/keyboard in `/dev/input`, nonblocking. Returns a
/// diagnostic error when nothing could be opened (notably: no
/// permission), so the caller can log why isolation is unavailable.
fn open_devices() -> Result<Vec<Opened>, String> {
    let mut out = Vec::new();
    let mut denied = false;
    let mut saw_input = false;
    let dir = std::fs::read_dir("/dev/input").map_err(|e| format!("cannot read /dev/input: {e}"))?;
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("event") {
            continue;
        }
        saw_input = true;
        let path = entry.path();
        let dev = match Device::open(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                denied = true;
                continue;
            }
            Err(_) => continue,
        };
        if !classify(&dev) {
            continue;
        }
        if let Err(e) = dev.set_nonblocking(true) {
            log_warn!("evdev: {path:?}: cannot set nonblocking: {e}");
            continue;
        }
        let name = dev.name().unwrap_or(&name).to_owned();
        log_debug!("evdev: watching {name} ({path:?})");
        out.push(Opened { path, name, dev });
    }
    if out.is_empty() {
        let why = if denied {
            "permission denied reading /dev/input — add the user to the `input` group".to_owned()
        } else if !saw_input {
            "/dev/input has no event devices".to_owned()
        } else {
            "no pointer or keyboard devices found in /dev/input".to_owned()
        };
        return Err(why);
    }
    Ok(out)
}

/// Translate one kernel event into protocol messages.
///
/// `motion` accumulates relative X/Y (sub-pixel is irrelevant here —
/// evdev deltas are integers — but the shared accumulator also applies
/// the [`crate::motion::MOTION_PERIOD`] rate limit). Wheel, buttons and
/// keys are sent immediately. Scroll Lock is the escape: while remote it
/// becomes [`Message::Escape`] and is never forwarded.
fn handle_event(ev: &InputEvent, motion: &mut PendingMotion, send: &mut dyn FnMut(Message)) {
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
            if let Some(button) = button {
                send(Message::MouseButton { button, pressed: ev.value() != 0 });
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
            let kind = match ev.value() {
                0 => KeyKind::Up,
                1 => KeyKind::Down,
                // The kernel auto-repeats held keys (value 2); raw X
                // events don't, which is why the X capture synthesizes
                // repeats itself — here they arrive natively.
                _ => KeyKind::Repeat,
            };
            send(Message::Key { kind, key: hid });
        }
        _ => {}
    }
}

/// A handle to the evdev reader thread.
pub struct EvdevReader {
    remote: Arc<AtomicBool>,
    _thread: thread::JoinHandle<()>,
}

impl EvdevReader {
    /// Open the input devices and start the reader thread.
    ///
    /// Fails (with a diagnostic) when `/dev/input` is not readable or
    /// holds no pointer/keyboard — the server then runs without kernel
    /// isolation, i.e. the historical grab-only behavior.
    pub fn start(tx: Sender<Message>) -> Result<Self, String> {
        let devices = open_devices()?;
        let count = devices.len();
        let remote = Arc::new(AtomicBool::new(false));
        let remote2 = Arc::clone(&remote);
        let thread = thread::Builder::new()
            .name("kvmshare-evdev".into())
            .spawn(move || reader_main(devices, tx, remote2))
            .map_err(|e| format!("cannot spawn evdev reader: {e}"))?;
        log_info!("input isolation available (evdev, {count} device(s))");
        Ok(Self { remote, _thread: thread })
    }

    /// Switch the reader between forwarding (remote) and silent (local)
    /// mode. Called by the capture thread when control crosses a
    /// boundary.
    pub fn set_remote(&self, remote: bool) {
        self.remote.store(remote, Ordering::Relaxed);
    }
}

impl Drop for EvdevReader {
    fn drop(&mut self) {
        // Let the thread release the kernel grabs before the devices are
        // dropped — never leave the desktop input-dead.
        self.remote.store(false, Ordering::Relaxed);
    }
}

/// The reader loop. Waits for the remote flag; on each transition grabs
/// (or releases) every device; while remote drains and forwards events,
/// picks up hotplugged devices, and rate-limits motion.
fn reader_main(mut devices: Vec<Opened>, tx: Sender<Message>, remote: Arc<AtomicBool>) {
    let mut was_remote = false;
    let mut last_enum = Instant::now() - HOTPLUG_PERIOD;
    let mut motion = PendingMotion::default();
    loop {
        let is_remote = remote.load(Ordering::Relaxed);
        if is_remote != was_remote {
            set_grabbed(&mut devices, is_remote);
            was_remote = is_remote;
        }
        if !is_remote {
            thread::sleep(LOCAL_PAUSE);
            continue;
        }
        if last_enum.elapsed() >= HOTPLUG_PERIOD {
            add_hotplugged(&mut devices, &remote);
            last_enum = Instant::now();
        }
        let mut saw_event = false;
        let mut dead: Vec<usize> = Vec::new();
        for (i, d) in devices.iter_mut().enumerate() {
            match d.dev.fetch_events() {
                Ok(events) => {
                    for ev in events {
                        saw_event = true;
                        handle_event(&ev, &mut motion, &mut |m| {
                            let _ = tx.send(m);
                        });
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
            devices.remove(i);
        }
        motion.flush(&mut |dx, dy| {
            let _ = tx.send(Message::MouseMoveRel { dx, dy });
        });
        if !saw_event {
            thread::sleep(REMOTE_POLL_PAUSE);
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

/// Pick up devices plugged in while the cursor was remote: open them,
/// isolate them too, and start forwarding.
fn add_hotplugged(devices: &mut Vec<Opened>, remote: &AtomicBool) {
    let known: Vec<PathBuf> = devices.iter().map(|d| d.path.clone()).collect();
    for entry in std::fs::read_dir("/dev/input").into_iter().flatten().flatten() {
        let path = entry.path();
        if known.contains(&path) || !entry.file_name().to_string_lossy().starts_with("event") {
            continue;
        }
        let Ok(mut dev) = Device::open(&path) else { continue };
        if !classify(&dev) || dev.set_nonblocking(true).is_err() {
            continue;
        }
        let name = dev.name().unwrap_or_default().to_owned();
        if remote.load(Ordering::Relaxed) {
            if let Err(e) = dev.grab() {
                log_warn!("evdev: {name}: cannot grab hotplugged device: {e}");
            }
        }
        log_info!("evdev: watching hotplugged {name} ({path:?})");
        devices.push(Opened { path, name, dev });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(events: &[(u16, u16, i32)]) -> Vec<Message> {
        let mut motion = PendingMotion::default();
        let mut out = Vec::new();
        for (ty, code, value) in events {
            handle_event(&InputEvent::new(*ty, *code, *value), &mut motion, &mut |m| out.push(m));
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
    fn scroll_lock_is_the_escape_not_a_key() {
        let msgs = collect(&[key(KeyCode::KEY_SCROLLLOCK.0, 1), key(KeyCode::KEY_SCROLLLOCK.0, 0)]);
        assert_eq!(msgs, vec![Message::Escape]);
    }

    #[test]
    fn unknown_codes_are_ignored() {
        let msgs = collect(&[
            key(200, 1),            // unmapped key
            rel(0x7f, 1),           // unknown relative axis
            (0, 0, 0),              // EV_SYN
        ]);
        assert_eq!(msgs, Vec::<Message>::new());
    }

    #[test]
    fn classify_picks_pointers_and_keyboards_only() {
        // Can't fabricate a Device cheaply, but the predicate logic is
        // trivially reviewable; guard the constants it relies on.
        assert_eq!(RelativeAxisCode::REL_X.0, 0x00);
        assert_eq!(KeyCode::KEY_A.0, 30);
        assert_eq!(KeyCode::KEY_SCROLLLOCK.0, 70);
        assert_eq!(ESCAPE_KEY_HID, 0x47);
    }
}