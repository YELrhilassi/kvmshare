use super::*;

fn collect(events: &[(u16, u16, i32)]) -> Vec<Message> {
    let mut motion = PendingMotion::default();
    let mut press = PressState::new();
    let mut out = Vec::new();
    for (ty, code, value) in events {
        handle_event(&InputEvent::new(*ty, *code, *value), &mut motion, &mut press, &mut |m| out.push(m));
    }
    motion.flush(&mut |dx, dy| out.push(Message::MouseMoveRel { dx, dy }));
    out
}

fn key(code: u16, value: i32) -> (u16, u16, i32) {
    (1, code, value) // EV_KEY
}

fn rel(code: u16, value: i32) -> (u16, u16, i32) {
    (2, code, value) // EV_REL
}

#[test]
fn motion_is_accumulated_and_rate_limited() {
    let msgs = collect(&[rel(RelativeAxisCode::REL_X.0, 5), rel(RelativeAxisCode::REL_Y.0, -3)]);
    assert_eq!(msgs, vec![Message::MouseMoveRel { dx: 5, dy: -3 }]);
}

#[test]
fn wheel_is_one_message_per_notch() {
    let msgs = collect(&[
        rel(RelativeAxisCode::REL_WHEEL.0, 1),
        rel(RelativeAxisCode::REL_WHEEL.0, -1),
        rel(RelativeAxisCode::REL_HWHEEL.0, 1),
    ]);
    assert_eq!(
        msgs,
        vec![
            Message::MouseWheel { dx: 0, dy: 1 },
            Message::MouseWheel { dx: 0, dy: -1 },
            Message::MouseWheel { dx: 1, dy: 0 },
        ]
    );
}

#[test]
fn buttons_map_to_canonical_ids() {
    let msgs = collect(&[
        key(KeyCode::BTN_LEFT.0, 1),
        key(KeyCode::BTN_LEFT.0, 0),
        key(KeyCode::BTN_RIGHT.0, 1),
        key(KeyCode::BTN_SIDE.0, 1),
        key(KeyCode::BTN_EXTRA.0, 1),
    ]);
    assert_eq!(
        msgs,
        vec![
            Message::MouseButton { button: buttons::LEFT, pressed: true },
            Message::MouseButton { button: buttons::LEFT, pressed: false },
            Message::MouseButton { button: buttons::RIGHT, pressed: true },
            Message::MouseButton { button: buttons::EXTRA_1, pressed: true },
            Message::MouseButton { button: buttons::EXTRA_2, pressed: true },
        ]
    );
}

#[test]
fn keys_travel_as_hid_usages_with_native_repeat() {
    let msgs = collect(&[key(KeyCode::KEY_A.0, 1), key(KeyCode::KEY_A.0, 2), key(KeyCode::KEY_A.0, 0)]);
    assert_eq!(
        msgs,
        vec![
            Message::Key { kind: KeyKind::Down, key: 0x04 },
            Message::Key { kind: KeyKind::Repeat, key: 0x04 },
            Message::Key { kind: KeyKind::Up, key: 0x04 },
        ]
    );
}

#[test]
fn media_keys_travel_as_hid_usages() {
    // Play/Pause and volume are declared by the Ergodox consumer
    // node and must reach the client as canonical usages.
    let msgs = collect(&[
        key(KeyCode::KEY_PLAYPAUSE.0, 1),
        key(KeyCode::KEY_PLAYPAUSE.0, 0),
        key(KeyCode::KEY_NEXTSONG.0, 1),
        key(KeyCode::KEY_VOLUMEUP.0, 1),
    ]);
    assert_eq!(
        msgs,
        vec![
            Message::Key { kind: KeyKind::Down, key: 0xcd },
            Message::Key { kind: KeyKind::Up, key: 0xcd },
            Message::Key { kind: KeyKind::Down, key: 0xb5 },
            Message::Key { kind: KeyKind::Down, key: 0xe9 },
        ]
    );
}

#[test]
fn repeats_and_releases_of_foreign_presses_are_suppressed() {
    // A key physically held when the cursor crossed onto the client
    // was pressed on the *other* capture path: its kernel repeats and
    // release must not replay a press the client never saw.
    let msgs = collect(&[
        key(KeyCode::KEY_A.0, 2), // repeat, no down seen
        key(KeyCode::KEY_A.0, 0), // release, no down seen
        key(KeyCode::KEY_A.0, 1), // a real press: from here on it is ours
        key(KeyCode::KEY_A.0, 2),
        key(KeyCode::KEY_A.0, 0),
    ]);
    assert_eq!(
        msgs,
        vec![
            Message::Key { kind: KeyKind::Down, key: 0x04 },
            Message::Key { kind: KeyKind::Repeat, key: 0x04 },
            Message::Key { kind: KeyKind::Up, key: 0x04 },
        ]
    );
}

#[test]
fn foreign_button_release_is_suppressed() {
    // Drag started on the local screen, crossing to the client while
    // held: the client never saw the press, so the release must not
    // reach it either.
    let msgs = collect(&[
        key(KeyCode::BTN_LEFT.0, 0), // release without a press
        key(KeyCode::BTN_LEFT.0, 1), // a real click: forwards both
        key(KeyCode::BTN_LEFT.0, 0),
    ]);
    assert_eq!(
        msgs,
        vec![
            Message::MouseButton { button: buttons::LEFT, pressed: true },
            Message::MouseButton { button: buttons::LEFT, pressed: false },
        ]
    );
}

#[test]
fn scroll_lock_is_the_escape_not_a_key() {
    let msgs = collect(&[key(KeyCode::KEY_SCROLLLOCK.0, 1), key(KeyCode::KEY_SCROLLLOCK.0, 0)]);
    assert_eq!(msgs, vec![Message::Escape]);
}

#[test]
fn unknown_codes_are_ignored() {
    let msgs = collect(&[key(200, 1), rel(0x7f, 1), (0, 0, 0)]);
    assert_eq!(msgs, Vec::<Message>::new());
}

#[test]
fn classify_constants_are_stable() {
    // The classification and translation rely on these raw codes.
    assert_eq!(RelativeAxisCode::REL_X.0, 0x00);
    assert_eq!(KeyCode::KEY_A.0, 30);
    assert_eq!(KeyCode::KEY_SCROLLLOCK.0, 70);
    assert_eq!(KeyCode::KEY_PLAYPAUSE.0, 164);
    assert_eq!(ESCAPE_KEY_HID, 0x47);
}
