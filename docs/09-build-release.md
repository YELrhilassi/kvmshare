# 9. Building & releasing

Everything is driven from the repository root via the `Makefile` (and
`scripts/dev.sh` for the watch loop). The GUI is a separate Go module
under `gui/`; the Rust side is a Cargo workspace.

## 9.1 Quick reference

| Command | What it does |
|---------|--------------|
| `make build` | Compile everything: release Rust + Vite frontend + Go GUI |
| `make install` | Copy binaries, sample config and launcher into `~/.local` |
| `make dev` | Watch sources; rebuild + reinstall on every change |
| `make test` | Full Rust suite (`cargo test --workspace`) + Go GUI tests |
| `make release` | Portable release archives (Linux + Windows) into `dist/` |
| `make publish` | Tag-checked GitHub release upload |
| `make clean` / `make uninstall` | Remove artifacts / installed files (config kept) |

Binaries land in `~/.local/bin` by default — `kvmshare-server`,
`kvmshare-client` and `kvmshare-gui` become launchable from dmenu/rofi/
a terminal directly. Override with `PREFIX`:
`make install PREFIX=/usr/local`.

## 9.2 Input-device access (Linux)

The server isolates physical input devices at the kernel while the
cursor is on a client (see [Platform — evdev](06-platform.md#63-linux-x11-backend)).
That needs read access to `/dev/input`, granted **without user
commands**:

- `make install` runs `ensure-input-access`: if the devices are already
  readable it does nothing; otherwise it self-elevates through pkexec
  and writes a **generated** udev rule with the installing user's uid
  baked in as `OWNER` (plus `TAG+="uaccess"` for logind systems), and
  chowns the live device nodes — no group membership, no re-login, no
  restart. At most one privilege prompt, ever.
- `packaging/99-kvmshare-input.rules` is the *reference* shape of the
  rule; the installer generates the actual one.
- The GUI also triggers this silently when it becomes a server
  (`ensure_input_linux.go`), via the sibling `kvmshare-install
  --input-access`.

## 9.3 The dev loop

`scripts/dev.sh` (invoked by `make dev`) polls file mtimes every second
— dependency-free (no inotifywait/entr) — and runs `make install` when a
source file under `crates/` or `gui/` changed. Both Rust and Go builds
are incremental, so a triggered rebuild takes a couple of seconds; the
installed binaries are then current for immediate testing.

## 9.4 The release pipeline

`make release`:

1. Builds release Rust binaries, the Vite frontend, the GUI, the CLI
   installer (`kvmshare-install`) and the GUI installer
   (`kvmshare-installer`) for the host platform.
2. Builds the Windows GUI + installers (pure Go, `CGO_ENABLED=0`,
   `-H windowsgui` for no console).
3. Builds the Windows Rust binaries **when the mingw-w64 toolchain is
   present** (`x86_64-w64-mingw32-gcc` + `rustup target add
   x86_64-pc-windows-gnu`); without it, the Windows archive is omitted
   and a note is printed. `cargo check --target x86_64-pc-windows-msvc`
   works without a linker (type-check only).
4. Assembles `dist/`: per-platform archives
   (`kvmshare_<ver>_<os>_<arch>.tar.gz` / `.zip`), the standalone
   installers, and `SHA256SUMS`.

`make publish` requires an exact git tag (`git tag v0.1.0`) matching the
injected version and uploads the archives + installers + checksums as a
GitHub release.

The version is injected at link time into `selfupdate.Version`
(`-ldflags "-X .../selfupdate.Version=v0.1.0"`); untagged builds get
`v0.0.0-dev`, which the updater always treats as older than a published
release.

## 9.5 Installer & self-update (one implementation, two faces)

`gui/internal/` holds the single implementation of "get kvmshare onto
this machine", shared by two front ends:

- **`internal/selfupdate`** — fetches releases from GitHub
  (`DefaultUpstream = "YELrhilassi/kvmshare"`, overridable with
  `KVMSHARE_UPSTREAM`), parses `SHA256SUMS`, verifies archives, extracts
  (tar.gz / zip), and **replaces binaries in place** (rename-based, safe
  while running — the old file is renamed aside first). Everything is
  compiled Go — no shell scripts, no curl pipelines.
- **`internal/installer`** — the install flow: resolve release → download
  → verify → extract → apply → **desktop integration**:
  - Linux: the generated udev rule + a `.desktop` launcher.
  - Windows: the icon, Start-Menu + desktop shortcuts (WScript.Shell
    via base64-encoded PowerShell), and an Add/Remove Programs entry;
    `--uninstall` restores the UAC prompt policy kvmshare changed.
  - `Uninstall` removes binaries + desktop integration; the GUI
    installer elevates itself (UAC) for the HKLM steps.

The two faces:

- **`gui/cmd/kvmshare-install`** — the CLI bootstrap: one file you
  download; `./kvmshare-install` installs the latest release, re-running
  updates in place. Flags: `--tag`, `--check`, `--local DIR`, `--input-access`,
  `--uninstall`.
- **`gui/installer`** — a Wails GUI window with the same flow (progress
  bar driven by the `Phase`/`Log` callbacks; the front end polls a
  snapshot).

The **GUI's in-app update** (`gui/update.go`) uses the same
`internal/selfupdate` package to replace all three binaries
(`kvmshare-gui`, `kvmshare-server`, `kvmshare-client`) and restart into
the new version.

## 9.6 Packaging files

- `packaging/99-kvmshare-input.rules` — reference udev rule (see §9.2).
- `packaging/kvmshare.desktop` — the application launcher
  (`Exec=kvmshare-gui`, `Categories=Utility;Network;`).
- `packaging/install.sh` / `packaging/install.ps1` / `packaging/npm/kvmshare.js`
  — the terminal bootstrap scripts (see §9.7); shipped into `dist/` and
  the release by `make release`.

## 9.7 Terminal one-liners (the unsigned-binary path)

The one-line installs, per platform:

```sh
# Linux / macOS / WSL (sh, bash, zsh; curl or wget)
curl -fsSL https://github.com/YELrhilassi/kvmshare/releases/latest/download/install.sh | sh
```

```powershell
# Windows PowerShell (5.1+ or 7+)
irm https://github.com/YELrhilassi/kvmshare/releases/latest/download/install.ps1 | iex
```

```text
# Anywhere Node 18+ runs (also Windows without curl)
npx github:YELrhilassi/kvmshare
```

All three are **bootstrap** scripts: resolve the latest release tag,
download the platform's standalone `kvmshare-install` binary, verify its
sha256 against the release's `SHA256SUMS`, then hand off. The Go
installer downloads the platform archive, verifies it the same way,
applies binaries atomically (rename-based, safe over a running copy),
and performs the desktop integration (udev input access on Linux;
shortcuts, elevation tasks and firewall rules on Windows). Pin a
version: `sh install.sh v0.8.7`, `install.ps1 -Tag v0.8.7`,
`npx github:YELrhilassi/kvmshare v0.8.7`. Forks: set `KVMSHARE_UPSTREAM`.

Why this channel exists — and why it is the honest answer to unsigned
executables:

- **Windows SmartScreen** blocks unsigned exes downloaded through a
  browser, because such files carry Mark-of-the-Web. Files written by a
  script/terminal get no MOTW, so SmartScreen is never in the loop; the
  PowerShell script additionally runs `Unblock-File` on the staged
  binary.
- **macOS Gatekeeper** quarantines browser downloads; terminal-created
  files are not quarantined, so the verified binary runs without the
  "cannot be opened" wall.
- **Nothing is unverified**: both stages (bootstrap and Go installer)
  check sha256 against the release's `SHA256SUMS` before executing, and
  mismatch aborts with the two hashes printed. What the script path
  skips is the *OS UI wall for unsigned code*, not the integrity check.
- This is the same trust decision as any `curl | sh` tool: the channel
  is HTTPS to the release you named (or `latest`), and the checksum is
  fetched from that same release. A code-signing certificate is the
  longer-term fix; until the project signs releases, these scripts are
  the no-clicks path.

`--uninstall` works the same way (`sh install.sh --uninstall`,
`install.ps1 -Uninstall` passes through, `npx ... -- --uninstall`) and
removes binaries, manifest and desktop integration.

---

**Next:** [10. Testing](10-testing.md).