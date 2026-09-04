# kvmshare

A from-scratch, cross-platform KVM (keyboard/video/mouse) sharer, built in
**Rust** with a **Wails** desktop GUI. One machine (the **server**) shares
its keyboard and mouse with other machines (**clients**) across a virtual
desktop: move the cursor off one screen edge and it appears on the
neighbor machine, taking keyboard and clipboard with it.

It is designed around the hard lessons of existing tools (Synergy,
Barrier, Deskflow): a single, warp-proof motion pipeline, a pure-logic
switching brain that is fully unit-tested, and a simple binary protocol
with no serialization framework.

## Layout

```
kvmshare/
├── crates/
│   ├── protocol/   # binary wire protocol: framing, messages, (de)serialization
│   ├── log/        # leveled, hot-reloadable logging shared by every crate
│   ├── core/       # layout model, cursor/screen-switching session, TCP server/client
│   ├── platform/   # OS backends: Linux/X11 (XI2 raw input, XFixes, XTest) + Windows (Raw Input, SendInput)
│   └── app/        # the kvmshare-server and kvmshare-client executables
├── gui/            # Wails v3 desktop app: React + shadcn/ui, 5 pages
│   └── frontend/   #   Vite + React + TypeScript (built to frontend/dist)
└── docs/
    └── architecture.md   # design decisions, threading model, future work
```

Each crate is small and readable on purpose — no file in the Rust core is
more than a few hundred lines, and every layer has tests.

## Install on a machine (end users)

No source checkout needed. Releases are published on
[GitHub](https://github.com/YELrhilassi/kvmshare/releases). Download one
file — the installer for your platform — and run it:

```bash
curl -sL -o kvmshare-install \
  https://github.com/YELrhilassi/kvmshare/releases/download/v0.1.0/kvmshare-install_v0.1.0_linux_amd64
chmod +x kvmshare-install
./kvmshare-install
```

The installer is a compiled Go binary (no shell scripts): it downloads
the release archive for your platform, verifies it against the
release's `SHA256SUMS`, installs the binaries to `~/.local/bin`, and
writes the desktop entry, icon and a sample config on first install.
Running it again updates everything in place.

Alternatively, the GUI has the same machinery built in: the version
line on the Home page checks GitHub for a newer release and installs +
restarts with one click.

## Build from source

Requirements: Rust (stable), Go, Node (for the React frontend), and — for
the Linux GUI — **GTK4 + WebKitGTK 6** development packages, plus a
DBus session for the tray (Void: `xbps-install gtk4-devel
libwebkitgtk60-devel`).

```bash
make build       # compile everything (release Rust + GUI)
make install     # copy binaries to ~/.local/bin, config to ~/.config/kvmshare,
                 # launcher to ~/.local/share/applications
```

After `make install`, `kvmshare-server`, `kvmshare-client` and `kvmshare-gui`
are on your PATH and launchable from dmenu/rofi/your terminal. The server
finds its config automatically at `~/.config/kvmshare/kvmshare-server.toml`
(`--config PATH` overrides it).

### Releasing

```bash
make release      # portable archives for Linux (+ Windows when mingw-w64 is present)
                  #   -> dist/ with kvmshare_<ver>_*.tar.gz, installers, SHA256SUMS
make publish      # tag-checked: builds and uploads a GitHub release (git tag first)
```

### Dev loop

```bash
make dev         # watch crates/ and gui/, rebuild + reinstall on every save
```

`make dev` stays running: edit any Rust/Go/frontend file, and within a few
seconds the installed binaries are rebuilt and re-installed — ready to
launch and test. Ctrl-C to stop.

Other targets: `make test` (full Rust **and** Go GUI suites), `make clean`,
`make uninstall`
(keeps your config), `make install PREFIX=/usr/local` to install elsewhere.

## Run

### Server (the machine whose keyboard/mouse is shared)

```toml
# kvmshare-server.toml — the virtual desktop. The FIRST screen is this
# machine (the server). Client names must match the client's hostname
# (or whatever --name the client was started with).
port = 24800

[[screens]]
name = "pc"
width = 1920
height = 1080
x = 0
y = 0

[[screens]]
name = "hp"
width = 1920
height = 1080
x = -1920   # hp sits to the LEFT of pc
y = 0
```

```bash
kvmshare-server --config kvmshare-server.toml
```

### Client (a machine being controlled)

```bash
kvmshare-client pc:24800            # name defaults to this machine's hostname
kvmshare-client pc:24800 --name hp  # ...or be explicit
```

That's it: move the cursor to the left edge of pc and it slides onto hp.

### GUI

Launch `kvmshare-gui` (from dmenu/rofi/terminal). A dark, minimal,
role-aware interface: **a machine runs as a server or a client — never
both** — so the top bar only shows the pages for the role you pick on
Home:

- **Home** — the role switch. Live status for the current role, the
  address clients connect to (server mode) or the machine this one
  connects to (client mode). Switching role stops whatever was running.
- **Server** (server role) — start/stop, configuration (port, config
  path), network details (all interfaces + addresses).
- **Client** (client role) — the server address + screen name this
  machine connects with, start/stop.
- **Layout** (server role) — the virtual desktop editor: drag screens to
  arrange them (with edge snapping), zoom/pan/fit, nudge with the arrow
  keys, duplicate/delete screens, and a lock toggle that freezes the
  layout against accidental edits. The canvas stays legible at any zoom:
  100% always fits the whole desktop, the grid is adaptive and always
  visible, and screen labels/borders keep their size while zooming.
- **Logs** (both roles) — the log of this machine's own instance
  (server in server mode, client in client mode), with a level selector
  up to **trace**, an enable switch, follow and Clear. Level and
  enable apply **live to the running process** — no restart — and
  persist for whichever role starts next.

Role exclusivity is enforced at the OS level too: the server and client
binaries take `flock`-based role locks, so a machine can never run both,
crashes leave no stale locks, and starting one role stops the other. Only
one GUI instance per machine is allowed.

**The GUI runs in the background.** Closing the window hides it to the
system tray (live role status + Start/Stop/Open/Quit) instead of
quitting, and the role processes are independent of the GUI entirely:
quit the GUI and they keep running; reopen it and it discovers and
adopts the running instance via the role locks. Reopening also restores
the window; Quit from the tray exits the GUI without touching the
background role. The tray also shows how many clients are connected, and
raises desktop notifications when a client connects or disconnects.

A client machine that loses its server (or starts before it) does not
die: it retries every 3 seconds until the server is reachable, then
picks the session right back up — no manual restarts.

The GUI's own state (role, client address, log level/enabled) persists
in `~/.local/state/kvmshare/gui.json`; process logs live in the same
folder, are tailed live by the Logs page, and are controlled there
through a small `*.logctl` file the running process polls — level
changes and the enable switch reach it within a fraction of a second.

## Test

```bash
make test   # cargo test --workspace + go test ./gui
```

Rust covers the protocol round-trips, the layout/adjacency math, the
entire switching session (parked hidden cursor, escape key, live layout
swaps), role-lock exclusivity, and real end-to-end tests:
server + client over TCP with mock input, a recording injector and a
config hot-reload. The Go suite covers config round-trips, settings
persistence, process start/stop + role exclusivity, instance locking,
network listing and log tailing.

## Status

- **Linux/X11**: full — input capture (XI2 raw), cursor control, input
  injection, clipboard sync, GUI, tray + notifications.
- **Windows**: full backend written (Raw Input capture, SendInput
  injection, Win32 clipboard, cursor control) and the whole workspace
  cross-compiles clean; the GUI builds to a PE32+ with no cgo. The
  backend is compile-checked — it still needs exercise on real Windows
  hardware.
- **macOS**: the architecture is ready (platform traits + stubs with
  clear errors); not attempted yet.

See `docs/platforms.md` for the full portability audit and the Windows
verification checklist.

## Known limitations (deliberate, documented)

- Keys travel as canonical USB HID usage ids (`key` in `Message::Key`),
  so any OS pair (Linux↔Windows included) delivers the physical key;
  each machine's own layout produces the character.
- Clipboard sync is text-only (`text/plain`), polled on both sides and
  echo-guarded.
- Layout/config changes apply **live**: the server watches its config
  file and adopts edits without a restart (returning the cursor home and
  dropping clients whose screens disappeared).

## License

MIT.