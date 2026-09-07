//! A stable machine id for this installation.
//!
//! Every kvmshare machine gets a random, permanent id on first run,
//! stored in the state directory as `machine.id` (16 bytes, hex — 32
//! characters). It travels in the client's `Hello` (so a server can
//! trust a specific machine regardless of its screen name) and in the
//! server's `Welcome` (so a client can trust a specific server for
//! discovery / auto-connect). The GUI reads the same file so the ids a
//! user sees in the UI are exactly what goes on the wire.

use std::io::Write;
use std::path::PathBuf;

/// File name inside the state directory.
const MACHINE_ID_FILE: &str = "machine.id";

/// Read this machine's id, creating a fresh one if none exists yet.
///
/// The id is written atomically (tmp + rename) so two processes racing
/// on a first run (e.g. GUI and a role binary) can never observe a torn
/// file; a lost race simply reads the winner's file.
pub fn machine_id(state_dir: &PathBuf) -> String {
    let path = state_dir.join(MACHINE_ID_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let id = text.trim();
        if !id.is_empty() {
            return id.to_owned();
        }
    }
    // Fresh id: 16 random bytes, hex-encoded (32 chars). `getrandom` is
    // the OS CSPRNG on every supported platform.
    let mut bytes = [0u8; 16];
    if getrandom::getrandom(&mut bytes).is_err() {
        // No CSPRNG (exotic sandbox): fall back to a time+pid mix so the
        // machine still has *a* stable id (trust is best-effort then).
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let p = std::process::id() as u64;
        for (i, chunk) in [t, p, t.wrapping_mul(0x9E37_79B9_7F4A_7C15)].iter().enumerate() {
            bytes[i * 8..(i + 1) * 8].copy_from_slice(&chunk.to_le_bytes());
        }
    }
    let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let _ = std::fs::create_dir_all(state_dir);
    let tmp = state_dir.join(format!("{MACHINE_ID_FILE}.tmp"));
    if std::fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(id.as_bytes()))
        .and_then(|_| std::fs::rename(&tmp, &path))
        .is_err()
    {
        // Unwritable state dir: the process still needs *an* id. Return
        // the generated one anyway — it just won't survive a restart.
        return id;
    }
    id
}