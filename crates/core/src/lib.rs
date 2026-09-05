//! # kvmshare-core
//!
//! Platform-independent brain of kvmshare:
//!
//! * [`layout`] — the virtual desktop: screens, rectangles, adjacency math.
//! * [`session`] — the cursor model and the enter/leave/switch decisions
//!   that turn raw motion into protocol messages.
//! * [`server`] — the TCP server that hosts the session and serves clients.
//! * [`client`] — the TCP client that receives events and injects them.
//!
//! The core knows nothing about X11, Windows, or macOS. All OS work lives
//! in the `kvmshare-platform` crate and is plugged in through the small
//! traits defined here.

pub mod client;
pub mod clipboard;
pub mod layout;
pub mod motion;
pub mod server;
pub mod session;
pub mod time;
pub mod transport;
pub mod udp;

pub use layout::{Direction, Layout};
pub use session::{Action, Session};

/// Where the cursor currently is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// On the local (server) screen.
    Local,
    /// On a remote client screen with the given id.
    Remote(u8),
}