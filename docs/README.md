# kvmshare documentation

kvmshare shares one keyboard and mouse across multiple machines. This
documentation set is written for a developer joining the project — no Rust
or Go background required. It takes you from *what the software is* to
*how each layer works*, so you can read the code, change it, or rebuild
the whole thing from scratch without getting lost.

## Reading order

| # | Doc | What it answers |
|---|-----|-----------------|
| 1 | [Overview](01-overview.md) | What kvmshare does, the core concepts, the vocabulary used everywhere in the code |
| 2 | [Architecture](02-architecture.md) | How the pieces fit together, the data flow for one mouse move, the threading model, the key design decisions |
| 3 | [Codebase tour](03-codebase-tour.md) | Where everything lives, the entry points, and how to start reading the code |
| 4 | [Wire protocol](04-wire-protocol.md) | The binary protocol between server and client, frame by frame |
| 5 | [Core crate](05-core.md) | The platform-independent brain: layout, cursor session, motion, server, client |
| 6 | [Platform crate](06-platform.md) | The OS backends: X11, evdev, Windows, and the key tables |
| 7 | [App crate & CLI](07-app.md) | The two binaries, the config file, role locking |
| 8 | [GUI](08-gui.md) | The Wails desktop app: Go backend + React frontend, process management, tray, installer |
| 9 | [Building & releasing](09-build-release.md) | Make targets, the dev loop, release archives, self-update |
| 10 | [Testing](10-testing.md) | What is tested, where, and how to run the suites |

## The one-paragraph mental model

Two machines run kvmshare. One is the **server** — its keyboard and mouse
are the *shared* ones. The other is a **client** — it is controlled
remotely. A **layout** (a config file) arranges the two screens on a
virtual desktop. When the cursor reaches the edge of the server's screen
next to the client, it **crosses** onto the client: control switches,
the server's input stream is forwarded, and the client's cursor is
steered by a closed loop until the user crosses back.

Everything about that flow — layout math, crossing decisions, message
routing, motion smoothing — is pure logic in the `core` crate, tested
without any OS. The OS-specific work (capturing physical input, moving
the cursor, injecting keys) lives behind two small traits implemented by
the `platform` crate. The GUI manages processes, the layout, logs and
updates; the protocol crate is the wire format between the two machines.

## Conventions used in these docs

- `crates/…` paths are relative to the repository root.
- Code identifiers link nowhere in plain Markdown; use `rg` to find them
  (`rg "PositionFollower" crates/`).
- "Server" with a capital S always means the kvmshare server role; "the
  server machine" means the physical machine running that role.