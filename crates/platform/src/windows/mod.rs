//! The Windows backend.
//!
//! Three pieces, mirroring the X11 backend's structure exactly — the
//! same [`Engine`]/[`Injector`] contracts, the same canonical HID key
//! model, the same message flow:
//!
//! * [`capture::InputCapture`] — a hidden message-only window registered
//!   for **Raw Input** (`RIDEV_INPUTSINK`), forwarding hardware mouse
//!   deltas, buttons, wheel and key transitions as protocol [`Message`]s
//!   on a background thread.
//! * [`engine::Win32Engine`] — the server's control of its own screen:
//!   `SetCursorPos` warp, `ShowCursor` hide/show, clipboard.
//! * [`injector::Win32Injector`] — the client's control of its own
//!   screen: `SetCursorPos` moves, `SendInput` injection of
//!   buttons/keys/wheel (scan-code mode — layout independent), clipboard.
//!
//! The engine and injector use *programmatic* cursor warps only, which
//! never generate raw input — so the server's park/warp can't feed
//! phantom motion into the session, exactly like the X11 raw-event
//! design.

mod buttons;
mod capture;
pub(crate) mod clipboard;
mod engine;
mod injector;
mod isolation;
mod timer;

use std::sync::mpsc::Receiver;
use std::sync::Arc;

use kvmshare_core::client::Injector;
use kvmshare_core::server::{Engine, Liveness};
use kvmshare_protocol::message::Message;

/// Raise this process above ordinary applications (see the docs on
/// [`crate::raise_priority`]). Called on both server and client start;
/// idempotent and best-effort.
pub fn raise_priority() {
    use windows_sys::Win32::System::Threading as thr;
    // SAFETY: GetCurrentProcess returns the process's pseudo-handle;
    // HIGH_PRIORITY_CLASS (not realtime) keeps our millisecond-paced
    // loops scheduled ahead of busy apps without risking the machine.
    unsafe {
        let _ = thr::SetPriorityClass(thr::GetCurrentProcess(), thr::HIGH_PRIORITY_CLASS);
    }
}

/// Make the process per-monitor DPI aware. Idempotent and process-wide;
/// called on both server and client start so all coordinates are
/// physical pixels. Failing is not fatal (older systems fall back to
/// system DPI and a 1.0 scale report).
fn set_dpi_aware() {
    use windows_sys::Win32::UI::HiDpi as hidpi;
    // SAFETY: passing a well-known DPI awareness context constant.
    unsafe {
        hidpi::SetProcessDpiAwarenessContext(hidpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// Whether the Winlogon secure desktop (the UAC consent prompt) is
/// currently the input desktop — a protected desktop no process can
/// inject into, not even an elevated one. See
/// [`isolation::secure_desktop_active`] for the full story.
pub fn secure_desktop_active() -> bool {
    isolation::secure_desktop_active()
}

/// The server-side Windows platform: local input capture + local cursor
/// engine + the standalone clipboard service.
pub struct Server {
    /// Incoming local input events (motion/buttons/keys).
    pub input: Receiver<Message>,
    /// Control of the local cursor.
    pub engine: Box<dyn Engine>,
    /// The local clipboard, on its own lock in the app layer. Split from
    /// the engine on purpose: a clipboard read can block on another
    /// process holding the clipboard open, and the engine lock
    /// serializes cursor motion.
    pub clipboard: Box<dyn kvmshare_core::client::Clipboard>,
    /// Liveness heartbeats for the server supervisor; the capture
    /// thread's tick is wired here.
    pub liveness: Arc<Liveness>,
}

impl Server {
    /// Start raw-input capture and build the engine. `display` is
    /// meaningless on Windows and ignored (kept for a uniform signature).
    pub fn start(_display: Option<&str>) -> Result<Self, String> {
        raise_priority();
        set_dpi_aware();
        // 1 ms timer resolution: the capture and session loops are paced
        // in milliseconds, and Windows' coarse default timer would turn
        // them into ~15 ms clumps (see [`timer`]).
        timer::HighResTimer::engage_forever();
        let (input, capture_tick) = capture::start()?;
        let engine = Box::new(engine::Win32Engine::new());
        let clipboard: Box<dyn kvmshare_core::client::Clipboard> = Box::new(clipboard::Clipboard::new());
        let liveness = Arc::new(Liveness { capture_tick_ms: capture_tick, ..Default::default() });
        Ok(Self { input, engine, clipboard, liveness })
    }
}

/// The client-side Windows platform: an input injector and the
/// standalone clipboard service (see [`core::client::Clipboard`] for why
/// the clipboard is split from the injector).
pub fn client_injector(
    _display: Option<&str>,
) -> Result<(Box<dyn Injector>, Box<dyn kvmshare_core::client::Clipboard>), String> {
    raise_priority();
    set_dpi_aware();
    // 1 ms timer resolution for the motion tick and beacon cadence (see
    // [`timer`] and [`Server::start`]).
    timer::HighResTimer::engage_forever();
    Ok((
        Box::new(injector::Win32Injector::new()),
        Box::new(injector::Win32Clipboard::new()),
    ))
}