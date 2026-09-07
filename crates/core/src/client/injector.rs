//! The platform hooks a client calls to affect the local machine.

use kvmshare_protocol::message::{KeyKind, ScreenInfo};

/// The platform hook the client calls to affect the local machine.
///
/// The server controls this machine, so every call here is "make the
/// local machine do what the server asked".
pub trait Injector: Send {
    /// The client's screen shape (resolution and scale). Re-queried by
    /// the sync thread so resolution changes are noticed and reported
    /// back to the server.
    fn screen_info(&mut self) -> ScreenInfo;
    /// Move the local cursor to local screen pixels `(x, y)` (absolute
    /// placement: used when control *enters*, for explicit positioning,
    /// and by absolute-motion backends for the whole motion stream).
    fn move_cursor(&mut self, x: i32, y: i32);
    /// Apply relative cursor motion. The client OS applies its own
    /// pointer transform (acceleration / speed settings) to relative
    /// input, so the shared cursor feels exactly like a physical mouse on
    /// this machine — the model every mature KVM uses.
    fn move_rel(&mut self, dx: i32, dy: i32);
    /// Whether this backend places the cursor **absolutely** for motion
    /// (each received delta accumulates into a commanded position and the
    /// cursor is set exactly there each tick) rather than forwarding
    /// relative input for the OS to transform.
    ///
    /// Absolute placement bypasses the client OS's pointer acceleration
    /// entirely: the shared cursor lands exactly where commanded, the OS
    /// can never over-run the hand, and a lost frame self-heals (the
    /// next placement lands the whole command). Backends that return
    /// `true` skip the closed-loop correction — the placement *is* the
    /// loop. The server compensates for its own pointer transform by
    /// scaling the counts it sends (see `GainTracker`), so the client
    /// cursor mirrors the server cursor pixel-for-pixel.
    fn absolute_motion(&self) -> bool {
        false
    }
    /// The cursor's *real* current position in local screen pixels.
    /// Reported to the server on a cadence while being controlled, so
    /// the server knows exactly where the shared cursor sits for edge
    /// crossings.
    fn cursor_position(&mut self) -> (i32, i32);
    fn button(&mut self, button: u8, pressed: bool);
    fn wheel(&mut self, dx: i32, dy: i32);
    /// Press/release/repeat a key, addressed by its canonical USB HID
    /// usage id (the platform backend maps it to the local key identity).
    fn key(&mut self, kind: KeyKind, key: u32);
    /// Control has entered this machine: hide the local cursor so the
    /// server's stream is the only visible one.
    fn enter(&mut self);
    /// Control has left this machine: show the local cursor again.
    fn leave(&mut self);
    /// Called by the motion thread once per steering tick while this
    /// machine is controlled. Backends with a remote-control watchdog
    /// (Windows input isolation) use it as the liveness heartbeat: if
    /// steering stops while the machine is being driven remotely, the
    /// watchdog releases local input so the machine is never trapped.
    /// Default: nothing.
    fn steer_heartbeat(&mut self) {}
    /// Force-restore local input ownership after a client-side stall.
    /// The client's supervisor thread calls this when a worker wedges
    /// (e.g. a blocking OS call holds the injector lock and the cursor
    /// can no longer be steered). Backends that silence local hardware
    /// while remotely controlled must undo that here so the machine is
    /// never left trapped: the user's own mouse and keyboard always
    /// work again, even if the shared session has to restart.
    /// Default: nothing (backends without hardware silencing need
    /// nothing to undo).
    fn emergency_release(&mut self) {}
    /// Whether the OS reported this machine resuming from sleep since
    /// the session started. A resume invalidates the remote-control
    /// state by definition (the user is wherever they are; the
    /// pre-sleep "cursor on this machine" state cannot be trusted), so
    /// the client ends the session and reconnects — a clean, local
    /// start instead of a machine left with its input silenced and its
    /// cursor hidden. Read once per session (the backend clears it).
    /// Default: never.
    fn system_resumed(&mut self) -> bool {
        false
    }
    /// Whether the OS currently shows a desktop that cannot receive
    /// injected input — the UAC secure desktop on Windows. While it is
    /// up, this machine must return to its user (the prompt needs a
    /// physical answer and no process can inject into it), so the client
    /// ends the session and reconnects; the same live poll also drives
    /// the isolation pump's immediate release. A **live check**, unlike
    /// [`Injector::system_resumed`]: the session ends for exactly as
    /// long as the condition holds. Default: never.
    fn secure_desktop_active(&mut self) -> bool {
        false
    }
}

/// Clipboard access, split from [`Injector`] **on purpose**: reading or
/// writing the system clipboard can block indefinitely while another
/// process holds it open (some apps open it and never close it), so a
/// clipboard call must never share a lock with the cursor. The client
/// gives the clipboard its own lock, serviced by its own thread — a
/// stalled clipboard read delays only clipboard sync, never a cursor
/// placement.
///
/// The trait itself lives in [`crate::clipboard`] — the server role
/// needs the identical contract — and is re-exported here so the
/// client-facing name [`Clipboard`] is unchanged.
pub use crate::clipboard::Clipboard;