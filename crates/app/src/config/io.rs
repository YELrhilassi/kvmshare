//! Persistence of the role configs: loading, atomic saving, and where the
//! server and the client each look for their file.

use std::path::{Path, PathBuf};

use fs2::FileExt;

use super::Config;

impl Config {
    /// Persist this config to `path` atomically (temp file + rename), so
    /// a concurrent reader (the hot-reload watcher, the GUI) never sees
    /// a torn write.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        write_toml(path, self)
    }

    /// Load the config at `path`, creating a machine-accurate default
    /// ([`Config::for_this_machine`]) when nothing is there yet. The
    /// caller learns whether the file was created so it can say so.
    ///
    /// This is the only path a *first* server start goes through: a
    /// missing config is never an error (the server would otherwise die
    /// on a file it was about to create) and never a stale copy of
    /// another machine's layout.
    pub fn load_or_create(path: &Path) -> Result<(Self, bool), String> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let cfg: Config = toml::from_str(&text)
                    .map_err(|e| format!("parse {}: {e}", path.display()))?;
                cfg.validate()?;
                Ok((cfg, false))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Self::for_this_machine();
                let text = toml::to_string_pretty(&cfg)
                    .map_err(|e| format!("encode default config: {e}"))?;
                atomic_write(path, &text)?;
                Ok((cfg, true))
            }
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }
}

/// Serialize `value` as TOML and write it atomically. Shared by every
/// config type in this module, so the on-disk discipline (parent
/// directories, the cross-process lock, temp file + rename) is written
/// once instead of once per section.
pub(crate) fn write_toml<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let text = toml::to_string_pretty(value).map_err(|e| format!("encode config: {e}"))?;
    atomic_write(path, &text)
}

/// Write `text` to `path` atomically (temp file + rename) while holding
/// the cross-process config lock, creating parent directories.
///
/// Why the lock: two writers exist — this process (auto-trust, screen
/// correction) and the GUI (layout saves, trust edits). Both do
/// read-modify-write of the *whole* file; without a shared mutex a
/// GUI save and a server auto-trust can interleave and one write
/// erases the other's change (a trusted id vanishing, re-adding, and
/// vanishing again — visible to the user as connection flapping). The
/// lock file lives beside the config and is advisory; every writer in
/// this repo takes it, which is what makes it real.
fn atomic_write(path: &Path, text: &str) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let _lock = ConfigLock::acquire(path)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
}

/// The cross-process config lock: an flock (or LockFileEx on Windows)
/// on `<config>.lock`, held for the whole read-modify-write. Locking is
/// blocking — writers are rare and quick, and blocking is correct: the
/// alternative (failing) would drop a policy write silently.
struct ConfigLock(std::fs::File);

impl ConfigLock {
    fn acquire(config_path: &Path) -> Result<Self, String> {
        let mut p = config_path.as_os_str().to_owned();
        p.push(".lock");
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&p)
            .map_err(|e| format!("open {}: {e}", p.to_string_lossy()))?;
        f.lock_exclusive().map_err(|e| format!("lock {}: {e}", p.to_string_lossy()))?;
        Ok(ConfigLock(f))
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Where the server looks for its config when `--config` is not given:
/// the `KVMSHARE_CONFIG` env var, then `~/.config/kvmshare/`, then the
/// current directory.
pub fn default_config_path() -> PathBuf {
    role_config_path("KVMSHARE_CONFIG", "kvmshare-server.toml")
}

/// Where the **client** looks for its own config.
///
/// The client deliberately obeys the remote server's layout — screens,
/// positions and shortcuts are the server's business — but `[audio]`
/// describes *this* machine's hardware and consent, so it cannot come from
/// the other end of the wire. That is the entire content of the client's
/// config file, and the reason it is a separate file rather than a section
/// of the server's: the two roles are configured by different people on
/// different machines, and neither should be able to edit the other's.
///
/// Resolution mirrors [`default_config_path`] (env var, then the per-user
/// directory, then the current directory) with the client's own names, so
/// a machine that runs both roles keeps two files side by side instead of
/// one file that means different things depending on who read it.
pub fn client_config_path() -> PathBuf {
    role_config_path("KVMSHARE_CLIENT_CONFIG", "kvmshare-client.toml")
}

/// The shared resolution rule behind both paths above: an explicit env
/// var wins (the operator owns it), then the per-user `.config/kvmshare`
/// directory when the file is actually there, then a file beside the
/// working directory as the portable-install fallback.
fn role_config_path(env_var: &str, file_name: &str) -> PathBuf {
    if let Ok(p) = std::env::var(env_var) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(".config/kvmshare").join(file_name);
        if p.exists() {
            return p;
        }
    }
    PathBuf::from(file_name)
}