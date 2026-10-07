//! The media keys on X11: which keycodes they are, and how a media command
//! is performed locally.
//!
//! # Why the key set is derived, not written down
//!
//! Every media key is classified from its canonical USB HID usage at the
//! capture edge (see [`MediaCommand::from_hid`]), and the platform already
//! maps HID usages to X keycodes for ordinary typing (see [`crate::keys`]).
//! So the set of keycodes to intercept is derived from the *same* table the
//! classifier and the injector use:
//!
//! ```text
//! MediaCommand -> to_hid() -> evdev_from_hid() -> keycode (evdev + 8)
//! ```
//!
//! Three tables that cannot drift, because there is only one. A key the
//! user's keyboard sends is therefore classified and intercepted, and a
//! command routed to this machine is injected as that same key — the exact
//! property that makes a routed media key indistinguishable from pressing
//! it here.
//!
//! [`MediaCommand::from_hid`]: kvmshare_protocol::message::MediaCommand::from_hid

use kvmshare_protocol::message::MediaCommand;

/// XTest event type codes (the core protocol's `KeyPress` / `KeyRelease`;
/// x11rb does not export them as constants). The injector names the same
/// two for its own taps.
pub(crate) const PRESS: u8 = 2;
pub(crate) const RELEASE: u8 = 3;

/// The X keycode for a command's canonical media key, or `None` when this
/// build's key tables do not name it (the documented degradation: the key
/// is then not intercepted, and a media command routed here has no key to
/// tap).
///
/// `keycode = evdev + 8` is the standard Xorg evdev mapping — the same
/// identity [`crate::keys`] documents, and the one the chord grabs already
/// rely on.
pub(crate) fn keycode_for(command: MediaCommand) -> Option<u8> {
    crate::keys::evdev_from_hid(command.to_hid())
        .map(|evdev| (evdev as u8).wrapping_add(8))
}

/// Every media keycode this build knows, sorted and deduplicated — the set
/// the capture grabs while media routing is enabled.
///
/// Sorted and deduplicated because it is used as a diff baseline: a policy
/// change that arrives with the same set must not release and re-take every
/// grab (that would be a sub-millisecond window in which a media key reaches
/// the desktop for nothing).
pub(crate) fn keycodes() -> Vec<u8> {
    let mut codes: Vec<u8> = MediaCommand::ALL.iter().filter_map(|c| keycode_for(*c)).collect();
    codes.sort_unstable();
    codes.dedup();
    codes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command maps to a keycode on this platform: a command with no
    /// key is a command that cannot be performed locally, and the user
    /// would see a dead key on a machine whose policy routes volume here.
    #[test]
    fn every_command_has_a_keycode() {
        for command in MediaCommand::ALL {
            assert!(
                keycode_for(command).is_some(),
                "{command:?} has no X keycode in this build's key tables"
            );
        }
    }

    /// The Xorg evdev convention: keycode 8 is evdev 1. Checked on the two
    /// ends of the media range so a table edit that broke the offset would
    /// fail here rather than on a user's keyboard.
    #[test]
    fn keycodes_follow_the_xorg_evdev_offset() {
        // evdev 163 = KEY_NEXTSONG -> keycode 171 (XF86AudioNext).
        assert_eq!(keycode_for(MediaCommand::Next), Some(171));
        // evdev 207 = KEY_PLAY -> keycode 215 (XF86AudioPlay).
        assert_eq!(keycode_for(MediaCommand::Play), Some(215));
        // evdev 113 = KEY_MUTE -> keycode 121 (XF86AudioMute).
        assert_eq!(keycode_for(MediaCommand::Mute), Some(121));
    }

    /// The grab set covers every command, with no duplicates — a duplicate
    /// would make the "same set" check in the capture fail to recognise an
    /// unchanged policy.
    #[test]
    fn the_grab_set_covers_every_command_once() {
        let codes = keycodes();
        assert_eq!(codes.len(), MediaCommand::ALL.len());
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, codes, "sorted and deduplicated");
    }

    /// Play and PlayPause are different keys on X11 (unlike Windows, which
    /// has only the toggle): a machine that can express both must not
    /// collapse them.
    #[test]
    fn discrete_play_and_pause_are_distinct_keys() {
        assert_ne!(keycode_for(MediaCommand::Play), keycode_for(MediaCommand::Pause));
        assert_ne!(keycode_for(MediaCommand::PlayPause), keycode_for(MediaCommand::Play));
    }
}
