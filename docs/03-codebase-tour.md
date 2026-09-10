# 3. Codebase tour

Where everything lives, how to start reading, and a walkthrough of a
running system. Read this after [Overview](01-overview.md) and
[Architecture](02-architecture.md) — it maps the concepts onto the
actual files.

## 3.1 Repository layout

```
kvmshare/
├── Cargo.toml              # Rust workspace: protocol, log, core, platform, app
├── Makefile                # build, install, dev, test, release, publish
├── scripts/                # dev.sh (watch → rebuild), gen_icon.py (icon assets)
├── packaging/              # udev input rule (reference) + .desktop launcher
│
├── crates/
│   ├── protocol/           # the binary wire format (frame + messages)
│   ├── log/                # leveled, hot-reloadable logging
│   ├── core/               # layout, session brain, server & client transport logic
│   ├── platform/           # OS backends: X11 + evdev (Linux), Windows
│   └── app/                # the kvmshare-server / kvmshare-client binaries
│   #
│   # Tests live next to the code they exercise, as `foo/tests.rs`
│   # submodules (or a `tests/` dir for a multi-file suite, e.g.
│   # core/src/session/tests/); integration e2e in app/tests/e2e/.
│
└── gui/                    # Wails v3 desktop app (Go backend + React frontend)
    ├── main.go             # app entry: single instance, window, tray
    ├── app.go              # bound service: state + construction
    ├── paths.go / settings.go / instance.go   # file layout, persisted settings, single-instance
    ├── process.go / roles.go / rolelock.go    # spawn plumbing, role start/stop, lock detection
    ├── peers_api.go / discovery_host.go       # discovery bridge methods + Host adapter
    ├── clients.go / clientstate.go / live.go  # connected-clients, client state, live status
    ├── config.go / trust.go / update.go / netlog.go / machine_id.go   # config, pairing trust, updates, log tailing
    ├── tray.go / tray_xembed_linux.go / tray_menu_linux.go /           # tray + XEmbed cgo engine /
    │       tray_xembed_bridge_linux.go                                  #   popup menu / pure-Go wiring
    ├── frontend/           # Vite + React + TypeScript UI (embedded in the binary)
    ├── cmd/kvmshare-install/   # CLI installer/updater
    ├── installer/          # GUI installer (Wails window)
    └── internal/           # decoupled packages: discovery, ids, installer,
                            # selfupdate, fileutil (atomic writes),
                            # sessionbus (D-Bus ownership), notify (watcher)
```

The Rust side is a **Cargo workspace**: `cargo build --workspace`
builds all five crates and both binaries. The GUI is a separate Go
module under `gui/` with its own build (the frontend is compiled by Vite
and embedded with `//go:embed`).

## 3.2 Entry points

There are four executables:

| Executable | Source | What it does |
|------------|--------|--------------|
| `kvmshare-server` | `crates/app/src/bin/kvmshare-server.rs` | The server role: layout, listen, forward input |
| `kvmshare-client` | `crates/app/src/bin/kvmshare-client.rs` | The client role: connect, inject input |
| `kvmshare-gui` | `gui/main.go` | The desktop app that manages both roles, the layout, logs, updates |
| `kvmshare-install` | `gui/cmd/kvmshare-install/main.go` | Standalone installer/updater bootstrap |

**If you want to trace "how does input move", start at
`crates/app/src/bin/kvmshare-server.rs`** and follow the links in
section 3.4. That single file is the shortest path from process start to
"cursor crosses onto a client".

## 3.3 Reading order for the Rust code

1. **`crates/protocol/src/message/mod.rs`** — the `Message` enum is the
   vocabulary of the whole system. Read it first: every other crate
   speaks these messages.
2. **`crates/core/src/lib.rs`** — the crate map (`layout`, `session`,
   `server`, `client`, `motion`, `transport`, `udp`).
3. **`crates/core/src/session/mod.rs`** — the switching brain. Pure
   logic; read `boundary.rs` for the crossing model.
4. **`crates/core/src/server/mod.rs`** — how the server wires the
   session to the network.
5. **`crates/core/src/client/mod.rs`** — how the client wires the
   injector to the network.
6. **`crates/platform/src/lib.rs`** — where the OS backends plug in.
7. **`crates/app/src/bin/kvmshare-server.rs`** — the full server
   startup, top to bottom.

## 3.4 Walkthrough: server startup

```
kvmshare-server [--config PATH] [--port N] [--log-level L] [--logctl PATH]
```

1. `parse_server_args()` → `ServerArgs` (`crates/app/src/args.rs`).
2. `kvmshare_log::init(...)` — logger up (level from `--log-level` or
   `KVMSHARE_LOG`; optional `--logctl` file for hot reload).
3. `kvmshare_platform::raise_priority()` — the role must outrank busy
   apps (best-effort).
4. `guard::acquire(guard::ROLE_SERVER)` — take the `server.lock` role
   lock. Refuses if a client runs on this machine. **Holding this
   `RoleGuard` for the process lifetime is what enforces "one role per
   machine".**
5. `Config::load_or_create(&config_path)` — read
   `~/.config/kvmshare/kvmshare-server.toml`; **create a
   machine-accurate default if missing** (this machine's real hostname +
   display geometry, no invented clients).
6. `kvmshare_platform::server(None)` → `(input, engine, clipboard,
   liveness)`:
   - `input`: a channel of local input messages (the capture thread
     feeds it)
   - `engine`: `Box<dyn Engine>` — control of the local cursor
   - `clipboard`: `Box<dyn Clipboard>` — local clipboard, own lock
   - `liveness`: heartbeats for the supervisor
7. `Server::with_control(session, port, Some(ctl_rx))` — bind TCP +
   UDP on the same port. The control channel delivers layout
   hot-reloads.
8. `spawn_server_clipboard(...)` — a thread polls the local clipboard
   every 500 ms and broadcasts changes to all clients.
9. `spawn_config_watcher(path, ctl_tx)` — a thread polls the config
   file every 600 ms; on change it sends `Control::Reload(new_layout)`.
10. `server.run(input, engine, clipboard, liveness)` — spawns the
    supervisor, the accept thread, the UDP receiver, then processes
    local input forever.

That's the whole server. The heavy lifting is inside `core::server`
(see [Core](05-core.md)).

## 3.5 Walkthrough: client startup

```
kvmshare-client SERVER[:PORT] [--name NAME] [--log-level L] [--logctl PATH]
```

1. Parse args; init the logger; raise priority.
2. `guard::acquire(guard::ROLE_CLIENT)` — the client role lock.
3. Loop forever (reconnect every 3 s on failure):
   - If Windows shows the UAC secure desktop, wait it out (no injected
     input can land there).
   - `kvmshare_platform::client(None)` → `(injector, clipboard)`.
   - `Client::connect(addr, name, screen_info)` — TCP handshake
     (`Hello`/`Welcome`), open the UDP cursor stream, register.
   - `client.run(injector, clipboard, &outbox)` — spawns the motion /
     UDP / sync / supervisor threads and services TCP until the link
     closes, then loops back to reconnect.

## 3.6 Walkthrough: the GUI

`gui/main.go`:

1. `NewApp()` — resolve config, binaries, logs, state dir.
2. `SingleInstance()` — take `gui.lock`; a second launch raises the
   running window and exits.
3. `ensureSessionBus(stateDir)` — Linux: make sure a D-Bus session bus
   exists (adopt one or start exactly one private one; this prevents
   the immortal per-launch process stacks).
4. `ensureInputAccess()` (Linux server) — grant udev input access via
   the sibling installer, silently, at most one privilege prompt.
5. `ensureUacAnswerable()` (Windows) — move UAC prompts to the normal
   desktop so the shared mouse can answer them.
6. Create the Wails app + window, register the `App` service, set up
   the tray, start the notification watcher, run.

The GUI never talks the wire protocol; it manages processes, config,
logs, and updates (see [GUI](08-gui.md)).

## 3.7 The state directory

Everything that is not the config lives under
`~/.local/state/kvmshare/` (`KVMSHARE_STATE` overrides it; on Windows it
resolves via `USERPROFILE`/`LOCALAPPDATA` when `HOME` is unset):

| File | Purpose |
|------|---------|
| `server.lock` / `client.lock` | Role locks (held while the role runs) |
| `server.pid` / `client.pid` | Pid recorded beside the lock (stop-by-pid) |
| `gui.lock` / `gui.pid` | Single-GUI instance lock |
| `server.log` / `client.log` | The role's log (stderr redirected here) |
| `server.logctl` / `client.logctl` | Log level/enabled control files (hot-reloaded) |
| `gui.json` | GUI settings (role, client addr/name, log prefs) |
| `dbus.sock` / `dbus.pid` | Private session bus (Linux, when none exists) |
| `update/` | Temp dir for in-place updates |

## 3.8 Where the config lives

`~/.config/kvmshare/kvmshare-server.toml` (`KVMSHARE_CONFIG` overrides;
`--config` wins over both). The GUI and the server agree on this file:
the server creates a machine-accurate default on first start, and the
GUI edits the same file. See [App — config](07-app.md#71-the-config-file).

---

**Next:** [4. Wire protocol](04-wire-protocol.md) — the exact bytes on
the wire.