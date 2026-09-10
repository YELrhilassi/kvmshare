use super::*;

#[test]
fn roundtrip_packs_and_unpacks() {
    let msg = Message::MouseMoveRel { dx: -12, dy: 3 };
    let bytes = pack(7, 42, &msg);
    assert_eq!(bytes.len(), ENVELOPE_LEN + msg.encode().len());
    let d = unpack(&bytes).expect("valid datagram");
    assert_eq!(d.id, 7);
    assert_eq!(d.seq, 42);
    assert_eq!(d.msg, msg);
}

#[test]
fn malformed_datagrams_are_rejected() {
    assert!(unpack(b"").is_none());
    assert!(unpack(&[0; 4]).is_none()); // truncated envelope
    // Envelope + garbage frame.
    let mut bad = pack(1, 1, &Message::KeepAlive);
    bad.push(0xAA);
    assert!(unpack(&bad).is_none());
}

#[test]
fn seq_freshness_handles_wrap_and_duplicates() {
    assert!(is_newer(1, 0));
    assert!(!is_newer(0, 0), "duplicate must not apply twice");
    assert!(!is_newer(0, 1), "older traffic is stale");
    // Wrap: after u32::MAX comes 0 again.
    assert!(is_newer(0, u32::MAX));
    assert!(is_newer(u32::MAX, u32::MAX - 1));
    assert!(!is_newer(u32::MAX, 0));
}
