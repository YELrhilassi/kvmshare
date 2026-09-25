# 11. Media control and audio

This document specifies two features that are deliberately **independent
of cursor switching**:

- the **media control router** — where playback and volume keys go;
- **audio streaming** — carrying one machine's output to the other's
  speakers.

Both are generic: neither assumes a particular number of machines, a
particular layout, a particular desktop environment, or a particular
audio server. Every behaviour described here is policy-driven and
configurable, and every default preserves what the keyboard did before
kvmshare existed.

## 11.1 Why these are separate from cursor focus

The naive model is *"whichever machine has the cursor receives the
keys"*. For ordinary typing that is exactly right, and it is what
kvmshare does today. For media keys it is wrong:

```
You are working on  laptop   →  laptop has keyboard focus
Play/Pause pressed           →  ...but the music is on the desktop
```

Moving the cursor just to pause a track defeats the purpose of a KVM.
The fix is not to special-case the mouse, but to stop conflating two
different questions:

| Question | Answered by |
|---|---|
| Where does typing go? | cursor focus (unchanged) |
| Where does playback control go? | the media routing policy |
| Where does volume control go? | the audio routing policy |

Three independent systems that can cooperate, instead of one system
trying to infer everything from pointer position.

## 11.2 The media control router

### Semantic commands, not relayed key codes

kvmshare already carries keys OS-neutrally as USB HID usages. Sending a
raw media key from machine A to machine B works, but it can only ever
mean *"press this same key over there"* — it cannot express *"raise the
volume on the machine I am listening to"*, and it cannot be routed by
category.

So media keys are promoted to a **semantic command** before routing:

```rust
enum MediaCommand {
    PlayPause, Play, Pause, Stop,
    Next, Previous,
    SeekForward, SeekBackward,
    VolumeUp, VolumeDown, Mute,
}
```

`MediaCommand` crosses the wire as its own message
(`Message::MediaControl`). The receiving machine translates the command
into *its own* native control — on Linux an XF86 media keysym, on
Windows a `VK_MEDIA_*`/`VK_VOLUME_*` keystroke. Neither side needs to
know how the other implements it, or even which OS it runs.

> Because the command is translated at the destination edge, adding an
> OS is a matter of adding one injection mapping — no protocol change,
> and no coordination with the other machines.

### Classification

A media key is recognised at the **capture edge**, before the session
sees it, so it can never leak into the remote keyboard stream or
accidentally follow the cursor. Classification lives with the shared HID
tables (`kvmshare_protocol::message::MediaCommand::from_hid`), so every
capture backend — X11, Windows Raw Input, evdev — feeds the same
classifier:

```
physical media key
   │
   ▼
capture backend → HID usage → MediaCommand::from_hid
   │
   ├─ None  → ordinary key, unchanged behaviour
   └─ Some  → the router decides the destination
```

### Policy

Two categories, because they mean different things:

- **transport** — `PlayPause`, `Play`, `Pause`, `Stop`, `Next`,
  `Previous`, `SeekForward`, `SeekBackward`: *control the media source*.
- **volume** — `VolumeUp`, `VolumeDown`, `Mute`: *control the output you
  are actually listening to*.

Each category resolves a target independently:

| Target | Meaning |
|---|---|
| `local` | This machine — the command never leaves it. |
| `follow_focus` | Wherever the cursor is (the classic behaviour). |
| `machine("id")` | A pinned machine, by machine id. Stable across reconnects and address changes. |
| `last_active_source` | The machine whose audio most recently had something playing. |
| `focus_or_last_active` | Follow the cursor, but when the cursor is home fall back to the last active source. |

A target that cannot be resolved **falls back to `local`**, and that
fallback is on by default. This is the single most important rule here:

> If kvmshare cannot prove where a media key should go, it hands the key
> to the local OS. A feature that silently swallows your volume keys
> when no peer is connected is a bug, not a feature.

There is also a master switch (`route_media_keys`). With it off, capture
does not classify at all and media keys behave exactly as they would
without kvmshare installed. This makes the whole feature opt-in and
safe to ship on by default.

### The override

`last_active_source` is inferred, so it can be wrong (two machines
playing at once). Rather than guessing harder, the router exposes an
explicit override: a user-bound shortcut that pins the media target for
as long as it is held, or cycles it. Deliberate overrides are opt-in —
the inferred policy is the default, and the GUI shows the resolved
target so the user can see where a key will go before pressing it.

## 11.3 Audio streaming

### What it is

Each machine can **send its own output** to the other, and play what it
receives. Direction is independent: A→B, B→A, both, or neither.

Two properties fall out of the design for free:

- **Both at once works.** Playback goes to the ordinary output device,
  so the OS's own mixer combines the remote stream with local audio —
  there is no custom mixer and no need to choose "which machine you are
  listening to". This is exactly the situation the media router's
  `last_active_source` policy exists to disambiguate.
- **No desktop-environment integration.** Capture happens at the *audio
  server / device* level, not inside any application, so it does not
  care whether the sound comes from a browser, a media player, or a
  game.

### The pipeline

```
capture (this machine's output)
   │  PCM frames, fixed cadence
   ▼
packetise: [stream id][seq][timestamp][payload]
   │  UDP — loss-tolerant, like the cursor stream
   ▼
jitter buffer (reorder, absorb jitter, drop late)
   │
   ▼
playback (remote machine's output device)
```

Audio rides **UDP**, for the same reason cursor motion does: a dropped
frame is a click, not a corruption, and retransmitting it would arrive
too late to matter. Nothing about audio is allowed to couple its latency
to the reliable TCP stream.

### Format

The format is **negotiated in the control stream** before any audio
flows, and the payload is raw `s16le` PCM:

```rust
struct AudioFormat {
    sample_rate: u32,   // 48000 typical
    channels: u8,       // 2
    frame_ms: u16,      // 10 — the packet cadence
    codec: u8,          // 0 = pcm_s16le (the only codec today)
}
```

48 kHz stereo `s16le` costs ≈1.5 Mbit/s. On the LAN a KVM is built for,
that is nothing — and it costs **zero CPU** to encode and decode, which
matters more than bandwidth on a machine that is simultaneously
forwarding input at sub-millisecond latency. The `codec` field exists so
a compressed codec can be added later *without breaking the wire*: both
peers advertise what they accept and pick the best common option. The
default never changes silently.

### Activity, and why it matters to the router

The sender measures the RMS of what it captures and reports whether
anything is actually playing. This is what makes `last_active_source`
work, and it is deliberately *not* built on MPRIS, GSMTC, or any other
desktop-specific media API:

```
sender:   captured audio is above the silence floor  →  "playing"
receiver: remembers which machine spoke last          →  last active source
```

Every platform can answer "is there sound on this machine" without
knowing anything about the applications making it, so the policy works
identically on Linux, Windows, and any backend added later — and it
keeps working when the audio comes from something no media API knows
about.

## 11.4 Configuration

```toml
[media]
# Master switch. false = media keys are never intercepted; the local OS
# handles them exactly as if kvmshare were not running.
route_media_keys = true

# Where playback control goes.
transport = "focus_or_last_active"
# Where volume/mute goes.
volume = "local"

# Fall back to the local machine when the target cannot be resolved.
# Leave this on unless you really want a media key to be dropped.
fallback_local = true

# Optional pin, when a category is set to a fixed machine.
# transport_machine = "98980a4d9afac273a9aac53ec1c57c35"

[audio]
# Send this machine's output to the peer.
send = false

# Play what the peer sends.
receive = false

# Capture/playback device. Empty or "default" = the system default.
capture_device = ""
playback_device = ""

# Silence floor for "playing" detection, in dBFS. -50 is a good start.
activity_floor_db = -50.0
```

Every value above has a default that is either harmless (routing on with
a local fallback) or inert (audio off), so an existing config file keeps
working untouched and an upgrade never changes behaviour by surprise.

## 11.5 Platform backends

| Concern | Linux | Windows |
|---|---|---|
| Media injection | XF86 media keysym (XTest) | `VK_MEDIA_*` / `VK_VOLUME_*` via `SendInput` |
| Capture | `parec` on the default sink's monitor (works on PulseAudio and PipeWire) | WASAPI loopback capture |
| Playback | `pacat` to the configured/default sink | WASAPI render (also via `AUTOCONVERTPCM`, so a device running at 44.1 kHz still takes the negotiated 48 kHz stream) |
| Device listing | `pactl` | WASAPI enumerator |

**One dependency worth knowing about.** Every Win32 call in
`kvmshare-platform` uses the raw `windows-sys` bindings — except the
audio backend, which needs the `windows` crate. That is not a
preference: WASAPI is a COM API (`IMMDeviceEnumerator`, `IAudioClient`,
`IAudioCaptureClient`, `IAudioRenderClient`) and `windows-sys` ships no
COM interfaces at all, so the alternatives were hand-rolling four
objects' worth of vtables — hundreds of lines of unsafe code that cannot
be exercised off Windows — or the component bindings. The dependency is
confined to the Windows target and to `platform::audio::windows`, so the
rest of the crate keeps the raw bindings it already had.

macOS is not implemented. The seams it needs are `Injector::media`, the
`AudioCapture`/`AudioPlayback` traits, and one mapping table; nothing in
the protocol, router, or config is platform-specific.

## 11.6 What is honest about this feature

- **Audio is opt-in and off by default.** Nothing is captured, and
  nothing is sent, until a machine's config says so.
- **Capture requires permission.** On Linux, capturing the monitor
  source means reading the audio server as the logged-in user; on
  Windows, WASAPI loopback capture needs no elevation. The GUI surfaces
  a clear failure instead of silently doing nothing.
- **The router never guesses in silence.** With no peer reachable, a
  media key is local, always.
- **Bandwidth is real.** PCM is ~1.5 Mbit/s per direction. On Wi-Fi
  shared with the input stream this is the first thing to watch if
  audio stutters, and the reason `codec` is negotiated rather than
  hardcoded.
