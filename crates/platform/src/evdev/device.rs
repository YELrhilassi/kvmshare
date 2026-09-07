//! Device discovery (open/classify/absorb) and kernel event →
//! protocol-message translation.

use std::collections::HashSet;
use std::io;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use evdev::{Device, EventType, InputEvent, KeyCode, RelativeAxisCode};

use kvmshare_log::{log_info, log_warn};
use kvmshare_protocol::id::buttons;
use kvmshare_protocol::message::{KeyKind, Message};

use kvmshare_core::motion::PendingMotion;

use crate::keys::{hid_from_evdev, ESCAPE_KEY_HID};

/// One opened input device.
pub(crate) struct Opened {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) dev: Device,
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
pub(crate) fn open_devices(known: &HashSet<PathBuf>) -> (Vec<Opened>, bool) {
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
pub(crate) fn absorb(
    devices: &mut Vec<Opened>,
    fresh: Vec<Opened>,
    remote: &AtomicBool,
    known: &Mutex<HashSet<PathBuf>>,
) {
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
pub(crate) struct PressState {
    pressed: HashSet<u32>,
}

impl PressState {
    pub(crate) fn new() -> Self {
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
/// `kvmshare_core::motion::MOTION_PERIOD` rate limit. Wheel, buttons and keys
/// are sent immediately. Scroll Lock is the escape: while remote it
/// becomes [`Message::Escape`] and is never forwarded.
pub(crate) fn handle_event(
    ev: &InputEvent,
    motion: &mut PendingMotion,
    press: &mut PressState,
    send: &mut dyn FnMut(Message),
) {
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

#[cfg(test)]
#[path = "evdev_tests.rs"]
mod tests;