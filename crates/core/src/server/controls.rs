//! The app layer's control channel into a running server.
//!
//! The server binary owns the config file and its watcher; the server
//! core owns the live state. [`Control`] is the one-way message channel
//! between them: each variant carries a change that can be applied live,
//! and each variant documents the one side effect a layout edit must
//! never trigger (the grab re-arm, the client disconnects, the audio
//! swap), which is why they travel separately rather than as one
//! "config changed" blob.

use std::sync::Arc;

use crate::layout::Layout as Desktop;

/// Control messages from the app layer (never travel over the wire).
#[derive(Debug)]
pub enum Control {
    /// The config changed on disk — adopt this new desktop layout,
    /// shortcut bindings and input preferences now.
    Reload(Desktop, crate::actions::BindSection, crate::input::InputPrefs),
    /// The `[network]` policy changed on disk — adopt it now. Side
    /// effect a layout edit must never have: any *connected* client
    /// whose machine id is in the new `revoked_ids` is disconnected on
    /// the spot.
    SetPolicy(crate::server::policy::Policy),
    /// Send an operational command to one connected client, looked up by
    /// its screen name. `command` is a [`kvmshare_protocol::id::control`]
    /// constant. The GUI writes these via the `server.cmd` control file.
    ClientCommand { name: String, command: u8 },
    /// The `[media]` routing policy changed on disk — adopt it now, and
    /// re-arm (or release) the media-key grab to match. The grab is the
    /// one side effect every other reload path must never touch.
    SetMediaPrefs(crate::media::MediaPrefs),
    /// The `[audio]` configuration changed on disk — adopt it now. The
    /// audio link is re-settled by the connection paths from the shared
    /// setup, so the only immediate duty is swapping the setup itself.
    SetAudioOptions(Option<Arc<crate::server::audio::ServerAudio>>),
}
