use super::*;

fn engine() -> ActionEngine {
    ActionEngine::new(BindSection::default())
}

#[test]
fn scroll_lock_tap_fires_once_on_press() {
    let mut e = engine();
    let fired = e.key_down(hid::SCROLL_LOCK, Mods::NONE, true);
    assert!(fired.is_some(), "the chord fires on press");
    // The release is consumed (belongs to the engine), no second fire.
    assert_eq!(e.key_up(hid::SCROLL_LOCK), Some(hid::SCROLL_LOCK));
    // Repeatable: press again, fires again.
    assert!(e.key_down(hid::SCROLL_LOCK, Mods::NONE, true).is_some());
    assert_eq!(e.key_up(hid::SCROLL_LOCK), Some(hid::SCROLL_LOCK));
}

#[test]
fn unbound_keys_pass_through() {
    let mut e = engine();
    assert!(e.key_down(0x04, Mods::NONE, true).is_none()); // 'a'
    assert!(e.key_up(0x04).is_none());
}

#[test]
fn chord_key_down_is_consumed_and_never_leaks() {
    let mut e = engine();
    // Scroll Lock down fires the chord (cycle) and is consumed.
    assert!(e.key_down(hid::SCROLL_LOCK, Mods::NONE, false).is_some());
    // Release belongs to the engine — the caller must not forward it.
    assert!(e.key_up(hid::SCROLL_LOCK).is_some());
}

#[test]
fn second_non_modifier_key_abandons_the_chord() {
    let mut e = engine();
    e.key_down(hid::SCROLL_LOCK, Mods::NONE, true);
    // 'a' arrives mid-chord: the chord is abandoned, 'a' passes through.
    assert!(e.key_down(0x04, Mods::NONE, true).is_none());
    assert!(e.key_up(0x04).is_none());
    // Scroll Lock's release still belongs to the engine (its press was
    // consumed), so it can never leak to the client as a stuck key.
    assert_eq!(e.key_up(hid::SCROLL_LOCK), Some(hid::SCROLL_LOCK));
}

#[test]
fn modifier_press_passes_through() {
    let mut e = engine();
    assert!(e.key_down(hid::SHIFT_L, Mods::NONE, true).is_none());
    assert!(e.key_up(hid::SHIFT_L).is_none());
}

#[test]
fn disabled_engine_is_inert() {
    let mut e = engine();
    let mut cfg = BindSection::default();
    cfg.enabled = false;
    e.set_config(cfg);
    assert!(e.key_down(hid::SCROLL_LOCK, Mods::NONE, true).is_none());
    assert!(e.key_up(hid::SCROLL_LOCK).is_none());
}

#[test]
fn custom_switch_binding_fires_with_its_target() {
    let mut e = ActionEngine::new(BindSection {
        enabled: true,
        bindings: vec![Binding {
            mods: Mods { ctrl: true, ..Mods::NONE },
            key: 0x04, // 'a'
            action: "switch".into(),
            screen: "hp".into(),
        }],
    });
    let fired = e.key_down(0x04, Mods { ctrl: true, ..Mods::NONE }, true);
    assert_eq!(fired, Some(UserAction::SwitchToScreen { to: "hp".into() }));
}

#[test]
fn wrong_mods_do_not_fire() {
    let mut e = ActionEngine::new(BindSection {
        enabled: true,
        bindings: vec![Binding { mods: Mods { ctrl: true, ..Mods::NONE }, key: 0x04, action: "lock".into(), screen: String::new() }],
    });
    // Not a binding under these mods — but the key IS a chord key, so
    // the press is consumed and its release must be consumed too.
    assert!(e.key_down(0x04, Mods::NONE, true).is_none());
    assert_eq!(e.key_up(0x04), Some(0x04));
}

#[test]
fn go_home_only_means_something_away() {
    let mut e = ActionEngine::new(BindSection {
        enabled: true,
        bindings: vec![Binding { mods: Mods::NONE, key: hid::PAUSE, action: "home".into(), screen: String::new() }],
    });
    // At home: the binding consumes but resolves to nothing.
    assert!(e.key_down(hid::PAUSE, Mods::NONE, true).is_none());
    assert!(e.key_up(hid::PAUSE).is_some());
    // Away: it fires.
    assert!(e.key_down(hid::PAUSE, Mods::NONE, false).is_some());
    assert!(e.key_up(hid::PAUSE).is_some());
}

#[test]
fn default_section_serializes_round_trip() {
    let s = toml::to_string(&BindSection::default()).unwrap();
    let back: BindSection = toml::from_str(&s).unwrap();
    assert_eq!(back, BindSection::default());
}

#[test]
fn config_reload_drops_pending_chords() {
    let mut e = engine();
    e.key_down(hid::SCROLL_LOCK, Mods::NONE, true);
    e.set_config(BindSection { enabled: false, bindings: vec![] });
    // The press was consumed under the old config, so the release is
    // still the engine's — a config flip mid-chord can never strand a
    // key in the client's down state.
    assert_eq!(e.key_up(hid::SCROLL_LOCK), Some(hid::SCROLL_LOCK));
}
