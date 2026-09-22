# kvmshare — refactor plan + CPU/RAM audit

Branch: `backend-cleanup` (work in progress; nothing below is done).
Baseline: `main` at `b57fe0b` — last known-good working version.

---

## 1. Why this plan exists

Two independent problem reports from real two-machine use
(PC = Linux/i3, HP = Windows 10/11):

1. **CPU/RAM bloat.** After extended use or long idle, CPU sits around
   **40% while doing nothing**. GUI lifecycle bugs have also left orphan
   processes behind in the past (repeated `dbus-daemon`,
   `xdg-desktop-portal`, `gvfsd`, `at-spi2` forks were observed — those
   were traced to the GUI restart loop and fixed on main, but the
   no-op CPU burn remains).
2. **Accumulated hacks.** The backend grew by patching: poll loops with
   sleeps, bounded-queue drops, GUI-driven backend state, per-platform
   shortcuts patched in ad hoc. The user approved a clean rewrite of the
   Rust core on this branch.

Both must land before the next two-machine test.

---

## 2. Ground rules (apply to every task)

- **No busy-wait, ever.** Every loop blocks on an event source (channel
  recv, epoll/IOCP, condvar). Any remaining periodic operation must be
  listed in §5 with its interval and justification.
- **UI never drives the backend.** Layout canvas scale, table state,
  GUI-local prefs must not leak into session/protocol state.
- **No hardcoded hosts, sizes, ports, paths.** Defaults live in one
  config module; everything else is discovered or negotiated.
- **Cross-platform by construction.** No `#[cfg]` inside shared logic —
  platform differences live behind the Engine/Injector/Isolation
  contracts (§6). Code must build for Windows and Linux without knowing
  the target.
- **Small files, pure functions.** If a file passes ~400 lines or a
  function mixes IO with decision-making, split it.
- **After every step: `cargo check` + `cargo test` green before moving on.**

---

## 3. Track A — CPU/RAM audit and fixes (on top of the rewrite)

### A1. Inventory
List every thread, loop, timer, and poller in `kvmshare-server`,
`kvmshare-client`, and the GUI process. For each: what wakes it, how
often, and what it costs when idle. Output: a table in this file (§5)
filled in with *found* values.

### A2. Measure before fixing
Loopback test: run server + client on one machine connected to itself,
5-minute sample of `top -H -p <pids>` and RSS. Record per-thread CPU.
This gives the before/after numbers so "fixed" is verifiable, not a vibe.

### A3. Convert pollers to event-driven
Candidates already suspected from the old code:
- input capture pump → XI2 fd blocked read (Linux), message-window wait
  (Windows) — no tick fallback
- outbound queue wait → condvar/notify, not interval sweep
- state push to GUI → Wails events on change, not periodic snapshot poll
- discovery → mDNS cache with TTL events, not sweep-and-rebuild loops

### A4. GUI process hygiene
No spawn-retry loops; single instance enforced via lockfile; tray and
window share one process; `pstree` before/after check that closing the
window, stopping services, and relaying produce zero orphans.

### A5. RAM bounds
Every growable structure gets a hard cap with an eviction policy:
telemetry ring, log ring, discovery cache, per-client outbound queues,
event replay log. Verify RSS is flat over 30 min idle.

### A6. Frontend
No `setInterval` polling; react to Wails events; no duplicated state
mirrors of backend state.

### A7. Re-measure and record
Repeat A2. Target: **<1% CPU per process idle**, flat RSS. Record the
numbers in §5.

---

## 4. Track B — backend rewrite (`crates/kvmshare_core` + friends)

The old crates (`crates/core`, `crates/platform`, …) stay compiling
until the new path is green, then get deleted path-by-path.

### B1. Pure layout model
Screens, adjacency, normalization, boundary mapping — pure functions,
no IO, deterministic, unit-tested. Coordinates are virtual and
platform-agnostic (fractions of the composed desktop, not pixels), so a
3840-wide Linux screen next to a 1920-wide Windows screen maps without
per-platform fudging.

### B2. Deterministic replayable session
A single state machine: `fn reduce(state, event) -> (state, Vec<Action>)`.
Every input (mouse, key, wheel, network message, timer tick) is an event;
every output (inject, send, isolate, log) is an action. Replay = fold the
event log. Integration test: seed events, assert full action transcript.

### B3. Platform contracts
Traits: `Engine` (capture: mouse/keys/wheel/clipboard), `Injector`
(remote control of this machine), `Isolation` (mute local input while
controlled — declared by session, implemented by platform; Linux: XI2
grab, Windows: BlockInput). Implement for X11 now, stub Wayland-ready;
Windows via SendInput + hooks. No logic behind the traits.

### B4. Transport — DECIDED (2026-09-22): keep TCP control + UDP cursor datagrams

QUIC was evaluated and declined, with evidence:
- The current split already provides QUIC's two framings: the TCP
  control channel is the reliable ordered stream; the UDP cursor stream
  is the unreliable-datagram lane. Head-of-line blocking is a non-issue
  because the latency-sensitive traffic (cursor) never shares a lane
  with control — that was the point of the split.
- The last two-machine session with the current stack crossed smoothly
  both directions; churn would risk regressions for zero measured gain.
- QUIC adds TLS/certificate provisioning to a LAN-only, trust-by-id
  protocol — new failure modes (cert expiry, clock skew) for nothing
  the user asked for.
- QUIC's userspace retransmit/timer loop costs more CPU than kernel
  TCP/UDP sockets that sleep in the kernel — directly against the
  idle-cost goal.

Kept instead: explicit failure modes in the harnesses (connect timeout,
keepalive, clean reconnect with pacing). Revisit only if a measured
problem appears that TCP+UDP cannot fix.

### B5. Harnesses
`kvmshare-server` and `kvmshare-client` become thin: own threads/tasks,
translate OS events → session events, execute session actions. No
business logic. CLI flags may change; GUI gets rewired to match.

### B6. Discovery & state push
mDNS advertise + browse; peers appear/disappear as events, not polls.
All state the GUI shows is pushed on change.

### B7. Delete the old path
Per-path deletion, workspace green after each step, no dead code left.

---

## 5. Idle-cost ledger — A1/A2 DONE on Linux; HP numbers pending

**Measured 2026-09-22, Linux, client role, connected session, idle:**
GUI 0.0% CPU (RSS 149 MB webview — normal), client 0.0% (3 MB), wheel-daemon 0.0% (2 MB).
Logging off (`client.logctl enabled=0`). Every loop verified blocking — no busy-loop exists on Linux idle:

| Process / thread | Wakes by | Interval | Idle CPU (measured) |
|---|---|---|---|
| GUI + webview | Wails events + 1 s ticks | 1 s | 0.0% |
| client motion loop | condvar while idle / 4 ms while active | — | 0.0% |
| client UDP cursor stream | recv timeout | 8 ms | 0.0% (cheap) |
| client control loop | read timeout | 100 ms | 0.0% |
| client supervisor / server supervisor | sleep | 500 ms | — |
| client reconnect pacing | sleep | 3 s | — |
| server main loop | recv_timeout | 100 ms | — |
| evdev reader / X11 capture | poll(2) / XPending | event | — |
| discovery beacon | ticker | 2 s seek / 15 s connected | — |
| discovery health | ticker | 30 s | — |
| live-state push (Go) | ticker | 1 s | — |
| frontend | 2 × setInterval | 1 s / 1.5 s | — (no rAF loops) |

**Conclusion:** the 40% burn is state-dependent — trace logging left on after debug sessions, the server role, or HP/Windows. Windows measurement queued for when HP is reachable again.

**Follow-up (2026-09-22, live occurrence caught at ~101% CPU, ~15 min after HP vanished):** burn was pure user-space CPU in GUI threads (utime 4318 vs stime 22 — no syscalls), starting exactly when the client flipped to `disconnected`; Go loops were all verified ticker-paced, so the spin was in WebKit rendering. Two causes addressed:
1. **Wails GPU policy** — Wails can default the Linux webview to `WebviewGpuPolicyNever` (software rendering → WebKit burns a core per paint thread while the window is open). `WebviewGpuPolicyAlways` is now set explicitly in `gui/main.go`; machines without a GPU fall back on their own.
2. **Invisible evidence** — launched from autostart, GUI stderr (Go panics, WebKit diagnostics) was lost. `gui/errlog_unix.go` now redirects fd 2 to `~/.local/state/kvmshare/gui-stderr.log`; verified working live.

Not yet reproduced synthetically (kill-client and client-retry simulations both stayed ≤3%); if the burn recurs, the stderr log will now name the culprit.

**Structural costs to trim in the rewrite (B-track):** cursor beacons at 8 ms = 125 wakeups/s per side while merely connected (replace with edge-triggered sends in the QUIC rewrite); `udp.rs` error-path 10 ms sleep (error-only); wheel-daemon 25 ms startup wait (startup-only, fine).

Target after: every row "event" except keepalive (a single low-frequency
timer, justified in a comment at its definition).

---

## 6. Verification gates

1. `cargo check` + `cargo test` green (workspace).
2. Replay test: seeded event log produces byte-identical action transcript.
3. Loopback 30-min idle: <1% CPU per process, flat RSS (numbers in §5).
4. pstree clean: no orphans after stop/start/close cycles.
5. Two-machine test both directions (Linux server ↔ Windows server):
   crossing, wheel, keys, clipboard, sleep/wake recovery.
6. Docs updated to match reality; commit on `backend-cleanup`.
