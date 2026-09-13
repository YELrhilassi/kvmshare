//! Build script: stamp a **build id** into every binary of this crate.
//!
//! The build id is a 64-bit FNV-1a hash of (hostname, user, build
//! timestamp). It is stable for one build and different for the next —
//! exactly the property the install manifest needs: the GUI checks that
//! the server, client and GUI on this machine were built together by
//! comparing their ids, so a half-updated install (a deploy that
//! replaced the binaries but not a sidecar manifest, a partial copy, an
//! old binary that PATH lookup resurfaced) is *detected* instead of
//! running mixed versions.
//!
//! The env var is emitted as `cargo:rustc-env=` so binaries read it
//! with `env!("KVMSHARE_BUILD_ID")` — a compile-time constant, no
//! runtime cost, and `--version` can print it.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // Rerun whenever the sources change (normal) — the id is stamped per
    // build, not per source state, so a rebuild for any reason refreshes
    // it. Env vars are deliberately *not* rerun triggers: the id may
    // change mid-session and that is fine (nothing caches it).
    println!("cargo:rerun-if-changed=build.rs");

    let host = Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // FNV-1a 64-bit: tiny, deterministic, plenty for "were these built
    // together" — collisions between two builds a user actually has are
    // not a realistic event, and the check errs toward "consistent".
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in format!("{host}|{user}|{secs}").bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }

    println!("cargo:rustc-env=KVMSHARE_BUILD_ID={hash:016x}");
}
