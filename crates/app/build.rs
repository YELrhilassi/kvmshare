//! Build script: stamp a **build id** into every binary of this crate.
//!
//! The build id fingerprints the **source tree**: sha256 over the
//! workspace version plus the workspace member list, truncated to 64
//! bits. It is deterministic — every binary linked from one workspace
//! reports the same id no matter when it was compiled, so cargo's
//! incremental rebuilds (which refresh some crates and not others)
//! can never split one release into "different builds".
//!
//! A timestamp hash was tried first and was exactly wrong for this:
//! each cargo invocation got a fresh id, so a release built in one
//! `cargo build` looked internally consistent but the next build —
//! reusing freshly compiled crates — reported different ids per
//! binary, and the install check refused every install.
//!
//! The Makefile stamps the GUI (Go) with the same id, parsing the same
//! two facts from the same file — this workspace's Cargo.toml is the
//! single source of truth for both toolchains.

use sha2::{Digest, Sha256};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The id changes exactly when the workspace's version or membership
    // changes — both live in the root manifest two directories up
    // (this build script runs with cwd = crates/app; ../ is crates/).
    println!("cargo:rerun-if-changed=../../Cargo.toml");

    // The version is part of the id: a version bump is a new build even
    // when no other source moved. (Inherited from the workspace root,
    // so it equals the root manifest's `version` — the same value the
    // Makefile parses.)
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();

    let members = workspace_members();

    let mut h = Sha256::new();
    h.update(version.as_bytes());
    h.update(b"|");
    for m in &members {
        h.update(m.as_bytes());
        h.update(b"|");
    }
    let digest = h.finalize();
    let id: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();

    println!("cargo:rustc-env=KVMSHARE_BUILD_ID={id}");
}

/// The workspace member list, parsed from the root Cargo.toml (the
/// `members = [...]` array, quotes/commas stripped, sorted). The
/// Makefile parses the same array with sed/tr — keep both simple and
/// in lockstep.
fn workspace_members() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(root) = std::fs::read_to_string("../../Cargo.toml") {
        if let Some(start) = root.find("members") {
            if let Some(open) = root[start..].find('[') {
                let rest = &root[start + open + 1..];
                if let Some(close) = rest.find(']') {
                    for part in rest[..close].split(',') {
                        let m = part.trim().trim_matches('"').trim();
                        if !m.is_empty() {
                            out.push(m.to_string());
                        }
                    }
                }
            }
        }
    }
    out.sort();
    out
}
