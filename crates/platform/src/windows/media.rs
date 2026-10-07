//! Media control on Windows: which key expresses a [`MediaCommand`], how to
//! tap it, and how a tap of ours is told apart from a real keypress.
//!
//! # One mapping, two users
//!
//! A media command is performed identically whether this machine is acting
//! as a **client** (the server routed the key here, and the injector taps
//! it) or as a **server** (the router resolved the command as local, and the
//! engine taps it). Both therefore go through [`tap`], and both therefore
//! degrade the same way on the keys Windows does not have.
//!
//! # Windows has no discrete Play, Pause, fast-forward or rewind
//!
//! Windows exposes media transport as four extended scan codes — next
//! (`E0 19`), previous (`E0 10`), stop (`E0 24`) and a single play/pause
//! **toggle** (`E0 22`) — plus the three volume keys. So:
//!
//! * `Play`/`Pause` become the toggle. Pressing "play" on a machine that
//!   only has a toggle does what the user meant often enough to be worth
//!   doing, and doing nothing would look like a dead key.
//! * `SeekForward`/`SeekBackward` have no equivalent at all. Faking them
//!   with the toggle would *pause the music* when the user asked to skip
//!   ahead — actively wrong — so they are logged (once per 30 s) and
//!   ignored.
//!
//! This is exactly the OS-specific degradation the semantic command space
//! exists for: the machine that sent the command does not need to know any
//! of it.
//!
//! # Why a tap is marked
//!
//! The server's capture installs a low-level keyboard hook that suppresses
//! media keys while media routing is enabled (see [`super::capture`]) — it
//! has to, or a key routed to the other machine would also act on this one.
//! A tap **we** produce has to be exempt from that suppression, for two
//! independent reasons:
//!
//! * it is the command being performed *here*, so suppressing it would make
//!   `local` a no-op, and the hook would forward it back to the session,
//!   which would resolve it as local again — a loop;
//! * it is not user input, so it must not be captured as input either.
//!
//! Windows provides the marker for exactly this: `KEYBDINPUT::dwExtraInfo`,
//! a field the hook's `KBDLLHOOKSTRUCT` carries through unchanged. Every
//! event this module injects is stamped with [`INJECTED_MEDIA`], and the
//! hook lets those through untouched.

use windows_sys::Win32::UI::Input::KeyboardAndMouse as km;

use kvmshare_protocol::message::MediaCommand;

/// The `dwExtraInfo` marker on every media tap this process injects.
///
/// Spells `KVMEDIA` in ASCII, so a debugger (or a future reader of a hook
/// trace) can recognise it without a lookup table.
pub(crate) const INJECTED_MEDIA: usize = 0x4B56_4D45_44_49_41;

/// Whether a hook event carries our media-tap marker — i.e. whether it is
/// the local half of media routing rather than something the user pressed.
pub(crate) fn is_our_injection(extra_info: usize) -> bool {
    extra_info == INJECTED_MEDIA
}

/// The (set-1 scan code, E0-extended flag) a command is performed with, or
/// `None` for a command Windows has no key for (see the module docs).
///
/// The mapping is the *only* place Windows' media key table lives, so the
/// injector and the engine cannot disagree about which key "next track" is.
pub(crate) fn scancode_for(command: MediaCommand) -> Option<(u16, bool)> {
    let hid = match command {
        // No discrete keys: the toggle is the closest real operation.
        MediaCommand::Play | MediaCommand::Pause | MediaCommand::PlayPause => 0xcd,
        MediaCommand::Next => 0xb5,
        MediaCommand::Previous => 0xb6,
        MediaCommand::Stop => 0xb7,
        MediaCommand::VolumeUp => 0xe9,
        MediaCommand::VolumeDown => 0xea,
        MediaCommand::Mute => 0xe8,
        MediaCommand::SeekForward | MediaCommand::SeekBackward => {
            log_unsupported(command);
            return None;
        }
    };
    match crate::keys::scancode_from_hid(hid) {
        Some(pair) => Some(pair),
        None => {
            // The shared key tables do not name this HID usage: a platform
            // gap, not a user error, so say which command lost a key.
            log_unsupported(command);
            None
        }
    }
}

/// Perform a media command on this machine: press and release its key
/// outright.
///
/// A tap rather than a held key, because that is what a media key is — there
/// is no down-state to track, so nothing can be left stuck if the caller
/// goes away mid-call. The marker (see the module docs) is what keeps the tap
/// from being suppressed by this machine's own capture hook, and from being
/// mistaken for the user's input.
pub(crate) fn tap(command: MediaCommand) {
    let Some((scan, extended)) = scancode_for(command) else {
        return;
    };
    for key_up in [false, true] {
        // SAFETY: a well-formed KEYBDINPUT in scan-code mode (the layout
        // independent model — the physical key identity is what travels).
        let mut input: km::INPUT = unsafe { std::mem::zeroed() };
        input.r#type = km::INPUT_KEYBOARD;
        input.Anonymous.ki.wVk = 0; // scan-code mode: virtual key unused
        input.Anonymous.ki.wScan = scan;
        let mut flags = km::KEYEVENTF_SCANCODE;
        if extended {
            flags |= km::KEYEVENTF_EXTENDEDKEY;
        }
        if key_up {
            flags |= km::KEYEVENTF_KEYUP;
        }
        input.Anonymous.ki.dwFlags = flags;
        input.Anonymous.ki.dwExtraInfo = INJECTED_MEDIA;
        super::injector::send_input(&input);
    }
}

/// A media command Windows has no key for. Logged (at most once per 30 s)
/// rather than silently ignored: the user pressed a key and the machine did
/// nothing, and "seek is not supported on this target" is the answer they
/// need. Throttled because a held key repeats.
fn log_unsupported(command: MediaCommand) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST_WARNED: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now >= LAST_WARNED.swap(now, Ordering::Relaxed) + 30 {
        kvmshare_log::log_warn!(
            "media: {command:?} has no Windows key equivalent — command ignored (no fast-forward/rewind media keys on Windows)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command either has a key or is a documented gap: nothing may
    /// fall through the mapping by accident, because a command with no key
    /// and no log is a key the user presses to no effect and no explanation.
    #[test]
    fn every_command_is_mapped_or_a_known_gap() {
        for command in MediaCommand::ALL {
            let mapped = scancode_for(command).is_some();
            let known_gap = matches!(
                command,
                MediaCommand::SeekForward | MediaCommand::SeekBackward
            );
            assert!(mapped || known_gap, "{command:?} is neither mapped nor a known gap");
        }
    }

    /// The four transport keys Windows really has are distinct keys — the
    /// toggle must not be reused for next/previous/stop, which would make
    /// "next track" pause the music.
    #[test]
    fn transport_keys_are_distinct() {
        let next = scancode_for(MediaCommand::Next).unwrap();
        let previous = scancode_for(MediaCommand::Previous).unwrap();
        let stop = scancode_for(MediaCommand::Stop).unwrap();
        let toggle = scancode_for(MediaCommand::PlayPause).unwrap();
        for (a, b) in [(next, previous), (next, stop), (next, toggle), (previous, stop), (previous, toggle), (stop, toggle)] {
            assert_ne!(a, b);
        }
    }

    /// Play and Pause deliberately collapse to the one key Windows has. This
    /// is the documented degradation, pinned so a future "improvement" that
    /// silently dropped one of them fails here instead.
    #[test]
    fn play_and_pause_collapse_to_the_toggle() {
        let toggle = scancode_for(MediaCommand::PlayPause).unwrap();
        assert_eq!(scancode_for(MediaCommand::Play).unwrap(), toggle);
        assert_eq!(scancode_for(MediaCommand::Pause).unwrap(), toggle);
        // Windows' media keys are extended (E0-prefixed).
        assert!(toggle.1, "the play/pause toggle is an E0 key");
    }

    /// The marker is a real value and is recognised — an accidental 0 would
    /// mark nothing, and the tap would be suppressed by our own hook.
    #[test]
    fn the_marker_is_set_and_recognised() {
        assert_ne!(INJECTED_MEDIA, 0);
        assert!(is_our_injection(INJECTED_MEDIA));
        assert!(!is_our_injection(0));
        assert!(!is_our_injection(INJECTED_MEDIA.wrapping_add(1)));
    }
}
