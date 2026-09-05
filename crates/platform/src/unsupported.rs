//! Stubs for platforms without a backend yet.
//!
//! The crate compiles everywhere; trying to *run* on an unsupported OS
//! fails with a clear error instead of a crash. Adding a backend is a
//! matter of implementing `core::server::Engine`, `core::client::Injector`
//! and the shared `core::clipboard::Clipboard` for the new OS and wiring
//! it into `lib.rs`.

use std::sync::mpsc::Receiver;

use kvmshare_core::client::{Clipboard, Injector};
use kvmshare_core::server::Engine;
use kvmshare_protocol::message::Message;

const MSG: &str = "kvmshare: this OS does not have a platform backend yet (Linux/X11 is implemented)";

/// A clipboard that never works — only reachable on unsupported OSes,
/// where [`server`] errors before anything could use it. Exists so the
/// type contract is satisfied on every platform.
#[allow(dead_code)]
struct StubClipboard;

impl Clipboard for StubClipboard {
    fn set(&mut self, _mime: &str, _data: &[u8]) {}
    fn get(&mut self) -> Option<(String, Vec<u8>)> {
        None
    }
    fn last_injected(&mut self) -> Option<(String, Vec<u8>)> {
        None
    }
}

pub fn server(
    _display: Option<&str>,
) -> Result<(Receiver<Message>, Box<dyn Engine>, Box<dyn Clipboard>), String> {
    Err(MSG.into())
}

pub fn client(
    _display: Option<&str>,
) -> Result<(Box<dyn Injector>, Box<dyn Clipboard>), String> {
    Err(MSG.into())
}
