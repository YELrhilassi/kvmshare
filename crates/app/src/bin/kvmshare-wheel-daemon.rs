//! kvmshare-wheel-daemon: emits wheel events through a virtual uinput
//! mouse so every application sees exactly what a physical wheel
//! produces (XI2 scroll-valuator device events + server-emulated core
//! Button4/5).
//!
//! Linux only: runs as root (setuid binary or a sudoers rule) because
//! /dev/uinput is root-only; it drops to the session user after the two
//! privileged steps (opening the device, binding the socket) and serves
//! wheel frames from a 0600 socket only the session owner can write to.
//! See `kvmshare_platform::x11::wheel_daemon` for the protocol and the
//! rationale.

#[cfg(target_os = "linux")]
fn main() {
    if let Err(e) = kvmshare_log::init("info", None) {
        eprintln!("wheel-daemon: logging: {e}");
    }
    if let Err(e) = kvmshare_platform::x11::wheel_daemon::server::run() {
        eprintln!("wheel-daemon: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("kvmshare-wheel-daemon: not supported on this platform (Linux only)");
    std::process::exit(1);
}
