# 1. Overview

kvmshare is a from-scratch, cross-platform **KVM** — keyboard / video /
mouse — sharer. One machine's keyboard and mouse control several
machines at once: move the cursor off one screen's edge and it appears
on the neighboring machine, taking keyboard and clipboard with it.

Think of the well-known tools Synergy, Barrier or Deskflow — kvmshare is
a clean-room implementation built around the hard lessons of those
tools, with a design that is deliberately different where they struggle.

## 1.1 What it does, concretely

- **One keyboard and mouse, many machines.** The machine with the
  physical keyboard/mouse is the *server*. Other machines are *clients*.
- **Screen-edge crossing.** The screens are arranged in a virtual
  desktop. Pushing the cursor to the shared edge crosses to the
  neighbor.
- **Keyboard follows the cursor.** When the cursor is on a client, the
  server's keyboard types on the client. Keyboard (and clipboard) "ride
  along" with the cursor.
- **Clipboard sync.** Text copied on either side appears on the other.
- **One role per machine.** A machine runs as a server **or** a client,
  never both. The same software does either job — the role is a
  selection, not a separate product.
- **Runs in the background.** Closing the GUI hides it to the system
  tray; the role processes keep running and are discovered/adopted when
  the GUI returns. There is no "start this then keep this window open"
  workflow.

## 1.2 The two roles

| Role | Machine's job | What it needs |
|------|---------------|---------------|
| **Server** | Owns the shared keyboard/mouse and the virtual desktop layout | The physical input devices; a network listen port (default `24800`) |
| **Client** | Is controlled by a server; injects received input locally | The server's address (e.g. `192.168.1.86:24800`) and a screen name (its hostname by default) |

On the server, "local" means the server machine's own screen. On a
client, "local" means the client machine's screen.

## 1.3 The virtual desktop (layout)

A **layout** is a list of **screens**, each with a name, a size
(width × height) and a position (`x`, `y`) on a shared 2D plane.

```
              virtual desktop
   ┌──────────────┐   ┌──────────────┐
   │   client     │   │   server     │
   │   "hp"       │   │   "pc"       │
   │ 1920×1080    │   │ 1920×1080    │
   │ x=-1920      │   │ x=0          │
   └──────────────┘   └──────────────┘
```

- The **first screen (id 0) is always the server's own screen.**
- Clients are matched to screens **by name** — the name a client sends
  in its `Hello` (its hostname by default, or `--name`).
- Positions are relative to the server screen: `x < 0` means "to the
  left", `x > 0` "to the right", negative `y` means "above".
- The layout lives in a TOML config file, editable by hand or through
  the GUI's Layout page. The server hot-reloads it: saving applies live,
  no restart.

A client whose name is **not** in the layout is not rejected — it is
*admitted dynamically* (placed to the right of the desktop) so a fresh
pair of machines works before either has been configured. The Layout
page is where a permanent position is pinned.

## 1.4 The two links between machines

Each connection uses **two transports** on the same port:

- **TCP** — the reliable control channel: handshake, layout, enter/leave,
  buttons, keys, wheel, clipboard, keepalives. Ordered and lossless.
- **UDP** — the cursor stream: relative mouse motion (server → client)
  and real-cursor position beacons (client → server). Both are
  *additive and loss-tolerant* — a dropped delta just means the cursor
  travels a few pixels less, so nothing is retransmitted.

Why two? Mouse motion is latency-critical and loss-tolerant, so it must
never be delayed by the reliable stream's buffering or a busy peer's
TCP backpressure — that coupling is exactly what made other KVMs feel
"clumpy" under load. Everything that must not be lost stays on TCP.

## 1.5 Key vocabulary (used everywhere in the code)

| Term | Meaning |
|------|---------|
| **Layout** | The list of screens on the virtual desktop |
| **Session** | The pure-logic "brain": the virtual cursor + the crossing rules. Lives in `core::session` |
| **Local screen** | The server's own screen (id 0) |
| **Active client** | The client the cursor is currently on (`None` = cursor is local) |
| **Raw input** | Device deltas straight from the hardware, *before* the OS's pointer acceleration |
| **Beacon** | A real cursor position report (the *visible* cursor, post-acceleration), used as ground truth |
| **Wall / edge band** | The screen edge; the narrow band (2 px) in which a real cursor counts as "at the wall" |
| **Crossing** | The act of the cursor moving from one screen to a neighbor |
| **Enter / Leave** | Server → client messages handing control to / taking control from a client |
| **Engine** | The platform trait a *server* uses to control its own machine (warp, grab, hide cursor) |
| **Injector** | The platform trait a *client* uses to affect its own machine (move cursor, click, type) |
| **Gain** | The server's measured pixels-per-count transform, applied to forwarded motion so the client's cursor mirrors the server's |
| **Role lock** | An OS file lock that enforces "one role per machine" and "one instance per role" |

## 1.6 Feature status

- **Linux / X11** — full: XI2 raw capture, kernel-level device isolation
  while remote, XTest injection, XFixes cursor control, clipboard sync,
  GUI with tray + notifications.
- **Windows** — full backend written (Raw Input capture, SendInput
  injection, Win32 clipboard, low-level-hook input isolation, UAC secure
  desktop handling) and cross-compiles clean; exercised on real
  hardware.
- **macOS** — not implemented; stubs fail with a clear error. The
  architecture is ready (see [Platform crate](06-platform.md)).
- **Wayland** — planned; the input reader is X-free by design so a
  Wayland backend reuses it unchanged (see [Platform crate](06-platform.md)).

---

**Next:** [2. Architecture](02-architecture.md) — how the pieces fit
together and why the mouse motion is smooth.