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

**The classifier never eats ordinary keys.** Every backend canonicalises
keystrokes to the *keyboard* usage page, where `0xe2` is Left Alt — but
on the consumer page `0xe2`-adjacent codes are media keys, and an early
draft accepted `0xe2` as Mute. With routing on (the default) that
classified the Alt of Alt+Tab and Alt+F4 as Mute and swallowed it. The
rule now: only the consumer-page media codes are media, Mute is `0xe8`
alone, and the neighbouring keyboard-page modifiers (`0xe0`–`0xe2`,
`0xe6`) are pinned as non-media by a regression test. A routing feature
that can eat your chords is worse than no routing feature.

### Policy

Every media key — playback (`PlayPause`, `Next`, `Stop`, …) and volume
(`VolumeUp`, `Mute`) alike — resolves through **one** configured target.
Playback and volume were once configured separately, on the theory that
the media source and the output you are listening to can be different
machines; in practice that made the user answer the same question twice
and get one of the answers wrong, so it is one choice now.

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

### Arming is the same decision as routing

The server intercepts classified keys *before* the session, and the
media-key **grab** on the local machine is armed exactly when routing is
enabled — never separately. The two are one indivisible decision:
keys are consumed either way, and a consumed key that was not grabbed
would simply be lost, while a grab without consumption would double-act.
The grab is (re-)armed at startup, on every hot policy change, and never
while the cursor is on a client (re-grabbing away would fight the
forwarded input); the next idle moment applies it.

Routed keys also work when the cursor is **home** — that is the feature's
headline case. A client performs a routed command even while its motion
loop is idle: the injection queue is drained on idle wakes, without
steering the cursor (zero-CPU idle is preserved).

### The override

`last_active_source` is inferred, so it can be wrong (two machines
playing at once). Rather than guessing harder, the router exposes an
explicit override: the user binds a chord to the `media_pin` shortcut
naming a screen, and pressing it **latches** that machine as the media
target — every category, until the same chord is pressed again. It is a
toggle, not a momentary hold (a held modifier set cannot also be a
media press, and a latch survives the chord). The rules around it are
deliberately conservative:

- an override outranks every configured policy — an inference must
  never outvote a human;
- a shortcut naming a machine that is not in the layout is a no-op, so
  a typo cannot wipe an override that is doing its job;
- the override dies with the machine it pinned (a pin to nobody is not
  a policy), and never expires on its own;
- with routing off the override is inert — the master switch means
  "kvmshare is not here".

The pin is the GUI's per-machine answer to "the router picked wrong":
press the chord once while looking at the machine you want, press it
again to hand control back to the policy.

## 11.3 Audio streaming

### What it is

Each machine either **sends its own output** to the other or plays what
it receives — one direction at a time, never both. Two streams landing
on one mixer are indistinguishable: there is no way to tell which
machine the sound you hear came from, and no way to mute just one. So
the direction is a *single choice* (off / send / receive), and both
files store at most one live direction — a legacy config that somehow
had both on is collapsed to one on load and the correction is written
back (see §11.6).

A few properties fall out of the design:

- **Sending *moves* the sound; it does not copy it.** While a machine is
  sending, its own speakers are **silent** and the sound comes out of the
  other machine only. Capturing a loopback is a *tap*, not a redirect, so
  sending would otherwise play the same audio on both machines — the user
  cannot then tell which one is the shared output, and the two drift out
  of sync. The sender therefore routes its whole output through a
  **virtual output** that kvmshare creates, makes the system default, and
  captures; nothing is played to the real speakers. See *Exclusive output*
  below.
- **Playing does not replace local audio.** The *receiving* machine plays
  the peer's stream through its ordinary output device, so its own mixer
  combines the remote stream with that machine's local sound — there is
  no custom mixer, and nothing on this machine has to be muted to hear
  the other one.
- **One direction at a time.** A machine either sends or plays, never
  both, so the pair never mixes two streams into one output.
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

### Exclusive output (sending)

A loopback capture only *listens* to the default output; it does not stop
it. To make a sending machine genuinely silent, kvmshare routes the
machine's whole output through a virtual one:

1. a null sink (**`kvmshare_send`**, a `module-null-sink`, which has no
   hardware and therefore makes no sound) is created and made the system
   default;
2. everything already playing is moved onto it, and everything played
   afterwards goes there by default;
3. its **monitor** — `kvmshare_send.monitor` — is what capture reads.

The real speakers now receive nothing, and the stream carries the whole
system's sound. When sending stops the real output is made default again
and the virtual sink is removed.

Three details make this seamless rather than fragile:

- **A brief reconnect does not flap the sound.** A control-link blip
  tears the stream down and rebuilds it a few seconds later. Releasing
  the route instantly would drop the sound back onto this machine's
  speakers for the gap and then yank it away again. The route is held for
  a short **grace window** (6 s) after a stream stops, and a stream that
  resumes within it simply carries on. If the peer does not return, the
  route is released and this machine's sound comes home.
- **A crash cannot leave the machine silent.** A hard kill skips the
  cleanup, so the virtual output would outlive the process and leave the
  machine quiet with nothing running to explain it. The next start
  **recovers**: it puts the real output back and removes the leftover
  sink before anything else touches audio.
- **It is reported, never forced silently.** If the route cannot be set
  up (no `pactl`, an audio server that refuses), sending still works and
  the live panel says the speakers will keep playing what is sent, rather
  than pretending the machine is silent.

Because playback and volume keys are routed by *one* target, a machine
used as a shared output is also the machine those keys control.

> **Platform note.** Exclusive output needs a way to create a virtual
> device, which today only the Linux backend has (`pactl` +
> `module-null-sink`, present on PulseAudio and PipeWire alike). On
> Windows a machine can still *send* — WASAPI loopback capture — but
> silencing its own output needs a virtual audio device that is not part
> of the OS, so the Windows backend captures the tap and leaves the
> speakers alone. Sending *to* a Linux machine, and both directions
> between any pair, work regardless of which end is which.

### More than two machines

An audio link is between exactly two machines, but a server may have
three, five, or a rack of clients. The two directions are then **not the
same kind of question**, and answering them the same way was the feature's
first real limitation:

| Direction | Question | Answer |
|---|---|---|
| sending | *whose speakers should my sound come out of?* | a machine the user chose |
| receiving | *whose sound do I want to hear?* | the machine that is **actually playing** |

*Sending* is never inferred. With several machines connected and no peer
pinned the link stops, because "send this machine's output to the peer"
has no defensible answer when there are three peers, and quietly picking
one (first connected, lowest id) would stream a user's audio to a machine
they never chose. The Media page turns that refusal into an action: the
connected machines are listed, and picking one writes `peer`.

*Receiving* can be **observed rather than guessed**, because every machine
already tells the link whether it has something playing — the same
`AudioState` answer that drives `last_active_source` (§11.3, *Activity*).
With no peer pinned and several machines connected, the link follows the
first one that reports playing. The rules are deliberately sticky, since a
link that chased activity naively would flap between two machines:

- a machine that starts playing takes the link **only when no other machine
  holds it** — an established link is never interrupted;
- a machine that stops playing releases it **only if it holds it**, and
  nothing takes its place until some machine says it is playing.

Pinning outranks both halves: `peer` is the user's decision, and an
observation must not overrule it. The two halves are one function
(`server::audio::on_peer_activity`) so there is one place to look when the
link is on a machine the user did not expect, and the page says which
machine it is playing from.

The choice is therefore configurable *and* observable: a user with three
machines hears whichever one is making noise, and pins one when the
inference is not what they wanted.

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

Two files, because the settings describe two different machines:

- **the server's `kvmshare-server.toml`** owns `[media]` (where media
  keys go when this machine is typed on) and `[audio]` (which machine it
  streams its output to). The server hot-reloads both sections live —
  the grab re-arms for a routing change, and an audio change applies at
  the next connection, because a live audio link is attached to a
  client connection.
- **the client's `kvmshare-client.toml`** owns this machine's own
  `[audio]` — its capture/playback consent. A client obeys the
  *server's* layout over the wire; the wire cannot carry hardware
  consent, so it is a separate file with a separate schema
  (`KVMSHARE_CLIENT_CONFIG`, else `~/.config/kvmshare/kvmshare-client.toml`).
  It is re-read on every (re)connect, so a save applies at the next
  connection without restarting the client.

```toml
[media]
# Master switch. false = media keys are never intercepted; the local OS
# handles them exactly as if kvmshare were not running.
route_media_keys = true

# Where every media key goes — playback and volume alike. One target:
#   local | follow_focus | last_active_source | focus_or_last_active |
#   machine:<machine id>
# (Configs written before playback and volume were collapsed still work:
# the old `transport`/`volume` keys are read, `transport` first, and the
# next save rewrites them as `target`.)
target = "focus_or_last_active"

# Fall back to the local machine when the target cannot be resolved.
# Leave this on unless you really want a media key to be dropped.
fallback_local = true

[audio]
# Direction — at most one of these is true. The GUI presents them as a
# single choice (off / send / receive) and collapses a config that has
# both on, so "both" cannot survive an edit on the page.
send = false
receive = false

# Capture/playback device. Empty or "default" = the system default.
capture_device = ""
playback_device = ""

# Silence floor for "playing" detection, in dBFS. -50 is a good start.
activity_floor_db = -50.0

# Server role only: pin the audio peer by machine id (short form
# works). Empty = automatic — with one client connected that client is
# the peer; with several, see §11.3.1. The GUI writes the machine id it
# read from the connected machine, e.g. peer = "98980a4d9afac273".
peer = ""
```

Pinned media targets are written inline as `machine:<id>` (e.g.
`target = "machine:98980a4d"`); the GUI's media page writes exactly
this form, resolving the pin against the machine *id* of a connected
machine — a name the page shows, but never the value it writes, so a
rename cannot silently retarget the keys. Every value above has a default that is either harmless
(routing on with a local fallback) or inert (audio off), so an existing
config file keeps working untouched and an upgrade never changes
behaviour by surprise. Unknown target values are a config error, not a
silent default — a typo in a routing policy must be visible.

## 11.5 Platform backends

| Concern | Linux | Windows |
|---|---|---|
| Media injection | XF86 media keysym (XTest) | `VK_MEDIA_*` / `VK_VOLUME_*` via `SendInput` |
| Capture | `parec` on the default sink's monitor (works on PulseAudio and PipeWire) | WASAPI loopback capture |
| Exclusive send | `module-null-sink` `kvmshare_send` made default, its monitor captured, real output restored on stop (see §11.3, *Exclusive output*) | — (needs a virtual audio device; the tap is captured and the local speakers keep playing) |
| Playback | `pacat` to the configured output, or `@DEFAULT_SINK@` (the specifier, *not* the literal name `default`, which no sink has — `pacat` answers "No such entity") | WASAPI render (also via `AUTOCONVERTPCM`, so a device running at 44.1 kHz still takes the negotiated 48 kHz stream) |
| Device listing | `pactl` | WASAPI enumerator |

The GUI's device pickers read that list through the **server binary**
(`kvmshare-server --audio-devices`, a JSON `{capture, playback}` on
stdout, then exit) rather than re-implementing `pactl` and WASAPI in Go.
The GUI is a separate process and cannot link `kvmshare-platform`, so
asking the one place that already speaks to the audio server keeps a
single enumeration — the picker offers exactly the devices the runtime
will accept.

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

## 11.6 The GUI surface

The GUI's **Media & audio** page edits exactly what this document
describes: server-role media routing, audio sharing, and the audio peer —
which is picked from the machines that are **actually connected** (the same
list the Clients page shows), not typed as a machine id the GUI already
knows. A pinned machine that is not connected right now stays visible as a
card, so a setting never quietly disappears because a laptop is off, and
an explicit *Automatic* card hands the choice back to the rules in §11.3.1.
A client keeps only the media/audio page's
client-local half — its own sound sharing — because media routing is a
concern of the machine being typed on; its sidebar entry therefore
reads **Audio**, and the server-side sections never render there. All
writes go through typed Go mappings of the Rust serde schemas (never
generic maps — numbers must stay numbers), preserve every unrelated
toml section, and take the same cross-process config lock the Rust
side takes: the server hot-reloads the file, and an unlocked
read-modify-write could erase the server's own concurrent writes
(auto-trust, screen-size correction). A save the GUI accepts is a
config the Rust parser never rejects — the validators agree by test.

The audio section offers one direction chooser — **Off** / **Send this
machine's sound** / **Play the other machine's sound here** — and the
device picker for the chosen direction appears beneath it. For *send*,
exclusive output is automatic and there is nothing to choose — the page
says so — and the **Capture from** picker is kept only as a fallback for a
platform that cannot route the output (it offers this machine's monitors,
never a microphone); for *receive*, **Play to** offers the outputs.
Both default to *System default*, which follows the user when they switch
outputs; a configured device that the audio server is not currently
reporting stays pinned at the top of the list, so a setting is never
silently dropped. Picking one direction turns the other off, so the
invalid "both" state is not reachable from the page.

The page has **no Save button**: every edit is written automatically
(debounced, so a burst of clicks is one write), and a quiet readout by
the title says *Saving…* / *Saved* — or the failure, which is the only
state that means the screen and the files disagree. Leaving the page
flushes a still-pending write rather than dropping it, and two writes
never overlap (an edit that lands mid-write is queued behind it). The
server file holds both `[media]` and `[audio]`, so an autosave of one
half still sends the last-saved copy of the other, never a half-finished
edit.

Beneath the sections the page shows **what is happening now**, because a
switch is not evidence. "Send this machine's sound" is a wish; a stream
with sound in it is a fact, and telling a paying user which one they are
looking at is the difference between a feature they trust and one they
cannot debug. So the page shows, separately:

- which direction is actually streaming, and to or from **which machine**
  by name;
- the device the stream uses;
- a **level meter per direction**, with *silent* / *playing* beside it.
  This is the same RMS the role reports to the media router, so the
  "playing" on the page is exactly the "playing" that decides where the
  media keys go — the two features are one picture, and a machine shown
  playing here is the machine the keys follow;
- the failure, when there is one ("capture stopped: …"), because a link
  that looks on while nothing is heard is the worst state to leave a user
  in;
- a **warning about the capture path** when the platform can name one — a
  muted output, a muted monitor, a monitor whose recording gain was left at
  21%. None of these is a failure (the link is doing exactly what it was
  asked to), and all three are invisible from the audio: the stream is
  simply silent. Finding this out from a log is a support call; finding it
  out from the page is a five-second fix.

All of it comes from the role process, which writes
`<state_dir>/audio.state` on every transition and every meter tick (the
same key=value, atomic-write pattern `client.state` uses); the GUI folds it
into the live snapshot, clears the meters for a role that is not running,
and never shows a stream that is not there. The meter is published on a
human cadence rather than per packet — a reading only when the level has
moved and enough time has passed — so a live meter does not mean a file
write per frame.

Finally, a **test tone**: one press plays a short `440 Hz` tone through the
device that is actually under test. When this machine *receives*, that is the
playback device. When it *sends*, it is the **system default output** — while
sharing that is the virtual output the route created, so the tone is captured
and sent on to the other machine (and is heard *there*, not here); with
nothing shared it is a plain speaker check. It is deliberately not the
capture device, which is a loopback monitor — an input, to which nothing can
be played. While the tone plays, the page watches the capture meter, and
reports one of three things:

- *heard* — the tone played and the capture saw it, with the peak level;
- *played, not heard* — the output works and the loopback that feeds the
  link does not, which is the single most confusing failure the feature has
  ("audio is on and it is silent"), and now has a name;
- *refused* — the output device itself said why.

The tone is produced by the same role binary the GUI already asks for its
device list (`--audio-test-tone`, JSON on stdout, one tone and exit), for
the same reason: the audio backend is `kvmshare-platform`, which a Go
program cannot link, so the test exercises the code the link uses rather
than a second implementation of "play a sound". A capture or playback
device that fails mid-stream reports the failure to the page *and* tells
the peer to stop, instead of leaving a choice that looks on while nothing
is heard.

The server's media policy also accepts the pin shortcut through the
ordinary `[shortcuts]` machinery: an action named `media_pin` with a
`screen` value (see §11.2, *The override*).

## 11.7 What is honest about this feature

- **A routed key is consumed exactly once.** It is never also forwarded
  as an ordinary keystroke — and the local grab is armed exactly when
  routing is on, so no state exists where a key acts twice or vanishes.
- **Windows seek is throttled and logged, not delivered.** There is no
  seek target to name on the wire today; pretending otherwise would
  drop the events silently.
- **Audio is opt-in and off by default.** Nothing is captured, and
  nothing is sent, until a machine's config says so.
- **Capture requires permission.** On Linux, capturing the monitor
  source means reading the audio server as the logged-in user; on
  Windows, WASAPI loopback capture needs no elevation. The GUI surfaces
  a clear failure instead of silently doing nothing — including one that
  happens *after* a stream started, which is written to `audio.state`
  and shown on the Media page.
- **Sending is chosen, never guessed.** With several machines connected and
  none pinned, this machine's sound goes nowhere — the page lists the
  machines to pick from. Receiving is *observed* instead (the machine that
  is playing), pinned by the user when that is not what they wanted.
- **The router never guesses in silence.** With no peer reachable, a
  media key is local, always.
- **A meter is a level, not a proof of what is audible.** It measures what
  this machine captures and plays; it cannot know whether the speakers are
  on, or whether anyone is in the room.
- **Sending silences the sender, and says so if it cannot.** While a
  machine shares its output, its own speakers are quiet (see §11.3,
  *Exclusive output*) — the sound comes out of the other machine only. On
  a platform that cannot create a virtual output the stream still runs,
  the speakers keep playing, and the live panel says exactly that rather
  than claiming a silence it did not create.
- **A short disconnect does not flap the audio.** The route is held for a
  brief grace window across a reconnect, so a blip does not push the sound
  back onto this machine's speakers and then take it away again; and an
  interrupted share is repaired on the next start, so a crash cannot leave
  a machine silent.
- **A silent machine says why, and does not fix it.** Loopback capture taps
  the output *after* its volume, so a muted output feeds its monitor digital
  silence, and a monitor whose recording gain was left low attenuates every
  frame — a monitor at 21% is about −41 dB, quiet enough that the activity
  detector never sees the machine as playing at all. Both look exactly like a
  working link from inside the stream. kvmshare therefore **detects and
  reports** them ("alsa_output… is muted, so nothing it plays can be
  captured" on the live panel, and a warning in the log), and deliberately
  does not change the user's mixer on its own: taking over someone's volume
  settings to make a diagnostic green would be a worse bug than the silence.
- **The test tone tests one machine.** It proves this machine's output and
  loopback; whether the *other* machine hears the result is what the
  receiving side's meter and the link state are for.
- **Bandwidth is real.** PCM is ~1.5 Mbit/s per direction. On Wi-Fi
  shared with the input stream this is the first thing to watch if
  audio stutters, and the reason `codec` is negotiated rather than
  hardcoded.
