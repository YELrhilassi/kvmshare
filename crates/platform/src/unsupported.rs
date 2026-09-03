//! Stubs for platforms without a backend yet.
//!
//! The crate compiles everywhere; trying to *run* on an unsupported OS
//! fails with a clear error instead of a crash. Adding a backend is a
//! matter of implementing `core::server::Engine` and `core::client::Injector`
//! for the new OS and wiring it into `lib.rs`.

use std::sync::mpsc::Receiver;

use kvmshare_core::client::Injector;
use kvmshare_core::server::Engine;
use kvmshare_protocol::message::Message;

const MSG: &str = "kvmshare: this OS does not have a platform backend yet (Linux/X11 is implemented)";

pub fn server() -> Result<(Receiver<Message>, Box<dyn Engine>), String> {
    Err(MSG.into())
}

pub fn client() -> Result<Box<dyn Injector>, String> {
    Err(MSG.into())
}