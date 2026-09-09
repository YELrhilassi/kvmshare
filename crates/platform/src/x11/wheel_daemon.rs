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
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::time::Duration;

use kvmshare_log::{log_info, log_warn};

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
    let dir = if privileged { PathBuf::from(format!("/run/user/{uid}")) } else { socket_dir(uid) };
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
    UnixDatagram::unbound().and_then(|s| s.connect(path)).is_ok()
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
    if euid == 0 && ruid != 0 { ruid } else { euid }
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
        Some(Self { sock, path, bound: false, was_dead: false })
    }

    /// The injector's own return address, so the daemon can attribute
    /// (and drop) strays. Not used for replies. `UnixDatagram::bind`
    /// constructs a bound socket, so the client socket is swapped for
    /// its bound twin here.
    fn ensure_bound(&mut self) -> bool {
        if self.bound {
            return true;
        }
        let local = self.path.with_extension(format!("client-{}", std::process::id()));
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

/// Daemon side: read wheel frames and push them into the kernel.
///
/// The uinput setup is deliberately a *pointer* (INPUT_PROP_POINTER,
/// buttons + relative axes): libinput binds `REL_WHEEL` on pointer
/// devices to the scroll valuator path kitty et al. listen to.
pub mod server {
    use std::io::ErrorKind;
    use std::os::unix::net::UnixDatagram;

    use kvmshare_log::{log_info, log_warn};

    use super::{socket_path, unpack, WHEEL_DGRAM_LEN};

    // --- uinput constants (linux/uinput.h + input-event-codes.h) ---
    const UI_SET_EVBIT: u64 = 0x40045564;
    const UI_SET_KEYBIT: u64 = 0x40045565;
    const UI_SET_RELBIT: u64 = 0x40045566;
    const UI_DEV_CREATE: u64 = 0x5501;
    const UI_DEV_DESTROY: u64 = 0x5502;
    const UI_DEV_SETUP: u64 = 0x405c5503; // _IOW('U', 3, struct uinput_setup)
    const UI_SET_PHYS: u64 = 0x4008556c;

    const EV_SYN: u16 = 0x00;
    const EV_KEY: u16 = 0x01;
    const EV_REL: u16 = 0x02;
    const SYN_REPORT: u16 = 0;

    const REL_WHEEL: u16 = 0x08;
    const REL_HWHEEL: u16 = 0x06;
    const REL_WHEEL_HI_RES: u16 = 0x0b;
    const REL_HWHEEL_HI_RES: u16 = 0x0c;

    const BTN_LEFT: u16 = 0x110;

    const UINPUT_PATH: &str = "/dev/uinput";

    /// `input_id` (struct layout from linux/input.h).
    #[repr(C)]
    struct InputId {
        bustype: u16,
        vendor: u16,
        product: u16,
        version: u16,
    }

    /// `uinput_setup`.
    #[repr(C)]
    struct UinputSetup {
        id: InputId,
        name: [u8; 80],
        ff_effects_max: u32,
    }

    /// `input_event`.
    #[repr(C)]
    struct InputEvent {
        time: libc::timeval,
        type_: u16,
        code: u16,
        value: i32,
    }

    /// The running daemon state: the uinput fd and the virtual left
    /// button's current kernel state.
    struct Daemon {
        fd: i32,
        left_held: bool,
    }

    /// The session user to drop to (set by the sudo/setuid wrapper from
    /// `SUDO_UID`/`SUDO_GID`), if we were started elevated.
    fn session_owner() -> Option<(libc::uid_t, libc::gid_t)> {
        let uid = std::env::var("SUDO_UID").ok().and_then(|v| v.parse().ok());
        let gid = std::env::var("SUDO_GID").ok().and_then(|v| v.parse().ok());
        Some((uid?, gid?))
    }

    /// Drop root: auxiliary groups first, then group, then user. Called
    /// after the uinput fd is open and the socket is bound+owned, so no
    /// privilege is needed on the serving path.
    fn drop_privileges(uid: libc::uid_t, gid: libc::gid_t) {
        unsafe {
            if libc::setgroups(0, std::ptr::null()) != 0 {
                log_warn!("wheel daemon: setgroups failed (continuing)");
            }
            if libc::setgid(gid) != 0 {
                log_warn!("wheel daemon: setgid {gid} failed (continuing)");
            }
            if libc::setuid(uid) != 0 {
                log_warn!("wheel daemon: setuid {uid} failed (continuing)");
            }
        }
    }

    impl Daemon {
        fn open() -> Result<Self, String> {
            // CString, not as_ptr(): a Rust &str is not NUL-terminated,
            // so passing it raw to open() made the kernel read past the
            // end — usually failing with ENOENT despite the node being
            // right there.
            let path = std::ffi::CString::new(UINPUT_PATH).expect("uinput path has no NUL");
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
            if fd < 0 {
                return Err(format!(
                    "cannot open {UINPUT_PATH} ({}): deploy the setuid daemon or a udev/sudoers rule",
                    std::io::Error::last_os_error()
                ));
            }
            let mut name = [0u8; 80];
            let name_str = b"kvmshare virtual wheel\0";
            name[..name_str.len()].copy_from_slice(name_str);
            let setup = UinputSetup {
                id: InputId { bustype: 0x03 /* BUS_USB */, vendor: 0x1d52 /* local */, product: 0x2026, version: 1 },
                name,
                ff_effects_max: 0,
            };
            // UI_SET_PHYS wants a NUL-terminated C string pointer.
            let phys = b"kvmshare/wheel\0";
            let ret = unsafe { libc::ioctl(fd, UI_SET_PHYS, phys.as_ptr()) };
            if ret < 0 {
                return Err(format!("UI_SET_PHYS: {}", std::io::Error::last_os_error()));
            }
            for (req, bit) in [
                (UI_SET_EVBIT, EV_KEY as u64),
                (UI_SET_EVBIT, EV_REL as u64),
                (UI_SET_EVBIT, EV_SYN as u64),
                (UI_SET_RELBIT, REL_WHEEL as u64),
                (UI_SET_RELBIT, REL_HWHEEL as u64),
                (UI_SET_RELBIT, REL_WHEEL_HI_RES as u64),
                (UI_SET_RELBIT, REL_HWHEEL_HI_RES as u64),
                (UI_SET_KEYBIT, BTN_LEFT as u64),
            ] {
                let ret = unsafe { libc::ioctl(fd, req as libc::c_ulong, bit as libc::c_int) };
                if ret < 0 {
                    return Err(format!("uinput ioctl {req:#x}: {}", std::io::Error::last_os_error()));
                }
            }
            let ret = unsafe { libc::ioctl(fd, UI_DEV_SETUP as libc::c_ulong, &setup) };
            if ret < 0 {
                return Err(format!("UI_DEV_SETUP: {}", std::io::Error::last_os_error()));
            }
            let ret = unsafe { libc::ioctl(fd, UI_DEV_CREATE) };
            if ret < 0 {
                return Err(format!("UI_DEV_CREATE: {}", std::io::Error::last_os_error()));
            }
            Ok(Self { fd, left_held: false })
        }

        /// Emit one event; SYN_REPORT is appended by the caller's batch.
        fn emit(&self, type_: u16, code: u16, value: i32) -> bool {
            let ev = InputEvent {
                time: libc::timeval { tv_sec: 0, tv_usec: 0 },
                type_,
                code,
                value,
            };
            let written = unsafe {
                libc::write(self.fd, &ev as *const InputEvent as *const libc::c_void, std::mem::size_of::<InputEvent>())
            };
            written == std::mem::size_of::<InputEvent>() as isize
        }

        fn flush(&self) -> bool {
            self.emit(EV_SYN, SYN_REPORT, 0)
        }

        /// Apply one frame: wheel deltas (notches + matching hi-res
        /// halves) and the tracked left button.
        fn apply(&mut self, dx: i32, dy: i32, left_held: bool) {
            if left_held != self.left_held {
                let _ = self.emit(EV_KEY, BTN_LEFT, i32::from(left_held));
                self.left_held = left_held;
                let _ = self.flush();
            }
            if dx != 0 {
                // One notch = ±120 in hi-res units (the kernel convention
                // every consumer already understands).
                let _ = self.emit(EV_REL, REL_HWHEEL, dx);
                let _ = self.emit(EV_REL, REL_HWHEEL_HI_RES, dx * 120);
            }
            if dy != 0 {
                let _ = self.emit(EV_REL, REL_WHEEL, dy);
                let _ = self.emit(EV_REL, REL_WHEEL_HI_RES, dy * 120);
            }
            if dx != 0 || dy != 0 {
                let _ = self.flush();
            }
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            unsafe {
                libc::ioctl(self.fd, UI_DEV_DESTROY);
                libc::close(self.fd);
            }
        }
    }

    /// Run until the socket is removed or a fatal error hits. Cleans up
    /// the socket file on exit (bound sockets do not unlink themselves).
    ///
    /// Privilege order when started elevated (setuid/sudo):
    /// open `/dev/uinput` → bind the socket → hand both to the session
    /// user → serve. Nothing on the serving path needs root, and the
    /// socket ends up owned by the session (0600), writable only by the
    /// user whose desktop receives the events. The normal deployment
    /// needs none of that: the installer's udev rule grants the desktop
    /// user write access to `/dev/uinput` (alongside the physical-input
    /// read grant), so the daemon runs as the plain session user — no
    /// root, no sudo, at any point after install.
    pub fn run() -> Result<(), String> {
        let path = socket_path();
        // A live daemon holds a bound socket on `path`: a datagram
        // `connect` succeeds iff something is listening. Never steal a
        // live daemon's socket — exit quietly (the spawner treats this
        // as success; wheel just works).
        if UnixDatagram::unbound().and_then(|s| s.connect(&path)).is_ok() {
            log_info!("wheel daemon: already running on {}", path.display());
            return Ok(());
        }
        let _ = std::fs::remove_file(&path); // stale file, not a live socket
        let sock = UnixDatagram::bind(&path).map_err(|e| format!("bind {}: {e}", path.display()))?;
        // Unlink the socket on *every* exit path — a dead daemon must
        // never leave a socket file behind for clients to find.
        let _guard = SocketCleanup(path.clone());
        // Only the owner's session may send wheel events.
        restrict_permissions(&path)?;
        let mut daemon = Daemon::open()?;
        if let Some((uid, gid)) = session_owner() {
            if unsafe { libc::geteuid() } == 0 {
                // Hand the socket to the session user before dropping:
                // after setuid we could no longer chown it.
                chown_as_root(&path, uid, gid)?;
                drop_privileges(uid, gid);
            }
        }
        log_info!("wheel daemon: listening on {} (virtual wheel up)", path.display());
        let mut buf = [0u8; WHEEL_DGRAM_LEN];
        loop {
            // `recv` (not recv_from): we do not reply, and unbound peers
            // that cannot be replied to must not matter.
            match sock.recv(&mut buf) {
                Ok(n) if n == WHEEL_DGRAM_LEN => {
                    if let Some((dx, dy, left)) = unpack(&buf) {
                        daemon.apply(dx, dy, left);
                    }
                }
                Ok(_) => {} // short/stray datagram: ignore
                Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::ConnectionReset) => continue,
                Err(e) => return Err(format!("recv: {e}")),
            }
        }
    }

    /// Unlinks the socket file when dropped.
    struct SocketCleanup(std::path::PathBuf);
    impl Drop for SocketCleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn chown_as_root(path: &std::path::Path, uid: libc::uid_t, gid: libc::gid_t) -> Result<(), String> {
        let cpath = match std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) {
            Ok(c) => c,
            Err(_) => return Err("socket path not representable".into()),
        };
        if unsafe { libc::chown(cpath.as_ptr(), uid, gid) } != 0 {
            return Err(format!("chown {}: {}", path.display(), std::io::Error::last_os_error()));
        }
        Ok(())
    }

    fn restrict_permissions(path: &std::path::Path) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod {}: {e}", path.display()))
    }
}

/// Spawn the daemon if it is not already running. Called by the client
/// at startup; failure is logged once and everything keeps working via
/// the XTest fallback.
pub fn ensure_daemon() {
    if daemon_alive(&socket_path()) {
        return; // a live daemon is listening — nothing to do
    }
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return,
    };
    // Same directory as the client binary — the deploy layout keeps
    // them together.
    let daemon = exe.with_file_name("kvmshare-wheel-daemon");
    if !daemon.exists() {
        return;
    }
    match std::process::Command::new(&daemon)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(child) => {
            // Reap the daemon whenever it exits. A dropped `Child` is
            // never waited on, so a daemon that exits at startup (no
            // /dev/uinput permission, port taken) lingered as a zombie
            // for the client's whole lifetime — visible as a `defunct`
            // entry in the process list.
            let reap = std::thread::Builder::new()
                .name("wheel-daemon-reap".into())
                .spawn(move || {
                    let mut child = child;
                    let _ = child.wait();
                });
            if reap.is_err() {
                // Without the reaper thread (thread spawn failed —
                // effectively never), the daemon zombie falls to init
                // when the client exits; nothing better is available
                // without blocking the input path here.
                log_warn!("cannot reap wheel daemon exits (thread spawn failed)");
            }
            // Wait briefly for the socket to appear so the first wheel
            // frame is not lost to the fallback — judged by liveness,
            // not file existence (the daemon binds late; a leftover file
            // must not end the wait early).
            for _ in 0..20 {
                if daemon_alive(&socket_path()) {
                    log_info!("wheel daemon started");
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            log_warn!("wheel daemon did not come up — wheel uses XTest (kitty-style apps may not scroll)");
        }
        Err(e) => log_warn!("cannot spawn wheel daemon: {e}"),
    }
}

#[cfg(test)]
#[path = "wheel_daemon_tests.rs"]
mod tests;
