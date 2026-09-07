# 4. Wire protocol

The protocol crate (`crates/protocol/`) defines the binary format the
server and clients exchange. It is deliberately small: plain binary, no
serialization framework, hand-written encode/decode.

## 4.1 Frame layout

Every message travels inside one **frame**:

```
+--------+---------+--------+------------+-----------------+
| magic  |  type   | flags  |  length    |  payload        |
| 4 bytes| 1 byte  | 1 byte | u32 BE     | length bytes    |
+--------+---------+--------+------------+-----------------+
```

- **magic** — the 4 bytes `K V M 1` (`0x4B 0x56 0x4D 0x31`). Lets a
  receiver detect desync and rescan for the next magic instead of
  hanging on garbage.
- **type** — which message this is (see §4.3).
- **flags** — reserved (one `COMPRESSED` bit defined, unused).
- **length** — payload length, big-endian `u32`.
- **payload** — the message body.

Header size: 10 bytes. Maximum accepted payload: 8 MiB (clipboard
payloads); oversized lengths are rejected before allocation.

## 4.2 Wire primitives

Hand-written readers/writers in `crates/protocol/src/wire.rs`:

| Primitive | Encoding |
|-----------|----------|
| `u8` | 1 byte |
| `u16` | 2 bytes BE |
| `u32` | 4 bytes BE |
| `i32` | 4 bytes BE (two's complement) |
| `u64` | 8 bytes BE |
| `str` | `u32` byte length + UTF-8 bytes (length ≤ 64 KiB) |
| bytes | `u32` length + raw bytes |

Conventions: integers big-endian; `+y` is *down* (screen convention);
coordinates are in pixels. Decoding is strict — trailing bytes in a
payload are an error (catches encode/decode bugs at the source).

## 4.3 Message catalog

Defined in `crates/protocol/src/message/mod.rs` (the `Message` enum) and
`crates/protocol/src/id.rs` (the type ids).

### Handshake / session

| Type id | Message | Direction | Payload |
|---------|---------|-----------|---------|
| `0x01` | `Hello { version, id, name, info }` | client → server | protocol version, **machine id**, screen name, `ScreenInfo` |
| `0x02` | `Welcome { server_version, server_id, layout, own_screen_id }` | server → client | version, **server machine id**, the layout, this client's screen id |
| `0x03` | `ScreenInfo { info }` | client → server | screen shape changed (resolution/scale) |
| `0x04` | `Layout { layout }` | server → client | layout changed |
| `0x05` | `Control { command }` | server → client | operational command (§4.3a) |
| `0x7e` | `KeepAlive` | both | — |
| `0x7f` | `Error { code, text }` | server → client | error code + text |

### §4.3a Control commands

| Code | Command | Meaning |
|------|---------|---------|
| `1` | `DISCONNECT` | end the session and **do not** reconnect — the client process exits its reconnect loop |
| `2` | `RECONNECT` | end the session and reconnect immediately (fresh handshake) |
| `3` | `RESTART` | end the session and reconnect immediately — a session-level restart of the controlled link |

### Cursor control

| Type id | Message | Direction | Payload |
|---------|---------|-----------|---------|
| `0x10` | `Enter { screen_id, x, y }` | server → client | cursor entering at local `(x, y)` |
| `0x11` | `Leave { screen_id }` | server → client | cursor leaving |
| `0x14` | `CursorPos { x, y }` | client → server | the client's *real* cursor position (beacon) |
| `0x20` | `MouseMoveAbs { x, y }` | server → client | absolute position (defensive; entry travels in `Enter`) |
| `0x21` | `MouseMoveRel { dx, dy }` | server → client | relative motion (the hot path) |
| `0x22` | `MouseButton { button, pressed }` | server → client | canonical button id + state |
| `0x23` | `MouseWheel { dx, dy }` | server → client | wheel notches |
| `0x13` | `Escape` | local only | never on the wire; the capture emits it when the user presses Scroll Lock while remote |

### Content

| Type id | Message | Direction | Payload |
|---------|---------|-----------|---------|
| `0x30` | `Key { kind, key }` | server → client | key kind (down/up/repeat) + **USB HID usage id** |
| `0x40` | `Clipboard { mime, data }` | both | clipboard content |

### Button ids (canonical)

`0` left, `1` middle, `2` right, `3` extra 1, `4` extra 2. Each platform
backend maps these to its native representation.

### Error codes

`1` protocol, `2` version mismatch, `3` name conflict, `4` internal,
`5` not allowed (name absent from layout and machine id not trusted),
`6` not local (peer outside the server's local network).

## 4.4 The two transports

### TCP (reliable control channel)

Everything in §4.3 *except* `MouseMoveRel` rides TCP: handshake,
Enter/Leave, buttons, keys, wheel, clipboard, layout, keepalives.
Ordered and lossless. `TCP_NODELAY` is set (no Nagle batching on the hot
path).

The transport (`crates/core/src/transport.rs`) reads one frame at a
time, keeps trailing bytes of a shared TCP segment (several frames often
arrive in one read), resyncs on bad magic, and treats a connection reset
the same as a clean EOF (the peer is gone either way).

### UDP (cursor stream)

`MouseMoveRel` (server → client) and `CursorPos` beacons (client →
server) ride UDP — both are additive and loss-tolerant. Each datagram is
one frame inside a tiny envelope (`crates/core/src/udp.rs`):

```
[ client id: u8 ] [ seq: u32 BE ] [ frame bytes ]
```

- **client id** — routes the datagram to the right client.
- **seq** — a per-stream sequence number; the receiver drops stale and
  duplicate datagrams (`is_newer` handles 32-bit wrap). A replayed
  "at the wall" beacon can never arm a crossing the user didn't push
  for.

The client registers its UDP address with its first datagram (a
`KeepAlive` sent right after the handshake); the server's writer learns
the address and sends motion there.

## 4.5 Versioning

`kvmshare_protocol::VERSION` is currently `3`. The client sends its
version in `Hello`; a mismatch is answered with `Error` (code 2) and the
connection is refused. Bump the constant on any breaking wire change.

## 4.6 A frame in the wild

`MouseMoveRel { dx: -100, dy: 0 }` (client id 1, seq 42):

```
UDP datagram:
  id  : 01
  seq : 00 00 00 2A
  frame:
    magic : 4B 56 4D 31        ("KVM1")
    type  : 21                 (MOUSE_MOVE_REL)
    flags : 00
    len   : 00 00 00 08
    body  : FF FF FF 9C 00 00 00 00   (dx=-100, dy=0, i32 BE each)
```

---

**Next:** [5. Core crate](05-core.md) — the platform-independent brain.