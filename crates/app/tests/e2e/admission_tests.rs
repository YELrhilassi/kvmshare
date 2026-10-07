//! Who may connect: dynamic admission, the allowlist, trust by full or
//! short machine id, revocation — at the handshake and hot.

use super::*;

#[test]
fn unknown_client_is_admitted_dynamically() {
    // A client the layout has never heard of is admitted on the spot
    // (placed right of the desktop) instead of being rejected — a fresh
    // pair of machines works before either has been configured.
    let h = start_server(); // pc at origin, hp configured to the left
    let info = screen_info();
    let client = Client::connect(&format!("127.0.0.1:{}", h.port), "not-in-layout", "machine-new", info).unwrap();
    // pc=0 and hp=1 are taken; the newcomer gets the next free id and
    // the server registered it.
    assert_eq!(client.own_id(), 2);
    h.wait_for_clients(1);
    assert_eq!(h.server.client_count(), 1);
}
/// The allowlist policy: a name absent from the layout is refused unless
/// its machine id is trusted. Trusted ids are admitted dynamically.
#[test]
fn allowlist_refuses_unknown_and_admits_trusted() {
    let session = Session::new(two_screen_layout(), 0);
    let (_control_tx, control_rx) = mpsc::channel::<Control>();
    let policy = Policy {
        allowlist: true,
        local_only: false, // localhost must pass the network check
        trusted_ids: vec!["machine-trusted".into()],
        ..Policy::default()
    };
    let server = Arc::new(
        Server::with_options(
            session,
            0,
            Options {
                control: Some(control_rx),
                policy,
                events: None,
                server_id: "server-pc".into(),
                audio: None,
            },
        )
        .unwrap(),
    );
    let port = server.local_addr().unwrap().port();
    let (_input_tx, input_rx) = mpsc::channel::<Message>();
    let engine = Arc::new(Mutex::new(Box::new(MockEngine { calls: Arc::new(Mutex::new(Vec::new())) }) as Box<dyn Engine>));
    let clipboard: kvmshare_core::server::ServerClipboard =
        Arc::new(Mutex::new(Box::new(NoClipboard) as Box<dyn Clipboard>));
    thread::spawn({
        let server = server.clone();
        let engine = engine.clone();
        let clipboard = clipboard.clone();
        move || {
            server
                .run(input_rx, engine, clipboard, Arc::new(kvmshare_core::server::Liveness::default()))
                .unwrap()
        }
    });

    let info = screen_info();

    // Untrusted and unnamed: refused with a typed NOT_ALLOWED refusal.
    let err = Client::connect(&format!("127.0.0.1:{port}"), "stranger", "machine-stranger", info.clone())
        .unwrap_err();
    let refusal = kvmshare_core::client::refusal_from(&err)
        .unwrap_or_else(|| panic!("unknown untrusted client should be refused, got: {err}"));
    assert_eq!(
        refusal.code,
        kvmshare_protocol::id::errors::NOT_ALLOWED,
        "a policy refusal must carry its code: {refusal}"
    );
    // Wait a beat so the refused connection is fully torn down; the
    // server never registered it.
    thread::sleep(Duration::from_millis(50));
    assert_eq!(server.client_count(), 0, "refused client must not be registered");

    // Trusted id (even without a layout name): admitted dynamically.
    let client = Client::connect(&format!("127.0.0.1:{port}"), "trusted-peer", "machine-trusted", info)
        .unwrap();
    assert_eq!(client.own_id(), 2);
    for _ in 0..100 {
        if server.client_count() >= 1 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.client_count(), 1, "trusted client should be admitted");
}
/// A trusted id may be the 8-char short form (prefix match): the user
/// pastes the short id shown in the GUI, and the server still admits the
/// machine whose full id starts with it.
#[test]
fn allowlist_admits_by_short_id_prefix() {
    let session = Session::new(two_screen_layout(), 0);
    let (_control_tx, control_rx) = mpsc::channel::<Control>();
    let full = "70b97d38631dda4b8f6ef627d753022d";
    let policy = Policy {
        allowlist: true,
        local_only: false,
        trusted_ids: vec![full[..8].to_string()], // short form
        ..Policy::default()
    };
    let server = Arc::new(
        Server::with_options(
            session,
            0,
            Options {
                control: Some(control_rx),
                policy,
                events: None,
                server_id: "server-pc".into(),
                audio: None,
            },
        )
        .unwrap(),
    );
    let port = server.local_addr().unwrap().port();
    let (_input_tx, input_rx) = mpsc::channel::<Message>();
    let engine = Arc::new(Mutex::new(Box::new(MockEngine { calls: Arc::new(Mutex::new(Vec::new())) }) as Box<dyn Engine>));
    let clipboard: kvmshare_core::server::ServerClipboard =
        Arc::new(Mutex::new(Box::new(NoClipboard) as Box<dyn Clipboard>));
    thread::spawn({
        let server = server.clone();
        let engine = engine.clone();
        let clipboard = clipboard.clone();
        move || {
            server
                .run(input_rx, engine, clipboard, Arc::new(kvmshare_core::server::Liveness::default()))
                .unwrap()
        }
    });

    // Full id starts with the trusted 8-char prefix → admitted even
    // though the name is not in the layout.
    let info = screen_info();
    let client = Client::connect(&format!("127.0.0.1:{port}"), "short-id-peer", full, info).unwrap();
    assert_eq!(client.own_id(), 2);
    for _ in 0..100 {
        if server.client_count() >= 1 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.client_count(), 1, "short-id-trusted client should be admitted");

    // A DIFFERENT id sharing only 4 chars of the prefix must NOT be
    // admitted (the 8-char short form is specific enough).
    let err = Client::connect(&format!("127.0.0.1:{port}"), "wrong-peer", "70b97dXXffffffffffffffffffffffffff", info)
        .unwrap_err();
    let refusal = kvmshare_core::client::refusal_from(&err)
        .unwrap_or_else(|| panic!("id not matching the short prefix should be refused, got: {err}"));
    assert_eq!(refusal.code, kvmshare_protocol::id::errors::NOT_ALLOWED);
}
/// A revoked machine id is refused **even though its name is in the
/// layout** — revocation is a hard deny that outranks the allowlist and a
/// pinned screen. (Previously "revoke" only removed the id from the
/// trusted list, so a client whose screen was pinned — i.e. every machine
/// after its first connect — kept being admitted.)
#[test]
fn revoked_machine_is_refused_even_when_named_in_the_layout() {
    let session = Session::new(two_screen_layout(), 0);
    let (_control_tx, control_rx) = mpsc::channel::<Control>();
    let policy = Policy {
        allowlist: true,
        local_only: false, // localhost must pass the network check
        revoked_ids: vec!["machine-hp".into()],
        ..Policy::default()
    };
    let server = Arc::new(
        Server::with_options(
            session,
            0,
            Options {
                control: Some(control_rx),
                policy,
                events: None,
                server_id: "server-pc".into(),
                audio: None,
            },
        )
        .unwrap(),
    );
    let port = server.local_addr().unwrap().port();
    let (input_tx, input_rx) = mpsc::channel::<Message>();
    drop(input_tx);
    let engine = Arc::new(Mutex::new(Box::new(MockEngine { calls: Arc::new(Mutex::new(Vec::new())) }) as Box<dyn Engine>));
    let clipboard: kvmshare_core::server::ServerClipboard =
        Arc::new(Mutex::new(Box::new(NoClipboard) as Box<dyn Clipboard>));
    thread::spawn({
        let server = server.clone();
        let engine = engine.clone();
        let clipboard = clipboard.clone();
        move || {
            server
                .run(input_rx, engine, clipboard, Arc::new(kvmshare_core::server::Liveness::default()))
                .unwrap()
        }
    });

    let info = screen_info();
    // "hp" IS the layout's screen 1 — named, matching, and still refused.
    let err = Client::connect(&format!("127.0.0.1:{port}"), "hp", "machine-hp", info)
        .unwrap_err();
    let refusal = kvmshare_core::client::refusal_from(&err)
        .unwrap_or_else(|| panic!("revoked client must be refused, got: {err}"));
    assert_eq!(
        refusal.code,
        kvmshare_protocol::id::errors::REVOKED,
        "the refusal must carry the revoked code: {refusal}"
    );
    assert!(refusal.text.contains("revoked"), "the refusal must say why, got: {refusal}");
    thread::sleep(Duration::from_millis(50));
    assert_eq!(server.client_count(), 0, "a revoked client must never be registered");
}
/// A hot policy change that revokes a *connected* machine drops it
/// immediately: "revoke" must end the live session, not only the next
/// connect.
#[test]
fn hot_revoke_disconnects_a_connected_client() {
    let h = start_server();
    let (client, injector, _client_calls, out_rx) = connect_client(h.port);
    thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx, None).unwrap());
    h.wait_for_clients(1);

    // Revoke the machine that is connected right now.
    h.control_tx
        .send(Control::SetPolicy(Policy { revoked_ids: vec!["machine-hp".into()], ..Policy::default() }))
        .unwrap();
    for _ in 0..100 {
        if h.server.client_count() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(h.server.client_count(), 0, "the revoked machine must be dropped at once");
}
/// Un-revoking re-admits the machine fresh: the revoke removed its
/// dynamically admitted screen, so when it returns it gets a new slot
/// instead of silently re-inheriting a stale position.
#[test]
fn unrevoked_machine_is_readmitted_fresh() {
    let session = Session::new(two_screen_layout(), 0);
    let (control_tx, control_rx) = mpsc::channel::<Control>();
    // Start with the machine revoked but its screen pinned ("hp" IS in
    // the layout): the pinned copy is what the operator configured, and
    // it must survive a revoke/un-revoke cycle unchanged.
    let policy = Policy {
        allowlist: true,
        local_only: false,
        revoked_ids: vec!["machine-hp".into()],
        ..Policy::default()
    };
    let server = Arc::new(
        Server::with_options(
            session,
            0,
            Options {
                control: Some(control_rx),
                policy,
                events: None,
                server_id: "server-pc".into(),
                audio: None,
            },
        )
        .unwrap(),
    );
    let port = server.local_addr().unwrap().port();
    // The input channel must stay open: the main loop exits when it
    // closes, and a closed main loop never drains the control channel.
    let (_input_tx, input_rx) = mpsc::channel::<Message>();
    let engine = Arc::new(Mutex::new(Box::new(MockEngine { calls: Arc::new(Mutex::new(Vec::new())) }) as Box<dyn Engine>));
    let clipboard: kvmshare_core::server::ServerClipboard =
        Arc::new(Mutex::new(Box::new(NoClipboard) as Box<dyn Clipboard>));
    thread::spawn({
        let server = server.clone();
        let engine = engine.clone();
        let clipboard = clipboard.clone();
        move || {
            server
                .run(input_rx, engine, clipboard, Arc::new(kvmshare_core::server::Liveness::default()))
                .unwrap()
        }
    });

    let info = screen_info();
    // Revoked: refused.
    assert!(Client::connect(&format!("127.0.0.1:{port}"), "hp", "machine-hp", info.clone()).is_err());
    thread::sleep(Duration::from_millis(50));
    assert_eq!(server.client_count(), 0);

    // Un-revoked: admitted again (its screen was pinned in the config,
    // so it lands back in the operator's slot, not a dynamic one). The
    // policy is swapped through the control channel — the same hot path
    // the GUI's trust toggle drives.
    control_tx
        .send(Control::SetPolicy(Policy { allowlist: true, local_only: false, ..Policy::default() }))
        .unwrap();
    thread::sleep(Duration::from_millis(150)); // let the main loop drain it
    let client = Client::connect(&format!("127.0.0.1:{port}"), "hp", "machine-hp", info).unwrap();
    assert_eq!(client.own_id(), 1, "the pinned screen slot is reused");
    for _ in 0..100 {
        if server.client_count() >= 1 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.client_count(), 1, "an un-revoked machine is admitted again");
}
/// The operator's Disconnect button, then reconnect: the session ends
/// but the *layout screen stays*, so the client's next connect is
/// admitted by the same rule as the first one. This is the regression
/// behind "after clicking disconnect I can't reconnect" — the disconnect
/// used to delete the client's screen from the running session, and the
/// allowlist then refused every reconnect (NOT_ALLOWED) while the GUI
/// showed a forever-"connecting…".
#[test]
fn operator_disconnect_then_reconnect_is_admitted() {
    let session = Session::new(two_screen_layout(), 0);
    let (control_tx, control_rx) = mpsc::channel::<Control>();
    // Allowlist on, trusted list empty: "hp" is admitted only because
    // its name is pinned in the layout — exactly the production setup
    // this regression came from.
    let policy = Policy { allowlist: true, local_only: false, ..Policy::default() };
    let server = Arc::new(
        Server::with_options(
            session,
            0,
            Options {
                control: Some(control_rx),
                policy,
                events: None,
                server_id: "server-pc".into(),
                audio: None,
            },
        )
        .unwrap(),
    );
    let port = server.local_addr().unwrap().port();
    let (_input_tx, input_rx) = mpsc::channel::<Message>();
    let engine = Arc::new(Mutex::new(Box::new(MockEngine { calls: Arc::new(Mutex::new(Vec::new())) }) as Box<dyn Engine>));
    let clipboard: kvmshare_core::server::ServerClipboard =
        Arc::new(Mutex::new(Box::new(NoClipboard) as Box<dyn Clipboard>));
    thread::spawn({
        let server = server.clone();
        move || {
            server
                .run(input_rx, engine, clipboard, Arc::new(kvmshare_core::server::Liveness::default()))
                .unwrap()
        }
    });

    let info = screen_info();

    // First connect: admitted via the pinned layout screen.
    let client = Client::connect(&format!("127.0.0.1:{port}"), "hp", "machine-hp", info.clone()).unwrap();
    assert_eq!(client.own_id(), 1);
    drop(client);

    // The operator disconnects the machine (the GUI's Disconnect). The
    // main loop drains the control channel on an idle poll.
    control_tx
        .send(Control::ClientCommand { name: "hp".into(), command: kvmshare_protocol::id::control::DISCONNECT })
        .unwrap();
    // The first client's reader thread also needs a moment to observe
    // its ended session and tear down.
    for _ in 0..100 {
        if server.client_count() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(server.client_count(), 0, "the operator disconnect must end the session");
    thread::sleep(Duration::from_millis(150)); // let the main loop finish teardown

    // Reconnect: must be admitted again — the screen survived the
    // disconnect. Before the fix this failed with NOT_ALLOWED.
    let client = Client::connect(&format!("127.0.0.1:{port}"), "hp", "machine-hp", info).unwrap();
    assert_eq!(client.own_id(), 1, "the disconnect must not have removed the layout screen");
    for _ in 0..100 {
        if server.client_count() >= 1 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.client_count(), 1, "the reconnect must be admitted");
}
