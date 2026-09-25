#!/bin/sh
# kvmshare bootstrap installer (POSIX shell: Linux, macOS, WSL, Git Bash).
#
#   curl -fsSL https://github.com/YELrhilassi/kvmshare/releases/latest/download/install.sh | sh
#   sh install.sh                 # latest
#   sh install.sh v0.8.7          # pinned version
#   KVMSHARE_UPSTREAM=owner/repo sh install.sh
#
# This script does NOT implement installation. It gets the real installer
# — the Go `kvmshare-install` binary published with every release — onto
# the machine **verified**, then hands off to it (it downloads the
# platform archive, verifies it, applies binaries atomically, and does
# the desktop integration: udev input access on Linux, elevation tasks
# and shortcuts on Windows).
#
# Verification, in order of preference:
#   1. the release's SHA256SUMS file (published by `make publish`),
#   2. GitHub's per-asset `digest` field from the release API (HTTPS),
#      which is how releases that predate SHA256SUMS still verify.
# No unverified byte is ever executed.
#
# Downloads, in order of size:
#   1. the standalone `kvmshare-install` asset (a few MB), when the
#      release carries it (v0.8.8+),
#   2. otherwise the full platform archive, from which the installer is
#      extracted and run with `--local` (works on every release).
# Re-running is safe: the installer updates in place.

set -eu

REPO="${KVMSHARE_UPSTREAM:-YELrhilassi/kvmshare}"
TAG=""
if [ "$#" -gt 0 ]; then
    TAG="$1"
    shift # the rest of the args pass through to kvmshare-install
fi
API="https://api.github.com/repos/${REPO}/releases"

say() { printf 'kvmshare-install: %s\n' "$*"; }
die() { printf 'kvmshare-install: %s\n' "$*" >&2; exit 1; }

# --- tool discovery --------------------------------------------------------
if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL -H "User-Agent: kvmshare-bootstrap" "$1" -o "$2"; }
    fetch_stdout() { curl -fsSL -H "User-Agent: kvmshare-bootstrap" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
    fetch_stdout() { wget -qO - "$1"; }
else
    die "need curl or wget to download the installer"
fi

# --- platform --------------------------------------------------------------
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64 | Linux-amd64) plat="linux_amd64" ;;
    Linux-aarch64 | Linux-arm64) plat="linux_arm64" ;;
    Linux-armv7l) plat="linux_arm64" ;; # not built yet; clearer error below
    Darwin-arm64) plat="darwin_arm64" ;;
    Darwin-x86_64) plat="darwin_amd64" ;;
    MING* | MSYS* | CYGWIN*) die "use the PowerShell one-liner on Windows (docs 9.7)" ;;
    *) die "unsupported platform: $(uname -s)-$(uname -m)" ;;
esac

# --- release metadata: tag + per-asset digests (one API call) --------------
if [ -n "$TAG" ]; then
    say "resolving release ${TAG}..."
    json=$(fetch_stdout "${API}/tags/${TAG}") || die "release ${TAG} not found in ${REPO}"
else
    say "resolving the latest release..."
    json=$(fetch_stdout "${API}/latest") || die "GitHub unreachable (or rate-limited)?"
fi
TAG=$(printf '%s' "$json" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
[ -n "$TAG" ] || die "could not read the release tag"
say "installing ${TAG}"

# Digest extraction without jq: asset names and their digests appear in
# the JSON in the same order, each exactly once, and only asset names
# start with "kvmshare". Zip the two lists; take the digest for our asset.
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
printf '%s' "$json" | grep -o '"name": *"kvmshare[^"]*"' | sed 's/.*: *"//;s/"$//' > "${tmp}/names"
printf '%s' "$json" | grep -o '"digest": *"sha256:[0-9a-f]*"' | sed 's/.*sha256://;s/"$//' > "${tmp}/digests"
digest_of_asset() {
    i=1
    while :; do
        n=$(sed -n "${i}p" "${tmp}/names")
        [ -n "$n" ] || break
        if [ "$n" = "$1" ]; then
            sed -n "${i}p" "${tmp}/digests"
            return 0
        fi
        i=$((i + 1))
    done
    return 1
}
verify() { # verify <file> <asset-name> — checks SHA256SUMS then API digest
    _f="$1"; _a="$2"; _want=""
    if fetch "${DLURL}/SHA256SUMS" "${tmp}/SHA256SUMS" 2>/dev/null; then
        _want=$(grep " ${_a}\$" "${tmp}/SHA256SUMS" 2>/dev/null | awk '{print $1}' | head -n 1)
    fi
    if [ -z "$_want" ]; then
        _want=$(digest_of_asset "$_a" || true)
    fi
    [ -n "$_want" ] || return 1
    if command -v sha256sum >/dev/null 2>&1; then
        _got=$(sha256sum "$_f" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        _got=$(shasum -a 256 "$_f" | awk '{print $1}')
    else
        die "need sha256sum or shasum to verify downloads"
    fi
    [ "$_got" = "$_want" ] || die "checksum mismatch for ${_a}
  expected: ${_want}
  got:      ${_got}
(the release may be corrupted, or this download was tampered with)"
    say "checksum ok (${_a})"
    return 0
}

DLURL="https://github.com/${REPO}/releases/download/${TAG}"
installer_asset="kvmshare-install_${TAG}_${plat}"
archive_asset="kvmshare_${TAG}_${plat}.tar.gz"

# --- path 1: the standalone installer (v0.8.8+) ----------------------------
if printf '%s' "$json" | grep -q "\"name\": *\"${installer_asset}\""; then
    say "downloading ${installer_asset}..."
    fetch "${DLURL}/${installer_asset}" "${tmp}/kvmshare-install" ||
        die "download failed"
    verify "${tmp}/kvmshare-install" "$installer_asset"
    chmod +x "${tmp}/kvmshare-install"
    say "handing off to the installer..."
    # Terminal-created files carry no macOS quarantine attribute, so the
    # verified binary runs without Gatekeeper's unsigned-binary wall.
    exec "${tmp}/kvmshare-install" "$@"
fi

# --- path 2: full archive (any release, e.g. v0.8.7) -----------------------
say "this release has no standalone installer asset — using the full archive"
say "downloading ${archive_asset}..."
fetch "${DLURL}/${archive_asset}" "${tmp}/release.tar.gz" ||
    die "download failed — does ${TAG} carry ${archive_asset}?"
verify "${tmp}/release.tar.gz" "$archive_asset"
tar -xzf "${tmp}/release.tar.gz" -C "$tmp"
found=$(find "$tmp" -type f -name kvmshare-install | head -n 1)
[ -n "$found" ] || die "archive has no kvmshare-install inside — unexpected layout"
chmod +x "$found"
archdir=$(dirname "$found")
say "handing off to the installer (--local)..."
exec "$found" --local "$archdir" "$@"
