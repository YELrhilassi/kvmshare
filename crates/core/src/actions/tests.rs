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
fn integral_float_key_deserializes() {
    // The GUI round-trips this file through JavaScript, where every
    // number is an f64: an old writer could emit `key = 71.0`. Strict
    // rejection dropped the whole [shortcuts] section, so bindings
    // silently never registered.
    let s = r#"
        enabled = true
        [[bindings]]
        mods = { ctrl = true }
        key = 71.0
        action = "switch"
        screen = "hp"
    "#;
    let back: BindSection = toml::from_str(s).unwrap();
    assert_eq!(back.bindings[0].key, 71);
    // A real fraction is corruption, not a writer quirk — still an error.
    assert!(toml::from_str::<BindSection>(
        r#"
        enabled = true
        [[bindings]]
        key = 71.5
        action = "cycle"
    "#
    )
    .is_err());
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

#[test]
fn default_chords_survive_sanitization() {
    // Scroll Lock and Pause are bare but app-meaningless: the defaults
    // must never be silently dropped by the safety gate.
    let d = BindSection::default();
    assert_eq!(d.clone().sanitized().bindings.len(), d.bindings.len());
    for b in &d.bindings {
        assert!(b.is_bindable(), "default binding {} must be bindable", b.key);
    }
}

#[test]
fn bare_tab_is_dropped_by_sanitization() {
    // The field report: a bare Tab binding ate Tab system-wide (and on
    // X11 passive-grabbed it), making the whole desktop feel dead while
    // "cycle" flapped on every auto-repeat. The engine refuses it.
    let cfg = BindSection {
        enabled: true,
        bindings: vec![Binding {
            mods: Mods::NONE,
            key: 0x2b, // Tab
            action: "cycle".into(),
            screen: String::new(),
        }],
    };
    let mut e = ActionEngine::new(cfg);
    // The surviving config binds nothing: Tab passes through untouched.
    assert!(e.key_down(0x2b, Mods::NONE, true).is_none());
    assert!(e.key_up(0x2b).is_none());
}

#[test]
fn bare_workhorse_keys_are_dropped_bare_modifier_keys_never_bind() {
    // Letters/digits/Enter without modifiers: dropped. A modifier usage
    // (0xE0) as the chord key: never a chord at all — also dropped.
    for key in [0x04, 0x1e, 0x28, 0x2c, 0xe0] {
        let cfg = BindSection {
            enabled: true,
            bindings: vec![Binding { mods: Mods::NONE, key, action: "cycle".into(), screen: String::new() }],
        };
        assert!(
            ActionEngine::new(cfg).key_down(key, Mods::NONE, true).is_none(),
            "bare key 0x{key:02x} must not swallow presses"
        );
    }
}

#[test]
fn hot_reload_applies_the_same_safety_gate() {
    let mut e = engine();
    // Start sane.
    e.set_config(BindSection {
        enabled: true,
        bindings: vec![Binding { mods: Mods { ctrl: true, ..Mods::NONE }, key: 0x2b, action: "cycle".into(), screen: String::new() }],
    });
    assert!(e.key_down(0x2b, Mods { ctrl: true, ..Mods::NONE }, true).is_some());
    // A mid-session edit that arms bare Tab must not take effect.
    e.set_config(BindSection {
        enabled: true,
        bindings: vec![Binding { mods: Mods::NONE, key: 0x2b, action: "cycle".into(), screen: String::new() }],
    });
    assert!(e.key_down(0x2b, Mods::NONE, true).is_none(), "hot reload must sanitize too");
}

#[test]
fn modified_chords_always_bind() {
    // The rule's positive case: any real key with a modifier is fine,
    // including workhorse keys the bare form would break.
    for key in [0x04, 0x2b, 0x2c, 0x28] {
        let cfg = BindSection {
            enabled: true,
            bindings: vec![Binding { mods: Mods { meta: true, ..Mods::NONE }, key, action: "cycle".into(), screen: String::new() }],
        };
        let mut e = ActionEngine::new(cfg);
        e.key_down(hid::META_L, Mods::NONE, true);
        assert!(e.key_down(key, Mods::NONE, true).is_some(), "modified key 0x{key:02x} must bind");
    }
}
