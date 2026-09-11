//! The session-level flows: crossing, beacons, reconnect, admission,
//! allowlist, hot reload, duplicate replacement.

use super::*;

#[test]
fn cursor_enters_moves_and_crosses_back_over_tcp() {
    let h = start_server();

    let (client, injector, client_calls, out_rx) = connect_client(h.port);
    thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx).unwrap());
    h.wait_for_clients(1);

    // -- Cross from pc onto hp (left screen). --
    // The real cursor parks at the shared edge (beacon), then an outward
    // push crosses. (Deltas alone never cross — see core::session.)
    feed(&h, Message::MouseMoveAbs { x: 0, y: SCREEN_H / 2 });
    feed(&h, Message::MouseMoveRel { dx: -5, dy: 0 });

    let cc = calls(&client_calls);
    assert!(cc.contains(&"enter".to_string()), "client should enter, got {cc:?}");
    // Entry point is hp's right edge inset past the seam
    // (1919 - ENTRY_INSET, 540); the server also sends an absolute move
    // for the entry position. (The inset stops the seam-jitter bounce: an
    // entry exactly on the wall makes the first beacon a park, which
    // re-crosses.)
    assert!(
        cc.iter().any(|c| c == &format!("move {},{}", SCREEN_W - 25, SCREEN_H / 2)),
        "client should move to entry point, got {cc:?}"
    );

    let ec = calls(&h.engine_calls);
    assert!(ec.iter().any(|c| c == "cursor false"), "server should hide its cursor, got {ec:?}");
    // The hidden cursor must stay exactly where it crossed — the server
    // must NOT warp it (a warp would sweep hover/enter effects across
    // the local desktop, and a visible one would dash the cursor to the
    // screen center on every crossing).
    assert!(
        !ec.iter().any(|c| c.starts_with("warp ")),
        "server must not warp its cursor when switching away, got {ec:?}"
    );

    // -- Roam around on hp: forwarded as *relative* motion. --
    // The client's OS applies its own pointer transform to relative
    // input, so the shared cursor feels native there. The server must
    // never send absolute positions in the motion stream (the hidden
    // local cursor never moves while we are away).
    feed(&h, Message::MouseMoveRel { dx: -100, dy: 0 });
    let cc = calls(&client_calls);
    // Under the closed-loop model the -100 frame is fed forward
    // immediately (half a frame) and the damped corrections deliver the
    // rest against the read-back cursor — what must hold is that the
    // command trajectory is honored exactly: the recorded relative
    // stream totals -100 px on x, nothing on y.
    let (rel_x, rel_y): (i64, i64) = cc
        .iter()
        .filter_map(|c| c.strip_prefix("rel "))
        .map(|r| {
            let (x, y) = r.split_once(',').unwrap();
            (x.parse::<i64>().unwrap(), y.parse::<i64>().unwrap())
        })
        .fold((0, 0), |(ax, ay), (x, y)| (ax + x, ay + y));
    assert_eq!(rel_x, -100, "motion must deliver the full -100 px command, got {cc:?}");
    assert_eq!(rel_y, 0, "no motion outside the command axis, got {cc:?}");
    assert!(
        cc.iter().all(|c| !c.starts_with("move ") || c == &format!("move {},{}", SCREEN_W - 25, SCREEN_H / 2)),
        "only the entry move may be absolute, got {cc:?}"
    );

    // -- Buttons and keys forward while on the client. --
    feed(&h, Message::MouseButton { button: 0, pressed: true });
    let cc = calls(&client_calls);
    assert!(cc.contains(&"button 0 true".to_string()), "button should forward, got {cc:?}");

    feed(&h, Message::Key { kind: KeyKind::Down, key: 0x04 }); // canonical HID usage: 'a'
    let cc = calls(&client_calls);
    assert!(cc.contains(&"key Down 4".to_string()), "key should forward, got {cc:?}");

    // -- Cross back to pc. --
    // The client's real cursor must be pinned on the shared edge (its
    // right edge) for a crossing; first push moves it there, then the
    // next outward push (a frame later, as in real use) crosses. The
    // entry point sits inset inside hp, so the roam must cover that
    // ground before the cursor can park on the wall.
    feed(&h, Message::MouseMoveRel { dx: 250, dy: 0 }); // roam back to the right wall
    feed(&h, Message::MouseMoveRel { dx: 10, dy: 0 }); // keep pushing: cross home

    let cc = calls(&client_calls);
    assert!(cc.contains(&"leave".to_string()), "client should leave, got {cc:?}");
    let ec = calls(&h.engine_calls);
    assert!(ec.iter().any(|c| c == "cursor true"), "server should restore its cursor, got {ec:?}");
}

/// A raw peer that speaks just enough of the protocol to register as a
/// client, flood the server's UDP beacon stream with `n` cursor
/// beacons, then vanish. Used to simulate a previous client session whose
/// UDP sequence counter reached `n` before it disconnected — the
/// reconnect must not inherit that state (stale beacons must never
/// deafen a fresh peer).
fn raw_beacon_client(port: u16, n: u32) {
    let mut tcp = Transport::new(TcpStream::connect(("127.0.0.1", port)).unwrap()).unwrap();
    let info = screen_info();
    tcp.send(&Message::Hello { version: VERSION, id: "machine-hp".into(), name: "hp".into(), info }).unwrap();
    let id = match tcp.recv().unwrap() {
        RecvResult::Msg(Message::Welcome { own_screen_id, .. }) => own_screen_id,
        other => panic!("expected welcome, got {other:?}"),
    };
    // A UDP stream to the same server port, like the real client's.
    let udp_sock = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
    udp_sock.connect(("127.0.0.1", port)).unwrap();
    for seq in 1..=n {
        udp_sock
            .send(&udp::pack(id, seq, &Message::CursorPos { x: SCREEN_W / 2, y: SCREEN_H / 2 }))
            .unwrap();
    }
    // Give the server a moment to drain the datagrams, then vanish.
    thread::sleep(Duration::from_millis(50));
}

#[test]
fn client_reconnect_is_not_deafened_by_stale_udp_sequences() {
    let h = start_server();

    // A previous "hp" session ran long enough that the server's UDP
    // sequence tracker for its screen id climbed high, then it
    // disconnected. (The tracker must be cleared on disconnect — a fresh
    // session starts its own sequence at 1.)
    raw_beacon_client(h.port, 500);
    for _ in 0..100 {
        if h.server.client_count() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(h.server.client_count(), 0, "old session should have disconnected");

    // The real client reconnects (same screen id) and must work normally:
    // beacons from sequence 1 on are fresh and drive the crossing back.
    let (client, injector, client_calls, out_rx) = connect_client(h.port);
    thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx).unwrap());
    h.wait_for_clients(1);

    // Cross onto hp.
    feed(&h, Message::MouseMoveAbs { x: 0, y: SCREEN_H / 2 });
    feed(&h, Message::MouseMoveRel { dx: -5, dy: 0 });
    let cc = calls(&client_calls);
    assert!(cc.contains(&"enter".to_string()), "client should enter, got {cc:?}");
    assert!(calls(&h.engine_calls).iter().any(|c| c == "cursor false"));

    // Cross back: push the real cursor across the entry inset to the
    // shared edge; its (fresh) beacons arm it and the outward push
    // fires the crossing.
    feed(&h, Message::MouseMoveRel { dx: 100, dy: 0 });

    let mut cc = calls(&client_calls);
    for _ in 0..50 {
        if cc.contains(&"leave".to_string()) {
            break;
        }
        thread::sleep(Duration::from_millis(20));
        cc = calls(&client_calls);
    }
    assert!(cc.contains(&"leave".to_string()), "reconnected client should cross back, got {cc:?}");
    assert!(
        calls(&h.engine_calls).iter().any(|c| c == "cursor true"),
        "server cursor should be restored after the return crossing"
    );
}

#[test]
fn crossing_after_idle_is_not_dropped_by_the_beacon_watchdog() {
    let h = start_server();

    // A raw peer that registers (TCP + UDP) like a real client but only
    // starts beaconing well after the server activates it — the shape of
    // a real LAN client, whose first beacon lands tens of milliseconds
    // after the crossing, never within the watchdog's ~1 ms first check.
    // (On localhost a full client's immediate beacon can beat the check,
    // hiding the race this guards against.)
    let mut tcp = Transport::new(TcpStream::connect(("127.0.0.1", h.port)).unwrap()).unwrap();
    let info = screen_info();
    tcp.send(&Message::Hello { version: VERSION, id: "machine-hp".into(), name: "hp".into(), info }).unwrap();
    let id = match tcp.recv().unwrap() {
        RecvResult::Msg(Message::Welcome { own_screen_id, .. }) => own_screen_id,
        other => panic!("expected welcome, got {other:?}"),
    };
    assert_eq!(id, 1);
    let udp_sock = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
    udp_sock.connect(("127.0.0.1", h.port)).unwrap();
    udp_sock.send(&udp::pack(id, 0, &Message::KeepAlive)).unwrap(); // UDP registration
    h.wait_for_clients(1);

    // Idle on the server side past the active-beacon timeout (1.5 s).
    // The client's beacons only flow while it is active, so its UDP
    // "last heard" goes stale during this stretch — exactly the state
    // that used to make the beacon watchdog drop the client the instant
    // it was activated again.
    thread::sleep(Duration::from_millis(2000));

    // Cross onto hp.
    feed(&h, Message::MouseMoveAbs { x: 0, y: SCREEN_H / 2 });
    feed(&h, Message::MouseMoveRel { dx: -5, dy: 0 });
    assert!(
        calls(&h.engine_calls).iter().any(|c| c == "cursor false"),
        "server should hide its cursor (crossed onto hp)"
    );

    // The client stays silent for a beat (real network: its first beacon
    // takes a few ms to come back). Without the activation reset, the
    // watchdog drops it within ~1 ms — its registration timestamp is
    // already >1.5 s stale — and the local cursor is restored. With the
    // reset it is granted the full watchdog window.
    thread::sleep(Duration::from_millis(100));
    assert!(
        !calls(&h.engine_calls).iter().any(|c| c == "cursor true"),
        "client must survive the activation gap without beaconing"
    );

    // Now it beacons normally and must stay alive: a wedge drop would
    // restore the local cursor.
    for _ in 0..10 {
        udp_sock.send(&udp::pack(id, 1, &Message::CursorPos { x: SCREEN_W - 25, y: SCREEN_H / 2 })).unwrap();
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !calls(&h.engine_calls).iter().any(|c| c == "cursor true"),
        "client must stay alive while beaconing"
    );
}

#[test]
fn client_disconnect_returns_cursor_home() {
    let h = start_server();

    // Connect but never run the loop: the socket stays open.
    let (client, _injector, _client_calls, _out_rx) = connect_client(h.port);
    h.wait_for_clients(1);
    feed(&h, Message::MouseMoveAbs { x: 0, y: SCREEN_H / 2 }); // beacon at the shared edge
    feed(&h, Message::MouseMoveRel { dx: -5, dy: 0 }); // outward push: on hp now

    // Dropping the client closes the TCP connection; the server notices
    // and returns the cursor to the local screen.
    drop(client);
    thread::sleep(Duration::from_millis(150));

    let ec = calls(&h.engine_calls);
    assert!(ec.iter().any(|c| c == "cursor true"), "cursor should be restored after disconnect, got {ec:?}");
}

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
            Options { control: Some(control_rx), policy, events: None, server_id: "server-pc".into() },
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

    // Untrusted and unnamed: refused with a NOT_ALLOWED error.
    let err = Client::connect(&format!("127.0.0.1:{port}"), "stranger", "machine-stranger", info.clone())
        .unwrap_err();
    assert!(
        err.to_string().contains("rejected"),
        "unknown untrusted client should be refused, got: {err}"
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
            Options { control: Some(control_rx), policy, events: None, server_id: "server-pc".into() },
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
    assert!(
        err.to_string().contains("rejected"),
        "id not matching the short prefix should be refused, got: {err}"
    );
}

#[test]
fn config_hot_reload_returns_cursor_home_and_broadcasts() {
    let h = start_server();

    let (client, injector, client_calls, out_rx) = connect_client(h.port);
    thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx).unwrap());
    h.wait_for_clients(1);

    // Move onto hp, then reload a layout that no longer has hp: the
    // cursor must come home and the client must be told to leave.
    feed(&h, Message::MouseMoveAbs { x: 0, y: SCREEN_H / 2 }); // beacon at the shared edge
    feed(&h, Message::MouseMoveRel { dx: -5, dy: 0 }); // outward push
    assert!(calls(&h.engine_calls).iter().any(|c| c == "cursor false"));

    let new_layout = Layout::new(vec![Screen {
        id: 0,
        name: "pc".into(),
        rect: Rect { x: 0, y: 0, w: 1920, h: 1080 },
    }]);
    h.control_tx
        .send(Control::Reload(new_layout, Default::default(), Default::default()))
        .unwrap();
    thread::sleep(Duration::from_millis(200));

    let ec = calls(&h.engine_calls);
    assert!(ec.iter().any(|c| c == "cursor true"), "cursor should return home, got {ec:?}");
    assert!(ec.iter().any(|c| c == "warp 960,540"), "cursor should warp to local center, got {ec:?}");

    let cc = calls(&client_calls);
    assert!(cc.contains(&"leave".to_string()), "client should be told to leave, got {cc:?}");

    // hp was dropped from the layout, so it must be unregistered.
    assert_eq!(h.server.client_count(), 0, "stale client should be dropped after reload");
}

#[test]
fn duplicate_connection_replaces_stale_one_without_losing_the_live_client() {
    // The shape of a reconnect race: a machine reconnects before the
    // server noticed the old socket's death (or a second instance
    // bypassed the role lock). Both connections carry the same name, so
    // both get the same screen id. The fresh connection must become the
    // registered one, and the stale connection's eventual teardown must
    // NOT unregister the live client — otherwise the server forgets a
    // connected machine: the GUI flips it to "nearby", crossing stops
    // routing, while the client keeps its (working) session.
    let h = start_server();

    // First connection (the one that will turn stale). It must survive
    // long enough to be replaced, so the client object stays alive on a
    // thread; a channel lets the test close it (drop) at the right
    // moment. Its session is never run — the server-side reader only
    // needs the TCP connection to exist, then to see it close.
    let (client1, _inj1, _calls1, _out1) = connect_client(h.port);
    let (close1_tx, close1_rx) = mpsc::channel::<()>();
    let handle1 = thread::spawn(move || {
        let _ = close1_rx.recv(); // wait for the signal, then drop (close TCP)
        drop(client1);
    });
    h.wait_for_clients(1);

    // Second connection, same machine name → same screen id. The fresh
    // one replaces the stale registration (the map holds exactly one).
    let (client2, injector2, client_calls2, out_rx2) = connect_client(h.port);
    let handle2 = thread::spawn(move || client2.run(Box::new(injector2), Box::new(NoClipboard), &out_rx2).unwrap());
    h.wait_for_clients(1);
    assert_eq!(h.server.client_count(), 1, "duplicate connection must replace, not stack");

    // Kill the STALE connection: its TCP closes, and its server-side
    // reader runs teardown — which must be a no-op for the live
    // registration (the identity check). Without the fix, this teardown
    // unregisters the live client and the server forgets it: the GUI
    // would flip it to "nearby" and crossings would stop routing.
    close1_tx.send(()).unwrap();
    let _ = handle1.join();
    thread::sleep(Duration::from_millis(100)); // let the reader's EOF land
    assert_eq!(
        h.server.client_count(),
        1,
        "stale connection's teardown must not evict the live replacement"
    );

    // The live (replacement) client must still be serviced: a crossing
    // reaches it and it reports home again.
    feed(&h, Message::MouseMoveAbs { x: 0, y: SCREEN_H / 2 });
    feed(&h, Message::MouseMoveRel { dx: -5, dy: 0 });
    let cc = calls(&client_calls2);
    assert!(cc.contains(&"enter".to_string()), "live client should still receive crossings, got {cc:?}");
    assert!(calls(&h.engine_calls).iter().any(|c| c == "cursor false"));

    // Cross back, then end the live session via the server's disconnect
    // control (the client ends its session; the reader then finishes).
    feed(&h, Message::MouseMoveRel { dx: 100, dy: 0 });
    let mut cc = calls(&client_calls2);
    for _ in 0..50 {
        if cc.contains(&"leave".to_string()) {
            break;
        }
        thread::sleep(Duration::from_millis(20));
        cc = calls(&client_calls2);
    }
    assert!(cc.contains(&"leave".to_string()), "live client should cross back, got {cc:?}");

    h.control_tx.send(Control::ClientCommand { name: "hp".into(), command: kvmshare_protocol::id::control::DISCONNECT }).unwrap();
    let _ = handle2.join();
    for _ in 0..100 {
        if h.server.client_count() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(h.server.client_count(), 0);
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
            Options { control: Some(control_rx), policy, events: None, server_id: "server-pc".into() },
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
        .unwrap_err()
        .to_string();
    assert!(err.contains("rejected"), "revoked client must be refused, got: {err}");
    assert!(err.contains("revoked"), "the refusal must say why, got: {err}");
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
    thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx).unwrap());
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

/// The GUI's connected list is driven by `clients.json`, which the app
/// layer rewrites per lifecycle event. A server-initiated drop (revoke,
/// operator disconnect) must emit `ClientDisconnected` — the regression
/// here was the GUI showing "connected to you" for a machine that had
/// been dropped, until the client happened to reconnect.
#[test]
fn server_initiated_disconnect_updates_the_client_list() {
    let h = start_server();
    let (client, injector, _client_calls, out_rx) = connect_client(h.port);
    thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx).unwrap());
    h.wait_for_clients(1);

    // The operator disconnects the machine from the GUI. The control
    // channel is drained by the main loop's idle poll (CONTROL_POLL =
    // 100 ms), so allow a few polls for the command to land.
    h.control_tx
        .send(Control::ClientCommand { name: "hp".into(), command: kvmshare_protocol::id::control::DISCONNECT })
        .unwrap();
    for _ in 0..100 {
        if h.server.client_count() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(h.server.client_count(), 0);
    // The GUI's list is driven by the disconnect event; without it,
    // `clients.json` (and therefore the Home page) kept claiming the
    // machine was connected. `Harness::events` drains the channel, so
    // the vec must be captured once and asserted on — re-draining in
    // the assert silently re-reads an empty channel.
    let mut events = Vec::new();
    for _ in 0..100 {
        events = h.events();
        if events.iter().any(|e| e == "disconnected:hp") {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        events.iter().any(|e| e == "disconnected:hp"),
        "the operator disconnect must emit ClientDisconnected, got: {events:?}"
    );
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
            Options { control: Some(control_rx), policy, events: None, server_id: "server-pc".into() },
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
