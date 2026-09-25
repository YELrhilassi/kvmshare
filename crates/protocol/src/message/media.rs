//! Media-control commands: the semantic vocabulary for playback and
//! volume keys.
//!
//! # Why not just relay the key?
//!
//! kvmshare already carries ordinary keys OS-neutrally as USB HID usages,
//! and a media key *could* simply be forwarded like any other. It is not,
//! for two reasons:
//!
//! * **Routing needs categories.** "Volume up" and "next track" are not
//!   the same kind of request (see [`MediaCommand::is_volume`]) and a
//!   policy may well send them to different machines. A raw key code
//!   cannot express *"raise the volume on the machine I am listening
//!   to"*.
//! * **Destination capability differs by OS.** Windows has a single
//!   Play/Pause toggle and no distinct fast-forward scan code; Linux has
//!   discrete keysyms. A semantic command lets each machine degrade to
//!   what it actually has, instead of the sender guessing.
//!
//! # Where classification happens
//!
//! [`MediaCommand::from_hid`] recognises a media key at the **capture
//! edge**, before the session sees it, so a media key can never leak into
//! the remote keyboard stream or accidentally follow the cursor. Every
//! capture backend (X11, Windows Raw Input, evdev) feeds the same
//! classifier because they all speak HID usages already.
//!
//! The recognised usages are exactly the consumer-page (0x0C) entries in
//! the platform key tables, so classification and injection can never
//! disagree about what a given key is.

use crate::wire::{ReadBuf, WireError, WriteBuf};

/// A media-control request, independent of any OS's key codes.
///
/// Stored as a plain `u8` on the wire (see [`crate::id::media`]) so a
/// future command is an addition, not a protocol break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCommand {
    /// Toggle play/pause — the only transport control every OS has.
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    SeekForward,
    SeekBackward,
    VolumeUp,
    VolumeDown,
    Mute,
}

impl MediaCommand {
    /// Every command, for exhaustive tests and GUI enumeration.
    pub const ALL: [MediaCommand; 11] = [
        MediaCommand::PlayPause,
        MediaCommand::Play,
        MediaCommand::Pause,
        MediaCommand::Stop,
        MediaCommand::Next,
        MediaCommand::Previous,
        MediaCommand::SeekForward,
        MediaCommand::SeekBackward,
        MediaCommand::VolumeUp,
        MediaCommand::VolumeDown,
        MediaCommand::Mute,
    ];

    /// Which category this command belongs to, for routing.
    ///
    /// Volume is deliberately its own category: it acts on the *output
    /// the user is listening to*, while transport acts on the *media
    /// source*. When both machines are audible at once those are
    /// different machines, and one policy cannot serve both.
    pub fn is_volume(self) -> bool {
        matches!(
            self,
            MediaCommand::VolumeUp | MediaCommand::VolumeDown | MediaCommand::Mute
        )
    }

    /// The wire id for this command.
    pub fn to_id(self) -> u8 {
        use crate::id::media;
        match self {
            MediaCommand::PlayPause => media::PLAY_PAUSE,
            MediaCommand::Play => media::PLAY,
            MediaCommand::Pause => media::PAUSE,
            MediaCommand::Stop => media::STOP,
            MediaCommand::Next => media::NEXT,
            MediaCommand::Previous => media::PREVIOUS,
            MediaCommand::SeekForward => media::SEEK_FORWARD,
            MediaCommand::SeekBackward => media::SEEK_BACKWARD,
            MediaCommand::VolumeUp => media::VOLUME_UP,
            MediaCommand::VolumeDown => media::VOLUME_DOWN,
            MediaCommand::Mute => media::MUTE,
        }
    }

    /// Decode a wire id. `None` for an unknown command — a peer running a
    /// newer build must never make an older one act on a guess.
    pub fn from_id(id: u8) -> Option<Self> {
        use crate::id::media;
        Some(match id {
            media::PLAY_PAUSE => MediaCommand::PlayPause,
            media::PLAY => MediaCommand::Play,
            media::PAUSE => MediaCommand::Pause,
            media::STOP => MediaCommand::Stop,
            media::NEXT => MediaCommand::Next,
            media::PREVIOUS => MediaCommand::Previous,
            media::SEEK_FORWARD => MediaCommand::SeekForward,
            media::SEEK_BACKWARD => MediaCommand::SeekBackward,
            media::VOLUME_UP => MediaCommand::VolumeUp,
            media::VOLUME_DOWN => MediaCommand::VolumeDown,
            media::MUTE => MediaCommand::Mute,
            _ => return None,
        })
    }

    /// Recognise a media key from its canonical USB HID usage.
    ///
    /// Returns `None` for an ordinary key, which is the overwhelming
    /// majority of traffic: classification is a small match over the
    /// consumer-page usages the platform key tables define, so it costs
    /// a jump table lookup on the hot path.
    ///
    /// Two usages are worth explaining:
    ///
    /// * `0xcd` is the consumer page's *Play/Pause* toggle — the key
    ///   actually printed on most keyboards. `0xb0`/`0xb1` are the
    ///   discrete Play and Pause some QMK boards send instead.
    /// * `0xe8` is Mute. The consumer page also defines `0xe2` as Mute;
    ///   the shared key tables map `0xe8`, and both are accepted here so
    ///   a keyboard using either convention is classified rather than
    ///   silently forwarded as a normal key.
    pub fn from_hid(hid: u32) -> Option<Self> {
        Some(match hid {
            0xcd => MediaCommand::PlayPause,
            0xb0 => MediaCommand::Play,
            0xb1 => MediaCommand::Pause,
            0xb7 => MediaCommand::Stop,
            0xb5 => MediaCommand::Next,
            0xb6 => MediaCommand::Previous,
            0xb3 => MediaCommand::SeekForward,
            0xb4 => MediaCommand::SeekBackward,
            0xe9 => MediaCommand::VolumeUp,
            0xea => MediaCommand::VolumeDown,
            0xe2 | 0xe8 => MediaCommand::Mute,
            _ => return None,
        })
    }

    /// The canonical HID usage for this command — the key a destination
    /// machine presses to express it.
    ///
    /// This is the *preferred* usage, chosen so the common commands inject
    /// cleanly on every supported platform. Backends whose OS lacks a
    /// discrete key (Windows has no separate Play/Pause/Fast-forward
    /// scancodes) degrade deliberately rather than dropping the request —
    /// see each backend's media mapping.
    pub fn to_hid(self) -> u32 {
        match self {
            MediaCommand::PlayPause => 0xcd,
            MediaCommand::Play => 0xb0,
            MediaCommand::Pause => 0xb1,
            MediaCommand::Stop => 0xb7,
            MediaCommand::Next => 0xb5,
            MediaCommand::Previous => 0xb6,
            MediaCommand::SeekForward => 0xb3,
            MediaCommand::SeekBackward => 0xb4,
            MediaCommand::VolumeUp => 0xe9,
            MediaCommand::VolumeDown => 0xea,
            MediaCommand::Mute => 0xe8,
        }
    }

    /// Encode as one byte.
    pub(crate) fn encode(self, w: &mut WriteBuf) {
        w.put_u8(self.to_id());
    }

    /// Decode from one byte.
    pub(crate) fn decode(r: &mut ReadBuf<'_>) -> Result<Self, WireError> {
        let id = r.get_u8()?;
        MediaCommand::from_id(id).ok_or(WireError { what: "unknown media command" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command survives a round trip through its wire id.
    #[test]
    fn commands_roundtrip_through_wire_ids() {
        for cmd in MediaCommand::ALL {
            assert_eq!(MediaCommand::from_id(cmd.to_id()), Some(cmd), "{cmd:?}");
        }
    }

    /// Wire ids are distinct — a collision would silently re-route a key.
    #[test]
    fn wire_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for cmd in MediaCommand::ALL {
            assert!(seen.insert(cmd.to_id()), "duplicate wire id for {cmd:?}");
        }
    }

    /// An unknown id decodes to nothing rather than to a guess. A peer on
    /// a newer build may define commands this build has never heard of;
    /// acting on the nearest known one would press the wrong key.
    #[test]
    fn unknown_id_is_rejected() {
        assert_eq!(MediaCommand::from_id(0xff), None);
    }

    /// Every command's preferred HID usage is itself classified back to
    /// that command, wherever the consumer page defines such a key. This
    /// is what makes the classification table and the injection table
    /// impossible to drift apart: a destination machine pressing the
    /// canonical key of command X produces command X again.
    #[test]
    fn preferred_hid_usages_classify_back() {
        for cmd in MediaCommand::ALL {
            let hid = cmd.to_hid();
            let back = MediaCommand::from_hid(hid);
            assert!(back.is_some(), "{cmd:?} (hid {hid:#x}) is not classified");
            // The only legitimate difference is Mute, which the consumer
            // page defines twice — injection picks 0xe8, and 0xe2 is
            // accepted as the same command.
            if let Some(back) = back {
                assert_eq!(back, cmd, "{cmd:?} (hid {hid:#x}) classified as {back:?}");
            }
        }
    }

    /// Ordinary keys are not media keys. A classifier that matched too
    /// much would swallow typing.
    #[test]
    fn ordinary_keys_are_not_classified() {
        for hid in [0x04u32, 0x1e, 0x2c, 0x39, 0xe0, 0xe1, 0x4c, 0x28] {
            assert_eq!(MediaCommand::from_hid(hid), None, "hid {hid:#x}");
        }
    }

    /// Volume is the category that targets the output, not the source.
    #[test]
    fn volume_category_is_exactly_the_output_controls() {
        for cmd in MediaCommand::ALL {
            let expected = matches!(
                cmd,
                MediaCommand::VolumeUp | MediaCommand::VolumeDown | MediaCommand::Mute
            );
            assert_eq!(cmd.is_volume(), expected, "{cmd:?}");
        }
    }
}
