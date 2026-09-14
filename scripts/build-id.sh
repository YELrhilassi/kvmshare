#!/bin/sh
# The workspace build id: sha256 over (version, then each workspace
# member from the root Cargo.toml, each followed by '|'), truncated to
# 16 hex chars. crates/app/build.rs computes the same fingerprint in
# Rust from the same file — the single source of truth both toolchains
# parse — so the Go GUI and the Rust binaries of one release report
# one identical build id (the fact the install check verifies).
#
# Usage: build-id.sh [path-to-workspace-root]   (default: script dir)

set -eu

root="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
manifest="$root/Cargo.toml"

# Flatten the manifest to one line: the members array may span one
# line or many, and the captures below must stop at their closing
# brackets wherever they sit.
flat=$(tr '\n' ' ' < "$manifest")

# Workspace version: the first `version = "..."` after the
# [workspace.package] header.
version=$(printf '%s' "$flat" |
    sed -n 's/.*\[workspace\.package\][^"]*version *= *"\([^"]*\)".*/\1/p' |
    head -1)

# Workspace members: the contents of the `members = [...]` array,
# comma-split, stripped, sorted.
members=$(printf '%s' "$flat" |
    sed -n 's/.*members *= *\[\([^]]*\)\].*/\1/p' |
    head -1 |
    tr ',' '\n' | tr -d '" ' | sed '/^$/d' | sort | tr '\n' '|')

printf '%s|%s' "$version" "$members" | sha256sum | cut -c1-16
