//! Canonical key identity for the whole system: **USB HID usage ids**.
//!
//! Every OS has its own way of naming a key (X11 keycodes, Wayland/libinput
//! evdev codes, Windows scan codes). Over the wire kvmshare always speaks
//! HID usage ids, and each platform backend converts at its edge:
//!
//! ```text
//!   Linux X11:   keycode - 8  -> evdev -> HID    (capture)
//!                HID -> evdev -> keycode + 8     (injection)
//!   Linux WL:    evdev -> HID / HID -> evdev     (future backend, same table)
//!   Windows:     set-1 scancode (+E0 flag) -> HID / HID -> set-1 scancode
//! ```
//!
//! Because the wire format is OS-neutral, a Windows server driving a Linux
//! client (or any other pair) gets the *physical* key the user pressed;
//! each machine's own layout then produces the character.
//!
//! The tables below are the standard mappings (USB HID Usage Tables 1.4,
//! keyboard page 0x07; Linux `input-event-codes.h`; PC/AT set-1 make
//! codes). They cover the desktop keys a KVM needs; unknown codes are
//! dropped at the platform edge with a debug log.
//!
//! Both directions are kept in sync by the `tables_are_consistent` test,
//! so a bad entry can never silently break a cross-OS pair.

/// The evdev key code for a HID usage id, when known.
pub fn evdev_from_hid(hid: u32) -> Option<u16> {
    lookup(HID_TO_EVDEV, hid)
}

/// The HID usage id for an evdev key code, when known.
pub fn hid_from_evdev(evdev: u16) -> Option<u32> {
    lookup(EVDEV_TO_HID, evdev)
}

/// (set-1 make code, E0-extended flag) for a HID usage id, when known.
pub fn scancode_from_hid(hid: u32) -> Option<(u16, bool)> {
    HID_TO_SCAN.iter().find(|(h, _, _)| *h == hid).map(|(_, s, e)| (*s, *e))
}

/// The HID usage id for a (set-1 make code, E0-extended flag) pair,
/// when known.
pub fn hid_from_scancode(scan: u16, extended: bool) -> Option<u32> {
    HID_TO_SCAN.iter().find(|(_, s, e)| *s == scan && *e == extended).map(|(h, _, _)| *h)
}

/// HID usage of Scroll Lock — the escape key. While the cursor is on a
/// client, a Scroll Lock press hands control back to this machine
/// immediately, even if the client has stopped responding (elevated
/// window swallowing its input, a wedged session, a dead client). The
/// classic KVM "unstick", matching synergy/barrier muscle memory.
/// Scroll Lock is deliberately chosen: no application depends on it, and
/// it is only intercepted while the cursor is *away* — at home it passes
/// through untouched. Shared by every capture backend (X11 raw events
/// and evdev).
pub const ESCAPE_KEY_HID: u32 = 0x47;

fn lookup<A: Copy + PartialEq, B: Copy>(table: &[(A, B)], needle: A) -> Option<B> {
    // Tiny table, linear scan is fine (keys are ~10/s, this is negligible).
    table.iter().find(|(a, _)| *a == needle).map(|(_, b)| *b)
}

/// (HID usage, evdev code) pairs. Sorted by HID usage.
const HID_TO_EVDEV: &[(u32, u16)] = &[
    // Letters
    (0x04, 30), // a
    (0x05, 48), // b
    (0x06, 46), // c
    (0x07, 32), // d
    (0x08, 18), // e
    (0x09, 33), // f
    (0x0a, 34), // g
    (0x0b, 35), // h
    (0x0c, 23), // i
    (0x0d, 36), // j
    (0x0e, 37), // k
    (0x0f, 38), // l
    (0x10, 50), // m
    (0x11, 49), // n
    (0x12, 24), // o
    (0x13, 25), // p
    (0x14, 16), // q
    (0x15, 19), // r
    (0x16, 31), // s
    (0x17, 20), // t
    (0x18, 22), // u
    (0x19, 47), // v
    (0x1a, 17), // w
    (0x1b, 45), // x
    (0x1c, 21), // y
    (0x1d, 44), // z
    // Digits
    (0x1e, 2),  // 1
    (0x1f, 3),  // 2
    (0x20, 4),  // 3
    (0x21, 5),  // 4
    (0x22, 6),  // 5
    (0x23, 7),  // 6
    (0x24, 8),  // 7
    (0x25, 9),  // 8
    (0x26, 10), // 9
    (0x27, 11), // 0
    // Editing / whitespace
    (0x28, 28), // Enter
    (0x29, 1),  // Escape
    (0x2a, 14), // Backspace
    (0x2b, 15), // Tab
    (0x2c, 57), // Space
    (0x2d, 12), // - _
    (0x2e, 13), // = +
    (0x2f, 26), // [ {
    (0x30, 27), // ] }
    (0x31, 43), // \ |
    (0x32, 86), // # ~ (non-US)
    (0x33, 39), // ; :
    (0x34, 40), // ' "
    (0x35, 41), // ` ~
    (0x36, 51), // , <
    (0x37, 52), // . >
    (0x38, 53), // / ?
    (0x39, 58), // Caps Lock
    // Function row
    (0x3a, 59), // F1
    (0x3b, 60), // F2
    (0x3c, 61), // F3
    (0x3d, 62), // F4
    (0x3e, 63), // F5
    (0x3f, 64), // F6
    (0x40, 65), // F7
    (0x41, 66), // F8
    (0x42, 67), // F9
    (0x43, 68), // F10
    (0x44, 87), // F11
    (0x45, 88), // F12
    // Navigation cluster
    (0x46, 99),  // Print Screen
    (0x47, 70),  // Scroll Lock
    (0x48, 119), // Pause
    (0x49, 110), // Insert
    (0x4a, 102), // Home
    (0x4b, 104), // Page Up
    (0x4c, 111), // Delete
    (0x4d, 107), // End
    (0x4e, 109), // Page Down
    (0x4f, 106), // Right
    (0x50, 105), // Left
    (0x51, 108), // Down
    (0x52, 103), // Up
    // Keypad
    (0x53, 69), // Num Lock
    (0x54, 98), // KP /
    (0x55, 55), // KP *
    (0x56, 74), // KP -
    (0x57, 78), // KP +
    (0x58, 96), // KP Enter
    (0x59, 79), // KP 1
    (0x5a, 80), // KP 2
    (0x5b, 81), // KP 3
    (0x5c, 75), // KP 4
    (0x5d, 76), // KP 5
    (0x5e, 77), // KP 6
    (0x5f, 71), // KP 7
    (0x60, 72), // KP 8
    (0x61, 73), // KP 9
    (0x62, 82), // KP 0
    (0x63, 83), // KP .
    (0x67, 117), // KP = (KEY_KPEQUAL)
    (0x85, 85), // KP , (KEY_KPJPCOMMA)
    // International (JIS etc.)
    (0x87, 89),  // Int'l 1 (Ro)
    (0x88, 93),  // Int'l 2 (Katakana)
    (0x89, 124), // Int'l 3 (Yen)
    (0x8a, 94),  // Int'l 4 (Henkan)
    (0x8b, 95),  // Int'l 5 (Muhenkan)
    // Media transport (USB HID consumer page 0x0C). QMK keyboards
    // (e.g. the Ergodox EZ) send these from their media layers.
    (0xb0, 207), // Play
    (0xb1, 201), // Pause
    (0xb3, 208), // Fast Forward
    (0xb4, 168), // Rewind
    (0xb5, 163), // Next Track
    (0xb6, 165), // Previous Track
    (0xb7, 166), // Stop
    (0xb8, 161), // Eject
    (0xb9, 398), // Shuffle (Random Toggle)
    (0xcd, 164), // Play/Pause
    // (Bass Boost is deliberately unmapped: its consumer usage 0xE5
    // collides with Right Shift in this single keyboard-page namespace.)
    // Browser / AC keys (consumer page 0x022x)
    (0x194, 150), // WWW (AL Internet Browser)
    (0x221, 217), // AC Search
    (0x223, 172), // AC Home
    (0x224, 158), // AC Back
    (0x225, 159), // AC Forward
    (0x226, 156), // AC Bookmarks
    (0x227, 173), // AC Refresh
    (0x228, 128), // AC Stop
    // Modifiers
    (0xe0, 29),  // Left Control
    (0xe1, 42),  // Left Shift
    (0xe2, 56),  // Left Alt
    (0xe3, 125), // Left GUI / Meta
    (0xe4, 97),  // Right Control
    (0xe5, 54),  // Right Shift
    (0xe6, 100), // Right Alt
    (0xe7, 126), // Right GUI / Meta
    // Media / volume
    (0xe8, 113), // Mute
    (0xe9, 115), // Volume Up
    (0xea, 114), // Volume Down
];

/// (evdev code, HID usage) pairs — the reverse of [`HID_TO_EVDEV`].
const EVDEV_TO_HID: &[(u16, u32)] = &[
    (1, 0x29),   // ESC
    (2, 0x1e), (3, 0x1f), (4, 0x20), (5, 0x21), (6, 0x22), (7, 0x23), (8, 0x24), (9, 0x25), (10, 0x26), (11, 0x27),
    (12, 0x2d), // -
    (13, 0x2e), // =
    (14, 0x2a), // Backspace
    (15, 0x2b), // Tab
    (16, 0x14), (17, 0x1a), (18, 0x08), (19, 0x15), (20, 0x17), (21, 0x1c), (22, 0x18), (23, 0x0c), (24, 0x12), (25, 0x13),
    (26, 0x2f), // [
    (27, 0x30), // ]
    (28, 0x28), // Enter
    (29, 0xe0), // Left Ctrl
    (30, 0x04), (31, 0x16), (32, 0x07), (33, 0x09), (34, 0x0a), (35, 0x0b), (36, 0x0d), (37, 0x0e), (38, 0x0f),
    (39, 0x33), // ;
    (40, 0x34), // '
    (41, 0x35), // `
    (42, 0xe1), // Left Shift
    (43, 0x31), // Backslash
    (44, 0x1d), (45, 0x1b), (46, 0x06), (47, 0x19), (48, 0x05), (49, 0x11), (50, 0x10),
    (51, 0x36), // ,
    (52, 0x37), // .
    (53, 0x38), // /
    (54, 0xe5), // Right Shift
    (55, 0x55), // KP *
    (56, 0xe2), // Left Alt
    (57, 0x2c), // Space
    (58, 0x39), // Caps Lock
    (59, 0x3a), (60, 0x3b), (61, 0x3c), (62, 0x3d), (63, 0x3e), (64, 0x3f), (65, 0x40), (66, 0x41), (67, 0x42), (68, 0x43),
    (69, 0x53), // Num Lock
    (70, 0x47), // Scroll Lock
    (71, 0x5f), (72, 0x60), (73, 0x61), // KP 7 8 9
    (74, 0x56), // KP -
    (75, 0x5c), (76, 0x5d), (77, 0x5e), // KP 4 5 6
    (78, 0x57), // KP +
    (79, 0x59), (80, 0x5a), (81, 0x5b), // KP 1 2 3
    (82, 0x62), // KP 0
    (83, 0x63), // KP .
    (85, 0x85), // KP ,
    (86, 0x32), // Non-US #
    (87, 0x44), (88, 0x45), // F11 F12
    (89, 0x87), // Ro
    (93, 0x88), // Katakana
    (94, 0x8a), // Henkan
    (95, 0x8b), // Muhenkan
    (96, 0x58), // KP Enter
    (97, 0xe4), // Right Ctrl
    (98, 0x54), // KP /
    (99, 0x46), // Print Screen
    (100, 0xe6), // Right Alt
    (102, 0x4a), // Home
    (103, 0x52), // Up
    (104, 0x4b), // Page Up
    (105, 0x50), // Left
    (106, 0x4f), // Right
    (107, 0x4d), // End
    (108, 0x51), // Down
    (109, 0x4e), // Page Down
    (110, 0x49), // Insert
    (111, 0x4c), // Delete
    (113, 0xe8), // Mute
    (114, 0xea), // Volume Down
    (115, 0xe9), // Volume Up
    (117, 0x67), // KP =
    (119, 0x48), // Pause
    (124, 0x89), // Yen
    (125, 0xe3), // Left Meta
    (126, 0xe7), // Right Meta
    // Media transport
    (128, 0x228), // AC Stop
    (150, 0x194), // WWW
    (156, 0x226), // Bookmarks
    (158, 0x224), // Back
    (159, 0x225), // Forward
    (161, 0xb8),  // Eject
    (163, 0xb5),  // Next Track
    (164, 0xcd),  // Play/Pause
    (165, 0xb6),  // Previous Track
    (166, 0xb7),  // Stop
    (168, 0xb4),  // Rewind
    (172, 0x223), // Home
    (173, 0x227), // Refresh
    (201, 0xb1),  // Pause
    (207, 0xb0),  // Play
    (208, 0xb3),  // Fast Forward
    // (KEY_BASSBOOST 209 is deliberately unmapped — see HID_TO_EVDEV.)
    (217, 0x221), // Search
    (398, 0xb9),  // Shuffle
];

/// (HID usage, set-1 scancode, E0-extended flag) triples for Windows
/// Raw Input (`RAWKEYBOARD.MakeCode` + `RI_KEY_E0`) and `SendInput`
/// (scan-code mode). Set 1 is layout-independent, the same model as the
/// evdev table above.
const HID_TO_SCAN: &[(u32, u16, bool)] = &[
    // Letters
    (0x04, 0x1e, false), // a
    (0x05, 0x30, false), // b
    (0x06, 0x2e, false), // c
    (0x07, 0x20, false), // d
    (0x08, 0x12, false), // e
    (0x09, 0x21, false), // f
    (0x0a, 0x22, false), // g
    (0x0b, 0x23, false), // h
    (0x0c, 0x17, false), // i
    (0x0d, 0x24, false), // j
    (0x0e, 0x25, false), // k
    (0x0f, 0x26, false), // l
    (0x10, 0x32, false), // m
    (0x11, 0x31, false), // n
    (0x12, 0x18, false), // o
    (0x13, 0x19, false), // p
    (0x14, 0x10, false), // q
    (0x15, 0x13, false), // r
    (0x16, 0x1f, false), // s
    (0x17, 0x14, false), // t
    (0x18, 0x16, false), // u
    (0x19, 0x2f, false), // v
    (0x1a, 0x11, false), // w
    (0x1b, 0x2d, false), // x
    (0x1c, 0x15, false), // y
    (0x1d, 0x2c, false), // z
    // Digits
    (0x1e, 0x02, false), (0x1f, 0x03, false), (0x20, 0x04, false), (0x21, 0x05, false),
    (0x22, 0x06, false), (0x23, 0x07, false), (0x24, 0x08, false), (0x25, 0x09, false),
    (0x26, 0x0a, false), (0x27, 0x0b, false),
    // Editing / whitespace / punctuation
    (0x28, 0x1c, false), // Enter
    (0x29, 0x01, false), // Escape
    (0x2a, 0x0e, false), // Backspace
    (0x2b, 0x0f, false), // Tab
    (0x2c, 0x39, false), // Space
    (0x2d, 0x0c, false), // - _
    (0x2e, 0x0d, false), // = +
    (0x2f, 0x1a, false), // [ {
    (0x30, 0x1b, false), // ] }
    (0x31, 0x2b, false), // \ |
    (0x32, 0x56, false), // # ~ (ISO)
    (0x33, 0x27, false), // ; :
    (0x34, 0x28, false), // ' "
    (0x35, 0x29, false), // ` ~
    (0x36, 0x33, false), // , <
    (0x37, 0x34, false), // . >
    (0x38, 0x35, false), // / ?
    (0x39, 0x3a, false), // Caps Lock
    // Function row
    (0x3a, 0x3b, false), (0x3b, 0x3c, false), (0x3c, 0x3d, false), (0x3d, 0x3e, false),
    (0x3e, 0x3f, false), (0x3f, 0x40, false), (0x40, 0x41, false), (0x41, 0x42, false),
    (0x42, 0x43, false), (0x43, 0x44, false), (0x44, 0x57, false), (0x45, 0x58, false),
    // Navigation cluster (E0-extended)
    (0x46, 0x37, true),  // Print Screen
    (0x47, 0x46, false), // Scroll Lock
    (0x49, 0x52, true),  // Insert
    (0x4a, 0x47, true),  // Home
    (0x4b, 0x49, true),  // Page Up
    (0x4c, 0x53, true),  // Delete
    (0x4d, 0x4f, true),  // End
    (0x4e, 0x51, true),  // Page Down
    (0x4f, 0x4d, true),  // Right
    (0x50, 0x4b, true),  // Left
    (0x51, 0x50, true),  // Down
    (0x52, 0x48, true),  // Up
    // Keypad
    (0x53, 0x45, false), // Num Lock
    (0x54, 0x35, true),  // KP /
    (0x55, 0x37, false), // KP *
    (0x56, 0x4a, false), // KP -
    (0x57, 0x4e, false), // KP +
    (0x58, 0x1c, true),  // KP Enter
    (0x59, 0x4f, false), (0x5a, 0x50, false), (0x5b, 0x51, false), // KP 1 2 3
    (0x5c, 0x4b, false), (0x5d, 0x4c, false), (0x5e, 0x4d, false), // KP 4 5 6
    (0x5f, 0x47, false), (0x60, 0x48, false), (0x61, 0x49, false), // KP 7 8 9
    (0x62, 0x52, false), // KP 0
    (0x63, 0x53, false), // KP .
    (0x67, 0x59, false), // KP =
    // Media transport (standard set-1 extended scan codes)
    (0xb5, 0x19, true), // Next Track
    (0xb6, 0x10, true), // Previous Track
    (0xb7, 0x24, true), // Stop
    (0xcd, 0x22, true), // Play/Pause
    // International (JIS)
    (0x87, 0x73, false), // Int'l 1 (Ro)
    (0x88, 0x70, false), // Int'l 2 (Katakana)
    (0x89, 0x7d, false), // Int'l 3 (Yen)
    (0x8a, 0x79, false), // Int'l 4 (Henkan)
    (0x8b, 0x7b, false), // Int'l 5 (Muhenkan)
    // Modifiers
    (0xe0, 0x1d, false), // Left Control
    (0xe1, 0x2a, false), // Left Shift
    (0xe2, 0x38, false), // Left Alt
    (0xe3, 0x5b, true),  // Left GUI / Meta
    (0xe4, 0x1d, true),  // Right Control
    (0xe5, 0x36, false), // Right Shift
    (0xe6, 0x38, true),  // Right Alt
    (0xe7, 0x5c, true),  // Right GUI / Meta
    // Media (E0-prefixed scan codes)
    (0xe8, 0x20, true), // Mute
    (0xe9, 0x30, true), // Volume Up
    (0xea, 0x2e, true), // Volume Down
    // Browser / AC keys
    (0x194, 0x32, true), // WWW / Home
    (0x221, 0x65, true), // Search
    (0x223, 0x32, true), // Home (same scan as WWW)
    (0x224, 0x6a, true), // Back
    (0x225, 0x69, true), // Forward
    (0x226, 0x67, true), // Bookmarks
    (0x227, 0x66, true), // Refresh
    (0x228, 0x68, true), // Stop
];

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
