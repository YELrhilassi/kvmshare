//! The reader thread: the grab/release lifecycle, hot-plug enumeration,
//! and the drain loop that forwards (or discards) device events.

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use kvmshare_log::{log_debug, log_info, log_warn};
use kvmshare_protocol::message::Message;

use kvmshare_core::motion::PendingMotion;

use crate::evdev::device::{absorb, handle_event, open_devices, Opened, PressState};

/// How often the enumerator thread re-opens `/dev/input`, so a device
/// plugged in (or access granted) mid-session is picked up and grabbed
/// too. Runs on its own thread, so the cadence serves both modes; a
/// slow open (wedged driver) delays new devices, never the cursor
/// stream.
const HOTPLUG_PERIOD: Duration = Duration::from_millis(2000);
/// Idle poll pause while remote (no events flowing). Nonblocking reads
/// are polled in a loop; 1 ms keeps latency negligible.
const REMOTE_POLL_PAUSE: Duration = Duration::from_millis(1);
/// Pause while the cursor is local (nothing to do — the X capture owns
/// input then). Longer is fine: the only duty is watching the flag and
/// the slow re-enumeration cadence.
const LOCAL_PAUSE: Duration = Duration::from_millis(20);
/// How long [`EvdevReader::release_and_wait`] waits for the reader
/// thread to complete the release. A healthy reader processes a
/// transition within one iteration (~1 ms idle sleep + the ungrab
/// ioctls); this bound just keeps the capture thread from ever waiting
/// on a wedged reader.
const RELEASE_WAIT: Duration = Duration::from_millis(30);

/// A handle to the evdev reader thread.
pub struct EvdevReader {
    remote: Arc<AtomicBool>,
    /// Bumped by the reader thread every time it completes a grab/release
    /// transition. A caller that needs the devices in a *known* state
    /// (the crossing-back warp must land on a live stream) waits for the
    /// counter to advance — bounded, so nothing can ever hang.
    ack: Arc<AtomicU64>,
    _thread: thread::JoinHandle<()>,
}

impl EvdevReader {
    /// Start the reader thread. Always succeeds: input isolation engages
    /// the moment devices become readable (see the module docs) and the
    /// server otherwise runs grab-only.
    pub fn start(tx: Sender<Message>) -> Self {
        let remote = Arc::new(AtomicBool::new(false));
        let remote2 = Arc::clone(&remote);
        let ack = Arc::new(AtomicU64::new(0));
        let ack2 = Arc::clone(&ack);
        // Device re-enumeration runs on its own thread: `open(2)` on a
        // wedged or slow device can block for hundreds of milliseconds,
        // and while the cursor is on a client this reader *is* the whole
        // cursor stream — a stall here is a client cursor freeze. The
        // reader only ever applies freshly opened lists (fast grabs), so
        // a slow hot-plug pass delays new devices, never motion.
        let (enum_tx, enum_rx) = mpsc::channel();
        // Devices the reader already watches; the enumerator skips them
        // so a pass never hands back duplicate handles (whose `Drop` —
        // an EVIOCGRAB(0) ungrab ioctl — would stall the reader).
        let known_paths = Arc::new(Mutex::new(HashSet::new()));
        spawn_enumerator(enum_tx, Arc::clone(&known_paths));
        let thread = thread::Builder::new()
            .name("kvmshare-evdev".into())
            .spawn(move || reader_main(tx, remote2, ack2, enum_rx, known_paths))
            .expect("cannot spawn evdev reader");
        Self { remote, ack, _thread: thread }
    }

    /// Switch the reader between forwarding (remote) and silent (local)
    /// mode. Called by the capture thread when control crosses a
    /// boundary. Async: the reader picks the flag up on its next
    /// iteration (≤ ~2 ms) and grabs/releases then.
    pub fn set_remote(&self, remote: bool) {
        self.remote.store(remote, Ordering::Relaxed);
    }

    /// Release the kernel grabs **and wait until the reader has actually
    /// done it** (bounded — never blocks more than [`RELEASE_WAIT`]).
    /// The crossing-back path uses this so the entry warp lands on a
    /// live input stream instead of racing an async release that can
    /// swallow the first moments of physical motion. Safe to call when
    /// already local (the flag is already false; the wait just times out
    /// quietly).
    pub fn release_and_wait(&self) {
        let before = self.ack.load(Ordering::Acquire);
        self.remote.store(false, Ordering::Release);
        let deadline = Instant::now() + RELEASE_WAIT;
        while self.ack.load(Ordering::Acquire) == before && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for EvdevReader {
    fn drop(&mut self) {
        // Let the thread release the kernel grabs before the devices are
        // dropped — never leave the desktop input-dead.
        self.remote.store(false, Ordering::Relaxed);
    }
}

/// The re-enumeration thread: opens `/dev/input` on a cadence and hands
/// the fresh device list to the reader over a channel. Everything that
/// can block (the device opens themselves) happens here — the reader
/// never waits on it. The thread is detached: if a device open wedges
/// for a long time, the reader simply keeps its current devices until
/// the next pass completes.
fn spawn_enumerator(enum_tx: Sender<(Vec<Opened>, bool)>, known_paths: Arc<Mutex<HashSet<PathBuf>>>) {
    thread::Builder::new()
        .name("kvmshare-evdev-enum".into())
        .spawn(move || loop {
            thread::sleep(HOTPLUG_PERIOD);
            // Diagnostic: a slow pass is now harmless to the cursor
            // stream, but still worth knowing about.
            let t0 = Instant::now();
            let known = known_paths.lock().unwrap().clone();
            let (fresh, denied) = open_devices(&known);
            let took = t0.elapsed();
            // A pass is normally 100-200 ms (the device opens) and runs
            // off the motion thread, so it is not worth a WARN on every
            // cadence — that was log spam. Only a genuinely pathological
            // pass (a wedged device open) rises to WARN.
            if took > Duration::from_millis(400) {
                log_warn!("evdev: re-enumeration took {took:?} (off the motion thread)");
            } else if took > Duration::from_millis(20) {
                log_debug!("evdev: re-enumeration took {took:?}");
            }
            if enum_tx.send((fresh, denied)).is_err() {
                return; // reader gone
            }
        })
        .ok();
}

/// The reader loop. Waits for the remote flag; on each transition grabs
/// (or releases) every device. **Devices are drained in both modes** —
/// the kernel ring buffer would otherwise replay events that happened
/// while the cursor was local (a mute tap, a Win+E, a click) to the
/// client the moment forwarding starts. Drain-and-discard while local,
/// drain-and-forward while remote, so a boundary crossing never carries
/// stale events across it. Freshly opened device lists arrive from the
/// enumerator thread over the channel (see [`spawn_enumerator`]).
fn reader_main(
    tx: Sender<Message>,
    remote: Arc<AtomicBool>,
    ack: Arc<AtomicU64>,
    enum_rx: Receiver<(Vec<Opened>, bool)>,
    known_paths: Arc<Mutex<HashSet<PathBuf>>>,
) {
    let (mut devices, mut denied) = open_devices(&HashSet::new());
    known_paths.lock().unwrap().extend(devices.iter().map(|d| d.path.clone()));
    let mut was_remote = false;
    let mut press = PressState::new();
    let mut motion = PendingMotion::default();
    let mut logged = u8::MAX; // never-logged sentinel
    let mut last_remote_drain: Option<Instant> = None;
    log_presence(&devices, denied, &mut logged);
    loop {
        let iter_start = Instant::now();
        let is_remote = remote.load(Ordering::Relaxed);
        if is_remote != was_remote {
            set_grabbed(&mut devices, is_remote);
            // Acknowledge the transition so a caller waiting on
            // [`EvdevReader::release_and_wait`] can proceed.
            ack.fetch_add(1, Ordering::Release);
            if is_remote {
                // Events that landed in the ring buffers in the moments
                // before the grab belong to the local side (the buffer is
                // normally drained continuously, but the last instant can
                // slip through): purge them so the crossing never
                // replays them on the client.
                purge(&mut devices);
                last_remote_drain = None;
            } else {
                // Local again: the X capture owns input; any press state
                // this reader accumulated belongs to the past.
                press = PressState::new();
            }
            was_remote = is_remote;
        }
        // Apply any fresh device list the enumerator produced (fast:
        // just merges and grabs new devices — all the slow opens already
        // happened on the enumerator thread).
        let enum_start = Instant::now();
        while let Ok((fresh, d)) = enum_rx.try_recv() {
            denied = d;
            absorb(&mut devices, fresh, &remote, &known_paths);
            log_presence(&devices, denied, &mut logged);
        }
        let enum_took = enum_start.elapsed();
        // Drain every device. Forward only while remote; while local the
        // events belong to the X capture and are discarded here — but
        // they must be *read* so they never replay later.
        let drain_start = Instant::now();
        let mut saw_event = false;
        let mut dead: Vec<usize> = Vec::new();
        for (i, d) in devices.iter_mut().enumerate() {
            match d.dev.fetch_events() {
                Ok(events) => {
                    for ev in events {
                        saw_event = true;
                        if is_remote {
                            handle_event(&ev, &mut motion, &mut press, &mut |m| {
                                let _ = tx.send(m);
                            });
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    log_warn!("evdev: {}: {e} — removed", d.name);
                    dead.push(i);
                }
            }
        }
        for i in dead.into_iter().rev() {
            if let Some(removed) = devices.get(i) {
                known_paths.lock().unwrap().remove(&removed.path);
            }
            devices.remove(i);
        }
        if is_remote {
            motion.flush(&mut |dx, dy| {
                let _ = tx.send(Message::MouseMoveRel { dx, dy });
            });
        }
        // Diagnostic: an iteration that took longer than ~15 ms while
        // remote means this thread itself was blocked or starved for the
        // whole stream (everything the client needs flows through here).
        // The phase split shows whether the block was the device drain
        // or the (off-thread-open) enumerator apply.
        let iter = iter_start.elapsed();
        if is_remote && iter > Duration::from_millis(15) {
            let drain_took = drain_start.elapsed();
            log_warn!("evdev: reader iteration took {iter:?} (enum apply {enum_took:?}, drain {drain_took:?})");
        }
        if !saw_event {
            // Idle: pause a tick. While local the pause is longer (the
            // X capture owns input; we only keep the buffers drained).
            thread::sleep(if is_remote { REMOTE_POLL_PAUSE } else { LOCAL_PAUSE });
        } else if is_remote {
            // Diagnostic: while the cursor is on a client this thread is
            // the whole cursor stream. A long gap between event-bearing
            // drains means the stream stalled *here* (this thread blocked
            // on a device open/grab/read) — the client would see exactly
            // that gap as a frozen cursor.
            if let Some(last) = last_remote_drain {
                let gap = last.elapsed();
                if gap > Duration::from_millis(100) {
                    log_warn!("evdev: remote motion stream gap of {gap:?}");
                }
            }
            last_remote_drain = Some(Instant::now());
        }
    }
}

/// Drain and discard everything currently buffered on every device
/// (nonblocking). Used at the moment forwarding starts so events from
/// just before the crossing are never replayed on the client.
fn purge(devices: &mut [Opened]) {
    for d in devices.iter_mut() {
        loop {
            match d.dev.fetch_events() {
                Ok(events) => {
                    for _ in events {}
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }
}

/// Grab or release every device with `EVIOCGRAB`. While grabbed, X (and
/// every other reader) receives nothing from these devices — the local
/// desktop is inert no matter what the app reads.
fn set_grabbed(devices: &mut [Opened], grab: bool) {
    let mut ok = 0;
    for d in devices.iter_mut() {
        let res = if grab { d.dev.grab() } else { d.dev.ungrab() };
        match res {
            Ok(()) => ok += 1,
            Err(e) => log_warn!("evdev: {}: cannot {}: {e}", d.name, if grab { "grab" } else { "release" }),
        }
    }
    if grab {
        log_debug!("evdev: {ok}/{} device(s) isolated from X", devices.len());
    } else {
        log_debug!("evdev: {ok}/{} device(s) released", devices.len());
    }
}

/// Log whether isolation is live — but only when the state changes, since
/// re-enumeration runs on a cadence forever.
fn log_presence(devices: &[Opened], denied: bool, logged: &mut u8) {
    let state = match (devices.is_empty(), denied) {
        (false, _) => 1,    // live
        (true, true) => 2,  // permission denied
        (true, false) => 3, // no devices
    };
    if *logged == state {
        return;
    }
    *logged = state;
    match state {
        1 => log_info!("input isolation available (evdev, {} device(s))", devices.len()),
        2 => log_warn!("input isolation unavailable: permission denied reading /dev/input (granting input access fixes it)"),
        _ => log_warn!("input isolation unavailable: no pointer/keyboard devices in /dev/input"),
    }
}