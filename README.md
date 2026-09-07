# kvmshare

Share one keyboard and mouse across your machines. A from-scratch,
cross-platform KVM — one machine (the **server**) shares its keyboard
and mouse with other machines (**clients**) across a virtual desktop:
move the cursor off one screen's edge and it appears on the neighbor,
taking keyboard and clipboard with it.

Built in **Rust** (protocol, core logic, OS backends, CLI) with a
**Wails v3** desktop GUI (React + TypeScript). Linux/X11 and Windows are
implemented; macOS and Wayland slot in through the same seams
([platform docs](docs/06-platform.md)).

## Install (end users)

Download the installer for your platform from the
[GitHub releases](https://github.com/YELrhilassi/kvmshare/releases) page
and run it — it fetches the release archive, verifies the checksum,
installs everything (desktop entry, shortcuts, input access) and can
update itself in place. On Linux:

```bash
curl -sL -o kvmshare-install \
  https://github.com/YELrhilassi/kvmshare/releases/download/v0.1.0/kvmshare-install_v0.1.0_linux_amd64
chmod +x kvmshare-install
./kvmshare-install
```

The GUI also updates in place (Home page → version line → check for
updates).

## Quick start (from source)

Requirements: Rust, Go, Node, and for the Linux GUI GTK4 + WebKitGTK 6
dev packages plus a D-Bus session.

```bash
make build       # release Rust + frontend + GUI
make install     # binaries to ~/.local/bin (on PATH), launcher, input access
make dev         # watch sources; rebuild + reinstall on every change
```

Then, on the machine whose keyboard/mouse you share:

```bash
kvmshare-server          # creates a machine-accurate layout on first start
```

and on a controlled machine:

```bash
kvmshare-client 192.168.1.86:24800
```

That's it — move the cursor to the shared edge and it crosses over. The
GUI (`kvmshare-gui`) manages roles, the layout, logs and updates; it
runs in the tray and roles keep running when it closes.

## Documentation

The docs are written for a developer joining the project — no Rust or Go
background required — and take you from *what the software is* to *how
every layer works*:

- [Docs index](docs/README.md) — reading order + the one-paragraph mental model
- [1. Overview](docs/01-overview.md) — concepts and vocabulary
- [2. Architecture](docs/02-architecture.md) — data flow, threading, design decisions
- [3. Codebase tour](docs/03-codebase-tour.md) — where everything lives, entry points
- [4. Wire protocol](docs/04-wire-protocol.md) — the binary format, frame by frame
- [5. Core crate](docs/05-core.md) — layout, session brain, motion, server, client
- [6. Platform crate](docs/06-platform.md) — X11, evdev, Windows, key tables
- [7. App crate & CLI](docs/07-app.md) — the two binaries, config, role locking
- [8. GUI](docs/08-gui.md) — Go backend, React frontend, processes, tray, installer
- [9. Building & releasing](docs/09-build-release.md) — Make targets, releases, self-update
- [10. Testing](docs/10-testing.md) — test suites and philosophy

## Development

```bash
make test      # full Rust + Go suites
make release   # portable archives for Linux + Windows into dist/
make publish   # tag-checked GitHub release (git tag v0.1.0 first)
```

See [Building & releasing](docs/09-build-release.md) for details,
including cross-compiling Windows and the input-device grant.