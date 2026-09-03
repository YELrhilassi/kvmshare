# Platforms

kvmshare is written to be portable: the protocol, the switching core and
the GUI shell have no OS-specific code, and every platform touch-point is
behind a small, explicit seam. This document audits each layer, states
what is verified today, and is the porting guide for a new OS.

## What runs where today

| Layer                | Linux (X11)        | Windows            | macOS              |
| -------------------- | ------------------ | ------------------ | ------------------ |
| `kvmshare-protocol`  | ✅                 | ✅ compiles        | ✅ compiles        |
| `kvmshare-core`      | ✅                 | ✅ compiles        | ✅ compiles        |
| `kvmshare-app` (CLI) | ✅                 | ✅ compiles        | ✅ compiles        |
| `kvmshare-platform`  | ✅ X11 backend     | ⚠️ stub, no input  | ⚠️ stub, no input  |
| GUI (Wails v3)       | ✅ GTK4/WebKitGTK6 | ✅ compiles (PE32+) | ⚠️ untested        |

✅ = built, tested, in use. "Compiles" = cross-compiled clean, zero
warnings (details below). ⚠️ = builds but does not yet *do* the thing.

## Verified portability (commands)

Rust, from the workspace root:

```sh
rustup target add x86_64-pc-windows-msvc
cargo check --target x86_64-pc-windows-msvc --workspace   # zero warnings
```

Go GUI, from `gui/`:

```sh
GOOS=windows GOARCH=amd64 CGO_ENABLED=0 go build -tags production .
# produces a PE32+ executable — wails v3 on Windows is pure Go (w32),
# no cgo toolchain needed
```

## Architecture: where the OS is allowed to leak

Three seams concentrate everything platform-specific:

1. **`kvmshare-core` traits** — the core never touches the OS. The server
   drives the machine through [`Engine`] (warp cursor, show/hide cursor,
   clipboard); the client through [`Injector`] (move/click/keyboard,
   clipboard). Any OS can implement these.
2. **`kvmshare-platform`** — the only crate with OS code. `lib.rs` picks
   a backend per `target_os`; [`unsupported`] is the fallback that
   compiles everywhere and fails at runtime with a clear message.
3. **The GUI's process/lock helpers** — `gui/process_os_unix.go` and
   `gui/process_os_windows.go` are the only OS-tagged Go files. They hold
   process-group signalling, termination, and the file locks (flock on
   Unix, `LockFileEx` on Windows — the role/instance locks must work on
   every OS because they are the "one instance per machine" contract).

Everything else (protocol framing, the switching session, layout math,
config, the whole frontend) is plain portable code with zero `#ifdef`s.

## Windows: what is left to make it real

The GUI, CLI binaries and core all compile for Windows today. What is
missing is the input/desktop backend in `kvmshare-platform`:

| Piece            | Linux impl (X11)         | Windows port                    |
| ---------------- | ------------------------ | ------------------------------- |
| Input capture    | XI2 raw device events    | Raw Input (`RegisterRawInputDevices`) or low-level hooks |
| Cursor control   | XTest / XFixes warp+hide | `SetCursorPos` + hide via cursor state |
| Input injection  | XTest (client side)      | `SendInput`                     |
| Clipboard        | arboard                  | Win32 clipboard (CF_UNICODETEXT) |

Implement `core::server::Engine` + `core::client::Injector` in a
`kvmshare-platform/src/windows/` module and register it in `lib.rs` the
same way `x11` is. That is the entire port — the protocol, session,
config, GUI and tray logic do not change.

### Windows GUI notes

- **Notifications**: the watcher calls `org.freedesktop.Notifications`
  over DBus. On Windows there is no session bus, so the call fails
  silently (by design). A Windows port should implement the `fire`
  callback in `gui/notify.go` with a toast (e.g. via WebView2's
  notification support or a `notify_windows.go` build-tagged file).
- **Tray**: Wails v3 ships a Windows `SystemTray`; `tray.go` uses only
  the cross-platform API, so it should work once built on Windows. The
  tray icon (`assets/tray.png`) is a plain PNG, which Windows scales.
- **Close-to-tray**: `WindowClosing` → `Hide()` is handled by wails on
  all platforms; verified on Linux.
- **Single instance**: `gui.lock` uses `LockFileEx` on Windows (via the
  build-tagged helpers) — same "one GUI per machine" behaviour.

## macOS

Not attempted. The seams are identical: implement the platform backend
(IOKit/CGEventTap for capture+injection, `NSPasteboard` for clipboard)
and register it in `lib.rs`. The GUI is expected to build via Wails v3
(which supports macOS), with `tray.go` already using the
`darwin`-template-icon branch.

## Keeping the seams honest

- `cargo check --target x86_64-pc-windows-msvc --workspace` and the
  `GOOS=windows` build above are cheap; run them when touching `core`,
  `app`, or the GUI's Go files so portability regressions surface
  immediately.
- Never reach for `std::os::unix` / `syscall` directly outside
  `kvmshare-platform` (Rust) or `gui/process_os_*.go` (Go).

[`Engine`]: ../crates/core/src/server.rs
[`Injector`]: ../crates/core/src/client.rs
[`unsupported`]: ../crates/platform/src/unsupported.rs