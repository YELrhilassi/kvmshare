# 8. GUI

`gui/` is a **Wails v3** desktop app: a Go backend bound to a React +
TypeScript frontend built by Vite and embedded in the binary
(`//go:embed all:frontend/dist`). It manages the role processes, the
layout config, logs, the tray, notifications, and in-app updates. It
never speaks the wire protocol itself.

```
gui/
├── main.go                # entry: single instance, session bus, window, tray
├── app.go                 # the bound App service: settings, paths, processes, logs
├── config.go              # layout config (TOML) load/save for the Layout page
├── process.go             # process management (spawn/stop/adopt/auto-restart)
├── process_os_unix.go     #   Unix primitives (flock, process groups, signals)
├── process_os_windows.go  #   Windows primitives (LockFileEx, TerminateProcess)
├── notify.go              # client connect/disconnect notifications (tails server log)
├── tray.go                # system tray: status + Start/Stop/Open/Quit
├── netlog.go              # network interfaces + log tailing
├── update.go              # in-app self-update (GitHub releases)
├── sessionbus_linux.go    # D-Bus session-bus ownership (Linux)
├── ensure_input_linux.go  # one-time input-device grant (Linux server)
├── ensure_uac_windows.go  # move UAC prompts to the normal desktop (Windows)
├── fileutil.go            # atomic file writes
│
├── frontend/              # Vite + React + TS + shadcn/ui
│   └── src/
│       ├── app/           # AppProvider (role + running store), shell, nav
│       ├── features/      # one folder per page: home/, server/, client/,
│       │                  #   logs/, layout/ (canvas split into small pieces)
│       ├── components/    # shared primitives (Section, ui/*)
│       └── lib/           # bridge.ts (typed Wails calls), net, useLogTail
│
├── cmd/kvmshare-install/  # CLI installer/updater bootstrap
├── installer/             # GUI installer (Wails window)
└── internal/              # installer + selfupdate shared logic
```

## 8.1 The bound service

**File: `gui/app.go`** — `App` is the Wails-bound backend
(`window.go.main.App.*`). Its state:

- **Settings** (`gui.json`) — the role (server/client), the client
  address + name, log level and enabled flag. Persisted in the state
  dir.
- **Paths** — config path, both role binaries, both log files (resolved
  once at startup; env vars `KVMSHARE_CONFIG`, `KVMSHARE_SERVER`,
  `KVMSHARE_CLIENT` override).
- **Single instance** — `gui.lock` + `gui.pid`; a second launch raises
  the running window (SIGUSR2 on Unix — deliberately *not* SIGUSR1,
  which WebKit's JavaScriptCore claims for its GC) and exits quietly.

Key methods the frontend calls: `GetSettings`/`SetSettings`,
`GetPaths`, `LoadConfig`/`SaveConfig`, `ServerStart`/`ServerStop`/
`ServerRunning`, `ClientStart`/`ClientStop`/`ClientRunning`,
`StartActive`/`StopActive`, `ListInterfaces`, `TailLog`,
`GetLogSettings`/`SetLogSettings`, `ClearLog`, `GetVersion`,
`CheckForUpdate`, `ApplyUpdate`, `ConnectedClients`.

## 8.2 Process management

**File: `gui/process.go` + the `process_os_*.go` variants**

The GUI is a controller, not a babysitter:

- **Roles outlive the GUI.** Spawned children log to the role's log file
  and keep running when the GUI quits; the GUI *adopts* a running role
  by probing the role locks (`ServerRunning`/`ClientRunning` return true
  when the lock is held, ours or not). Start never spawns a second
  instance.
- **`proc` wrapper** — a reaper goroutine calls `Wait` and closes
  `done`, so "running" is accurate the moment the child dies (no ghost
  states); `stop()` is idempotent and signals the whole process group.
- **One role at a time** — starting a role stops the other first; a
  stop that fails (e.g. an elevated process the GUI cannot signal)
  surfaces as a clear error instead of a confusing "exited immediately".
  A conflict-retry handles the race where the other role reappears
  between cleanup and start.
- **Auto-restart** — when a server exits with code 66 (the supervisor's
  `EXIT_RESTART`: wedged input path), the GUI respawns it — bounded
  (max 3 consecutive, reset after a healthy 2-minute run) so a
  machine that keeps wedging surfaces as a real problem.
- **`checkStarted`** — a freshly spawned child gets ~350 ms to prove it
  is alive; an immediate death (role lock refused, port taken) surfaces
  as "exited immediately: <log tail>".
- **Tray Quit** stops every role (`StopAll`) — quitting the GUI must
  never strand the other machine's cursor or, on Windows, leave the
  elevated client's input gate on.

## 8.3 The Layout page and config editing

**Files: `gui/config.go` (Go), `frontend/src/features/layout/`**

The Layout page is the canvas where the user arranges the screens. It
is split into pure geometry (`geometry.ts`), a document reducer
(`useLayoutDocument.ts`), a view hook (`useCanvasView.ts`) and three
presentational pieces (Toolbar, Canvas with GridLayer/ScreenNode,
ScreenInspector):

- Real screen proportions on a world grid; **zoom is relative** — 100%
  always fits the whole desktop, so the full layout and grid are visible
  at the default zoom.
- Drag screens with **edge snapping** (12 px world), arrow-key nudge
  (1 px, 10 px with Shift), duplicate/delete, and a **lock** that
  freezes the layout against accidental edits.
- **Space is the leader key**: held down, dragging pans the canvas
  instead of moving screens (drag switch).
- Screen rectangles have a blurred translucent fill, and labels/borders
  counter-scale so they stay readable at any zoom.
- Saving writes the TOML atomically (tmp + rename) and the running
  server picks it up live — no restart.
- Screen index 0 (this machine's own screen) cannot be deleted.

## 8.4 Tray, notifications, session bus

- **`tray.go`** — a system tray item with live role status
  ("Server · running · 2 clients"), Start/Stop/Open/Quit. On Linux the
  tray is only used when a real tray host exists (a
  `org.kde.StatusNotifierWatcher` probe); with no tray host, closing the
  window quits cleanly instead of hiding into a ghost.
- **`notify.go`** — tails the server log for the stable
  `client X connected` / `disconnected` markers and raises desktop
  notifications over D-Bus; also feeds the tray's connected-client count.
- **`sessionbus_linux.go`** — makes sure a D-Bus session bus exists
  before anything touches D-Bus: adopt an existing one, else create
  **exactly one** private bus under the state dir (killed on exit).
  This prevents the classic per-launch immortal dbus-activated process
  stacks (portals, at-spi, gvfs, notification daemons) on bare-WM
  systems — see the file's docs for the full story.

## 8.5 Logs page

- Shows **this machine's own instance log** — server in server mode,
  client in client mode, never both.
- Level selector up to **trace** and an enable switch; both **hot-apply
  to the running process** via the `<role>.logctl` control files (no
  restart) and persist for whichever role starts next.
- Follow (stick-to-bottom) toggle, Clear (truncates the file; appends
  are atomic, so the running process cannot resurrect cleared lines).
- `TailLog` reads the last N lines (path validated to live under the
  state dir).

## 8.6 Updates

**File: `gui/update.go` + `gui/internal/selfupdate/`**

- The Home page version line checks GitHub for a newer release
  (`CheckForUpdate`) and applies it in place (`ApplyUpdate`): download →
  checksum verify → extract → **rename-based replacement** (safe while
  running) → restart into the new binary. Roles are separate processes,
  so a running server/client is never interrupted.
- The same machinery powers `kvmshare-install` (see
  [Build & release](09-build-release.md)).

## 8.7 Frontend architecture

- **`lib/bridge.ts`** — the typed bridge over the Wails runtime
  (`window.wails.Call.ByName("main.App.Method", ...)`); one constant
  (`APP_SERVICE`) holds the wire prefix; interfaces mirror the Go
  structs one-to-one.
- **`app/AppProvider.tsx`** — one store for the whole app: the role and
  live process state with a **single** 2 s poller. Pages read
  `useApp()` instead of running their own intervals, so they can never
  disagree about what is running. `app/App.tsx` is the shell: top bar
  with a compact nav that only shows pages belonging to the current
  role (server mode: Home/Server/Layout/Logs; client mode:
  Home/Client/Logs). A role switch on Home can invalidate the open
  page — it falls back to Home. The window title mirrors live status.
- **features/home/** — a grid dashboard: RolePicker (the one place a
  machine picks its role), ShareStatus (the *only* start/stop control;
  all other pages are read-only and point here), ConnectInfo (addresses
  to share / target to reach), QuickLinks, Updater.
- **features/server & features/client** — pure configuration pages
  (port + network / address + name). No duplicate start/stop.
- **features/layout/** — see §8.3. Canvas gestures are handled through
  refs with direct DOM writes during a drag (one state update on
  release); ScreenNode is memoized so only a changed screen re-renders.
- **features/logs/** — LogsPage (level select, enable switch, clear)
  over LogViewer (`lib/useLogTail` polls the file every 1.5 s, sticks
  to the bottom unless scrolled up).
- The design is deliberately card-free: sections are plain type over
  hairline rules with generous whitespace, dark theme default.

---

**Next:** [9. Building & releasing](09-build-release.md).