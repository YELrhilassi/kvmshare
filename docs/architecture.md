# Architecture

kvmshare is four small Rust crates plus a Go/Wails GUI. The guiding rule:
**the switching brain is pure logic, the OS work is behind two narrow
traits, and the wire protocol is plain binary.**

## The two traits that make everything pluggable

`kvmshare-core` defines the platform boundary. Nothing else in the core
knows what OS it runs on.

- **`core::server::Engine`** — what the server can do to its *own* machine:
  warp the cursor, hide/show it, read/write the clipboard. Implemented by
  `platform::x11::engine::X11Engine` (Linux) and
  `platform::windows::engine::Win32Engine` (Windows).
- **`core::client::Injector`** — what a client can do to its *own*
  machine: move the cursor, inject buttons/keys/wheel, hide the cursor
  while being controlled, read/write the clipboard. Implemented by
  `platform::x11::injector::X11Injector` and
  `platform::windows::injector::Win32Injector`.

A new backend (macOS, or a Wayland module next to X11) is therefore:
implement those two traits plus an input source, and wire them into
`platform::lib.rs`. Everything else — protocol, session, server, client,
GUI — is already platform-neutral.

## Why the mouse motion is smooth (and never oscillates)

This is the heart of the design, and it is a direct response to the bug
family that plagues Synergy/Barrier/Deskflow.

**1. Input is *raw* events, and only raw events** — XI2 raw events on
X11, Raw Input on Windows (`RegisterRawInputDevices` on a hidden
message-only window, `RIDEV_INPUTSINK`). Raw motion carries the device's
own deltas, straight from the driver (sub-pixel fixed-point on X11,
whole-pixel `lLastX/lLastY` on Windows). Two consequences:

- Slow moves are not truncated (X11 accumulates fractions; Windows
  reports whole pixels natively) — no lost micro-motion, no "stuck at
  one pixel".
- **A programmatic cursor warp generates no raw event** (`SetCursorPos`
  on Windows included). Warps are invisible to the input stream, so
  parking or re-centering the hidden server cursor can never feed
  phantom deltas back into the session. The classic KVM oscillation
  (warp → fake motion → warp back) cannot happen; there is no need for
  warp-suppression timers or edge hacks.

**2. The session tracks a *virtual* cursor and snaps on every switch.**
`core::session::Session` owns one virtual position across the whole
desktop. When the cursor crosses an edge, it is snapped exactly to the
destination screen's entry point, so absolute positions and `Enter`
always agree — no off-by-one drift accumulates over hours of use.

**3. The hidden server cursor has an edge guard.** While the user is on a
client, the server's own cursor is hidden and parked at its screen center
so it has room to roam. If it approaches the physical screen edge, the
session emits `Action::RecenterLocal` and the engine warps it back to
center — invisible to the raw input stream, so the remote cursor never
stops at "halfway across the client screen" (the bug the deskflow
debugging session hit, and the fix that resolved it).

## Keys: one identity across every OS

Keys travel over the wire as **USB HID usage ids** — the industry
standard for *physical* key identity. Each backend converts at its edge
from its native code (X11 keycode → evdev → HID; Windows set-1
scancode + E0 flag → HID), and back for injection (XTest on X11,
`SendInput` in scan-code mode on Windows). Because the wire format is
OS-neutral, a Windows server driving a Linux client — or any other pair
— delivers the exact physical key the user pressed; each machine's own
keyboard layout then produces the character.

The tables live in `platform::keys` with both directions kept in sync by
a consistency test, so a bad entry can never silently break a cross-OS
pair. Wayland delivers keys as evdev codes, so the future Wayland
backend uses the same table as X11.

## The wire protocol

`kvmshare-protocol` — plain binary, no serde, no codegen:

```
+--------+---------+--------+------------+-----------------+
| magic  |  type   | flags  |  length    |  payload        |
| KVM1   | 1 byte  | 1 byte | u32 BE     | length bytes    |
+--------+---------+--------+------------+-----------------+
```

- Big-endian integers, length-prefixed UTF-8 strings.
- The 4-byte magic lets a receiver detect desync and resynchronize by
  scanning, instead of hanging on garbage.
- The hot path (mouse move) is one ~16-byte frame; TCP_NODELAY keeps it
  on the wire immediately.
- `Message::decode` is strict (trailing bytes are an error), which catches
  encode/decode bugs at the source.

## The transport and threading model

- **Server:** one accept thread; each client gets a service thread with
  its own **lock-free reader** (`Transport::reader` clones the socket —
  TCP is full-duplex, so reads never contend with the main thread's
  writes). The main loop drains local input events and executes session
  actions. The engine is behind a mutex that is taken *per event*, so
  client threads (clipboard) and the GUI's poller can reach it.
- **Client:** a single loop with a 100 ms read timeout, which lets it
  drain the outbox, notice resolution changes, poll the clipboard up, and
  send keepalives while idle.
- **Framing in the transport:** a full frame is decoded per call and
  trailing bytes are *retained* — several frames often share one TCP
  segment, and dropping the remainder silently loses messages (a real bug
  the e2e test caught).

## Logging

Both binaries use `kvmshare-app::log`, a small leveled logger
(component names come from the binary name: `kvmshare-server`,
`kvmshare-client`). Levels are **error / warn / info / debug**; the
default is info, set per-run with `-v` (verbose → debug) or `--quiet`
(error only), and globally via the `KVMSHARE_LOG` env var (`error`,
`warn`, `info`, `debug`). Every line is timestamped and goes to the
state-dir log that the GUI tails and that the notification watcher
parses.

## Clipboard sync

Both sides poll their local clipboard every 500 ms and push changes to
the peer; the peer applies them and broadcasts to all clients. Echoes are
prevented by `clipboard_last_injected`: content that *arrived* from a
peer is remembered and skipped by the poller. Text-only for now.

## The GUI

A **Wails v3** app (GTK4 + WebKitGTK 6 on Linux) with a **React +
TypeScript + shadcn/ui** frontend built by Vite (output embedded into
the Go binary via `//go:embed all:frontend/dist`). Wails v3 pages load
through the `wails://` scheme, the runtime is served at
`/wails/runtime.js`, and the frontend's typed bridge (`frontend/src/lib/bridge.ts`)
calls the bound `App` service by fully-qualified name (`main.App.*` —
types in `package main` register under `main`). Dark-only theme, no
sidebar: a top bar shows only the pages that belong to the current role,
and the design is card-free — sections are plain type over hairline
rules with generous whitespace.

A machine runs **one role at a time**, and the UI mirrors that:

- **Home** (always) — the role switch, live status for the current role,
  the address clients connect to (server mode) or the target machine
  (client mode), and quick links. Switching role stops the running
  process.
- **Server + Layout** (server role only) — start/stop, config (port,
  path), network details, live log tail; and the virtual-desktop canvas
  (edge snapping, zoom/pan, arrow nudging, duplicate/delete, lock).
- **Client** (client role only) — server address + screen name,
  start/stop, live log.

GUI state (role, client address) persists in
`~/.local/state/kvmshare/gui.json`; both processes log to the same
folder, tailed live by the UI (`TailLog` reads the last N lines, polled
at ~1.5s).

### Process ownership and exclusivity

`proc` wrappers spawn each managed process with a reaper goroutine, so
"running" is accurate the moment a child dies (no ghost states) and
stops never leak or double-wait. The GUI enforces one role at a time:
starting a role stops the other, and switching role stops the old
process. A `flock` instance lock means only one GUI per machine; a
second instance exits with a clear error.

The Rust binaries enforce the same rule at the OS level via flock-based
role locks in `kvmshare-app::guard` (state dir `server.lock`/`client.lock`):
refuse to start when the other role runs, the lock dies with the
process (crashes leave nothing stale), and the lock file records the
instance's pid so a controller can signal it.

### Background model: roles outlive the GUI

The GUI is a controller, not a babysitter. Roles run as independent
background processes: **closing the window hides it** (a `WindowClosing`
hook cancels the close), the app stays alive as a system-tray item
(DBus StatusNotifierItem — status line + Start/Stop/Open/Quit, refreshed
whenever the role state changes), and quitting the GUI (tray → Quit)
leaves the running role untouched.

On startup the GUI *adopts*: `ServerRunning`/`ClientRunning` probe the
role locks (not just the GUI's own children), so a role started by an
earlier GUI session — or by hand — is discovered, reported, and
stoppable by pid; `Start` never spawns a second instance. Spawned
children get a brief health check so an immediate death (port taken,
role lock refused) surfaces as a clear error instead of a silent no-op.

### Live config changes

The server watches its config file and pushes the new layout through an
app-layer control channel (`Server::with_control` + `Session::swap_layout`),
serialized with input processing on the main loop. The cursor is brought
home if it was on a client, clients whose screens disappeared are
politely disconnected, and the new layout is broadcast — so saving in the
GUI applies with **no restart**.

Snappiness is deliberate: screen drags write styles directly to the DOM
(refs) during the gesture and commit to React state only on release;
log/status polling is mounted only while the relevant page is open; the
whole bundle is ~95 KB gzipped.

## Releases and updates

Releases are published to GitHub (`make publish`, tag-checked) with
portable archives per platform, standalone installers, and a
`SHA256SUMS`. Two compiled consumers share `gui/internal/selfupdate`:

- **`kvmshare-install`** — the one-file bootstrap. It fetches the latest
  release for its platform, verifies the archive checksum, extracts and
  installs to `~/.local/bin` (Linux desktop entry, icon and sample
  config included). Re-running updates in place.
- **The GUI** — the Home page version line checks GitHub for a newer
  release and applies it in place, then restarts into the new version.

Replacement is rename-based (old → `.old`, new → in) with a copy
fallback across filesystems, so a running process is never broken — and
roles are separate processes, so a running server/client is never
interrupted by an update (it picks up the new code on its next start).
The version is injected at link time; a `v0.0.0-dev` build always sees
published releases as newer, so development machines get updates too.

## Future work (in rough priority)

1. **macOS backend** — implement `Engine`/`Injector`/input source
   (CGEventTap / IOKit, `NSPasteboard`), register it in `lib.rs`. The
   traits, key tables and stubs are ready.
2. **Wayland backend** — slots in next to X11 with the same contracts
   and the same evdev key table (libinput delivers evdev codes).
3. **Verify the Windows backend on real hardware** — the backend
   compiles clean (Raw Input capture, SendInput injection, Win32
   clipboard, cursor control) but has not been exercised on a Windows
   desktop yet; see `platforms.md`.
4. **Live control socket** — replace the config-file watcher with a
   direct control channel when more than layout needs to change at
   runtime (the protocol already has `Message::Layout`).
5. **Richer clipboard** — images and multiple mimes.
6. **Encryption** — TLS or Noise between peers; the frame has a flags
   byte reserved for this.