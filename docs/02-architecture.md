# 2. Architecture

This document explains *how the system is put together*: the layers, the
data flow for one mouse move, the threading model, and the design
decisions that make it work (and make it different from other KVMs).

## 2.1 The layers

kvmshare is five small Rust crates plus a Go/Wails GUI. The rule that
holds it together: **the switching brain is pure logic, the OS work sits
behind two narrow traits, and the wire protocol is plain binary.**

```
┌──────────────────────────────────────────────────────────┐
│ gui/  (Go + React)   — processes, layout editor, logs,   │
│                        tray, installer, self-update      │
├──────────────────────────────────────────────────────────┤
│ crates/app  (Rust)   — the two executables, config file, │
│                        role locking, clipboard poller    │
├──────────────────────────────────────────────────────────┤
│ crates/core  (Rust)  — layout, session (crossing brain), │
│                        server & client (transport logic) │
├──────────────────────────────────────────────────────────┤
│ crates/protocol (Rust)— the binary wire format            │
│ crates/log (Rust)    — leveled, hot-reloadable logging   │
├──────────────────────────────────────────────────────────┤
│ crates/platform (Rust)— OS backends: Linux/X11, evdev,    │
│                        Windows. Implements core's traits │
└──────────────────────────────────────────────────────────┘
```

Dependency direction is strictly downward: `app` uses `core`+`protocol`+
`platform`+`log`; `core` uses `protocol`+`log`; `platform` uses `core`+
`protocol`+`log`. Nothing reaches back up.

### The two traits that make every OS pluggable

`kvmshare-core` defines the platform boundary. Nothing else in the core
knows what OS it runs on.

- **`core::server::Engine`** — what the *server* can do to its own
  machine: warp the local cursor, hide/show it, grab/isolate local input
  while the cursor is on a client. Implemented by `X11Engine` (Linux)
  and `Win32Engine` (Windows).
- **`core::client::Injector`** — what a *client* can do to its own
  machine: move the cursor, inject buttons/keys/wheel, hide the local
  cursor while being controlled, read/write the clipboard. Implemented
  by `X11Injector` and `Win32Injector`.
- **`core::clipboard::Clipboard`** — clipboard access, deliberately on
  its *own* lock and thread (a clipboard call can block for a long
  time, and must never serialize with the cursor).

A new OS backend (macOS, or a Wayland module next to X11) is therefore:
implement those traits plus an input source, and register it in
`platform::lib.rs`. Everything else — protocol, session, server, client,
GUI — is already platform-neutral.

## 2.2 The data flow: one mouse move

Follow a physical mouse movement on the server machine, end to end:

```
Physical mouse
   │  device events (raw deltas, pre-acceleration)
   ▼
platform capture thread (X11: XI2 raw / Windows: Raw Input)
   │  coalesced into MouseMoveRel at a fixed cadence (4 ms)
   ▼
core::server main loop  ──► session.on_local_event(msg)
   │                        (pure logic: move the virtual cursor,
   │                         decide: forward? cross? nothing?)
   ├─ forward  → per-client outbound queue → writer thread → UDP
   └─ crossing → Action::SwitchTo → Enter/Leave over TCP + engine calls
                                   (grab, isolate, hide local cursor)
   ▼
client: UDP thread receives the delta → motion state
   │  (follower: advance commanded position, inject feedforward)
   ▼
client motion thread (every ~4 ms) places the cursor on the command
   │  (absolute backends) or corrects toward it (relative backends)
   ▼
client's OS cursor moves — and the client beacons its real position
   │  back over UDP every ~8 ms
   ▼
server UDP receiver → session.on_remote_beacon → edge state updates
```

The same picture for a click or a key: it goes over **TCP** (reliable,
ordered), arrives at the client's control thread, and is queued as an
injection event that the motion thread executes *after* placing the
cursor — so the click lands exactly where the cursor pointed.

## 2.3 Why the mouse motion is smooth (and never oscillates)

This is the heart of the design, and it is a direct response to the bug
family that plagues Synergy/Barrier/Deskflow.

**1. Input is *raw* events, and only raw events.** XI2 raw events on
X11, Raw Input on Windows. Raw motion carries the device's own deltas,
straight from the driver. Two consequences:

- Slow moves are not truncated (X11 accumulates fractions, Windows
  reports whole pixels natively).
- **A programmatic cursor warp generates no raw event.** Parking or
  re-centering the hidden server cursor can never feed phantom deltas
  back into the session. The classic KVM oscillation (warp → fake motion
  → warp back) cannot happen; there are no warp-suppression timers or
  edge hacks.

**2. The session tracks a *virtual* cursor and snaps on every switch.**
The cursor has one virtual position across the whole desktop. On a
crossing it is snapped exactly to the destination's entry point, so
absolute positions and `Enter` always agree — no off-by-one drift
accumulates over hours of use.

**3. Crossings are armed by the *real* cursor and fired by a push.**
Two streams feed the session: raw deltas (instant but pre-acceleration)
and real-position beacons (a few ms behind, but ground truth). A
crossing needs **both**: a beacon must place the visible cursor within a
thin wall-band of a screen edge (the OS has *pinned* it there), and an
outward push must follow. Deltas alone never cross; resting at a wall
never crosses; moving away from a wall disarms it. (Details and the
exact constants live in [Core — session](05-core.md#52-the-boundary-model).)

**4. The hidden server cursor is parked and stays put.** While the user
is on a client, the server's own cursor is hidden *in place* and never
moves again until control returns. Moving a hidden cursor would sweep
hover/enter effects across every local window it crossed (local elements
visibly reacting while the user works on a client). The virtual cursor
is driven entirely by raw input and beacons, which do not depend on the
physical cursor's position at all.

**5. The client steers with a closed loop, not a replay queue.**
Every received motion frame advances a commanded position and is
injected verbatim (feedforward), and each tick the real cursor is
corrected toward the command with a damped move. There is no queue, so
no backlog can form; the client OS's own pointer transform (acceleration)
cannot make the cursor run away from the hand. See
[Core — motion](05-core.md#55-the-client-closed-loop-follower).

## 2.4 The threading model

Deliberately: **separate single-purpose threads, event-driven, no one
big loop.** A slow or wedged subsystem delays only itself, never the
cursor.

### Server (`core::server`, plus the platform capture)

| Thread | Job | Blocking? |
|--------|-----|-----------|
| Platform capture thread | Reads raw device events, coalesces motion, executes engine commands (grab, warp, hide) on its own X connection | polls every 2 ms |
| Beacon thread (X11) | Polls the real pointer position on its *own* X connection, sends position beacons | separate connection so a busy X server never stalls motion |
| Server main loop | Reads local input from the capture channel, runs the session, applies actions | never blocks on network (outbound is queued) |
| Per-client writer thread | Drains the client's outbound queue; owns TCP send + UDP motion | a wedged client delays only its own frames |
| Per-client service thread | Reads the client's TCP control channel (handshake, keepalives, clipboard) | blocking read with timeouts |
| UDP receiver thread | Owns the UDP socket: learns client addresses, routes beacons, fires beacon crossings | 1 ms idle poll |
| Supervisor watchdog | Watches liveness heartbeats; exits with a restart code if the input path wedges *while the cursor is on a client* | shares only atomics |

### Client (`core::client`)

| Thread | Job | Blocking? |
|--------|-----|-----------|
| TCP control thread (main) | Services the server's control channel (enter/leave, buttons, keys, wheel, clipboard, layout) | 100 ms read timeout |
| UDP thread | Drains the cursor stream (motion in, beacons out) | 8 ms read timeout, event-driven |
| Motion thread | Every ~4 ms: place/correct the cursor, execute queued injection events, send beacons | the only thread that touches the cursor |
| Sync thread | Slow duties: screen-geometry changes, clipboard uploads | owns the clipboard lock |
| Supervisor watchdog | If the motion thread stops ticking while being controlled, force-restore local input and end the session | shares only atomics |

Two rules appear again and again:

- **The clipboard never shares a lock with the cursor.** Reading/writing
  the system clipboard can block indefinitely (another process holds it
  open), so it lives on its own lock and thread on both roles.
- **Watchdogs share nothing but atomics.** Whatever wedges a worker
  thread cannot wedge the watchdog that is supposed to recover it.

## 2.5 Keys: one identity across every OS

Keys travel over the wire as **USB HID usage ids** — the industry
standard for *physical* key identity. Each backend converts at its edge
(X11 keycode → evdev → HID; Windows set-1 scancode + E0 flag → HID) and
back for injection. Because the wire format is OS-neutral, a Windows
server driving a Linux client delivers the exact physical key the user
pressed; each machine's own keyboard layout then produces the character.
Tables live in `platform::keys`, kept in sync by a consistency test.
See [Platform — key identity](06-platform.md#62-key-identity-usb-hid-usage-ids).

## 2.6 Logging

Every crate logs through the tiny shared `kvmshare-log` crate: one
leveled logger, no framework. Each line is `HH:MM:SS LEVEL component:
message` on stderr — the GUI spawns the binaries with stderr pointed at
the role's log file, so that is exactly what gets tailed. Levels,
quietest first: **error / warn / info (default) / debug / trace**.

The level and the enabled flag **hot-reload without a restart**: the GUI
writes a small `<role>.logctl` file (`level=debug`, `enabled=1`) that
the logger polls every 400 ms. A missing or malformed line leaves the
current setting untouched. See [Core — log](05-core.md#59-logging) and
[GUI — logs](08-gui.md#85-logs-page).

## 2.7 Process model and role exclusivity

- **A machine runs one role at a time.** Both Rust binaries take an
  exclusive OS file lock (`flock` on Unix, `LockFileEx` on Windows) on
  `server.lock` / `client.lock` in the state dir, and refuse to start
  when the *other* role's lock is held. The lock dies with the process —
  a crash leaves no stale lock, so there are no ghost instances.
- **The GUI is a controller, not a babysitter.** Role processes run in
  the background, independent of the GUI. Closing the window hides it to
  the tray; quitting the GUI leaves the running role alive. On startup
  the GUI *adopts* a running role (it probes the locks, never spawns a
  second instance) and can stop it by the pid recorded beside the lock.
- **The GUI itself is single-instance** (`gui.lock`); a second launch
  raises the existing window and exits quietly (so launching from dmenu
  never looks like "nothing happened").

See [App — role locking](07-app.md#72-role-locking) and
[GUI — process management](08-gui.md#82-process-management).

## 2.8 Design decisions in one table

| Decision | Why |
|----------|-----|
| Raw input only (XI2 raw / Raw Input) | Warps can't feed phantom motion → no oscillation |
| UDP for motion + beacons, TCP for everything else | Motion must never wait on reliable-stream backpressure |
| Pure-logic session with unit tests | The crossing brain is testable without an OS or a display |
| Beacon+push crossing model | Deltas alone run ahead of the visible cursor; the real cursor is the only ground truth for edges |
| Parked hidden server cursor | Moving a hidden cursor sweeps hover effects over local windows |
| Closed-loop client follower | No replay queue → no backlog → no "gel" or jumps; self-healing against lost frames and OS acceleration |
| Canonical HID key ids | One wire format works for any OS pair |
| Clipboard on its own lock/thread | A stalled clipboard can never freeze the cursor |
| One role per machine via OS locks | Enforced even if binaries are started by hand |
| Allowlist + local-only connection policy | Only layout-named clients from the local network; trusted machine ids for headless first-connect |
| mDNS discovery + trusted ids | No IP/port typing; auto-connect and click-to-connect, pairing without plugging a mouse in |
| Auto-config on handshake | Client reports real screen geometry; the server reconfigures the layout automatically |
| Role processes outlive the GUI | The shared input keeps working when the window is closed |
| Tray on any desktop (SNI or XEmbed) | Wails SNI for modern bars, raw-Xlib XEmbed fallback for legacy bars |

---

**Next:** [3. Codebase tour](03-codebase-tour.md) — where everything
lives and how to start reading.