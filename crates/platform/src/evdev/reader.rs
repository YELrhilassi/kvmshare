//! The reader thread: the grab/release lifecycle, hot-plug enumeration,
//! and the drain loop that forwards (or discards) device events.
//!
//! The reader is **event-driven**: it blocks in `poll(2)` on the device
//! fds plus a wake pipe, and only wakes when a device reports events, a
//! grab/release transition happens, or a fresh device list arrives. Idle
//! it sleeps in the kernel — no busy loop, no periodic scan on the
//! cursor's own thread. Every wake source has a real signal behind it:
//!
//! * device events wake the poll directly (`POLLIN` on the evdev fd);
//! * `set_remote` / `release_and_wait` / `Drop` write a byte to the
//!   wake pipe (the transition state itself lives in an atomic — the
//!   byte is just the nudge to re-check it);
//! * the enumerator writes the same pipe after handing a fresh device
//!   list to the channel.

use std::collections::HashSet;
use std::io;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
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

/// Pause before re-polling after a `poll(2)` error other than EINTR.
/// Real errors on evdev fds are rare and usually permanent, so the pause
/// just keeps a wedged fd from spinning the thread.
const POLL_ERROR_PAUSE: Duration = Duration::from_millis(50);
/// How long [`EvdevReader::release_and_wait`] waits for the reader
/// thread to complete the release. A healthy reader processes a
/// transition within one wake (a poll return plus the ungrab ioctls);
/// this bound just keeps the capture thread from ever waiting on a
/// wedged reader.
const RELEASE_WAIT: Duration = Duration::from_millis(30);

/// A handle to the evdev reader thread.
pub struct EvdevReader {
    remote: Arc<AtomicBool>,
    /// The write end of the wake pipe: a byte here makes the reader
    /// re-check the remote flag / enum channel instead of sleeping in
    /// poll. Nonblocking — a full pipe drops the byte, never the caller.
    wake: UnixStream,
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
        // The wake pipe: the read end lives on the reader thread (one of
        // its poll fds), the write end here plus a clone for the
        // enumerator. Nonblocking so writers never stall the capture.
        let (wake_rx, wake_tx) = UnixStream::pair().expect("cannot create evdev wake pipe");
        wake_rx.set_nonblocking(true).ok();
        wake_tx.set_nonblocking(true).ok();
        // Device re-enumeration runs on its own thread (see
        // [`hotplug::spawn_enumerator`]): `open(2)` on a wedged or slow
        // device can block for hundreds of milliseconds, and while the
        // cursor is on a client this reader *is* the whole cursor
        // stream — a stall here is a client cursor freeze. The reader
        // only ever applies freshly opened lists (fast grabs), so a slow
        // hot-plug pass delays new devices, never motion. The enumerator
        // wakes this reader through the shared pipe after each hand-off.
        let (enum_tx, enum_rx) = mpsc::channel();
        let known_paths = Arc::new(Mutex::new(HashSet::new()));
        crate::evdev::hotplug::spawn_enumerator(
            enum_tx,
            Arc::clone(&known_paths),
            wake_tx.try_clone().expect("cannot clone wake pipe"),
        );
        let thread = thread::Builder::new()
            .name("kvmshare-evdev".into())
            .spawn(move || reader_main(tx, remote2, ack2, enum_rx, known_paths, wake_rx))
            .expect("cannot spawn evdev reader");
        Self {
            remote,
            wake: wake_tx,
            ack,
            _thread: thread,
        }
    }

    /// Switch the reader between forwarding (remote) and silent (local)
    /// mode. Called by the capture thread when control crosses a
    /// boundary. Async: wakes the reader, which picks the flag up and
    /// grabs/releases on its next iteration.
    pub fn set_remote(&self, remote: bool) {
        self.remote.store(remote, Ordering::Relaxed);
        self.wake();
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
        self.wake();
        let deadline = Instant::now() + RELEASE_WAIT;
        while self.ack.load(Ordering::Acquire) == before && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Nudge the reader out of poll. Nonblocking; the byte itself is
    /// meaningless (the state lives in the atomics/channel), it only
    /// needs to arrive.
    fn wake(&self) {
        // `&UnixStream` implements `Write`; the mutable binding is the
        // trait's `&mut self` receiver, not a mutable stream.
        let mut w = &self.wake;
        let _ = w.write(&[1]);
    }
}

impl Drop for EvdevReader {
    fn drop(&mut self) {
        // Let the thread release the kernel grabs before the devices are
        // dropped — never leave the desktop input-dead. The wake byte
        // releases it from poll if it was blocked.
        self.remote.store(false, Ordering::Relaxed);
        self.wake();
    }
}

/// Drain every buffered byte from the wake pipe (nonblocking). Called
/// after the poll reports it readable, so the pipe never fills and a
/// later transition is never left un-signalled.
fn drain_wake(rx: &UnixStream) {
    let mut buf = [0u8; 64];
    let mut r = rx;
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

/// The reader loop. Waits in `poll(2)` on the device fds plus the wake
/// pipe; on each wake it applies any fresh device list, processes a
/// grab/release transition, and drains the ready devices. **Devices are
/// drained in both modes** — the kernel ring buffer would otherwise
/// replay events that happened while the cursor was local (a mute tap, a
/// Win+E, a click) to the client the moment forwarding starts. Drain
/// and-discard while local, drain-and-forward while remote, so a
/// boundary crossing never carries stale events across it.
fn reader_main(
    tx: Sender<Message>,
    remote: Arc<AtomicBool>,
    ack: Arc<AtomicU64>,
    enum_rx: Receiver<(Vec<Opened>, bool)>,
    known_paths: Arc<Mutex<HashSet<PathBuf>>>,
    wake_rx: UnixStream,
) {
    let (mut devices, mut denied) = open_devices(&HashSet::new());
    known_paths
        .lock()
        .unwrap()
        .extend(devices.iter().map(|d| d.path.clone()));
    let mut was_remote = false;
    let mut press = PressState::new();
    let mut motion = PendingMotion::default();
    let mut logged = u8::MAX; // never-logged sentinel
    let mut last_remote_drain: Option<Instant> = None;
    log_presence(&devices, denied, &mut logged);
    // Reused poll array: device fds first, the wake pipe last.
    let mut pollfds: Vec<libc::pollfd> = Vec::with_capacity(devices.len() + 1);
    let wake_fd = wake_rx.as_raw_fd();
    loop {
        // Apply any fresh device list the enumerator produced (fast:
        // just merges and grabs new devices — all the slow opens already
        // happened on the enumerator thread). The enumerator wakes us
        // through the pipe, so this is checked exactly when there is
        // something to apply.
        while let Ok((fresh, d)) = enum_rx.try_recv() {
            denied = d;
            absorb(&mut devices, fresh, &remote, &known_paths);
            log_presence(&devices, denied, &mut logged);
        }
        // Apply the current remote flag: grab/release every device on a
        // transition (the capture thread woke us through the pipe).
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

        // Block until something happens. All wake sources are in the set:
        // device events, the wake pipe (transitions, fresh lists, drop).
        // No timeout: idle is the kernel's job, not this thread's.
        pollfds.clear();
        for d in devices.iter() {
            pollfds.push(libc::pollfd {
                fd: d.dev.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        pollfds.push(libc::pollfd {
            fd: wake_fd,
            events: libc::POLLIN,
            revents: 0,
        });
        // SAFETY: pollfds is a valid array of pollfd; the device fds are
        // owned by `devices` and stay alive for the call.
        let ready = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, -1) };
        if ready < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            log_warn!("evdev: poll: {err}");
            thread::sleep(POLL_ERROR_PAUSE);
            continue;
        }
        if ready == 0 {
            continue; // timeout — not expected with -1, but harmless
        }
        // The wake pipe fired: drain it and loop so the fresh
        // list/flag above is applied before anything else.
        let wake_idx = pollfds.len() - 1;
        if pollfds[wake_idx].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            drain_wake(&wake_rx);
            continue;
        }

        // Drain the ready devices.
        let iter_start = Instant::now();
        let drain_start = Instant::now();
        let mut saw_event = false;
        let mut dead: Vec<usize> = Vec::new();
        for (i, d) in devices.iter_mut().enumerate() {
            let revents = pollfds[i].revents;
            if revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
                continue;
            }
            if revents & (libc::POLLHUP | libc::POLLERR) != 0 && revents & libc::POLLIN == 0 {
                // The device vanished; a fetch would only error.
                dead.push(i);
                continue;
            }
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
        // Diagnostic: processing (post-poll) that took longer than
        // ~15 ms while remote means this thread itself was blocked or
        // starved for the whole stream (everything the client needs
        // flows through here). The poll block itself is idle and never
        // counted — that is the event-driven wait, not a stall.
        let iter = iter_start.elapsed();
        if is_remote && iter > Duration::from_millis(15) {
            let drain_took = drain_start.elapsed();
            log_warn!("evdev: reader pass took {iter:?} (drain {drain_took:?})");
        }
        if saw_event && is_remote {
            // Diagnostic: while the cursor is on a client this thread is
            // the whole cursor stream. A long gap between event-bearing
            // drains means the stream stalled *here* — the client would
            // see exactly that gap as a frozen cursor.
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
                Ok(events) => for _ in events {},
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
            Err(e) => log_warn!(
                "evdev: {}: cannot {}: {e}",
                d.name,
                if grab { "grab" } else { "release" }
            ),
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
