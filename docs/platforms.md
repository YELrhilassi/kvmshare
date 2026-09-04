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
| `kvmshare-platform`  | ✅ X11 backend     | ✅ backend, compile-checked | ⚠️ stub, no input  |
| GUI (Wails v3)       | ✅ GTK4/WebKitGTK6 | ✅ compiles (PE32+) | ⚠️ untested        |

✅ = built, tested, in use. "Compiles" = cross-compiled clean, zero
warnings (details below). "Compile-checked" = full backend written and
cross-compiled clean, but not yet exercised on real hardware (we
develop on Linux). ⚠️ = builds but does not yet *do* the thing.

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

## Windows: implemented, next is hardware verification

The Windows backend in `kvmshare-platform/src/windows/` is written and
cross-compiles clean (zero warnings). It mirrors the X11 backend
structure exactly — same `Engine`/`Injector` contracts, same canonical
HID key model, same message flow:

| Piece            | Linux impl (X11)            | Windows impl (written, compile-checked) |
| ---------------- | --------------------------- | --------------------------------------- |
| Input capture    | XI2 raw device events       | Raw Input on a hidden message-only window (`RegisterRawInputDevices`, `RIDEV_INPUTSINK`; absolute-mode deltas handled, keyboard auto-repeat deduplicated) |
| Cursor control   | XFixes warp+hide            | `SetCursorPos` + `ShowCursor` (transition-balanced, no ref-count drift) |
| Input injection  | XTest (client side)         | `SendInput` in scan-code mode (layout-independent, like XTest) |
| Clipboard        | arboard                     | Win32 clipboard via `CF_UNICODETEXT` with pure-Rust UTF-8/UTF-16 conversion |
| Key identity     | HID ↔ evdev (X11)           | HID ↔ set-1 scancode (+ E0 flag) — same `platform::keys` module, roundtrip-tested on Linux |
| DPI              | n/a (X11 pixels)            | per-monitor DPI aware at startup; `GetDpiForSystem` for the scale report |

Because `SetCursorPos` never generates raw input (same property as XI2
raw events), the anti-oscillation design carries over unchanged: parking
the hidden server cursor can never feed phantom motion into the session.

### What remains before Windows is "real"

- Run the server and client on actual Windows machines: raw-input
  capture, injection, cursor hide/show and the clipboard need hardware
  exercise (a Linux host can only compile-check them). Two runtime
  details were already hardened in review: E1-prefixed keyboard
  sequences (Pause) are dropped instead of mis-forwarded as Num Lock,
  and a failed `SetClipboardData` frees the block it allocated.
- `cargo test --target x86_64-pc-windows-msvc` on a Windows host so the
  platform unit tests actually execute.
- The GUI needs a Windows desktop session to verify tray, close-to-tray
  and notifications visually.

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
  all platforms. On Linux the close-to-tray decision probes for a
  StatusNotifierWatcher (no tray host → closing quits cleanly, never a
  hidden ghost instance); Windows and macOS always have a tray, so
  closing hides there unconditionally.
- **Single instance**: `gui.lock` uses `LockFileEx` on Windows (via the
  build-tagged helpers) — same "one GUI per machine" behaviour.

## Cross-OS pairs

There is no "server OS" or "client OS" — any machine can be either
role, and the roles pair freely: the canonical HID key identity, the
same binary protocol, the same clipboard mime model, and mirrored
backends mean a Windows server driving a Linux client works exactly
like a Linux server driving a Windows client. The only OS-specific
behavior is inside the per-OS backend module; nothing in core, protocol
or GUI knows which OS is on the other end of the wire.

## macOS

Not attempted. The seams are identical: implement the platform backend
(IOKit/CGEventTap for capture+injection, `NSPasteboard` for clipboard)
and register it in `lib.rs`. The GUI is expected to build via Wails v3
(which supports macOS), with `tray.go` already using the
`darwin`-template-icon branch.

## Keeping the seams honest

- `cargo check --target x86_64-pc-windows-msvc --workspace` and the
  `GOOS=windows` build above are cheap; run them when touching `core`,
  `app`, `platform`, or the GUI's Go files so portability regressions
  surface immediately.
- The pure key tables (`platform::keys`) are compiled and tested on
  every platform, so a bad HID↔evdev or HID↔scancode entry fails the
  Linux suite — not just the Windows one.
- Never reach for `std::os::unix` / `syscall` directly outside
  `kvmshare-platform` (Rust) or `gui/process_os_*.go` (Go).

[`Engine`]: ../crates/core/src/server.rs
[`Injector`]: ../crates/core/src/client.rs
[`unsupported`]: ../crates/platform/src/unsupported.rs