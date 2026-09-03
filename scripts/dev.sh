#!/usr/bin/env bash
#
# kvmshare dev loop: watch the sources and rebuild + reinstall on every
# change, so the installed binaries are always current for testing.
#
# Dependency-free by design: it polls file mtimes instead of requiring
# inotifywait/entr/watchexec. Both Rust and Go builds are incremental, so
# a triggered rebuild takes a couple of seconds at most.
#
# Usage:  make dev        (from the repo root)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAMP="$ROOT/target/.dev-watch.stamp"

# What we watch: every source file that affects the build.
# Build outputs (node_modules, dist, target) are excluded so the watcher
# doesn't retrigger on its own rebuilds.
WATCH_GLOBS=(
  -name '*.rs'
  -o -name '*.go'
  -o -name '*.html'
  -o -name '*.js'
  -o -name '*.jsx'
  -o -name '*.ts'
  -o -name '*.tsx'
  -o -name '*.css'
  -o -name '*.toml'
  -o -name '*.json'
)

changed() {
  # Any watched file modified after the stamp?
  find "$ROOT/crates" "$ROOT/gui" -type f \( "${WATCH_GLOBS[@]}" \) \
    ! -path '*/node_modules/*' ! -path '*/dist/*' ! -path '*/target/*' \
    -newer "$STAMP" -print -quit | grep -q .
}

rebuild() {
  echo "[kvmshare-dev] $(date +%H:%M:%S) change detected — rebuilding..."
  if (cd "$ROOT" && make install); then
    echo "[kvmshare-dev] $(date +%H:%M:%S) installed ✓  (launch: kvmshare-server / kvmshare-client / kvmshare-gui)"
  else
    echo "[kvmshare-dev] $(date +%H:%M:%S) build failed — fixing the error will retry"
  fi
  touch "$STAMP"
}

mkdir -p "$ROOT/target"

# Build once up front so the loop starts with everything current.
touch "$STAMP"
echo "[kvmshare-dev] initial build..."
(cd "$ROOT" && make install)
echo "[kvmshare-dev] watching $ROOT/crates and $ROOT/gui (Ctrl-C to stop)"

while true; do
  sleep 1
  if changed; then
    rebuild
  fi
done