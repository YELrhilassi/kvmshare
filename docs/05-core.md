# 5. Core crate

`crates/core/` is the platform-independent brain of kvmshare: the
layout, the cursor-switching session, the motion pipeline, and the
server/client transport logic. It knows nothing about X11, Windows or
macOS — all OS work plugs in through the traits defined here.

## 5.1 Module map

```
crates/core/src/
├── lib.rs          # crate root, re-exports (Layout, Session, Direction, Mode)
├── layout.rs       # the virtual desktop: screens, adjacency math
├── session/        # the crossing brain (pure logic)
│   ├── mod.rs      #   Session, Action, cursor model
│   ├── boundary.rs #   wall-band, push latches, constants
│   └── motion.rs   #   event handlers driving the boundary state
├── motion/         # cursor steering shared by both halves
│   ├── mod.rs      #   MOTION_PERIOD
│   ├── gain.rs     #   GainTracker (server's px-per-count)
│   ├── pending.rs  #   PendingMotion (coalesced deltas)
│   ├── follower.rs #   PositionFollower (client closed loop)
│   └── probe.rs    #   MotionProbe (requested-vs-actual telemetry)
├── server/         # the server role
│   ├── mod.rs      #   Server, Control, run loop
│   ├── engine.rs   #   the Engine trait + ServerClipboard
│   ├── liveness.rs #   Liveness heartbeats + supervisor watchdog
│   ├── actions.rs  #   applies Session::Action to the world
│   ├── client.rs   #   one connected client (writer, service, handshake)
│   └── udp.rs      #   UDP receiver thread + beacon watchdog
├── client/         # the client role
│   ├── mod.rs      #   Client: connect, run, thread wiring
│   ├── injector.rs #   the Injector trait (client platform hook)
│   ├── shared.rs   #   state shared by client threads + event queue
│   ├── supervisor.rs # client watchdog
│   ├── dispatch.rs #   applies one server message to the local machine
│   └── threads.rs  #   motion / UDP / sync worker loops
├── clipboard.rs    # the shared Clipboard trait
├── transport.rs    # one Message per frame over TCP
├── udp.rs          # the UDP datagram envelope
└── time.rs         # monotonic now_ms()
```

## 5.2 The session and its boundary model

**File: `crates/core/src/session/`**

The session owns the *virtual* cursor and answers "what should happen
next?" by returning [`Action`]s (send a message, switch to client X,
switch back local, nothing). The caller (the server main loop, or a test
harness) executes the actions.

**Coordinates.** The virtual cursor spans the whole desktop. On every
switch it is snapped to the destination's entry point, so absolute
positions and `Enter` always agree — no drift, no off-by-one
accumulation.

**Crossing model** (`boundary.rs`) — a crossing needs **both** streams:

1. **Arm** — a real-position beacon places the visible cursor within
   `EDGE_BAND` (2 px) of a screen edge. The OS has *committed* the
   cursor to that boundary.
2. **Fire** — an outward push (raw deltas toward that edge) while armed.

Key constants:

| Constant | Value | Meaning |
|----------|-------|---------|
| `EDGE_BAND` | 2 px | Wall-band width; absorbs pointer quantization |
| `EDGE_PUSH_FRESH` | 40 ms | A park beacon within this window of a push completes the crossing on the park itself (the flick-and-stop case) |
| `EDGE_PUSH_FALLBACK` | 150 ms | Sustained outward deltas with the *virtual* cursor outside the rect fire anyway — the rescue path for a stalled beacon stream |
| `ENTRY_INSET` | 48 px | Entry points sit this far inside the destination screen — prevents the seam-jitter bounce (a cursor placed exactly on the wall re-crosses immediately) |
| `REMOTE_BEACON_FRESH` | 120 ms | How old a client beacon may be and still be treated as the real position |

The session tracks the same wall state for the local screen (fed by the
server's own position beacons) and for the remote client (fed by the
client's `CursorPos` beacons) — `boundary.rs` owns the vocabulary,
`motion.rs` the handlers.

**Dynamic admission.** A client whose name is not in the layout is
admitted on the spot: a screen is created from its *reported* geometry
and placed to the right of the desktop (`Session::admit_client`), so a
fresh pair of machines works with zero configuration. The Layout page
pins a permanent position; until then the screen exists only in the
running session, and a config reload keeps it while the client is
connected and drops it once it is gone.

**Layout swaps.** `Session::swap_layout` adopts a new layout at runtime
(config hot-reload): if the cursor was on a client it comes home first;
clients whose screens vanished are dropped (`Server::drop_stale_clients`).

## 5.3 The layout

**File: `crates/core/src/layout.rs`**

Answers two questions:

1. Which screen is `(x, y)` on? — `screen_at`
2. Leaving screen A through edge `dir`, which screen is next and where
   exactly does the cursor enter? — `neighbor`

`Layout::normalized()` removes layout noise: near-adjacent screens
(within `EDGE_TOLERANCE`, 16 px) are snapped into exact contact and
their perpendicular spans aligned, so crossings land pixel-exact no
matter how the GUI canvas positioned the screens. The server applies it
to every layout it adopts.

`Direction` is the four edges; "Top" means moving *up* (decreasing y).

## 5.4 The server role

**Files: `crates/core/src/server/`**

The `Server` struct binds a TCP listener and a UDP socket on the same
port, hosts the `Session` (behind a mutex) and the connected clients.
`run()` spawns the supervisor, the accept thread and the UDP receiver,
then loops on local input from the platform capture:

- every input message feeds the pointer-gain tracker, then
  `session.on_local_event(msg)` returns actions;
- each action is applied by `actions::apply_action` with the engine lock
  held *only for the apply step* (other threads — clipboard, beacon
  crossings — can reach the engine between events).

**Per-client lifecycle** (`server/client.rs`): accept → `exchange_hello`
(version check + `admit_client`) → send `Welcome` + layout → split the
transport (writer keeps the sending half; a lock-free `reader()` clone
services the control channel) → writer thread + service thread. A
client silent for 10 s is dropped (`CLIENT_SILENT_TIMEOUT`); teardown
returns the cursor home if the client had it.

**Outbound routing** (`route()`): `MouseMoveRel` → UDP, everything else
→ TCP, all through one per-client queue drained by the writer thread —
the main loop never blocks on the network.

**UDP receiver** (`server/udp.rs`): one thread owns the socket, learns
each client's address from its first datagram, drops stale/duplicate
beacons by sequence number, and executes beacon-fired crossings right
on the park. It **blocks** on `recv_from` with an 8 ms read timeout —
datagrams wake it immediately, the timeout only bounds idle wakes — so
it sleeps in the kernel instead of busy-polling. Also runs the
**beacon watchdog**: an active client whose cursor stream is silent for
1.5 s is dropped (`ACTIVE_BEACON_TIMEOUT`) — a wedged motion loop is
invisible to TCP keepalives.

**Supervisor** (`server/liveness.rs`): while the cursor is on a client,
a main-loop or capture-thread heartbeat older than 3 s means the input
path is wedged — which would trap the local machine's keyboard/mouse
(engine lock blocked, grab held forever). It exits with
`EXIT_RESTART` (66); process exit releases every kernel and X grab, and
the GUI's process manager restarts a clean server. Disabled when there
is no real capture (test harnesses).

**Hot reload** (`Control::Reload`): the app layer watches the config
file; a change is drained on the main loop, the session adopts the new
layout, stale clients are dropped, and the new layout is broadcast.

## 5.5 The client closed-loop follower

**Files: `crates/core/src/motion/follower.rs`, `crates/core/src/client/`**

The shared cursor on a client is steered by a **closed loop**:

- every received motion frame advances a commanded position
  (`PositionFollower::push`) and returns a *feedforward* portion
  (`FEED_FORWARD = 0.5`) to inject immediately — zero added latency;
- each tick the real cursor is read back and corrected toward the
  command with a damped move (`correct`, `FOLLOW_GAIN = 0.4`, max step
  32 px) — so the client OS's pointer acceleration can never make the
  cursor run past the hand, and a lost frame is pushed forward;
- ordering-critical events (a click, a key) flush the residual first
  (`flush`, capped at 64 px) so they land where the motion pointed;
- the commanded position is **clamped to the client's screen bounds**
  (seeded from the injector at startup, re-bounded on resolution
  changes), so pushing against an edge can never run the command
  off-screen — reversing at an edge moves immediately because the
  command is already at the edge.

There is **no replay queue** — a backlog can never form. Absolute
backends (Windows) place the cursor at the whole command each tick; the
placement *is* the loop (`absolute_motion()` returns true, `advance()`
instead of `push()`).

**Client threads** (`client/threads.rs`):

| Thread | Duty |
|--------|------|
| TCP control (main) | Service server messages; drain outbox + sync channel; keepalive every 2 s; 100 ms read timeout |
| UDP | Drain motion datagrams → advance command; ignore motion outside Enter/Leave (it can beat the TCP Enter on the wire). Blocks on the socket with an 8 ms read timeout — frames wake it immediately, the timeout only bounds idle wakes |
| Motion | **Event-driven idle**: while this machine is not being controlled it blocks on a condvar (woken on Enter/Leave/stop) and costs zero CPU. While controlled: every `MOTION_PERIOD` (4 ms) place/correct the cursor, execute queued injection events, beacon the real position every 8 ms |
| Sync | Poll screen geometry (2 s) and clipboard (500 ms); report changes over the channel |
| Supervisor | If the motion thread stops ticking while controlled (3 s), force-restore local input and end the session |

**Injection event queue** (`client/shared.rs`): buttons/keys/wheel from
the TCP thread are *queued*, not injected inline — a `SendInput` call
can block (Windows UIPI / elevated windows), and a block on the control
thread would wedge the whole loop unrecoverably. The motion thread
executes the queue at its own cadence, confining any block to the thread
the supervisor can recover. Queue is bounded (512); when full the newest
event is dropped.

**Cursor-pin detection** (`MotionState::probe_window`): if the cursor
was commanded to move but did not travel (away from a screen edge) for
~6 consecutive windows, injected input is being eaten by the OS — the
client releases local input and restarts the session. The Windows
isolation watchdog is the same idea at the OS level
([Platform](06-platform.md#63-windows)).

## 5.6 Motion accumulation and calibration

**Files: `crates/core/src/motion/`**

- **`PendingMotion`** — the capture's accumulator: raw fractional deltas
  merged between sends, emitted as whole pixels at `MOTION_PERIOD` (4 ms,
  ≈250 Hz). Slow moves accumulate fractions instead of truncating.
- **`GainTracker`** — measures the server's own pointer transform
  (pixels of real travel per raw count) from the capture stream. The
  session scales forwarded motion by it, so a client's cursor mirrors
  the server's pixel-for-pixel whatever acceleration either machine
  has. Measured locally (beacons only flow while the cursor is local);
  jump-polluted windows are excluded; clamped 0.25–3.0.
- **`MotionProbe`** — diagnostic: compares requested vs actual cursor
  travel over fixed windows and emits a trace line, making a transform
  mismatch or laggy leg visible as a measurable error.

## 5.7 Transport and UDP helpers

- **`transport.rs`** — `Transport` wraps a TCP stream: `send(msg)`
  encodes one frame; `recv()` returns `Msg` / `NoData` (timed) / `Eof`.
  `reader()` clones the socket for a lock-free concurrent reader (TCP is
  full-duplex). Connection resets map to `Eof` (the peer is gone either
  way).
- **`udp.rs`** — `pack`/`unpack` the `[id][seq][frame]` datagram
  envelope and `is_newer` for wrap-safe sequence comparison.

## 5.8 Clipboard

**File: `crates/core/src/clipboard.rs`** — the `Clipboard` trait
(`set`, `get`, `last_injected`), shared by both roles. It is
deliberately **not** part of `Engine`/`Injector`: clipboard calls can
block indefinitely, so both roles give it its own lock, serviced by its
own thread. `last_injected` lets pollers skip content that arrived from
a peer (no echo).

## 5.9 Logging

**File: `crates/log/src/lib.rs`** — the tiny shared logger: levels
`error`/`warn`/`info` (default)/`debug`/`trace`, one line format
`HH:MM:SS LEVEL component: message` on stderr, and a **bounded async
writer thread** so no caller ever blocks on log I/O (a slow disk or
wedged stderr pipe delays only the writer, never the cursor threads).
Hot-reloadable via a `--logctl` file (level + enabled, polled every
400 ms; applied at startup too). `KVMSHARE_LOG` env var sets the startup
level.

---

**Next:** [6. Platform crate](06-platform.md) — the OS backends.