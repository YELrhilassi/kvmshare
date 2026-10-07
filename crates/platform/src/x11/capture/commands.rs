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
use x11rb::protocol::xtest::ConnectionExt as _;

use kvmshare_log::{log_debug, log_warn};
use kvmshare_protocol::message::{KeyKind, MediaCommand, Message};

use super::events::CaptureCommand;
use super::events::{REPEAT_DELAY, REPEAT_INTERVAL};
use super::thread::InputCapture;
use super::thread::BEACON_PERIOD;
use crate::x11::media;

/// How hard the crossing grab fights an existing grabber before giving
/// up (see [`InputCapture::set_grabbed`]).
const GRAB_ATTEMPTS: u32 = 6;

/// The wait between grab attempts.
const GRAB_RETRY_GAP: std::time::Duration = std::time::Duration::from_millis(20);

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
            CaptureCommand::MediaCapture(active, reply) => {
                let result = self.set_media_capture(active);
                // The engine is waiting on this; a receiver that has given
                // up (its timeout expired) is not an error to report.
                let _ = reply.send(result);
            }
            CaptureCommand::Media(command) => self.media(command),
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

    /// Arm (or disarm) the **media-key grab**.
    ///
    /// The mechanism is exactly the one [`Self::set_bound_chords`] uses for
    /// a bound chord, and for the same reason: a passive grab on the root
    /// window is how a client beats the desktop to a key. What differs is
    /// the lifetime — the chord grabs are as many as the user configured
    /// while these cover the whole media key set — and the fact that these
    /// are deliberately kept **asynchronous**: the grabbing client receives
    /// nothing (no key events are selected on the root window), so the key
    /// is consumed outright and the session still learns of it through the
    /// raw XI2 stream, which no grab can suppress.
    ///
    /// `Err` only when suppression cannot be honoured at all — no XTest,
    /// so a grabbed key could be performed nowhere. Arming the grab in that
    /// state would swallow the key and act on neither machine; refusing
    /// instead leaves media keys working locally, which is the documented
    /// degradation and strictly better than a dead key.
    ///
    /// Note what is *not* claimed: X grab requests have no reply, so a
    /// variant another client already owns fails silently (exactly as the
    /// chord grabs do). "Armed" therefore means the grabs were issued, and
    /// the honest way to state it in a log line is what
    /// [`CaptureCommand::MediaCapture`]'s reply feeds.
    pub(crate) fn set_media_capture(&mut self, active: bool) -> Result<(), String> {
        if active == self.media_grabbed {
            // Idempotent: a policy reload that changes nothing about media
            // routing must not blink the grab off and on — that would be a
            // window in which a media key reaches the desktop.
            self.media_active.store(active, Ordering::Release);
            return Ok(());
        }
        if !active {
            self.grab_media_keys(false);
            self.media_active.store(false, Ordering::Release);
            log_debug!("media keys released to the local desktop");
            return Ok(());
        }
        if !self.xtest_available {
            return Err(
                "XTest is unavailable, so a grabbed media key could not be performed locally"
                    .into(),
            );
        }
        self.grab_media_keys(true);
        self.media_active.store(true, Ordering::Release);
        log_debug!("media keys grabbed (routing owns them now)");
        Ok(())
    }

    /// Issue the media-key grabs (`grab`) or their release, and record the
    /// result. One place, so the two directions can never disagree about
    /// which keycodes are involved.
    fn grab_media_keys(&mut self, grab: bool) {
        for kc in media::keycodes() {
            for m in lock_variants(0) {
                let mods = xproto::ModMask::from(m);
                let key = xproto::Keycode::from(kc);
                if grab {
                    // owner_events=false: we want nothing delivered. ASYNC
                    // on both: the key is consumed rather than frozen for
                    // other clients, because there is nothing to hand them
                    // — a routed media key is either performed elsewhere or
                    // replayed by `Self::media`.
                    let _ = self.conn.grab_key(
                        false,
                        self.root,
                        mods,
                        key,
                        xproto::GrabMode::ASYNC,
                        xproto::GrabMode::ASYNC,
                    );
                } else {
                    let _ = self.conn.ungrab_key(key, self.root, mods);
                }
            }
        }
        let _ = self.conn.flush();
        self.media_grabbed = grab;
    }

    /// Perform a media command on this machine: tap the media key a
    /// physical keyboard would send.
    ///
    /// # Why the grabs come off for the tap
    ///
    /// An injected XTest key is an ordinary key event: the grabs that make
    /// routing possible would swallow it too, and the desktop would never
    /// see the command. So the suppression is lifted for the tap's duration
    /// — the passive media grabs, and the *keyboard* grab if the cursor is
    /// away — and put back after. Two round-trips bracket the tap: the
    /// first proves the releases have been processed before the key is
    /// injected, the second proves the tap has been delivered before the
    /// grabs return.
    ///
    /// The window this opens is real and worth stating plainly: for the two
    /// round-trips it takes (sub-millisecond on a healthy server) some
    /// *other* key pressed by hand in that instant could reach the local
    /// desktop instead of the client. It is bounded by the fact that the key being handled is
    /// already consumed, that a media key is human-paced, and that nothing
    /// else is in flight — and it is the only way to keep the promise that
    /// `local` means the machine actually acts.
    pub(crate) fn media(&mut self, command: MediaCommand) {
        // The router reaches this only while the grab is armed — but the
        // arm can fail, and then the desktop already acted on the physical
        // key. Tapping it again would act twice, so nothing is done.
        if !self.media_grabbed && !self.kbd_grabbed {
            return;
        }
        let Some(keycode) = media::keycode_for(command) else {
            log_warn!("media: {command:?} has no X keycode in this build — not performed");
            return;
        };
        // Record the tap so this thread's own raw stream can recognise the
        // echo: XTest events generate raw XI2 events, grabs do not stop
        // that, and an unrecognised tap would be classified, routed, and
        // tapped again — forever. See [`InputCapture::is_own_media_tap`].
        self.note_injected_media(keycode);
        let media_was = self.media_grabbed;
        let keyboard_was = self.kbd_grabbed;
        if media_was {
            self.grab_media_keys(false);
        }
        if keyboard_was {
            let _ = self.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
        }
        let _ = self.conn.flush();
        // Round-trip 1: the server has processed the releases.
        let _ = self.conn.get_input_focus().ok().and_then(|c| c.reply().ok());
        let _ = self.conn.xtest_fake_input(
            media::PRESS,
            keycode,
            x11rb::CURRENT_TIME,
            self.root,
            0,
            0,
            0,
        );
        let _ = self.conn.xtest_fake_input(
            media::RELEASE,
            keycode,
            x11rb::CURRENT_TIME,
            self.root,
            0,
            0,
            0,
        );
        let _ = self.conn.flush();
        // Round-trip 2: the tap has been delivered.
        let _ = self.conn.get_input_focus().ok().and_then(|c| c.reply().ok());
        if media_was {
            self.grab_media_keys(true);
        }
        if keyboard_was && !self.retake_keyboard_grab() {
            log_warn!("media: could not re-take the keyboard grab after performing {command:?}");
            self.kbd_grabbed = false;
        }
        log_debug!("media: {command:?} performed locally");
    }

    /// Re-take the keyboard grab after a media-key injection window, with
    /// the same retry policy [`Self::set_grabbed`] uses.
    fn retake_keyboard_grab(&mut self) -> bool {
        for attempt in 0..GRAB_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(GRAB_RETRY_GAP);
            }
            if self.keyboard_grab_attempt() == Some(true) {
                self.kbd_grabbed = true;
                return true;
            }
        }
        false
    }

    /// One `GrabKeyboard` request and its reply, or `None` when the
    /// request itself failed (a broken connection — the caller's retry is
    /// pointless, but harmless).
    fn keyboard_grab_attempt(&mut self) -> Option<bool> {
        self.conn
            .grab_keyboard(
                false,
                self.root,
                x11rb::CURRENT_TIME,
                xproto::GrabMode::ASYNC,
                xproto::GrabMode::ASYNC,
            )
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.status == xproto::GrabStatus::SUCCESS)
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
            // The grab races whoever else is grabbing right now — a WM
            // alt-tab popup, our own chord passive-grab re-arm, a
            // screenshot tool. A single attempt losing that race left the
            // local desktop live while the cursor was on the client (the
            // user's own typing acted on BOTH machines at once), so the
            // attempt is retried on a short jitter-free cadence: a popup
            // grab lives milliseconds, ours (GrabMode::SYNC passive
            // grabs) release on the next key. `x11rb::CURRENT_TIME` is
            // still correct — a retry re-issues a fresh request, and the
            // server resolves it against the latest timestamp it saw.
            let mut pointer = None;
            let mut keyboard = None;
            for attempt in 0..GRAB_ATTEMPTS {
                if attempt > 0 {
                    std::thread::sleep(GRAB_RETRY_GAP);
                }
                if pointer != Some(true) {
                    pointer = self
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
                }
                if keyboard != Some(true) {
                    keyboard = self.keyboard_grab_attempt();
                }
                if pointer == Some(true) && keyboard == Some(true) {
                    break;
                }
            }
            // The keyboard half is tracked on its own as well as through
            // `grabbed`: a media command performed locally has to know
            // whether *this* grab stands between it and the desktop (see
            // `Self::media`), while the combined flag reports the pointer
            // grab the beacon thread cares about. A partial success counts:
            // a held keyboard grab suppresses keys whether or not the
            // pointer grab came with it.
            self.kbd_grabbed = keyboard == Some(true);
            match (pointer, keyboard) {
                (Some(true), Some(true)) => true,
                (p, k) => {
                    log_warn!("input grab not acquired after {GRAB_ATTEMPTS} attempts (pointer: {p:?}, keyboard: {k:?})");
                    false
                }
            }
        } else {
            let _ = self.conn.ungrab_pointer(x11rb::CURRENT_TIME);
            let _ = self.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
            let _ = self.conn.flush();
            self.kbd_grabbed = false;
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
