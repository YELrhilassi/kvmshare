# 10. Testing

Testing is layered the same way the code is: pure-logic unit tests
(with no OS or display), integration tests over the real transports, and
Go tests for the GUI. `make test` runs the whole thing.

## 10.1 Running the suites

```bash
make test        # cargo test --workspace  +  go test  +  frontend vitest
```

Or individually:

```bash
cargo test --workspace           # Rust: protocol, core, platform, app, e2e
cd gui && go test ./...          # Go: app state, processes, notify, selfupdate
cd gui/frontend && npm test      # vitest: stores, components, page composition
cd gui/frontend && npm run typecheck   # tsc over the app (and the configs)
```

`npm run typecheck` matters on its own: `vite build` strips types
without checking them, so a real type error only shows up here.

Cross-platform compile checks (cheap; run when touching core/platform/
app or the GUI's Go files):

```bash
cargo check --target x86_64-pc-windows-msvc --workspace   # Rust (type-check only)
cd gui && GOOS=windows GOARCH=amd64 CGO_ENABLED=0 go build -tags production .
```

## 10.2 What the Rust tests cover

| Area | Where | What's proven |
|------|-------|---------------|
| Protocol round-trips | `crates/protocol/src/message/mod.rs`, `wire.rs`, `frame.rs` | every message encodes/decodes exactly; trailing bytes and unknown types rejected; oversized payloads rejected; partial reads assembled |
| Layout math | `crates/core/src/layout/tests.rs` | adjacency, entry points, exit direction, normalization |
| Session / crossings | `crates/core/src/session/tests/` | wall-band arming, push firing, the parked hidden cursor, entry-inset anti-bounce, escape key, dynamic admission, layout swaps, disconnect-returns-home |
| Motion | `crates/core/src/motion/{follower,gain,pending,probe}/tests.rs` | gain windows, pending motion accumulation/truncation, follower convergence/overshoot bounds, probe windows |
| Transport & UDP | `crates/core/src/udp/tests.rs` | framing, desync resync, envelope pack/unpack, sequence wrap (`is_newer`) |
| Role locking | `crates/app/src/guard.rs` | locks are exclusive, mutual exclusion between roles, re-acquire after release, locks survive the files existing |
| Config | `crates/app/src/config/` (mod + io + geometry) | round-trip to layout, duplicate-name rejection, local-screen correction |
| Args | `crates/app/src/args.rs` | default-port normalization |
| Logging | `crates/log/src/lib.rs` | level parsing/ordering, control-file hot reload, enabled toggle |
| Key tables | `crates/platform/src/keys.rs` | both directions consistent (a bad entry can never silently break a cross-OS pair) |
| Windows capture decode | `crates/platform/src/windows/capture/tests.rs` | raw-input → message translation |
| Audio pipeline | `crates/core/src/audio/` | packetising, jitter reorder/drop, activity detection, the per-peer UDP transport, format negotiation, and the runtime state machine — including that status transitions reach the sink, that a dead device reports and stops instead of hanging, that the meter publishes on a move and then waits (not per packet) and clears when a direction stops, that a peer which moves to a new port is told about this machine's stream again, that an inactive runtime costs nothing, and that starting a send engages the exclusive route (capturing the device it reports) while stopping hands the real output back exactly once |
| Media router | `crates/core/src/media.rs` | one target resolves every media key (playback and volume alike); the override outranks it; routing off never intercepts; an unresolved target is local, never swallowed; and a pinned machine that is disconnected falls back rather than vanishing |
| Audio peer rule | `crates/core/src/server/audio.rs` | a configured peer is authoritative, ids match by prefix, an unpinned link requires being alone, and — with several machines connected — only the *sending* direction is refused while receiving follows the machine that is playing |
| Audio state file | `crates/app/src/audio_state.rs` | every field is written (including the meters, the peer name and the capture warning), a failure reason, a peer name and a capture warning stay on one line, digital silence is written as the meter's floor rather than `-inf`, and a stale file is cleared |
| Audio platform (Linux) | `crates/platform/src/audio/linux/` | the `parec`/`pacat` argument contracts, the default output spelled as PulseAudio's own `@DEFAULT_SINK@` (the literal name `default` is rejected by `pacat` with "No such entity"), a configured output passed through untouched, the monitor's recording gain parsed from `pactl`'s reply, and the three capture warnings (muted sink, muted monitor, low gain) each naming what to change; `route.rs` pins the exclusive-send parsing — the share module found by its *exact* argument (a prefix match would unload the wrong sink), and the sink-input ids read from `pactl list short sink-inputs` |
| Test tone | `crates/app/src/audio_test.rs` | the tone is a real signal at the negotiated format, ramps in and out instead of clicking, and a device that refuses reports a reason rather than silence |
| **End-to-end** | `crates/app/tests/e2e/` | a real `Server` + real `Client` over real TCP/UDP with mock input + a recording injector. `session_tests.rs`: cursor enters/moves/crosses back, motion delivers the full command, buttons/keys forward, reconnect is not deafened by stale UDP sequences, crossing after idle survives the beacon watchdog, disconnect returns home, config hot-reload returns the cursor home and drops stale clients. `admission_tests.rs`: dynamic admission, the allowlist, trust by short id, revocation at the handshake and hot. `policy_tests.rs`: media routing to a pinned machine, the hot-reload re-arm, the media_pin override. `main.rs` holds the shared harness |

The e2e suite is the closest thing to a manual two-machine test that
runs without any OS plumbing — the platform traits are replaced by
recorders, so the whole pipeline (session → server → wire → client →
injector) is exercised.

## 10.3 What the Go tests cover

- `gui` package tests (`*_test.go` next to their files) — settings
  persistence (including the "absent log
  fields default to ON" migration), config load/save validation,
  process start/stop + role exclusivity, instance locking, network
  listing, log tailing.
- `gui/internal/notify/notify_test.go` — the client connect/disconnect
  log parser and transition detection.
- `gui/internal/discovery/discovery_test.go` — beacon datagram
  classification and address normalization.
- `gui/internal/selfupdate/selfupdate_test.go` — version comparison,
  checksum parsing, archive extraction.
- `gui/audio_test.go` — the `--audio-devices` JSON contract with the
  Rust binary, the `--audio-test-tone` JSON contract, and the
  `audio.state` reader/reconciliation (a missing file is inert, a file
  from a dead role never reads as a live stream and its meters are
  cleared with it, a non-finite level is no reading rather than a
  full-scale bar, a missing role binary is a reported error).

### Frontend (`gui/frontend/src/**/*.test.ts[x]`, vitest + jsdom)

- `features/media/mediaStore.test.ts` — the page's state machine: load
  for both roles, dirty tracking, "save only the dirty half", the
  autosave debounce (a burst of edits is one write, nothing dirty writes
  nothing, a pending edit flushes on unmount, two writes never overlap),
  the single-direction normalization, and the error paths (load/save
  failure, device-list failure is non-fatal).
- `features/media/AudioSectionEditor.test.tsx` — the single-direction
  chooser (picking one direction turns the other off) and the device
  pickers (options offered, a configured device kept even when not
  listed, the empty-list explanation).
- `features/media/AudioActivity.test.tsx` — the live panel's pure part:
  which device a test tone goes to per direction, where a level sits on
  the  meter (including a missing or impossible reading), and every
  wording of a test result — played and heard (with the "it is on its way
  to the other machine" tail while sending), played and *not* heard
  (the "capture device is wrong" case), refused, and played with no link
  to test.
- `features/media/MediaPage.test.tsx` — role-dependent composition, the
  live audio picture (playing with a level, linked-but-silent, failed,
  inactive), the peer picker built from the connected machines (and the
  warning when sending has more than one to choose between), the media
  machine pin writing the machine **id** (not the display name — the
  routing bug that made a pinned key stay local), the test tone's button
  and verdict, and that an edit saves with no Save button.
- `features/media/targets.test.ts`, `lib/store.test.ts` — routing-target
  labels, `pinnedMachine`/`sameMachine` (the pin and the connected client
  must agree, ids matching by prefix in either direction), and the
  observable-store primitive.

The bridge is faked at the `api()` boundary and the app context at
`useApp` — the same seams the real ones enter through — so these run with
no display and no Wails runtime.

## 10.4 Test philosophy

- **The session brain is tested exhaustively without an OS** — this is
  why crossings, edge cases and regressions are caught in CI rather than
  on real hardware.
- **Tests live next to the code** (each module's `tests.rs` submodule —
  e.g. `crates/core/src/layout/tests.rs`), so a module's tests move with
  it and there are no `#[path]` attributes to drift.
- **Platform behavior that cannot run in CI** (real X11 grab semantics,
  Windows Raw Input, hooks, DPI) is either isolated in unit-testable
  pure functions (the windows capture decode, the key tables) or
  documented for hardware exercise in the platform docs.
- The e2e suite pins the *wiring* so refactors of the transport or
  threading can't silently break the end-to-end flow.

---

That completes the documentation tour. If you want to build kvmshare
from scratch, start with the [Overview](01-overview.md) and
[Architecture](02-architecture.md), then use the
[Codebase tour](03-codebase-tour.md) as your map while you read the
[Core](05-core.md) and [Platform](06-platform.md) crates in parallel.