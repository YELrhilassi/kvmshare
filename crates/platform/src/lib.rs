//! # kvmshare-platform
//!
//! OS backends for kvmshare. The core crate defines the contracts
//! ([`Engine`] for the server side, [`Injector`] for the client side);
//! this crate provides the implementations.
//!
//! * **Linux** — X11 backend: XI2 raw events for input capture (deltas
//!   straight from the device, no warp feedback), XFixes for cursor
//!   hide/show, XTest for input injection, arboard for the clipboard.
//! * **Windows / macOS** — not yet implemented; [`unsupported`] stubs
//!   fail with a clear error so the rest of the system still builds.
//!
//! The entry points are [`server`] (start local input capture + get the
//! engine) and [`client`] (get an injector).

use std::sync::mpsc::Receiver;

use kvmshare_core::client::Injector;
use kvmshare_core::server::Engine;
use kvmshare_protocol::message::Message;

pub mod unsupported;

#[cfg(target_os = "linux")]
pub mod x11;

/// Start the server-side platform: capture local input and build the
/// engine that controls the local cursor/clipboard.
///
/// `display` is an X display string (`None` = `$DISPLAY`).
pub fn server(display: Option<&str>) -> Result<(Receiver<Message>, Box<dyn Engine>), String> {
    #[cfg(target_os = "linux")]
    {
        let s = x11::Server::start(display)?;
        Ok((s.input, s.engine))
    }
    #[cfg(not(target_os = "linux"))]
    {
        unsupported::server()
    }
}

/// Build a client-side injector for the local machine.
pub fn client(display: Option<&str>) -> Result<Box<dyn Injector>, String> {
    #[cfg(target_os = "linux")]
    {
        x11::client_injector(display)
    }
    #[cfg(not(target_os = "linux"))]
    {
        unsupported::client()
    }
}