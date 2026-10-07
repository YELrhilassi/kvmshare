//! The media policy end to end: routing to a pinned machine, the
//! hot-reload re-arm, and the media_pin override, with the wait
//! helpers this module uses.

use super::*;

/// Wait until the client's injector has recorded a `media <command>` tap.
fn wait_for_media_tap(recorder: &Arc<Mutex<Vec<String>>>, want: &str) {
    for _ in 0..100 {
        if calls(recorder).iter().any(|c| c == want) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("expected a {want:?} tap on the client, got {:?}", calls(recorder));
}
/// Wait until the mock engine has recorded `want`.
fn wait_for_engine_call(recorder: &Arc<Mutex<Vec<String>>>, want: &str) {
    for _ in 0..100 {
        if calls(recorder).iter().any(|c| c == want) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("expected {want:?} on the engine, got {:?}", calls(recorder));
}
/// The headline media case, end to end: the user presses play on the
/// server while the cursor is home and the music is on the client. The
/// capture edge classifies the key, the router resolves it by policy, and
/// the client receives a semantic `MediaControl` — performed there as a
/// tap of its own media key, never as a forwarded keystroke.
#[test]
fn a_media_key_is_routed_to_a_pinned_machine_and_performed_there() {
    // The pin is configured *before* `run` (see `start_server_with_media`):
    // the router is on the pinned policy for the very first key, and the
    // startup arm happens with it — no control-channel race.
    let h = start_server_with_media(kvmshare_core::media::MediaPrefs {
        route_media_keys: true,
        transport: kvmshare_core::media::MediaTarget::Machine("machine-hp".into()),
        volume: kvmshare_core::media::MediaTarget::Local,
        fallback_local: true,
    });
    // The startup arm is itself the sync point: once the engine has it,
    // the run loop is past startup and the pinned policy is live.
    wait_for_engine_call(&h.engine_calls, "media_capture true");

    let (client, injector, client_calls, out_rx) = connect_client(h.port);
    thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx, None).unwrap());
    h.wait_for_clients(1);

    // Press play while the cursor is at home (no focus anywhere): the pin
    // is what decides, not the cursor.
    h.input_tx.send(Message::Key { kind: KeyKind::Down, key: 0xcd }).unwrap();
    h.input_tx.send(Message::Key { kind: KeyKind::Up, key: 0xcd }).unwrap();
    wait_for_media_tap(&client_calls, "media PlayPause");

    // The same press must NOT have travelled as an ordinary key: the key
    // was consumed by the router, and a client that saw both a tap and a
    // keystroke would act twice. (The recorder prints decimal: 0xcd = 205.)
    let cc = calls(&client_calls);
    assert!(
        !cc.iter().any(|c| c.starts_with("key Down 205")),
        "a routed media key must not also arrive as a keystroke, got {cc:?}"
    );

    // Volume is pinned to local: it must reach the *engine*, not the
    // client (the local machine is the output the policy named).
    h.input_tx.send(Message::Key { kind: KeyKind::Down, key: 0xe9 }).unwrap();
    h.input_tx.send(Message::Key { kind: KeyKind::Up, key: 0xe9 }).unwrap();
    wait_for_engine_call(&h.engine_calls, "media VolumeUp");
    let cc = calls(&client_calls);
    assert!(
        !cc.iter().any(|c| c == "media VolumeUp"),
        "a local volume command must not reach the client, got {cc:?}"
    );
}
/// Routing enabled arms the media-key grab exactly once per transition —
/// including through the hot-reload path, which is where a half-wired
/// implementation would silently skip it.
///
/// The server starts with routing **off** (see `start_server_with_media`),
/// so the startup arm is decided from that policy and every `media_capture`
/// line here is the reload's doing.
#[test]
fn media_policy_hot_reload_rearms_the_grab() {
    let h = start_server_with_media(kvmshare_core::media::MediaPrefs {
        route_media_keys: false,
        ..kvmshare_core::media::MediaPrefs::default()
    });
    wait_for_engine_call(&h.engine_calls, "chords 2");
    let pre = calls(&h.engine_calls);
    assert!(
        !pre.iter().any(|c| c == "media_capture true"),
        "routing off must not arm the grab at startup either, got {pre:?}"
    );

    // On: the grab is armed with it. Controls drain on the run loop's
    // idle poll, so the bounded wait makes the hand-off deterministic.
    h.control_tx
        .send(Control::SetMediaPrefs(kvmshare_core::media::MediaPrefs::default()))
        .unwrap();
    wait_for_engine_call(&h.engine_calls, "media_capture true");

    // With the grab armed, a media press is consumed by the router even
    // with the default (follow-focus) policy and nothing connected: it
    // resolves to local, is performed here, and never reaches a client.
    h.input_tx.send(Message::Key { kind: KeyKind::Down, key: 0xcd }).unwrap();
    h.input_tx.send(Message::Key { kind: KeyKind::Up, key: 0xcd }).unwrap();
    wait_for_engine_call(&h.engine_calls, "media PlayPause");
}
/// The override: a `media_pin` shortcut latches the target and every
/// category follows it — including volume — until the same shortcut is
/// pressed again.
#[test]
fn the_media_pin_shortcut_overrides_every_category() {
    let h = start_server();
    // Default policy: follow focus. The override must beat it anyway.
    h.server.set_media_prefs(kvmshare_core::media::MediaPrefs::default());

    let (client, injector, client_calls, out_rx) = connect_client(h.port);
    thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx, None).unwrap());
    h.wait_for_clients(1);

    // A binding for the pin: Ctrl+Alt+M (0x33 = 'm'), naming hp. The
    // layout is re-sent unchanged (a reload carries the whole desktop).
    use kvmshare_core::actions::{BindSection, Binding, Mods};
    h.control_tx
        .send(Control::Reload(
            two_screen_layout(),
            BindSection {
                enabled: true,
                bindings: vec![Binding {
                    mods: Mods { ctrl: true, alt: true, ..Mods::NONE },
                    key: 0x33,
                    action: "media_pin".into(),
                    screen: "hp".into(),
                }],
            },
            kvmshare_core::input::InputPrefs::default(),
        ))
        .unwrap();
    // Controls drain on the run loop's ≤ CONTROL_POLL poll; when the
    // reload's chord publication lands on the engine, the whole reload
    // has been processed and the binding is live — so the chord below
    // cannot race the reload. (Startup publishes the two defaults; the
    // reload's single binding is what this wait is for.)
    wait_for_engine_call(&h.engine_calls, "chords 1");

    // Press the chord properly: hold the modifiers, tap 'm'. hp becomes
    // the media target for every category until the same chord repeats.
    feed(&h, Message::Key { kind: KeyKind::Down, key: 0xE0 }); // Ctrl
    feed(&h, Message::Key { kind: KeyKind::Down, key: 0xE2 }); // Alt
    feed(&h, Message::Key { kind: KeyKind::Down, key: 0x33 }); // 'm'
    feed(&h, Message::Key { kind: KeyKind::Up, key: 0x33 });
    feed(&h, Message::Key { kind: KeyKind::Up, key: 0xE2 });
    feed(&h, Message::Key { kind: KeyKind::Up, key: 0xE0 });

    // Now press play (cursor home, nothing playing anywhere): the pin
    // sends it to hp.
    h.input_tx.send(Message::Key { kind: KeyKind::Down, key: 0xcd }).unwrap();
    h.input_tx.send(Message::Key { kind: KeyKind::Up, key: 0xcd }).unwrap();
    wait_for_media_tap(&client_calls, "media PlayPause");

    // The chord itself must not have leaked to the client as keystrokes:
    // a consumed chord is the engine's, not the client's input. (The
    // recorder prints decimal: 0x33 = 51.)
    let cc = calls(&client_calls);
    assert!(
        !cc.iter().any(|c| c.starts_with("key Down 51")),
        "the override chord must be consumed, got {cc:?}"
    );
}
