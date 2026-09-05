//! # kvmshare-platform
//!
//! OS backends for kvmshare. The core crate defines the contracts
//! ([`Engine`] for the server side, [`Injector`] for the client side);
//! this crate provides the implementations.
//!
//! * **Linux** — X11 backend: XI2 raw events for input capture (deltas
//!   straight from the device, no warp feedback), XFixes for cursor
//!   hide/show, XTest for input injection, arboard for the clipboard.
//!   A Wayland backend slots in next to it later (same contracts, same
//!   evdev-based key table).
//! * **Windows** — Raw Input capture on a hidden message-only window,
//!   `SetCursorPos` cursor control, `SendInput` injection (scan-code
//!   mode), native clipboard. Mirrors the X11 backend structure; keys
//!   travel as canonical USB HID usages on both.
//! * **macOS / other** — not yet implemented; [`unsupported`] stubs
//!   fail with a clear error so the rest of the system still builds.
//!
//! The entry points are [`server`] (start local input capture + get the
//! engine) and [`client`] (get an injector). `display` is honored by the
//! X11 backend and ignored elsewhere (uniform signature).

use std::sync::mpsc::Receiver;
use std::sync::Arc;

use kvmshare_core::client::{Clipboard, Injector};
use kvmshare_core::server::Liveness;
use kvmshare_core::server::Engine;
use kvmshare_protocol::message::Message;

pub mod keys;
pub mod unsupported;

// Linux-only capture internals: shared motion rate-limiting, and the
// evdev reader that isolates/reads physical devices while the cursor is
// on a client. `evdev_reader` is X-free by design so a future Wayland
// backend reuses it unchanged.
#[cfg(target_os = "linux")]
pub mod motion;
#[cfg(target_os = "linux")]
pub mod evdev_reader;

// Per-OS backends. Each implements the same Engine/Injector contracts;
// the key table in `keys` is shared so the wire identity is identical
// everywhere.
#[cfg(target_os = "linux")]
pub mod x11;
#[cfg(target_os = "windows")]
pub mod windows;

/// Start the server-side platform: capture local input and build the
/// engine that controls the local cursor, plus the standalone clipboard
/// service (returned separately because the clipboard lives on its own
/// lock in the app layer — a clipboard call that stalls must never be
/// able to freeze the cursor).
///
/// `display` is an X display string (`None` = `$DISPLAY`); ignored on
/// platforms without displays.
pub fn server(
    display: Option<&str>,
) -> Result<(Receiver<Message>, Box<dyn Engine>, Box<dyn Clipboard>, Arc<Liveness>), String> {
    #[cfg(target_os = "linux")]
    {
        let s = x11::Server::start(display)?;
        Ok((s.input, s.engine, s.clipboard, s.liveness))
    }
    #[cfg(target_os = "windows")]
    {
        let s = windows::Server::start(display)?;
        Ok((s.input, s.engine, s.clipboard, s.liveness))
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        unsupported::server(display)
    }
}

/// Raise this process's scheduling priority above ordinary apps.
///
/// A kvmshare role owns the user's cursor: its motion loop wakes every
/// few milliseconds and every recovery watchdog depends on being
/// scheduled. At normal priority a busy app can starve it for seconds;
/// at the priority Task Scheduler launches processes with (BelowNormal
/// on some configurations) it is starved constantly — the exact failure
/// behind "the cursor freezes whenever I click or start something on the
/// client". High priority (not realtime — that could stall the whole
/// machine) keeps the motion and watchdog threads scheduled without
/// risking the OS. Best effort everywhere: failing is never fatal.
#[allow(dead_code)]
pub fn raise_priority() {
    #[cfg(target_os = "windows")]
    windows::raise_priority();
    #[cfg(target_os = "linux")]
    {
        // SAFETY: setpriority(PRIO_PROCESS, 0, …) targets this process.
        // Negative niceness needs privilege; as an unprivileged user it
        // fails harmlessly and the process keeps its normal class.
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, 0, -10);
        }
    }
}

/// Whether the OS currently shows a desktop that cannot receive
/// injected input — the Winlogon secure desktop on Windows, shown while
/// a UAC consent prompt is up. The client waits such a state out before
/// (re)connecting instead of churning a session into a wall it cannot
/// inject into; the same condition ends an already-running session (see
/// [`kvmshare_core::client::Injector::secure_desktop_active`]). Other
/// platforms never report it.
pub fn secure_desktop_active() -> bool {
    #[cfg(target_os = "windows")]
    {
        windows::secure_desktop_active()
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// The local machine's primary display geometry (physical pixels + DPI
/// scale). The server builds its first-run default layout from this, so
/// a freshly configured machine always describes *itself* correctly —
/// never a copy of some other machine's layout. Best-effort: `None`
/// when no display is reachable (headless); callers fall back to sane
/// defaults.
pub fn primary_display() -> Option<kvmshare_protocol::message::ScreenInfo> {
    #[cfg(target_os = "linux")]
    {
        x11::primary_display()
    }
    #[cfg(target_os = "windows")]
    {
        windows::primary_display()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        unsupported::primary_display()
    }
}

/// The local machine's host name, straight from the OS. Used as the
/// default client name and as the name of a server's own screen in a
/// freshly created layout. Returns an empty string only if the OS call
/// fails; callers decide the placeholder.
pub fn hostname() -> String {
    #[cfg(target_os = "windows")]
    {
        windows::hostname()
    }
    #[cfg(target_os = "linux")]
    {
        let mut buf = [0u8; 256];
        // SAFETY: gethostname writes at most `buf.len()` bytes and
        // NUL-terminates; `buf` outlives the call.
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if rc == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            return String::from_utf8_lossy(&buf[..end]).trim().to_owned();
        }
        String::new()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        String::new()
    }
}

/// Build the client-side platform: an input injector and the standalone
/// clipboard service. They are returned separately because the clipboard
/// lives on its own lock and thread in the client — a clipboard call
/// that stalls (another process holding the clipboard open) must never
/// be able to freeze the cursor.
pub fn client(
    display: Option<&str>,
) -> Result<(Box<dyn Injector>, Box<dyn Clipboard>), String> {
    #[cfg(target_os = "linux")]
    {
        x11::client_injector(display)
    }
    #[cfg(target_os = "windows")]
    {
        windows::client_injector(display)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        unsupported::client(display)
    }
}