//! Local input capture via XI2 *raw* events.
//!
//! Why raw events? They carry the device's own motion deltas (sub-pixel
//! fixed-point values), and they are generated only by the physical
//! device — never by programmatic cursor warps. That gives the session
//! two things at once:
//!
//! * **Smoothness**: deltas straight from the driver, accumulated to
//!   whole pixels without losing sub-pixel motion.
//! * **No warp feedback**: when the server parks or re-centers its hidden
//!   cursor, no phantom motion is produced, so the virtual cursor never
//!   oscillates (the classic KVM bug — see `core::session`).
//!
//! Raw button and key events are delivered to any client selecting them
//! on the root window, so no pointer grab or input window is needed —
//! the local desktop keeps working normally.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use x11rb::connection::Connection;
use x11rb::protocol::xinput::{self, EventMask, Fp3232, XIEventMask};
use x11rb::protocol::Event as XEvent;
use x11rb::rust_connection::RustConnection;

use kvmshare_protocol::message::{KeyKind, Message};

use super::buttons::{self, XButton};

/// Device id for "all devices" (XIAllDevices).
const DEVICE_ALL: u16 = 0;

/// Select XI2 raw events on `root`.
///
/// **Mask encoding gotcha (verified against the X server source):**
/// `mask_len` counts 4-byte words, but the mask bits are byte-packed
/// (event `j` lives at byte `j/8`, bit `j%8`). x11rb serializes each
/// `XIEventMask` as one native `u32` word, which produces exactly the
/// right bytes — as long as every wanted bit is OR'd into a **single**
/// word. Passing one `XIEventMask` per event instead puts each bit in
/// its own word and silently selects nothing.
fn select_raw_events(conn: &RustConnection, root: x11rb::protocol::xproto::Window) -> Result<(), String> {
    let mut word = 0u32;
    for bit in [
        XIEventMask::RAW_MOTION,
        XIEventMask::RAW_BUTTON_PRESS,
        XIEventMask::RAW_BUTTON_RELEASE,
        XIEventMask::RAW_KEY_PRESS,
        XIEventMask::RAW_KEY_RELEASE,
    ] {
        word |= u32::from(bit);
    }

    xinput::xi_select_events(&conn, root, &[EventMask { deviceid: DEVICE_ALL, mask: vec![XIEventMask::from(word)] }])
        .map_err(|e| format!("xi_select_events: {e}"))?
        .check()
        .map_err(|e| format!("xi_select_events reply: {e}"))?;
    conn.flush().map_err(|e| format!("X11 flush: {e}"))?;
    Ok(())
}

/// 32.32 fixed point → f64.
fn fp_to_f64(v: &Fp3232) -> f64 {
    v.integral as f64 + v.frac as f64 / 4294967296.0
}

/// Whole-pixel motion accumulator. XI2 raw deltas are fractional; we keep
/// the remainder so slow moves still accumulate into full pixels instead
/// of being truncated away.
#[derive(Debug, Default)]
struct Accum {
    fx: f64,
    fy: f64,
}

impl Accum {
    fn push(&mut self, dx: f64, dy: f64) -> (i32, i32) {
        self.fx += dx;
        self.fy += dy;
        let ix = self.fx.floor() as i32;
        let iy = self.fy.floor() as i32;
        self.fx -= ix as f64;
        self.fy -= iy as f64;
        (ix, iy)
    }
}

/// Captures local input on a background thread. Owns its X connection
/// exclusively — the engine uses a separate one.
struct InputCapture {
    conn: RustConnection,
    tx: Sender<Message>,
    acc: Accum,
}

/// Open the X display (`None` = `$DISPLAY`), select XI2 raw events on the
/// root window, and start the capture thread.
///
/// Returns the channel the server's main loop reads local input from.
pub fn start(display: Option<&str>) -> Result<Receiver<Message>, String> {
    let (conn, screen_num) = RustConnection::connect(display).map_err(|e| format!("X11 connect: {e}"))?;
    let root = conn.setup().roots[screen_num].root;

    // XI2 handshake (raw events need server XI >= 2.0).
    let version = xinput::xi_query_version(&conn, 2, 0).map_err(|e| format!("XI2 query: {e}"))?.reply().map_err(|e| format!("XI2 reply: {e}"))?;
    if version.major_version < 2 {
        return Err(format!("XI2 required, server has XI {}.{}", version.major_version, version.minor_version));
    }

    select_raw_events(&conn, root)?;

    let (tx, rx) = mpsc::channel();
    let capture = InputCapture { conn, tx, acc: Accum::default() };
    thread::spawn(move || {
        if let Err(e) = capture.run_forever() {
            eprintln!("input capture stopped: {e}");
        }
    });
    Ok(rx)
}

impl InputCapture {
    /// The capture loop: block on the X event queue and forward what
    /// matters. Runs forever; returns only on a fatal X error.
    fn run_forever(mut self) -> Result<(), String> {
        loop {
            let ev = self.conn.wait_for_event().map_err(|e| format!("X event: {e}"))?;
            match ev {
                XEvent::XinputRawMotion(e) => {
                    let dx = e.axisvalues_raw.first().map(fp_to_f64).unwrap_or(0.0);
                    let dy = e.axisvalues_raw.get(1).map(fp_to_f64).unwrap_or(0.0);
                    let (ix, iy) = self.acc.push(dx, dy);
                    if ix != 0 || iy != 0 {
                        self.send(Message::MouseMoveRel { dx: ix, dy: iy });
                    }
                }
                XEvent::XinputRawButtonPress(e) => self.on_button(e.detail, true),
                XEvent::XinputRawButtonRelease(e) => self.on_button(e.detail, false),
                XEvent::XinputRawKeyPress(e) => {
                    if let Some(key) = self.canonical_key(e.detail) {
                        self.send(Message::Key { kind: KeyKind::Down, key });
                    }
                }
                XEvent::XinputRawKeyRelease(e) => {
                    if let Some(key) = self.canonical_key(e.detail) {
                        self.send(Message::Key { kind: KeyKind::Up, key });
                    }
                }
                _ => {}
            }
        }
    }

    fn on_button(&mut self, x11_button: u32, pressed: bool) {
        match buttons::from_x11(x11_button) {
            XButton::Button(canon) => self.send(Message::MouseButton { button: canon, pressed }),
            XButton::Wheel(dx, dy) => self.send(Message::MouseWheel { dx, dy }),
            XButton::Ignore => {}
        }
    }

    /// XI2 raw key events carry X keycodes. The standard X11 evdev
    /// mapping is `keycode = evdev + 8`, so the canonical HID usage is
    /// looked up from `keycode - 8`. Unknown keys are dropped (with a
    /// debug log at the caller) rather than sent with a wrong identity.
    fn canonical_key(&self, keycode: u32) -> Option<u32> {
        let evdev = keycode.checked_sub(8)? as u16;
        crate::keys::hid_from_evdev(evdev)
    }

    fn send(&self, msg: Message) {
        // The channel is unbounded; the server main loop drains it at its
        // own pace. If the receiver is gone (shutdown), drop the message.
        let _ = self.tx.send(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulator_keeps_fractional_motion() {
        let mut acc = Accum::default();
        // Three slow leftward moves of -0.3px each: must sum to -1px,
        // not truncate to 0.
        assert_eq!(acc.push(-0.3, 0.0), (-1, 0));
        assert_eq!(acc.push(-0.3, 0.0), (0, 0));
        assert_eq!(acc.push(-0.3, 0.0), (0, 0));
        assert!((acc.fx - 0.1).abs() < 1e-9);
        // Fast rightward motion passes through whole.
        assert_eq!(acc.push(12.0, 5.9), (12, 5));
        assert!((acc.fy - 0.9).abs() < 1e-9);
    }

    #[test]
    fn fixed_point_converts_to_f64() {
        let v = Fp3232 { integral: 3, frac: 0x8000_0000 }; // 3.5
        assert_eq!(fp_to_f64(&v), 3.5);
        let v = Fp3232 { integral: -1, frac: 0 }; // -1.0
        assert_eq!(fp_to_f64(&v), -1.0);
    }
}