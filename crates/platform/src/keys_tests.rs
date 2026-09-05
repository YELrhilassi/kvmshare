use super::*;

#[test]
fn known_keys_roundtrip() {
    // (HID usage, evdev code) pairs every backend must get right.
    for (hid, evdev) in [
        (0x04, 30), // A
        (0x14, 16), // Q
        (0x28, 28), // Enter
        (0x2c, 57), // Space
        (0x39, 58), // Caps Lock
        (0x45, 88), // F12
        (0x52, 103), // Up
        (0xe1, 42), // Left Shift
        (0xe7, 126), // Right Meta
    ] {
        assert_eq!(evdev_from_hid(hid), Some(evdev), "HID 0x{hid:02x}");
        assert_eq!(hid_from_evdev(evdev), Some(hid), "evdev {evdev}");
    }
}

#[test]
fn unknown_codes_are_none() {
    assert_eq!(evdev_from_hid(0x0001), None); // HID 0x01 is "ErrorRollOver"
    assert_eq!(hid_from_evdev(200), None);    // unmapped
    assert_eq!(scancode_from_hid(0x0001), None);
    assert_eq!(hid_from_scancode(0x00, false), None);
    // Pause (E1 sequence) is intentionally unmapped.
    assert_eq!(hid_from_scancode(0x1d, false), Some(0xe0)); // left ctrl
}

#[test]
fn media_keys_roundtrip() {
    // (HID usage, evdev code) pairs QMK media layers send — the
    // Ergodox EZ declares exactly these on its consumer-control node.
    for (hid, evdev) in [
        (0xb0, 207), // Play
        (0xb1, 201), // Pause
        (0xb3, 208), // Fast Forward
        (0xb4, 168), // Rewind
        (0xb5, 163), // Next Track
        (0xb6, 165), // Previous Track
        (0xb7, 166), // Stop
        (0xb8, 161), // Eject
        (0xb9, 398), // Shuffle
        (0xcd, 164), // Play/Pause
        (0x194, 150), // WWW
        (0x221, 217), // Search
        (0x223, 172), // Home
        (0x224, 158), // Back
        (0x225, 159), // Forward
        (0x226, 156), // Bookmarks
        (0x227, 173), // Refresh
        (0x228, 128), // Stop (AC)
    ] {
        assert_eq!(evdev_from_hid(hid), Some(evdev), "HID 0x{hid:02x}");
        assert_eq!(hid_from_evdev(evdev), Some(hid), "evdev {evdev}");
    }
    // Transport keys have standard set-1 extended scan codes; the
    // media-only keys (rewind, eject, …) have none on Windows and are
    // correctly absent from the scan table.
    assert_eq!(scancode_from_hid(0xcd), Some((0x22, true))); // Play/Pause
    assert_eq!(scancode_from_hid(0xb5), Some((0x19, true))); // Next
    assert_eq!(scancode_from_hid(0x224), Some((0x6a, true))); // Back
    assert_eq!(scancode_from_hid(0xb4), None); // Rewind: no scan code
}

#[test]
fn windows_keys_roundtrip() {
    for (hid, scan, ext) in [
        (0x04, 0x1e, false), // a
        (0x14, 0x10, false), // q
        (0x28, 0x1c, false), // Enter
        (0x2c, 0x39, false), // Space
        (0x39, 0x3a, false), // Caps Lock
        (0x45, 0x58, false), // F12
        (0x52, 0x48, true),  // Up (extended)
        (0xe1, 0x2a, false), // Left Shift
        (0xe4, 0x1d, true),  // Right Ctrl (extended)
        (0xe7, 0x5c, true),  // Right Meta (extended)
    ] {
        assert_eq!(scancode_from_hid(hid), Some((scan, ext)), "HID 0x{hid:02x}");
        assert_eq!(hid_from_scancode(scan, ext), Some(hid), "scan 0x{scan:02x} ext={ext}");
    }
}

#[test]
fn windows_extended_and_plain_same_scan_do_not_clash() {
    // Left Ctrl is plain 0x1d, Right Ctrl is E0 0x1d — they must map
    // independently.
    assert_eq!(hid_from_scancode(0x1d, false), Some(0xe0));
    assert_eq!(hid_from_scancode(0x1d, true), Some(0xe4));
}

#[test]
fn tables_are_consistent() {
    // Every evdev entry in HID_TO_EVDEV maps back to the same HID.
    for (hid, evdev) in HID_TO_EVDEV {
        assert_eq!(hid_from_evdev(*evdev), Some(*hid), "HID 0x{hid:02x}");
    }
    for (evdev, hid) in EVDEV_TO_HID {
        assert_eq!(evdev_from_hid(*hid), Some(*evdev), "evdev {evdev}");
    }
}
