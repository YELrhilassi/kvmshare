//! Engine-command execution on the capture connection: warp, cursor
//! visibility, the input grab, and kernel-level device isolation.
//!
//! Split from the event loop so the grab/isolation state machine (the
//! part of capture that talks to evdev and changes what the desktop
//! sees) reads as one unit.

use std::sync::atomic::Ordering;
use std::time::Instant;

use x11rb::connection::Connection as _;
use x11rb::protocol::xfixes::{self};
use x11rb::protocol::xproto::{self, ConnectionExt as _};

use kvmshare_log::{log_debug, log_warn};
use kvmshare_protocol::message::{KeyKind, Message};

use super::events::CaptureCommand;
use super::events::{REPEAT_DELAY, REPEAT_INTERVAL};
use super::thread::InputCapture;
use super::thread::BEACON_PERIOD;

/// Map a chord's packed modifier mask (bit0 ctrl, bit1 alt, bit2 shift,
/// bit3 meta — the order documented on the core `Engine` trait) to the
/// X11 modifier mask. The modifier→modifier-mapping assignment is the
/// standard Xorg default (`xmodmap -pm` on any mainstream distro):
/// Mod1 carries Alt, Mod4 carries the Super/Win keys. A server with a
/// user-remapped modifier map is exotic enough that the chord still
/// resolves whenever the event reaches the session (the engine matches
/// HID identities, not X modifiers) — only the OS-level suppression is
/// lost, the same graceful degradation the Engine trait documents.
fn x_mods(packed: u8) -> u16 {
    const MOD1_ALT: u16 = 1 << 3;
    const MOD4_SUPER: u16 = 1 << 6;
    const CONTROL: u16 = 1 << 2;
    const SHIFT: u16 = 1 << 0;
    let mut m = 0u16;
    if packed & 0b0001 != 0 {
        m |= CONTROL;
    }
    if packed & 0b0010 != 0 {
        m |= MOD1_ALT;
    }
    if packed & 0b0100 != 0 {
        m |= SHIFT;
    }
    if packed & 0b1000 != 0 {
        m |= MOD4_SUPER;
    }
    m
}

/// Every combination of the ignorable lock modifiers (Caps Lock bit 2,
/// Num Lock bit 32, Scroll Lock bit 128 in the standard map), ORed into
/// `mods`: a passive grab bound to only the bare combo would stop
/// working the moment the user toggled Caps Lock — the LEDs must not
/// change whether a shortcut fires. 2^3 = 8 variants per chord; grabs
/// are cheap.
fn lock_variants(mods: u16) -> [u16; 8] {
    const CAPS_LOCK: u16 = 1 << 1;
    const NUM_LOCK: u16 = 1 << 4;
    const SCROLL_LOCK: u16 = 1 << 7;
    std::array::from_fn(|bits| {
        let mut m = mods;
        if bits & 1 != 0 {
            m |= CAPS_LOCK;
        }
        if bits & 2 != 0 {
            m |= NUM_LOCK;
        }
        if bits & 4 != 0 {
            m |= SCROLL_LOCK;
        }
        m
    })
}

/// The X keycodes a chord's HID usage can land on. X keycodes are not
/// portable, but they are *derivable* on the one family that matters:
/// Xorg with the default evdev keymap — `keycode = evdev code + 8`, the
/// same identity the raw-event path (`canonical_key`) already relies
/// on. A chord grabbed at the wrong keycode on some exotic server costs
/// nothing: the grab never fires and the chord still resolves through
/// the action engine (documented degradation, never a broken key).
fn chord_keycodes(key: u32) -> Vec<u8> {
    crate::keys::evdev_from_hid(key)
        .map(|evdev| (evdev as u8).wrapping_add(8))
        .into_iter()
        .collect()
}

impl InputCapture {
    /// Execute one engine command on this connection.
    pub(crate) fn apply(&mut self, cmd: CaptureCommand) {
        match cmd {
            CaptureCommand::Warp(x, y) => {
                // The session commands visible-local pixels; the warp is
                // a root-pixel operation (see [`VisibleDesktop`]).
                // src_win = NONE warps from the current position.
                let (rx, ry) = self.visible.to_root(x, y);
                let _ = self.conn.warp_pointer(
                    x11rb::NONE,
                    self.root,
                    0,
                    0,
                    0,
                    0,
                    rx as i16,
                    ry as i16,
                );
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
            CaptureCommand::BindChords(chords) => self.set_bound_chords(chords),
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
    pub(crate) fn set_grabbed(&mut self, grab: bool) {
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
                    false,
                    self.root,
                    mask,
                    xproto::GrabMode::ASYNC,
                    xproto::GrabMode::ASYNC,
                    x11rb::NONE,
                    x11rb::NONE,
                    x11rb::CURRENT_TIME,
                )
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| r.status == xproto::GrabStatus::SUCCESS);
            let keyboard = self
                .conn
                .grab_keyboard(
                    false,
                    self.root,
                    x11rb::CURRENT_TIME,
                    xproto::GrabMode::ASYNC,
                    xproto::GrabMode::ASYNC,
                )
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
        self.grabbed
            .store(if grab { ok } else { false }, Ordering::Relaxed);
        if self.grabbed.load(Ordering::Relaxed) {
            log_debug!("local input grabbed (cursor is on a client)");
        } else if !grab {
            log_debug!("local input released");
        }
    }

    /// Re-arm the **passive chord grabs** (see
    /// [`CaptureCommand::BindChords`]).
    ///
    /// ## Why core-protocol `GrabKey` on the root window
    ///
    /// A chord bound to a kvmshare action (Win+Tab, a media key, …) must
    /// beat whatever the desktop would do with it. The X-native answer
    /// is the same mechanism every window manager uses for its own
    /// shortcuts: a passive keyboard grab on the root window. When the
    /// combo is pressed with no other grab active, the server activates
    /// a keyboard grab owned by us — every client's keyboard delivery
    /// stops, so the desktop never reacts. We deliberately select **no**
    /// key events for those grabs: the grabbing client receives them
    /// only if it selected them, so the chord press is consumed outright
    /// while the session still learns of it through the raw XI2 key
    /// stream, which bypasses grabs entirely (the same property that
    /// keeps forwarding alive while remote). The grab self-terminates
    /// when the triggering key is released (the standard passive-grab
    /// lifetime), so there is no state to track and nothing to release
    /// by hand.
    ///
    /// The modifier map is the standard Xorg one (Mod1 = Alt, Mod4 =
    /// Super/Win); each chord is grabbed under every combination of the
    /// ignorable lock modifiers (Caps/Num/Scroll Lock) so the user's
    /// keyboard LEDs never change whether a shortcut works. The grabs
    /// are fire-and-forget: a variant another client already grabbed
    /// fails asynchronously and cost nothing (that client keeps the
    /// combo — the same conflict rule WMs live with).
    pub(crate) fn set_bound_chords(&mut self, chords: Vec<(u8, u32)>) {
        let mut wanted = chords;
        wanted.sort_unstable();
        wanted.dedup();
        if wanted == self.grabbed_chords {
            return; // a no-op publish (e.g. an unrelated config reload)
        }
        // Release exactly what is installed, then grab the new set.
        for &(mods, key) in &self.grabbed_chords {                for kc in chord_keycodes(key) {
                    for m in lock_variants(x_mods(mods)) {
                        // Fire-and-forget: these are cookie futures whose
                        // only failure modes are the documented ones (a
                        // chord another client owns, an exotic keycode).
                        let _ = self
                            .conn
                            .ungrab_key(xproto::Keycode::from(kc), self.root, xproto::ModMask::from(m));
                    }
                }
        }
        for &(mods, key) in &wanted {                for kc in chord_keycodes(key) {
                    for m in lock_variants(x_mods(mods)) {
                        // owner_events=false: we want nothing delivered.
                        // SYNC freezes other clients for the grab's (brief)
                        // lifetime instead of dropping what they press
                        // mid-hold; the frozen events reach them on release.
                        let _ = self.conn.grab_key(
                            false,
                            self.root,
                            xproto::ModMask::from(m),
                            xproto::Keycode::from(kc),
                            xproto::GrabMode::ASYNC,
                            xproto::GrabMode::SYNC,
                        );
                    }
                }
        }
        let _ = self.conn.flush();
        self.grabbed_chords = wanted;
    }

    /// Forward the coalesced position beacon at [`BEACON_PERIOD`]
    /// cadence, if one is pending. Called from the poll loop, so a beacon
    /// is never delayed longer than one period after the pointer stops
    /// (the loop wakes at the beacon's cadence while one is pending) —
    /// edge parks are confirmed to the session within ~8 ms even under
    /// load.
    pub(crate) fn flush_beacon(&mut self) {
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
    pub(crate) fn tick_repeats(&mut self) {
        if self.held.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut due: Vec<u32> = Vec::new();
        for (key, h) in self.held.iter_mut() {
            if now.duration_since(h.down_at) >= REPEAT_DELAY
                && now.duration_since(h.last_repeat) >= REPEAT_INTERVAL
            {
                h.last_repeat = now;
                due.push(*key);
            }
        }
        for key in due {
            self.send(Message::Key {
                kind: KeyKind::Repeat,
                key,
            });
        }
    }

    pub(crate) fn send(&self, msg: Message) {
        // The channel is unbounded; the server main loop drains it at its
        // own pace. If the receiver is gone (shutdown), drop the message.
        let _ = self.tx.send(msg);
    }
}
