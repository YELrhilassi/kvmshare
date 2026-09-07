//! Session construction from the config, for the server.

use kvmshare_core::session::Session;

use crate::config::Config;

/// Build a session from a config, for the server.
pub fn session_from_config(cfg: &Config) -> Session {
    let layout = cfg.to_layout();
    Session::new(layout, 0)
}