//! Session construction from the config, for the server.

use kvmshare_core::session::Session;

use crate::config::Config;

/// Build a session from a config, for the server.
///
/// The config's `[shortcuts]` section is applied **here**, not left to
/// the first hot-reload: the hot-reload watcher deliberately primes
/// itself with the file as it is now (so startup does not log a spurious
/// "layout reloaded"), which means the session would otherwise sit on
/// the default bindings until the *user's first edit* — every shortcut
/// recorded in the Layout page silently dead on arrival. Applying the
/// section at construction makes startup and reload paths agree.
pub fn session_from_config(cfg: &Config) -> Session {
    let layout = cfg.to_layout();
    let mut session = Session::new(layout, 0);
    session.set_bindings(cfg.shortcuts.clone());
    session.set_prefs(cfg.input.clone());
    session
}