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
    eprintln!("kvmshare-wheel-daemon: not supported on this platform (Linux only)");
    std::process::exit(1);
}
