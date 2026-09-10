# 10. Testing

Testing is layered the same way the code is: pure-logic unit tests
(with no OS or display), integration tests over the real transports, and
Go tests for the GUI. `make test` runs the whole thing.

## 10.1 Running the suites

```bash
make test        # cargo test --workspace  +  go test ./gui/...
```

Or individually:

```bash
cargo test --workspace           # Rust: protocol, core, platform, app, e2e
cd gui && go test ./...          # Go: app state, processes, notify, selfupdate
```

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
| **End-to-end** | `crates/app/tests/e2e/` | a real `Server` + real `Client` over real TCP/UDP with mock input + a recording injector: cursor enters/moves/crosses back, motion delivers the full command, buttons/keys forward, reconnect is not deafened by stale UDP sequences, crossing after idle survives the beacon watchdog, disconnect returns home, unknown clients admitted dynamically, config hot-reload returns the cursor home and drops stale clients |

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