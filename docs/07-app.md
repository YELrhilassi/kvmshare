# 7. App crate & CLI

`crates/app/` is the application layer: the two executables, the config
file model, argument parsing, role locking, and the server's clipboard
poller. Everything here is above the core/platform crates.

## 7.1 The config file

**File: `crates/app/src/config.rs`**

The server config (`kvmshare-server.toml`) describes **one machine's
role as a server**: the port to listen on, the connection policy, and
the virtual desktop layout.

```toml
port = 24800

[network]          # who may connect (see §7.1a)
allowlist = true
local_only = true
trusted_ids = []

[[screens]]
name = "pc"        # the FIRST screen is always this server's own
width = 1920
height = 1080
x = 0
y = 0

[[screens]]
name = "hp"        # a client, matched by the name it sends in Hello
width = 1920
height = 1080
x = -1920          # hp sits to the LEFT of pc
y = 0
```

- `Config::load` validates (at least one screen, unique non-empty
  names) and `to_layout()` builds the wire layout with `id 0` = server,
  then `1..n` in order — **normalized** (near-adjacent screens snapped
  into exact contact).

### §7.1a The `[network]` policy

Who may connect, enforced by the server at handshake time:

- `allowlist = true` (default) — only accept clients whose **exact
  name** appears in the layout, plus any `trusted_ids`. A client that
  is neither is refused with `Error` code 5 (`NOT_ALLOWED`).
- `local_only = true` (default) — only accept connections whose peer
  address is on the local network (private/loopback ranges); anything
  else is refused with code 6 (`NOT_LOCAL`).
- `trusted_ids` — machine ids allowed to connect even when their name
  is not in the layout yet. A trusted client is **admitted
  dynamically** on first connect: it gets the next screen id and the
  new layout is broadcast to every client, so pairing a fresh machine
  is headless — no need to plug a mouse in first.

The GUI edits the same `[network]` section live; the server hot-reloads
it with the rest of the config.
- `Config::for_this_machine()` — a default describing *this machine
  only*: the real hostname and real display geometry (via
  `platform::primary_display`, physical pixels ÷ DPI scale → logical
  pixels). No invented clients.
- `Config::load_or_create(path)` — the only path a *first* server start
  goes through: a missing config is never an error and never a stale
  copy of another machine's layout; it is created atomically.
- `default_config_path()` — `KVMSHARE_CONFIG`, else
  `~/.config/kvmshare/kvmshare-server.toml`, else `./kvmshare-server.toml`.

**The GUI edits the same file** (`gui/config.go`) — the server watches
it and hot-reloads, so saving in the Layout page applies live.

## 7.2 Role locking

**File: `crates/app/src/guard.rs`**

A machine runs **one** kvmshare role at a time. Each binary takes an
exclusive OS file lock (`fs2` → `flock(2)` on Unix, `LockFileEx` on
Windows) on its role file and refuses to start if the *other* role's
lock is held:

- `guard::acquire(ROLE_SERVER)` — holds `server.lock` for the process
  lifetime; fails if a client is running here.
- `guard::acquire(ROLE_CLIENT)` — the mirror image.
- Orphan-safe: the lock dies with the process — a crash leaves no stale
  lock, so there are no ghost instances and no manual cleanup.
- The pid is recorded in a **separate** `<role>.pid` file (never inside
  the lock file — on Windows a byte-range lock blocks reads of the
  locked range, which would make the pid unreadable by the GUI).
- `state_dir()` resolves `KVMSHARE_STATE` → `~/.local/state/kvmshare`
  (`HOME`/`USERPROFILE`) → `LOCALAPPDATA` (Windows without a profile) →
  `.kvmshare-state` fallback. The GUI pins its children to its own
  state dir via `KVMSHARE_STATE`, so both sides always coordinate on the
  same lock/log files.

## 7.3 Argument parsing

**File: `crates/app/src/args.rs`**

- `kvmshare-server [--config PATH] [--port N] [--log-level L] [--logctl PATH]`
- `kvmshare-client SERVER[:PORT] [--name NAME] [--log-level L] [--logctl PATH]`
  — a bare host gets the default port appended (`with_default_port`).
- `--logctl` points at the GUI-written control file for live log level /
  enable changes.

## 7.4 Machine id & hostname

**File: `crates/app/src/machine_id.rs`** — every machine has a stable
random id, stored in `machine.id` in the state dir. It is sent in
`Hello`/`Welcome`, shown in the GUI's client list, and used by the
`trusted_ids` policy — so a machine can be granted access by id without
any name bookkeeping.

**File: `crates/app/src/hostname.rs`** — the machine's host name (env,
`/proc/sys/kernel/hostname`, then `platform::hostname()`), used as the
default client name and as the name of a server's own screen in a
freshly created layout.

## 7.5 The server binary

**File: `crates/app/src/bin/kvmshare-server.rs`** — full startup is
walked through in [Codebase tour §3.4](03-codebase-tour.md#34-walkthrough-server-startup).
Two extra pieces live here:

- `spawn_server_clipboard` (`crates/app/src/clipboard.rs`) — polls the
  local clipboard every 500 ms and broadcasts changes to every client,
  skipping content that arrived from a client (`last_injected`).
- `spawn_config_watcher` — polls the config file every 600 ms; on a
  content change it sends `Control::Reload` to the server (hot reload,
  applied on the main loop).
- `spawn_client_events` — receives the server's `ServerEvent`s
  (connect/disconnect/screen change) and writes `clients.json` in the
  state dir (name, machine id, address, connected-at, screen info). The
  GUI watches this file for its client list.
- `spawn_control_watcher` — watches `server.cmd` in the state dir;
  each line is a `Control` command (`disconnect NAME`,
  `reconnect NAME`, `restart NAME`) sent to the named client's session.
  The GUI writes this file for its per-client buttons.
- Auto-config: when a client connects it reports its real `ScreenInfo`;
  the server updates the layout's entry for that name to the reported
  size (falling back to 1080p for an unknown-but-trusted client) and
  re-broadcasts the layout — the user never types screen sizes.

## 7.6 The client binary

**File: `crates/app/src/bin/kvmshare-client.rs`** — walks through in
[Codebase tour §3.5](03-codebase-tour.md#35-walkthrough-client-startup).
The notable bits:

- A reconnect loop (every 3 s) — a client that dies on a refused
  connection would be useless; it picks the session back up when the
  server returns.
- While the Windows UAC secure desktop is up it waits (injected input
  cannot land there); the session also ends when the secure desktop
  appears mid-session (control returns home so the prompt can be
  answered).
- The role lock keeps this the single client instance; SIGTERM ends the
  loop.
- On `Control::DISCONNECT` from the server it exits its reconnect loop
  entirely (the GUI's "Disconnect" button); `RECONNECT`/`RESTART` end
  the current session and start a fresh handshake immediately.

---

**Next:** [8. GUI](08-gui.md) — the desktop app.