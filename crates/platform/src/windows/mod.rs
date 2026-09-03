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
//! never generate raw input — so the server's park/recenter can't feed
//! phantom motion into the session, exactly like the X11 raw-event
//! design.

mod buttons;
mod capture;
mod clipboard;
mod engine;
mod injector;

use std::sync::mpsc::Receiver;

use kvmshare_core::client::Injector;
use kvmshare_core::server::Engine;
use kvmshare_protocol::message::Message;

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

/// The server-side Windows platform: local input capture + local cursor
/// engine.
pub struct Server {
    /// Incoming local input events (motion/buttons/keys).
    pub input: Receiver<Message>,
    /// Control of the local cursor and clipboard.
    pub engine: Box<dyn Engine>,
}

impl Server {
    /// Start raw-input capture and build the engine. `display` is
    /// meaningless on Windows and ignored (kept for a uniform signature).
    pub fn start(_display: Option<&str>) -> Result<Self, String> {
        set_dpi_aware();
        let input = capture::start()?;
        let engine = Box::new(engine::Win32Engine::new());
        Ok(Self { input, engine })
    }
}

/// The client-side Windows platform: an input injector.
pub fn client_injector(_display: Option<&str>) -> Result<Box<dyn Injector>, String> {
    set_dpi_aware();
    Ok(Box::new(injector::Win32Injector::new()))
}