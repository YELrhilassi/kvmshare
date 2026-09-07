//! The machine's host name, used as the default client name and as the
//! name of a server's own screen in a freshly created layout.

/// The machine's host name, used as the default client name and as the
/// name of a server's own screen in a freshly created layout. The OS
/// query itself lives in `kvmshare-platform` (per-OS); here we only add
/// the cheap portable hints on top and settle on a neutral placeholder
/// if even the OS refuses.
pub fn hostname() -> String {
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return h;
        }
    }
    #[cfg(target_os = "linux")]
    if let Ok(h) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let h = h.trim().to_owned();
        if !h.is_empty() {
            return h;
        }
    }
    let h = kvmshare_platform::hostname();
    if h.is_empty() {
        "client".into()
    } else {
        h
    }
}