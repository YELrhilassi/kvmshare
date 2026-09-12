//! The platform hook the server calls to control the *local* machine,
//! and the shared clipboard handle.

use std::sync::{Arc, Mutex};

/// The platform hook the server calls to control the *local* machine.
pub trait Engine: Send {
    /// Warp the local cursor to a local-screen position.
    fn warp_local(&mut self, x: i32, y: i32);
    /// Take (or release) exclusive ownership of the local keyboard and
    /// pointer while the cursor is on a client screen. Without it, the
    /// same physical input would act on the local desktop *and* be
    /// forwarded — clicks and typing would land on both machines at
    /// once. A best-effort call: platforms that cannot grab (yet) simply
    /// do nothing.
    fn grab_input(&mut self, grabbed: bool);
    /// Isolate (or release) the physical input devices from the local
    /// desktop entirely while the cursor is on a client — stronger than
    /// [`Engine::grab_input`], because it also stops *raw* event
    /// delivery to apps that read it directly (browsers, smooth-scroll
    /// terminals), which no grab can suppress. Best-effort: platforms
    /// without kernel device isolation do nothing.
    fn isolate_input(&mut self, _isolated: bool) {}

    /// Hide/show the local cursor while away / at home.
    fn show_local_cursor(&mut self, visible: bool);

    /// Publish the keyboard chords the capture layer must **intercept at
    /// the OS level**: a chord bound to a kvmshare action has to beat
    /// whatever the desktop would do with it (Win+Tab, media keys, …),
    /// and only the platform capture sits early enough to guarantee
    /// that. Each pair is `(mods, key)` — `key` is the chord's canonical
    /// HID usage, `mods` a 4-bit mask: bit0 ctrl, bit1 alt, bit2 shift,
    /// bit3 meta (the same order [`crate::actions::Mods`] serializes
    /// in). Called once at startup and again on every config reload;
    /// replaces the previous set. Best-effort: a platform without an
    /// interception mechanism does nothing, and the chord still resolves
    /// through the action engine when the event reaches the session —
    /// it may just lose a race against an OS binding.
    fn set_bound_chords(&mut self, _chords: Vec<(u8, u32)>) {}
}

/// A shared handle to the server's clipboard service.
///
/// The clipboard is deliberately **not** part of [`Engine`]: reading or
/// writing the system clipboard can block for a long time (an X11
/// selection owner busy or gone, another process holding the clipboard
/// open), and the engine lock serializes the *entire input path* — a
/// clipboard call under that lock would freeze every cursor motion on
/// every client for as long as the call blocks. The clipboard lives
/// behind its own lock instead, serviced by its own thread (the app's
/// poller) and only ever touched by clipboard work.
pub type ServerClipboard = Arc<Mutex<Box<dyn crate::clipboard::Clipboard>>>;