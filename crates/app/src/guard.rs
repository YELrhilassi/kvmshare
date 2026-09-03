//! Role locking: a machine runs **one** kvmshare role at a time.
//!
//! Each binary takes an exclusive `flock(2)` lock on a file in the state
//! directory when it starts, and refuses to start if the *other* role is
//! already running. Two properties matter here:
//!
//! * **Orphan-safe** — a `flock` dies with its process. A killed or
//!   crashed instance can never leave a stale lock behind, so there are
//!   no ghost instances and no manual cleanup.
//! * **Backend-enforced** — the GUI and the binaries agree on this, but
//!   the lock is authoritative even if someone starts a binary by hand
//!   while the other role is running.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;

pub const ROLE_SERVER: &str = "server";
pub const ROLE_CLIENT: &str = "client";

/// Holds this process's role lock for its lifetime. Dropping releases it.
pub struct RoleGuard {
    _file: File,
}

/// Where per-machine state lives. `KVMSHARE_STATE` overrides it (tests,
/// unusual installs); otherwise XDG's default `~/.local/state/kvmshare`.
pub fn state_dir() -> PathBuf {
    if let Ok(p) = std::env::var("KVMSHARE_STATE") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home).join(".local/state/kvmshare"),
        _ => PathBuf::from(".kvmshare-state"),
    }
}

/// Take the role lock for `role`. Errors when this role is already
/// running here, or when the other role is — a machine can't be both.
pub fn acquire(role: &str) -> Result<RoleGuard, String> {
    let dir = state_dir();
    acquire_in(&dir, role)
}

/// Testable core of [`acquire`]: take the role lock inside `dir`.
pub fn acquire_in(dir: &Path, role: &str) -> Result<RoleGuard, String> {
    if role != ROLE_SERVER && role != ROLE_CLIENT {
        return Err(format!("unknown role {role:?}"));
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("create state dir {}: {e}", dir.display()))?;

    // Take our own lock first: only one instance of this role may run.
    let ours_path = dir.join(format!("{role}.lock"));
    let ours = lock_file(&ours_path).map_err(|_| {
        format!(
            "another kvmshare {role} is already running on this machine ({} is locked)",
            ours_path.display()
        )
    })?;

    // Then make sure the other role is not running. Probe its lock; if we
    // can take it, the other role is free and we immediately release the
    // probe again.
    let other = if role == ROLE_SERVER { ROLE_CLIENT } else { ROLE_SERVER };
    let other_path = dir.join(format!("{other}.lock"));
    match lock_file(&other_path) {
        Ok(probe) => drop(probe),
        Err(_) => {
            drop(ours);
            return Err(format!(
                "a kvmshare {other} is already running on this machine — a machine runs as a server or a client, not both ({} is locked)",
                other_path.display()
            ));
        }
    }

    // Record our pid inside the lock file so a controller (the GUI) can
    // signal this instance later — e.g. to stop a background server.
    let mut f = ours;
    let _ = f.set_len(0);
    let _ = writeln!(f, "{}", std::process::id());
    let _ = f.flush();

    Ok(RoleGuard { _file: f })
}

fn lock_file(path: &Path) -> io::Result<File> {
    let f = OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
    // Non-blocking exclusive lock — flock(2) on Unix, LockFileEx on
    // Windows (via fs2). Fails immediately if another process holds it.
    // The lock is released automatically when `f` (or the process) goes
    // away, so a crash can never leave a stale lock behind.
    f.try_lock_exclusive()?;
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_locks_are_exclusive() {
        let dir = std::env::temp_dir().join(format!("kvmshare-guard-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // First server takes the lock.
        let g1 = acquire_in(&dir, ROLE_SERVER).expect("first server should lock");
        // A second server on the same machine must be refused.
        assert!(acquire_in(&dir, ROLE_SERVER).is_err());
        // A client is refused while the server runs.
        assert!(acquire_in(&dir, ROLE_CLIENT).is_err());

        // Dropping releases everything: the client can then start.
        drop(g1);
        let g2 = acquire_in(&dir, ROLE_CLIENT).expect("client should lock after server stops");
        assert!(acquire_in(&dir, ROLE_SERVER).is_err());
        assert!(acquire_in(&dir, ROLE_CLIENT).is_err());
        drop(g2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn locks_survive_the_file_existing() {
        // Lock files accumulate; opening them again must not fail.
        let dir = std::env::temp_dir().join(format!("kvmshare-guard-test2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let g1 = acquire_in(&dir, ROLE_SERVER).unwrap();
        drop(g1);
        let g2 = acquire_in(&dir, ROLE_SERVER).expect("re-acquire after drop");
        drop(g2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}