use super::*;

#[test]
fn utf8_roundtrip_through_clipboard_memory_model() {
    // The Win32 calls themselves need a desktop; what is testable
    // here is the data model: set records last_remote and rejects
    // non-text mimes without touching the system.
    let mut cb = Clipboard::new();
    cb.set_text("text/plain", "héllo — kvmshare".as_bytes());
    assert_eq!(
        cb.last_injected(),
        Some(("text/plain".into(), "héllo — kvmshare".as_bytes().to_vec()))
    );
    // Non-text mimes are ignored and leave last_remote untouched.
    cb.set_text("image/png", &[0, 1, 2]);
    assert_eq!(
        cb.last_injected(),
        Some(("text/plain".into(), "héllo — kvmshare".as_bytes().to_vec()))
    );
    // Invalid UTF-8 is rejected.
    cb.set_text("text/plain", &[0xff, 0xfe]);
    assert_eq!(
        cb.last_injected(),
        Some(("text/plain".into(), "héllo — kvmshare".as_bytes().to_vec()))
    );
}
