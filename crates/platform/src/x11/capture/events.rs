//! XI2 event selection and raw-event decoding, plus the command
//! vocabulary between the engine and the capture thread.

use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xinput::{self, EventMask, Fp3232, XIEventMask};
use x11rb::protocol::xproto;
use x11rb::rust_connection::RustConnection;

use crate::keys::ESCAPE_KEY_HID;

/// Device 0 = all devices for `xi_select_events`.
const DEVICE_ALL: u16 = 0;

/// When the escape key is consumed we must also swallow its release, or
/// a bare key-up would reach the client (or home) with no matching down.
pub(crate) fn is_escape(key: u32) -> bool {
    key == ESCAPE_KEY_HID
}

/// How long a key must be held before the first synthesized repeat.
pub(crate) const REPEAT_DELAY: Duration = Duration::from_millis(500);
/// Interval between synthesized repeats while a key stays held.
pub(crate) const REPEAT_INTERVAL: Duration = Duration::from_millis(33); // ≈ 30/s

/// A control action the engine wants performed on the local cursor.
///
/// These arrive over a channel and are executed on the *capture*
/// connection. That single-connection rule is what lets the same client
/// grab the pointer and still warp it (warps from a different client are
/// not honored while another client holds the grab).
#[derive(Debug, Clone, Copy)]
pub enum CaptureCommand {
    /// Grab (or release) the pointer and keyboard so local windows do not
    /// act on physical input while the cursor is on a client.
    Grab(bool),
    /// Isolate (or release) the physical input devices from the local
    /// desktop entirely — the evdev reader takes over. Stronger than
    /// [`CaptureCommand::Grab`]: it also stops *raw* event delivery to
    /// apps that read XI2 raw events (browsers, smooth-scroll
    /// terminals), which no X grab can suppress. No-op when evdev is
    /// unavailable (the grab-only fallback).
    IsolateRemote(bool),
    /// Warp the local cursor to (x, y) — parking it, re-centering it, or
    /// placing it at the entry point when control returns home.
    Warp(i32, i32),
    /// Hide/show the local cursor (XFixes).
    CursorVisible(bool),
}

/// Select XI2 raw events on `root`.
///
/// **Mask encoding gotcha (verified against the X server source):**
/// `mask_len` counts 4-byte words, but the mask bits are byte-packed
/// (event `j` lives at byte `j/8`, bit `j%8`). x11rb serializes each
/// `XIEventMask` as one native `u32` word, which produces exactly the
/// right bytes — as long as every wanted bit is OR'd into a **single**
/// word. Passing one `XIEventMask` per event instead puts each bit in
/// its own word and silently selects nothing.
pub(crate) fn select_input_events(conn: &RustConnection, root: xproto::Window) -> Result<(), String> {
    let mut word = 0u32;
    for bit in [
        // Raw events: device deltas / buttons / keys (warp-free).
        XIEventMask::RAW_MOTION,
        XIEventMask::RAW_BUTTON_PRESS,
        XIEventMask::RAW_BUTTON_RELEASE,
        XIEventMask::RAW_KEY_PRESS,
        XIEventMask::RAW_KEY_RELEASE,
        // Ordinary motion: the real, post-acceleration pointer position
        // (used as a resync beacon for the session).
        XIEventMask::MOTION,
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
pub(crate) fn fp_to_f64(v: &Fp3232) -> f64 {
    v.integral as f64 + v.frac as f64 / 4294967296.0
}

/// Extract the Rel X / Rel Y motion from a raw event.
///
/// XI2 raw events carry `axisvalues_raw` for **only** the axes whose
/// bits are set in `valuator_mask` — so positional indexing is wrong for
/// any device with more than two valuators (a mouse that also reports
/// scroll axes as valuators, a touchpad, a knob, …). A wheel notch on
/// such a device can arrive as a raw *motion* whose first value is the
/// scroll delta (e.g. ±120), which, taken as dx, shoves the cursor
/// sideways and eventually past the screen edge — the cause of
/// "scrolling moves the cursor" and phantom boundary jumps.
///
/// This walks the mask, pairs each set bit with its value in order, and
/// keeps only axes 0 (Rel X) and 1 (Rel Y). Everything else — scroll
/// deltas included — is never cursor motion. Wheel input on devices that
/// deliver it as valuators instead of buttons is a separate concern;
/// devices we have seen report wheel through buttons 4-7, which the
/// button path already turns into a `MouseWheel` message.
pub(crate) fn raw_xy(mask: &[u32], values: &[Fp3232]) -> (f64, f64) {
    let mut dx = 0.0;
    let mut dy = 0.0;
    let mut idx = 0usize;
    for (word_i, word) in mask.iter().enumerate() {
        for bit in 0..32 {
            if word & (1 << bit) != 0 {
                if let Some(v) = values.get(idx) {
                    let v = fp_to_f64(v);
                    match word_i * 32 + bit {
                        0 => dx = v,
                        1 => dy = v,
                        _ => {}
                    }
                }
                idx += 1;
            }
        }
    }
    (dx, dy)
}

/// Repeat state for one physically held key.
pub(crate) struct Held {
    pub(crate) down_at: Instant,
    pub(crate) last_repeat: Instant,
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;