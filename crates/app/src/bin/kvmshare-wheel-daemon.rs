//! kvmshare-wheel-daemon: emits wheel events through a virtual uinput
//! mouse so every application sees exactly what a physical wheel
//! produces (XI2 scroll-valuator device events + server-emulated core
//! Button4/5).
//!
//! Linux only. Runs as the plain session user: access to /dev/uinput
//! is granted once at install time by the GUI's installer step (udev
//! rule via pkexec — see `gui/internal/installer`), never at runtime.
//! It serves wheel frames from a 0600 datagram socket only the session
//! owner can write to. See `kvmshare_platform::x11::wheel_daemon` for
//! the protocol and `wheel_server` for the injection side.

#[cfg(target_os = "linux")]
fn main() {
    // --version before anything else: the GUI's install check reads the
    // build id from every binary in the set, this one included.
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!(
            "kvmshare-wheel-daemon {} (build {})",
            kvmshare_app::PKG_VERSION,
            kvmshare_app::BUILD_ID
        );
        return;
    }
    if let Err(e) = kvmshare_log::init("info", None) {
        eprintln!("wheel-daemon: logging: {e}");
    }
    if let Err(e) = kvmshare_platform::x11::wheel_server::run() {
        eprintln!("wheel-daemon: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    // --version still answers on every platform: the GUI's install check
    // reads the build id from every binary in the set, and a Windows
    // install carries this file as a stub.
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!(
            "kvmshare-wheel-daemon {} (build {})",
            kvmshare_app::PKG_VERSION,
            kvmshare_app::BUILD_ID
        );
        return;
    }
    eprintln!("kvmshare-wheel-daemon: not supported on this platform (Linux only)");
    std::process::exit(1);
}
