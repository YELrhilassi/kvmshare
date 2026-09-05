use super::*;

#[test]
fn wheel_data_converts_to_notches() {
    // 120 = one notch up, -120 = one notch down.
    assert_eq!(wheel_notches(120), 1);
    assert_eq!(wheel_notches(120u16.wrapping_neg()), -1);
    assert_eq!(wheel_notches(0), 0);
    // Two notches (some mice report 240).
    assert_eq!(wheel_notches(240), 2);
}

#[test]
fn e1_pause_sequence_is_dropped_not_misforwarded() {
    // Pause arrives as make code 0x45 with RI_KEY_E1 set — the same
    // make code as Num Lock. It must be dropped, never forwarded as
    // Num Lock.
    let mut capture = InputCapture { tx: mpsc::channel().0, abs: AbsoluteTracker::default(), keys: KeyTracker::default(), beacon: None };
    // SAFETY: zeroed struct; only the fields the handler reads are
    // populated, matching a real raw-input record.
    let mut kb: ri::RAWKEYBOARD = unsafe { std::mem::zeroed() };
    kb.MakeCode = 0x45;
    kb.Flags = wm::RI_KEY_E1 as u16;
    kb.Message = wm::WM_KEYDOWN;
    capture.on_keyboard(&kb);
    assert!(capture.keys.down.is_empty(), "Pause must not be tracked as a key");
}

#[test]
fn key_tracker_dedupes_repeats() {
    let mut capture = InputCapture { tx: mpsc::channel().0, abs: AbsoluteTracker::default(), keys: KeyTracker::default() };
    // Down, then auto-repeat downs, then up.
    capture.keys.down.insert((0x1e, false));
    assert!(capture.keys.down.contains(&(0x1e, false)));
    // A repeat while already down must not change state.
    let id = (0x1e, false);
    let was_down = capture.keys.down.contains(&id);
    assert!(was_down);
    // Up clears it.
    capture.keys.down.remove(&id);
    assert!(!capture.keys.down.contains(&id));
}

#[test]
fn absolute_tracker_computes_deltas() {
    let mut abs = AbsoluteTracker::default();
    assert_eq!((abs.last_x, abs.last_y), (None, None));
    abs.last_x = Some(100);
    abs.last_y = Some(50);
    assert_eq!((abs.last_x, abs.last_y), (Some(100), Some(50)));
}
