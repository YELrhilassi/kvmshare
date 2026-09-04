# Roadmap / known hardening items

Items below were identified during real two-machine testing and Windows
bring-up. They are tracked here so the work is not lost; each entry states the
problem observed, the fix direction, and why it matters for a plug-and-play
experience.

## 1. Transport resilience: survive transient network loss

**Observed:** during live testing the client laptop's wireless link dropped
intermittently (WiFi power-saving, brief disconnects). When the link blips, the
kvmshare TCP connection dies with it and the session does not come back until
the user manually restarts a role.

**Fix direction:**
- Reconnect with exponential backoff (client side) and automatic session
  re-establishment (server side) so a dropped link recovers without user
  action.
- Keepalive/application-level heartbeat tuned below the OS TCP timeout so a
  dead peer is detected in seconds, not minutes, and idle links stay fresh.
- On reconnect: re-send the layout + screen-shape state and resume cleanly;
  never leave the cursor "stuck" on one machine or the UI showing a stale
  connection.
- The GUI role state should reflect connected / reconnecting / disconnected
  live, driven by the backend, so the user sees exactly what is happening.

**Why:** "solid, non-dropping connections between client and server" is a core
requirement. A KVM session that requires manual restart after any network
blip is not production-solid.

## 2. Missing platform dependencies: handled by the app, not the user

**Observed:** the Windows GUI (Wails v3) silently fails to open when the
Microsoft Edge WebView2 Runtime is absent — there is no error message and no
guidance. The runtime had to be installed by hand.

**Fix direction:**
- On startup the GUI should detect required per-platform dependencies
  (WebView2 on Windows; on Linux the platform backend deps) and surface a
  clear, actionable state: "missing runtime — Install" with a link or a
  guided silent install (WebView2 bootstrapper supports `/silent /install`).
- The installer should bundle or fetch the WebView2 Evergreen bootstrapper
  (it is ~2 MB and architecture-aware) instead of requiring a manual visit.
- Check the WebView2 registry keys
  `HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-...}` and
  `HKCU\Software\Microsoft\EdgeUpdate\Clients\{F3017226-...}` (either one
  present with a version > 0.0.0.0 means the runtime exists).

**Why:** plug-and-play means the app handles its own prerequisites. Users
should never have to research why the GUI won't open.

## 3. Network details: always show the address that actually works

**Observed:** one test host held two DHCP leases on the same interface (a
primary and a stale secondary address). The stale secondary was displayed as
"the" server address and the remote client could not reach it — only the
primary address accepted connections.

**Fix direction:**
- When the GUI shows "your address" for the server, prefer the address used
  for the default route (the source address traffic actually leaves on), and
  clearly separate primary vs. secondary addresses.
- Consider warning when multiple leases exist on one interface.
- Long term: encourage/help pinning a stable address (DHCP reservation) since
  roaming IPs break client configs.

**Why:** a client pointed at a stale address fails with no obvious cause; this
exact confusion cost a whole debugging session.

## 4. Windows release build parity (build-machine note)

**Observed:** linking Windows Rust binaries on Linux requires the GNU target +
mingw-w64 (`cross-x86_64-w64-mingw32` on Void, `rustup target add
x86_64-pc-windows-gnu`). The `windows-msvc` target only type-checks on Linux
(it needs `link.exe` to produce binaries).

**Fix direction:** keep `make release` producing Windows assets whenever the
mingw toolchain is present, and document the toolchain requirement in
`platforms.md` so a fresh build machine doesn't silently ship Linux-only.

**Why:** portable releases must include Windows binaries without the developer
remembering undocumented manual steps.
