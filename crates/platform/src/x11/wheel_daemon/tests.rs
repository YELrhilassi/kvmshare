use super::{pack, socket_path, unpack, WHEEL_DGRAM_LEN};

#[test]
fn pack_unpack_roundtrip() {
    for &(dx, dy, left) in &[
        (0, 1, false),
        (0, -1, false),
        (0, -3, true),
        (2, 0, false),
        (-2, 0, true),
        (1, 1, true),
        (-7, 9, false),
        // Extreme deltas must round-trip untouched (no stolen sign bit).
        (i32::MAX, i32::MIN, false),
        (i32::MIN, i32::MAX, true),
        (-1, -1, true),
    ] {
        let buf = pack(dx, dy, left);
        assert_eq!(buf.len(), WHEEL_DGRAM_LEN);
        assert_eq!(
            unpack(&buf),
            Some((dx, dy, left)),
            "roundtrip of ({dx},{dy},{left})"
        );
    }
}

#[test]
fn unpack_rejects_short_and_garbage() {
    assert_eq!(unpack(&[]), None);
    assert_eq!(unpack(&[0u8; 4]), None);
    assert_eq!(unpack(&[0u8; 9]), None);
}

#[test]
fn flag_bit_is_not_part_of_the_delta() {
    // pack with the flag must not shift the delta into sign territory:
    // (dx=1, left) unpacks as dx=1 again, not as a huge negative.
    let buf = pack(1, -2, true);
    let (dx, dy, left) = unpack(&buf).unwrap();
    assert_eq!((dx, dy, left), (1, -2, true));
}

#[test]
fn socket_path_uses_runtime_dir() {
    // With XDG_RUNTIME_DIR set (any real session), the socket lives
    // there; without it, /tmp. Either way it must carry the uid.
    let path = socket_path();
    let name = path.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("kvmshare-wheel-"), "{name}");
    assert!(name.ends_with(".sock"), "{name}");
    let uid = super::session_uid();
    assert!(
        name.contains(&uid.to_string()),
        "path {name} must embed uid {uid}"
    );
}
