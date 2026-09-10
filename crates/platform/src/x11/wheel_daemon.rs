//! Wheel injection that every application accepts.
//!
//! ## Why XTest wheel events are not enough
//!
//! XTest can only synthesize *core protocol* button events. GTK apps
//! (Firefox, Chrome…) handle those fine, but GLFW-based apps — kitty is
//! the one everyone hits — deliberately ignore core Button4/5 when the
//! machine has any input device with scroll valuators: they expect
//! smooth scrolling from XI2 *device* events instead (kitty's
//! `x11_window.c`: `if (!_glfw.x11.xi.num_scroll_devices)
//! _glfwInputScroll(...)`). Touchpads and hi-res wheels put those
//! valuators on essentially every modern laptop, so core-emulated wheel
//! clicks silently do nothing there — while physical wheels work,
//! because the kernel emits XI2 scroll-valuator events for them.
//!
//! ## The fix: a virtual mouse
//!
//! A uinput device feeds events through the real kernel input pipeline,
//! so *every* consumer sees exactly what a physical wheel produces:
//! libinput turns `REL_WHEEL` into XI2 scroll-valuator device events
//! (kitty's smooth-scroll path) *and* emulates core Button4/5 from them
//! for legacy clients. One event, both paths, no double scrolling.
//!
//! The device file is root-owned, so the event emitter runs as a tiny
//! daemon (`kvmshare-wheel-daemon`, deployed setuid-root or under a
//! sudoers rule): it owns the uinput fd and applies `REL_WHEEL` deltas
//! from a datagram socket it listens on. The injector talks to it via
//! [`WheelClient`] and falls back to core XTest buttons when the daemon
//! is absent — correctness is never lost, only kitty's smooth path.
//!
//! Buttons are **not** routed through the daemon: XTest buttons are
//! delivered to the window under the cursor by the X server itself,
//! while a uinput button press would move the *kernel* pointer and
//! still need XTest for delivery. Wheel is the one axis where the
//! virtual device is strictly better.
//!
//! ## Left-drag wheel (shift+wheel selection, middle-paste drag)
//!
//! A real wheel while a physical button is held produces wheel events
//! *with the button held*. The daemon therefore tracks the injector's
//! button state ([`WheelClient::set_button`]) and holds the matching
//! `BTN_*` on the virtual device across wheel bursts: apps that
//! re-anchor selection state when the button is up (many terminals do)
//! stay consistent with a physical wheel.

use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;

use kvmshare_log::log_warn;

/// Where the daemon listens. A fixed path in the user's runtime dir:
/// one wheel daemon per desktop session, no discovery needed.
///
/// Both sides must compute the *same* path in every setup: the client
/// runs in the session (`XDG_RUNTIME_DIR` set, euid = the user), while
/// the daemon may run elevated via sudo, which strips
/// `XDG_RUNTIME_DIR` — there the path is derived from `SUDO_UID`
/// instead (session_uid below). The normal, non-elevated setups all
/// agree on `/run/user/<uid>/kvmshare-wheel-<uid>.sock`.
pub fn socket_path() -> PathBuf {
    let uid = session_uid();
    // Privileged (setuid-root or sudo): the daemon must not honor its
    // own (or an inherited, odd) XDG_RUNTIME_DIR — the logind session
    // convention /run/user/<uid> is the contract both sides agree on.
    let privileged = unsafe { libc::geteuid() } == 0;
    let dir = if privileged {
        PathBuf::from(format!("/run/user/{uid}"))
    } else {
        socket_dir(uid)
    };
    dir.join(format!("kvmshare-wheel-{uid}.sock"))
}

/// daemon_alive reports whether a *live* wheel daemon is listening on
/// `path`. The test is a datagram `connect`: it succeeds only against a
/// socket the kernel knows is bound — a leftover file from a killed
/// daemon (SIGTERM runs no destructors, so the unlink guard never
/// fires) fails here and gets re-spawned instead of being trusted
/// forever. This is the single liveness oracle for clients; nothing
/// else may consult the filesystem.
pub fn daemon_alive(path: &std::path::Path) -> bool {
    UnixDatagram::unbound()
        .and_then(|s| s.connect(path))
        .is_ok()
}

/// The uid whose session this socket belongs to. Covers all three
/// execution modes:
///
/// * normal user process — the euid;
/// * sudo-exported `SUDO_UID` — the invoking user (ruid is root);
/// * setuid-root binary — the *real* uid is the session user while the
///   euid is root, and no env var is needed (setuid cannot be trusted
///   to carry one).
fn session_uid() -> u32 {
    if let Ok(v) = std::env::var("SUDO_UID") {
        if let Ok(uid) = v.parse() {
            return uid;
        }
    }
    let euid = unsafe { libc::geteuid() };
    let ruid = unsafe { libc::getuid() };
    if euid == 0 && ruid != 0 {
        ruid
    } else {
        euid
    }
}

/// The runtime dir for `uid`: the session's own `XDG_RUNTIME_DIR` when
/// set, else the logind convention `/run/user/<uid>` (what the dir is
/// on every mainstream distro).
fn socket_dir(uid: u32) -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("/run/user/{uid}")))
}

/// One wheel command, as datagrams of 12 little-endian bytes:
/// `dx: i32 | dy: i32 | flags: u32`.
///
/// Not an enum: a wheel delta stream is exactly two counts and a flag
/// word, and hot-path injection must never allocate or parse tags.
/// Keeping the button flag in its own word (instead of a stolen sign
/// bit) keeps the deltas plain two's-complement values.
pub const WHEEL_DGRAM_LEN: usize = 12;

/// `flags` bit 0: the left button is held (a modifier for the wheel,
/// see the module docs).
const FLAG_LEFT_HELD: u32 = 1;

/// Pack a wheel frame into a datagram.
pub fn pack(dx: i32, dy: i32, left_held: bool) -> [u8; WHEEL_DGRAM_LEN] {
    let flags = if left_held { FLAG_LEFT_HELD } else { 0 };
    let mut buf = [0u8; WHEEL_DGRAM_LEN];
    buf[..4].copy_from_slice(&dx.to_le_bytes());
    buf[4..8].copy_from_slice(&dy.to_le_bytes());
    buf[8..].copy_from_slice(&flags.to_le_bytes());
    buf
}

/// Unpack the inverse of [`pack`]. Exposed for the daemon and tests.
pub fn unpack(buf: &[u8]) -> Option<(i32, i32, bool)> {
    if buf.len() != WHEEL_DGRAM_LEN {
        return None;
    }
    let mut raw = [0u8; 4];
    raw.copy_from_slice(&buf[..4]);
    let dx = i32::from_le_bytes(raw);
    raw.copy_from_slice(&buf[4..8]);
    let dy = i32::from_le_bytes(raw);
    raw.copy_from_slice(&buf[8..]);
    let flags = u32::from_le_bytes(raw);
    Some((dx, dy, flags & FLAG_LEFT_HELD != 0))
}

/// Client side: fire-and-forget wheel sender used by the injector.
///
/// Datagram sockets keep this lock-free from the X connection: a wheel
/// burst is a handful of `sendto` calls, each independent — a stalled
/// daemon can no more freeze the cursor than a stalled clipboard.
pub struct WheelClient {
    sock: UnixDatagram,
    path: PathBuf,
    /// Nothing is bound locally until the first send fails; binding to
    /// an abstract-free path in the runtime dir keeps /tmp clean.
    bound: bool,
    /// Whether the last send hit a dead daemon (socket exists, nobody
    /// reads it). One warning per liveness flip, never per event.
    was_dead: bool,
}

impl WheelClient {
    /// Try to talk to the daemon. `Ok(None)` = no daemon (caller falls
    /// back to XTest buttons); `Err` = unexpected (also falls back).
    pub fn connect() -> Option<Self> {
        let path = socket_path();
        // Probe liveness, not file existence: a SIGTERM-killed daemon
        // leaves its socket file behind (destructors do not run), and
        // trusting the file made clients skip spawning forever. A live
        // daemon has a bound socket: datagram `connect` succeeds only
        // against it.
        if !daemon_alive(&path) {
            return None;
        }
        let sock = UnixDatagram::unbound().ok()?;
        Some(Self {
            sock,
            path,
            bound: false,
            was_dead: false,
        })
    }

    /// The injector's own return address, so the daemon can attribute
    /// (and drop) strays. Not used for replies. `UnixDatagram::bind`
    /// constructs a bound socket, so the client socket is swapped for
    /// its bound twin here.
    fn ensure_bound(&mut self) -> bool {
        if self.bound {
            return true;
        }
        let local = self
            .path
            .with_extension(format!("client-{}", std::process::id()));
        let _ = std::fs::remove_file(&local);
        match UnixDatagram::bind(&local) {
            Ok(s) => {
                self.sock = s;
                self.bound = true;
                true
            }
            Err(_) => false,
        }
    }

    /// Send one wheel frame. Returns `true` if the daemon (probably)
    /// took it.
    pub fn send(&mut self, dx: i32, dy: i32, left_held: bool) -> bool {
        if !self.ensure_bound() {
            return false;
        }
        let buf = pack(dx, dy, left_held);
        match self.sock.send_to(&buf, &self.path) {
            Ok(n) if n == WHEEL_DGRAM_LEN => {
                if self.was_dead {
                    log_warn!("wheel: daemon is answering again");
                    self.was_dead = false;
                }
                true
            }
            _ => {
                if !self.was_dead {
                    log_warn!("wheel: daemon unreachable — using XTest buttons (some apps will not scroll)");
                    self.was_dead = true;
                }
                false
            }
        }
    }

    /// The virtual button state the daemon should hold while wheeling
    /// (see the module docs on left-drag wheel). Same datagram path,
    /// same fire-and-forget contract.
    pub fn set_button(&mut self, button: u8, pressed: bool) {
        // Only the left button matters today (terminals re-anchor
        // selection on button-up); the wire has room for more if a
        // platform's drag-wheel semantics ever differ.
        if button != 1 {
            return;
        }
        let _ = self.send(0, 0, pressed);
    }
}

#[cfg(test)]
mod tests;

// The spawn entry point lives with the daemon-side code; keep the
// injector call sites stable.
pub use super::wheel_server::ensure_daemon;
