//! The client side.
//!
//! Connects to a server over **two links** (see [`crate::server`]):
//!
//! * **TCP** — the reliable control channel: handshake, Enter/Leave,
//!   buttons, keys, wheel, clipboard, layout, keepalive.
//! * **UDP** — the cursor stream: relative motion in, real-cursor
//!   beacons out. Additive and loss-tolerant, so the cursor's latency is
//!   never coupled to the reliable stream's buffering.
//!
//! ## Threads, not one loop
//!
//! The old client ran everything in a single loop — draining UDP, a
//! timeout-driven TCP read, cursor placement, clipboard polling, screen
//! queries. One slow platform call (a clipboard read stalling on a busy
//! `OpenClipboard`, a resolution query) stalled the *motion tick* with
//! it, which showed up as the cursor stopping for a beat at a regular
//! cadence. The client now runs four isolated threads, each owning only
//! the work it can do without ever blocking the others:
//!
//! * **TCP thread** (the caller's thread in [`Client::run`]) — the
//!   control channel. Blocks on `recv`, dispatches Enter/Leave/events,
//!   drains the outbox and sends keepalives. Never touches the cursor
//!   hot path.
//! * **Motion thread** — a fixed-cadence steering loop: place the cursor
//!   at the commanded position (or correct it), beacon the real position
//!   back, sample telemetry. It does nothing else, ever.
//! * **UDP thread** — drains the cursor stream and advances the command.
//!   Blocks on the socket (event-driven, no polling), never sleeps
//!   beyond the socket's own read timeout.
//! * **Sync thread** — the slow periodic duties (resolution check,
//!   clipboard poll) at a 500 ms cadence, results handed to the TCP
//!   thread over a channel. A stalled clipboard read can delay a
//!   clipboard sync by 500 ms — it can never delay a cursor placement.
//!
//! Shared state is a single [`Shared`] with short critical sections
//! (lock order: `motion` → `injector`, held for microseconds). A slow
//! call on any thread delays only that thread's own next step.
//!
//! The cursor is steered by a **closed loop** ([`PositionFollower`]):
//! every received motion frame advances a commanded position, and the
//! motion thread places the real cursor on the command each tick (for
//! absolute backends) or corrects toward it (relative backends). There
//! is no replay queue, so no backlog can ever form. All OS-specific work
//! lives behind the [`Injector`] trait; this module is plain message
//! dispatch and thread wiring, tested with a fake injector.

use std::io;
use std::net::{TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use kvmshare_log::{log_debug, log_error, log_trace, log_warn};
use kvmshare_protocol::message::{KeyKind, Layout, Message, ScreenInfo};

use crate::motion::{MotionProbe, PositionFollower, MOTION_PERIOD};
use crate::transport::{RecvResult, Transport};
use crate::udp;

/// The platform hook the client calls to affect the local machine.
///
/// The server controls this machine, so every call here is "make the
/// local machine do what the server asked".
pub trait Injector: Send {
    /// The client's screen shape (resolution and scale). Re-queried by
    /// the sync thread so resolution changes are noticed and reported
    /// back to the server.
    fn screen_info(&mut self) -> ScreenInfo;
    /// Move the local cursor to local screen pixels `(x, y)` (absolute
    /// placement: used when control *enters*, for explicit positioning,
    /// and by absolute-motion backends for the whole motion stream).
    fn move_cursor(&mut self, x: i32, y: i32);
    /// Apply relative cursor motion. The client OS applies its own
    /// pointer transform (acceleration / speed settings) to relative
    /// input, so the shared cursor feels exactly like a physical mouse on
    /// this machine — the model every mature KVM uses.
    fn move_rel(&mut self, dx: i32, dy: i32);
    /// Whether this backend places the cursor **absolutely** for motion
    /// (each received delta accumulates into a commanded position and the
    /// cursor is set exactly there each tick) rather than forwarding
    /// relative input for the OS to transform.
    ///
    /// Absolute placement bypasses the client OS's pointer acceleration
    /// entirely: the shared cursor lands exactly where commanded, the OS
    /// can never over-run the hand, and a lost frame self-heals (the
    /// next placement lands the whole command). Backends that return
    /// `true` skip the closed-loop correction — the placement *is* the
    /// loop. The server compensates for its own pointer transform by
    /// scaling the counts it sends (see `GainTracker`), so the client
    /// cursor mirrors the server cursor pixel-for-pixel.
    fn absolute_motion(&self) -> bool {
        false
    }
    /// The cursor's *real* current position in local screen pixels.
    /// Reported to the server on a cadence while being controlled, so
    /// the server knows exactly where the shared cursor sits for edge
    /// crossings.
    fn cursor_position(&mut self) -> (i32, i32);
    fn button(&mut self, button: u8, pressed: bool);
    fn wheel(&mut self, dx: i32, dy: i32);
    /// Press/release/repeat a key, addressed by its canonical USB HID
    /// usage id (the platform backend maps it to the local key identity).
    fn key(&mut self, kind: KeyKind, key: u32);
    /// Control has entered this machine: hide the local cursor so the
    /// server's stream is the only visible one.
    fn enter(&mut self);
    /// Control has left this machine: show the local cursor again.
    fn leave(&mut self);
    /// Called by the motion thread once per steering tick while this
    /// machine is controlled. Backends with a remote-control watchdog
    /// (Windows input isolation) use it as the liveness heartbeat: if
    /// steering stops while the machine is being driven remotely, the
    /// watchdog releases local input so the machine is never trapped.
    /// Default: nothing.
    fn steer_heartbeat(&mut self) {}
    /// Force-restore local input ownership after a client-side stall.
    /// The client's supervisor thread calls this when a worker wedges
    /// (e.g. a blocking OS call holds the injector lock and the cursor
    /// can no longer be steered). Backends that silence local hardware
    /// while remotely controlled must undo that here so the machine is
    /// never left trapped: the user's own mouse and keyboard always
    /// work again, even if the shared session has to restart.
    /// Default: nothing (backends without hardware silencing need
    /// nothing to undo).
    fn emergency_release(&mut self) {}
}

/// Clipboard access, split from [`Injector`] **on purpose**: reading or
/// writing the system clipboard can block indefinitely while another
/// process holds it open (some apps open it and never close it), so a
/// clipboard call must never share a lock with the cursor. The client
/// gives the clipboard its own lock, serviced by its own thread — a
/// stalled clipboard read delays only clipboard sync, never a cursor
/// placement.
pub trait Clipboard: Send {
    /// Put `data` into the local clipboard (received from the server).
    fn set(&mut self, mime: &str, data: &[u8]);
    /// Read the current local clipboard, if any.
    fn get(&mut self) -> Option<(String, Vec<u8>)>;
    /// The last clipboard content applied from a *remote* source (set via
    /// [`Clipboard::set`]). Pollers compare against this so content that
    /// arrived from the server is never echoed back to it.
    fn last_injected(&mut self) -> Option<(String, Vec<u8>)>;
}

/// How often the client sends a keepalive when idle.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(2);
/// How long the TCP read can block. The control channel is quiet, and
/// nothing on it has a deadline tighter than this: motion lives on its
/// own thread and its own socket.
const READ_TIMEOUT: Duration = Duration::from_millis(100);
/// How often the sync thread polls the local clipboard to push changes
/// up. The poll lives on the sync thread, so even a clipboard read that
/// stalls for tens of milliseconds delays only the clipboard sync.
const CLIPBOARD_INTERVAL: Duration = Duration::from_millis(500);
/// How often the client reports its real cursor position to the server
/// while being controlled. The server anchors its virtual cursor and
/// edge decisions on these — a tight cadence keeps crossings exact
/// without flooding the wire. Beacons ride UDP; a lost one is replaced
/// by the next.
const CURSOR_BEACON_INTERVAL: Duration = Duration::from_millis(8);
/// UDP socket read timeout: the cursor stream thread blocks on `recv`
/// and wakes at this cadence when idle (to notice shutdown). Frames
/// themselves wake it immediately — this is not a poll.
const UDP_RECV_TIMEOUT: Duration = Duration::from_millis(8);
/// How often the sync thread re-checks the display geometry (rare
/// event; the poll exists so a resolution change is noticed without
/// restarting). Kept long so this thread rarely touches the injector
/// lock — a display query must never contend with cursor placement.
const SCREEN_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Consecutive 100 ms probe windows where the cursor was commanded to
/// move but did not (away from a screen edge) before the client treats
/// injected input as blocked and recovers. ~600 ms: long enough that a
/// single hiccup never trips it, short enough that a genuinely blocked
/// cursor recovers in about a second.
const PIN_WINDOWS_TO_TRIP: u32 = 6;

/// A connected client. The transport is owned by the TCP thread (the
/// thread that called [`Client::run`]); the cursor stream socket moves
/// into [`Shared`] when `run` starts.
#[derive(Debug)]
pub struct Client {
    transport: Transport,
    /// The id this machine has in the server's layout.
    own_id: u8,
    /// The full layout as last sent by the server (includes the server's
    /// own screen). Clients mostly ignore it today; it exists so future
    /// features have the data they need.
    layout: Layout,
    /// The cursor stream socket; handed to [`Shared`] when `run` starts
    /// (motion and UDP threads share it).
    udp: UdpSocket,
}

/// State shared by the client's worker threads. Every thread touches
/// only the fields it needs, and every critical section is short —
/// microsecond-scale lock holds on [`Shared::motion`] and
/// [`Shared::injector`] (always in that order).
struct Shared {
    /// The platform injector. Touched by the motion thread (placement),
    /// the TCP thread (events, enter/leave) and the sync thread (screen
    /// info). Never held across a slow call.
    injector: Mutex<Box<dyn Injector>>,
    /// The platform clipboard, on its own lock. A clipboard read or
    /// write can block (another process holding the clipboard open), so
    /// it must never serialize with the cursor — a stalled clipboard
    /// delays only clipboard sync.
    clipboard: Mutex<Box<dyn Clipboard>>,
    /// The cursor follower (command trajectory), telemetry probe, and
    /// per-window wire counters.
    motion: Mutex<MotionState>,
    /// Whether control is currently on this machine. Flipped by the TCP
    /// thread on Enter/Leave; read every tick by the motion and UDP
    /// threads.
    active: AtomicBool,
    /// The UDP cursor stream: motion in, beacons out.
    udp: UdpSocket,
    /// Sequence for outgoing beacon datagrams (registration used 0).
    udp_seq: AtomicU32,
    /// Set when the session ends; worker threads exit at their next wake.
    stop: AtomicBool,
    /// Monotonic millis of the motion thread's last loop wake. Bumped
    /// unconditionally every iteration, so a stalled value means the
    /// motion thread is genuinely wedged (not merely idle). Read by the
    /// supervisor to detect the stall and by the teardown path to decide
    /// whether joining would hang.
    motion_tick_ms: AtomicU64,
    /// Monotonic millis of the TCP thread's last wake (a dispatch or an
    /// idle NoData cycle). A stalled value while the link is healthy
    /// means the control loop is wedged inside a dispatch.
    tcp_tick_ms: AtomicU64,
}

/// Monotonic milliseconds since the first call (process anchor). Cheap
/// and immune to clock changes; shared by the liveness ticks and the
/// supervisor so they agree on an epoch.
fn now_ms() -> u64 {
    static BOOT: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let boot = *BOOT.get_or_init(std::time::Instant::now);
    boot.elapsed().as_millis() as u64
}

/// The cursor-side steering state, guarded by [`Shared::motion`].
struct MotionState {
    follower: PositionFollower,
    probe: MotionProbe,
    /// Motion frames accepted since the last telemetry window (wire
    /// health — a stall shows as a window with few frames).
    frames_win: u32,
    /// Steering ticks since the last telemetry window (loop health).
    ticks_win: u32,
    /// Pixels commanded since the last probe window (accumulated in
    /// `apply_motion_frame`). Compared against the real cursor's travel
    /// to detect a cursor the OS refuses to move.
    win_cmd_px: i64,
    /// Real cursor position when the current probe window opened.
    win_real_start: (i32, i32),
    /// Consecutive windows where the cursor was commanded to move but
    /// did not. Enough of them, away from a screen edge, means injected
    /// motion is being eaten by the OS — the recovery path releases
    /// local input and restarts the session (see the probe block).
    pin_windows: u32,
}

impl Client {
    /// Connect to `addr`, say hello with `name`, and wait for the server's
    /// welcome. Opens the UDP cursor stream and registers it with the
    /// server. Returns the client ready to run.
    pub fn connect(addr: &str, name: &str, info: ScreenInfo) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        let mut transport = Transport::with_read_timeout(stream, Some(READ_TIMEOUT))?;
        transport.send(&Message::Hello { version: kvmshare_protocol::VERSION, name: name.to_owned(), info })?;

        let (own_id, layout) = match transport.recv()? {
            RecvResult::Msg(Message::Welcome { server_version, layout, own_screen_id }) => {
                if server_version != kvmshare_protocol::VERSION {
                    return Err(io::Error::other(format!(
                        "server speaks v{server_version}, client speaks v{}",
                        kvmshare_protocol::VERSION
                    )));
                }
                (own_screen_id, layout)
            }
            RecvResult::Msg(Message::Error { code, text }) => Err(io::Error::other(format!(
                "server rejected the connection ({code}): {text}"
            )))?,
            other => return Err(io::Error::other(format!("unexpected first message: {other:?}"))),
        };

        // The cursor stream: one UDP socket, connected to the server's
        // address. The UDP thread blocks on `recv` with a short timeout
        // (frames wake it immediately, so the kernel receive buffer is
        // drained at full speed and only extreme bursts can drop a
        // frame — motion is loss-tolerant, the next frame self-heals).
        let udp = UdpSocket::bind(("0.0.0.0", 0))?;
        udp.set_read_timeout(Some(UDP_RECV_TIMEOUT))?;
        udp.connect(addr)?;
        // First datagram = registration: it carries the client id, so the
        // server learns both who we are and where to send motion.
        udp.send(&udp::pack(own_id, 0, &Message::KeepAlive))?;

        Ok(Self { transport, own_id, layout, udp })
    }

    /// Run the client until the connection closes.
    ///
    /// Spawns the motion, UDP and sync worker threads, then services the
    /// TCP control channel on the calling thread. When the link closes
    /// the workers are stopped and joined, and the cursor is placed once
    /// more at the final command so the last position is exact.
    ///
    /// `outbox` lets the app layer push messages to the server (future
    /// control messages). Drained on the TCP thread.
    pub fn run(
        self,
        injector: Box<dyn Injector>,
        clipboard: Box<dyn Clipboard>,
        outbox: &Receiver<Message>,
    ) -> io::Result<()> {
        // Destructure so each field is owned independently — `udp` moves
        // into [`Shared`] while `transport` stays on this thread.
        let Client { mut transport, own_id, layout, udp } = self;
        let mut layout = layout;
        let shared = Arc::new(Shared {
            injector: Mutex::new(injector),
            clipboard: Mutex::new(clipboard),
            motion: Mutex::new(MotionState {
                follower: PositionFollower::default(),
                probe: MotionProbe::default(),
                frames_win: 0,
                ticks_win: 0,
                win_cmd_px: 0,
                win_real_start: (0, 0),
                pin_windows: 0,
            }),
            active: AtomicBool::new(false),
            udp,
            udp_seq: AtomicU32::new(1),
            stop: AtomicBool::new(false),
            motion_tick_ms: AtomicU64::new(0),
            tcp_tick_ms: AtomicU64::new(0),
        });
        // The sync thread hands messages (resolution changes, clipboard
        // uploads) to the TCP thread over this channel.
        let (sync_tx, sync_rx) = mpsc::channel::<Message>();

        let sync = thread::Builder::new()
            .name("kvmshare-client-sync".into())
            .spawn({ let s = shared.clone(); move || sync_loop(s, sync_tx) })
            .expect("cannot spawn client sync thread");
        let udp_thread = thread::Builder::new()
            .name("kvmshare-client-udp".into())
            .spawn({ let s = shared.clone(); move || udp_loop(s, own_id) })
            .expect("cannot spawn client udp thread");
        let motion = thread::Builder::new()
            .name("kvmshare-client-motion".into())
            .spawn({ let s = shared.clone(); move || motion_loop(s, own_id) })
            .expect("cannot spawn client motion thread");
        // The supervisor: a watchdog that can never be blocked by
        // whatever wedges the workers, because it shares nothing with
        // them but two atomics. If a worker stalls while this machine is
        // being controlled, it force-restores local input and ends the
        // session so the reconnect path starts a clean client — a wedged
        // client must never leave this machine's mouse and keyboard
        // trapped. It also logs the stall so the root cause is visible
        // on the next occurrence instead of a mystery freeze.
        let supervisor = thread::Builder::new()
            .name("kvmshare-client-supervisor".into())
            .spawn({ let s = shared.clone(); move || supervisor_loop(s) })
            .expect("cannot spawn client supervisor thread");

        // TCP control channel on this thread. The read timeout is
        // constant — motion no longer needs the control loop to wake at
        // motion cadence.
        let mut last_keepalive = Instant::now();
        loop {
            // A recovery path (the supervisor or the cursor-pin detector)
            // can ask the session to end from another thread; the read
            // timeout bounds this loop's wake so the request is seen
            // within ~100 ms even when the link is silent.
            if shared.stop.load(Ordering::Relaxed) {
                break;
            }
            match transport.recv()? {
                RecvResult::Msg(msg) => {
                    shared.tcp_tick_ms.store(now_ms(), Ordering::Relaxed);
                    dispatch(&mut layout, &shared, own_id, msg);
                }
                RecvResult::Eof => break,
                RecvResult::NoData => {
                    shared.tcp_tick_ms.store(now_ms(), Ordering::Relaxed);
                    // App-layer outbox, slow-path traffic from the sync
                    // thread, and keepalives to keep the link warm. All
                    // cheap sends; nothing here can block for long.
                    while let Ok(msg) = outbox.try_recv() {
                        transport.send(&msg)?;
                    }
                    while let Ok(msg) = sync_rx.try_recv() {
                        transport.send(&msg)?;
                    }
                    if last_keepalive.elapsed() >= KEEPALIVE_INTERVAL {
                        transport.send(&Message::KeepAlive)?;
                        last_keepalive = Instant::now();
                    }
                }
            }
        }

        // Session over: stop the workers, then place the cursor once more
        // at the final command so the last position is exact. Joins are
        // bounded: a worker wedged on an OS call must not hang the
        // reconnect loop (the supervisor already released local input for
        // that case).
        shared.stop.store(true, Ordering::Relaxed);
        Self::join_bounded(motion, "motion");
        Self::join_bounded(udp_thread, "udp");
        Self::join_bounded(sync, "sync");
        let _ = supervisor.join();
        place_at_command(&shared);
        Ok(())
    }

    /// Join a worker thread with a grace period; a thread that has not
    /// exited by then is abandoned (dropping the handle detaches it).
    /// Wedged workers hold only their own locks — the fresh session's
    /// threads do not share them — so abandoning is safe and the machine
    /// is never held hostage by a stuck join.
    fn join_bounded(handle: thread::JoinHandle<()>, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !handle.is_finished() {
            thread::sleep(Duration::from_millis(20));
        }
        if !handle.is_finished() {
            log_error!("client {name} thread did not exit after stop — abandoning it");
            drop(handle);
        } else {
            let _ = handle.join();
        }
    }
}

/// The supervisor watchdog, on its own thread: it shares nothing with
/// the workers but two atomic liveness ticks, so nothing that wedges
/// them can wedge it. While control is on this machine, a motion thread
/// that stops ticking means the cursor is no longer being steered — a
/// blocking call is holding a lock the motion loop needs. Recovery is
/// deliberately blunt and always safe: force-restore local input (the
/// user's own mouse/keyboard work again, no matter what), then end the
/// session so the reconnect path starts a clean client. The stall is
/// logged loudly with the liveness facts so the root cause is visible.
fn supervisor_loop(shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(500));
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
        // Only guard while this machine is being controlled: local idle
        // is not a stall.
        if !shared.active.load(Ordering::Acquire) {
            continue;
        }
        let now = now_ms();
        let motion_age = now.saturating_sub(shared.motion_tick_ms.load(Ordering::Relaxed));
        let tcp_age = now.saturating_sub(shared.tcp_tick_ms.load(Ordering::Relaxed));
        // The motion loop ticks at ~4 ms; any age over 3 s while active
        // is a genuine wedge (a healthy loop cannot be quiet that long),
        // never a false positive.
        if motion_age > 3000 {
            // Probe which lock the stalled motion thread is likely
            // waiting on: a lock we cannot acquire here is held by a
            // wedged thread (the motion loop takes motion then injector
            // every tick).
            let motion_locked = shared.motion.try_lock().is_err();
            let injector_locked = shared.injector.try_lock().is_err();
            let clipboard_locked = shared.clipboard.try_lock().is_err();
            log_error!(
                "SUPERVISOR: motion thread stalled {motion_age} ms while control is on this machine (tcp thread {tcp_age} ms; locks held: motion={motion_locked} injector={injector_locked} clipboard={clipboard_locked}) — releasing local input and restarting the session"
            );
            // Force-restore the machine's own input. `try_lock` so the
            // supervisor itself can never block on a wedged lock.
            if let Ok(mut inj) = shared.injector.try_lock() {
                inj.emergency_release();
            }
            // End the session: the TCP loop wakes within its read
            // timeout, run() unwinds, and the binary reconnects fresh.
            shared.stop.store(true, Ordering::Relaxed);
            break;
        }
    }
}

    /// Apply one server message to the local machine. Ordering-critical
    /// events (button, key, wheel) first place the cursor on the command
    /// point; Enter/Leave flip control ownership; everything else is
    /// dispatch.
    fn dispatch(layout: &mut Layout, shared: &Arc<Shared>, own_id: u8, msg: Message) {
        match msg {
            Message::MouseMoveRel { dx, dy } => {
                // Defensive: motion normally arrives over UDP (drained by
                // the UDP thread); a frame on the control channel follows
                // the same path.
                apply_motion_frame(shared, dx, dy);
            }
            Message::MouseMoveAbs { x, y } => {
                // Absolute placement (defensive — entry placement travels
                // in the Enter message; the session never emits absolute
                // moves in the motion stream). The command follows the
                // placed point.
                let mut m = shared.motion.lock().unwrap();
                let mut inj = shared.injector.lock().unwrap();
                inj.move_cursor(x, y);
                m.follower.reanchor(x, y);
            }
            Message::Enter { screen_id: _, x, y } => {
                log_trace!("control entered at ({x},{y})");
                // Anchor the command at the entry point and place the
                // cursor there, *then* flip `active` — the motion and UDP
                // threads must never steer toward a stale command.
                let mut m = shared.motion.lock().unwrap();
                m.follower.enter(x, y);
                {
                    let mut inj = shared.injector.lock().unwrap();
                    inj.enter();
                    inj.move_cursor(x, y);
                    let (rx, ry) = inj.cursor_position();
                    m.follower.reanchor(rx, ry);
                    m.probe.enter((rx, ry));
                }
                drop(m);
                shared.active.store(true, Ordering::Release);
                // Report where we are right away so the server's edge
                // state is fresh from the first moment.
                let (x, y) = shared.injector.lock().unwrap().cursor_position();
                let _ = send_beacon(shared, own_id, x, y);
            }
            Message::Leave { screen_id: _ } => {
                log_trace!("control left");
                shared.active.store(false, Ordering::Release);
                let mut m = shared.motion.lock().unwrap();
                m.follower.leave();
                m.probe.leave();
                let mut inj = shared.injector.lock().unwrap();
                inj.leave();
            }
            // Buttons, keys and wheel are ordering-critical: the cursor
            // must sit on the command point before the event fires.
            Message::MouseButton { button, pressed } => {
                flush_before_event(shared);
                log_trace!("button {button} {}", if pressed { "down" } else { "up" });
                shared.injector.lock().unwrap().button(button, pressed);
            }
            Message::MouseWheel { dx, dy } => {
                flush_before_event(shared);
                log_trace!("wheel {dx},{dy}");
                shared.injector.lock().unwrap().wheel(dx, dy);
            }
            Message::Key { kind, key } => {
                flush_before_event(shared);
                log_trace!("key {kind:?} {key}");
                shared.injector.lock().unwrap().key(kind, key);
            }
            Message::Clipboard { mime, data } => {
                log_debug!("clipboard from server: {} ({} bytes)", mime, data.len());
                shared.clipboard.lock().unwrap().set(&mime, &data);
            }
            Message::Layout { layout: new_layout } => {
                log_debug!("layout updated: {} screens", new_layout.screens.len());
                *layout = new_layout;
            }
            Message::KeepAlive => {}
            Message::Error { code, text } => log_warn!("server error ({code}): {text}"),
            // Not valid client-side traffic; ignore defensively.
            Message::Hello { .. }
            | Message::Welcome { .. }
            | Message::ScreenInfo { .. }
            | Message::CursorPos { .. }
            | Message::Escape => {}
        }
    }

impl Client {
    pub fn own_id(&self) -> u8 {
        self.own_id
    }
}

/// The motion thread: a fixed-cadence steering loop. Each tick it places
/// the real cursor on the commanded position (absolute backends) or
/// corrects toward it (relative backends), beacons the real position
/// back at [`CURSOR_BEACON_INTERVAL`], and samples telemetry. It does
/// nothing else — no network reads, no clipboard, no screen queries — so
/// nothing can make the cursor wait.
fn motion_loop(shared: Arc<Shared>, own_id: u8) {
    let mut last_beacon = Instant::now();
    let mut beacon_failed = false;
    let mut recover = false;
    while !shared.stop.load(Ordering::Relaxed) && !recover {
        let tick = Instant::now();
        shared.motion_tick_ms.store(now_ms(), Ordering::Relaxed);
        if shared.active.load(Ordering::Acquire) {
            let mut m = shared.motion.lock().unwrap();
            m.ticks_win += 1;
            let mut inj = shared.injector.lock().unwrap();
            inj.steer_heartbeat();
            let (rx, ry) = inj.cursor_position();
            if inj.absolute_motion() {
                // Place exactly at the command. Skipped when the cursor
                // is already there: an idle cursor costs nothing, and a
                // stray native move is re-placed — self-healing.
                let (cx, cy) = m.follower.command();
                if (cx, cy) != (rx, ry) {
                    inj.move_cursor(cx, cy);
                }
            } else if let Some((dx, dy)) = m.follower.correct((rx, ry)) {
                inj.move_rel(dx, dy);
            }
            // Telemetry is collected under the locks but logged only
            // after they are released — a slow log sink must never hold
            // up the next placement.
            let mut trace_line: Option<String> = None;
            if m.probe.due() {
                let (ex, ey) = m.follower.error((rx, ry));
                let (frames, ticks) = (m.frames_win, m.ticks_win);
                m.frames_win = 0;
                m.ticks_win = 0;
                // Cursor-pin detection: this window commanded real
                // motion (`win_cmd_px`, accumulated as UDP frames
                // arrived) yet the real cursor did not travel. That
                // means the OS is silently eating injected motion — the
                // supervisor cannot see it (the motion thread is healthy;
                // the cursor just never moves). A few such windows away
                // from a screen edge is the signature of blocked input.
                let travel = ((rx - m.win_real_start.0).abs()
                    + (ry - m.win_real_start.1).abs()) as i64;
                let pinned = m.win_cmd_px > 60 && travel < 6;
                m.pin_windows = if pinned { m.pin_windows + 1 } else { 0 };
                let pin_age_ms = m.pin_windows as u64 * 100;
                m.win_cmd_px = 0;
                m.win_real_start = (rx, ry);
                if m.pin_windows >= PIN_WINDOWS_TO_TRIP {
                    // A cursor legitimately parked at a screen edge is a
                    // wall push (the clamp holds the command), not a
                    // block — only trip away from the edges.
                    let info = inj.screen_info();
                    let at_edge = rx <= 1
                        || ry <= 1
                        || rx as i64 >= info.width as i64 - 2
                        || ry as i64 >= info.height as i64 - 2;
                    if !at_edge {
                        log_error!(
                            "cursor pinned ~{pin_age_ms} ms while {frames} frames/tick commanded motion — injected input is being eaten; releasing local input and restarting the session"
                        );
                        inj.emergency_release();
                        shared.stop.store(true, Ordering::Relaxed);
                        recover = true;
                    }
                }
                m.probe.sample((rx, ry), &mut |rx, ry, ax, ay, _exp_x, _exp_y, gx, gy| {
                    trace_line = Some(format!(
                        "motion req=({rx},{ry}) act=({ax},{ay}) err=({ex},{ey}) real=({gx},{gy}) frames={frames} ticks={ticks}"
                    ));
                });
            }
            drop(m);
            drop(inj);
            if let Some(line) = trace_line {
                log_trace!("{line}");
            }
            if last_beacon.elapsed() >= CURSOR_BEACON_INTERVAL {
                last_beacon = Instant::now();
                if let Err(e) = send_beacon(&shared, own_id, rx, ry) {
                    if !beacon_failed {
                        log_warn!("cursor beacon send failed (first): {e}");
                        beacon_failed = true;
                    }
                }
            }
        }
        // Sleep until the next tick. The cadence is fixed by
        // [`MOTION_PERIOD`]; the tick work itself is microseconds.
        let elapsed = tick.elapsed();
        let rem = MOTION_PERIOD.saturating_sub(elapsed);
        thread::sleep(rem);
    }
}

/// The UDP thread: drains the cursor stream and advances the commanded
/// position. Blocks on the socket (event-driven — a frame wakes it
/// immediately); the read timeout only bounds idle wakes so shutdown is
/// noticed promptly.
fn udp_loop(shared: Arc<Shared>, own_id: u8) {
    let mut motion_seq: u32 = 0;
    let mut buf = [0u8; 512];
    while !shared.stop.load(Ordering::Relaxed) {
        match shared.udp.recv(&mut buf) {
            Ok(n) => {
                let Some(d) = udp::unpack(&buf[..n]) else { continue };
                if d.id != own_id || !udp::is_newer(d.seq, motion_seq) {
                    continue;
                }
                motion_seq = d.seq;
                if let Message::MouseMoveRel { dx, dy } = d.msg {
                    // Motion outside Enter/Leave is dropped: it can beat
                    // the TCP Enter on the wire (different transports),
                    // and at most a frame or two at the seam is lost —
                    // self-correcting.
                    if shared.active.load(Ordering::Acquire) {
                        apply_motion_frame(&shared, dx, dy);
                    }
                }
            }
            Err(_) => {} // timeout (idle) or link error: check stop, loop
        }
    }
}

/// The sync thread: slow periodic duties on their own thread so they can
/// never delay the cursor. Results are handed to the TCP thread over the
/// channel, which sends them on its next wake.
fn sync_loop(shared: Arc<Shared>, tx: Sender<Message>) {
    let mut last_info = shared.injector.lock().unwrap().screen_info();
    let mut last_screen_check = Instant::now();
    let mut last_clip_check = Instant::now();
    let mut last_clip_seen: Option<(String, Vec<u8>)> = None;
    while !shared.stop.load(Ordering::Relaxed) {
        // Resolution changes are rare. A monitor query every
        // SCREEN_POLL_INTERVAL is plenty, and it keeps this thread off
        // the injector lock almost all the time — a display query (which
        // can stall on a busy driver) must never serialize with the
        // motion thread's placements.
        if last_screen_check.elapsed() >= SCREEN_POLL_INTERVAL {
            last_screen_check = Instant::now();
            let info = shared.injector.lock().unwrap().screen_info();
            if info != last_info {
                let _ = tx.send(Message::ScreenInfo { info });
                last_info = info;
            }
        }
        // Push local clipboard changes up to the server. This read is the
        // one call that can legitimately stall (another process holding
        // the clipboard open) — which is exactly why it lives here, on
        // the clipboard's own lock, where a stall delays only clipboard
        // sync and never the cursor.
        if last_clip_check.elapsed() >= CLIPBOARD_INTERVAL {
            last_clip_check = Instant::now();
            let mut cb = shared.clipboard.lock().unwrap();
            let cur = cb.get();
            // Skip content we just applied from the server, and content
            // we have already sent.
            if let Some(cur) = cur {
                if last_clip_seen.as_ref() != Some(&cur)
                    && cb.last_injected().as_ref() != Some(&cur)
                {
                    let (mime, data) = cur.clone();
                    let _ = tx.send(Message::Clipboard { mime, data });
                    last_clip_seen = Some(cur);
                }
            }
        }
        thread::sleep(CLIPBOARD_INTERVAL / 2);
    }
}

/// Apply one motion frame to the shared command. Relative backends get
/// the follower's feedforward portion injected immediately (the motion
/// thread's corrections deliver the rest). Absolute backends only
/// advance the command — the motion thread places the cursor there at
/// its own cadence, so the wire cadence never reaches the cursor
/// directly and a burst of datagrams cannot clump it.
fn apply_motion_frame(shared: &Shared, dx: i32, dy: i32) {
    let mut m = shared.motion.lock().unwrap();
    m.frames_win += 1;
    // Feed the cursor-pin detector: how much motion was commanded this
    // probe window, compared against the real cursor's travel.
    m.win_cmd_px += dx.abs() as i64 + dy.abs() as i64;
    {
        let mut inj = shared.injector.lock().unwrap();
        if inj.absolute_motion() {
            m.follower.advance(dx, dy);
        } else {
            let (dx, dy) = m.follower.push(dx, dy);
            inj.move_rel(dx, dy);
        }
    }
    m.probe.requested(dx, dy);
}

/// Before an ordering-critical event (button, key, wheel) the cursor
/// must sit on the command point. Absolute backends place it there
/// exactly — the placement is the loop, and the command is the only
/// truth. Relative backends flush the follower's residual as one capped
/// move, so a click lands where the motion pointed without a wedged
/// cursor dragging it across the screen.
fn flush_before_event(shared: &Shared) {
    let mut m = shared.motion.lock().unwrap();
    let mut inj = shared.injector.lock().unwrap();
    if inj.absolute_motion() {
        let (cx, cy) = m.follower.command();
        inj.move_cursor(cx, cy);
        return;
    }
    let (rx, ry) = inj.cursor_position();
    if let Some((dx, dy)) = m.follower.flush((rx, ry)) {
        inj.move_rel(dx, dy);
    }
}

/// A final placement at the last command (used when the session ends).
/// Absolute backends land exactly; relative backends have already
/// converged (the follower only corrects toward the command).
fn place_at_command(shared: &Shared) {
    let m = shared.motion.lock().unwrap();
    let mut inj = shared.injector.lock().unwrap();
    if inj.absolute_motion() {
        let (cx, cy) = m.follower.command();
        inj.move_cursor(cx, cy);
    }
}

/// Send one real-cursor beacon over the UDP stream. Loss-tolerant: a
/// dropped beacon is replaced by the next. The stream is quiet enough
/// that an outright dead link surfaces via the TCP keepalives.
fn send_beacon(shared: &Shared, own_id: u8, x: i32, y: i32) -> io::Result<()> {
    let seq = shared.udp_seq.fetch_add(1, Ordering::Relaxed);
    let bytes = udp::pack(own_id, seq, &Message::CursorPos { x, y });
    shared.udp.send(&bytes).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, UdpSocket};
    use std::sync::mpsc;
    use std::time::Duration;
    use kvmshare_protocol::message::KeyKind;

    /// A fake server on an already-bound listener (no race with the
    /// client connecting). Replies with `reply`, then plays `script`, then
    /// hangs up so the client sees a clean EOF.
    fn fake_server(listener: TcpListener, reply: Message, script: &[Message]) {
        let script: Vec<Message> = script.to_vec();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).unwrap(); // hello
            stream.write_all(&reply.encode()).unwrap();
            stream.flush().unwrap();
            for msg in &script {
                stream.write_all(&msg.encode()).unwrap();
                stream.flush().unwrap();
                thread::sleep(Duration::from_millis(20));
            }
            thread::sleep(Duration::from_millis(50));
        });
    }

    /// A clipboard that never has anything on it.
    struct NoClipboard;

    impl Clipboard for NoClipboard {
        fn set(&mut self, _mime: &str, _data: &[u8]) {}
        fn get(&mut self) -> Option<(String, Vec<u8>)> {
            None
        }
        fn last_injected(&mut self) -> Option<(String, Vec<u8>)> {
            None
        }
    }

    /// Records calls into a shared vec so the test can inspect them after
    /// the (consuming) run loop has finished. `info` is shared too, so a
    /// test can simulate a resolution change while the loop is running.
    /// `pos` is the simulated real cursor position: absolute moves set
    /// it, relative moves shift it (a fake OS without acceleration).
    /// `abs` simulates an absolute-placement backend (SetCursorPos-style):
    /// each relative move lands the whole delta exactly.
    struct RecordingInjector {
        calls: Arc<Mutex<Vec<String>>>,
        info: Arc<Mutex<ScreenInfo>>,
        pos: Arc<Mutex<(i32, i32)>>,
        abs: bool,
    }

    impl Default for RecordingInjector {
        fn default() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                info: Arc::new(Mutex::new(ScreenInfo::default())),
                pos: Arc::new(Mutex::new((0, 0))),
                abs: false,
            }
        }
    }

    impl RecordingInjector {
        fn new(info: ScreenInfo) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                info: Arc::new(Mutex::new(info)),
                pos: Arc::new(Mutex::new((0, 0))),
                abs: false,
            }
        }
        fn absolute(mut self) -> Self {
            self.abs = true;
            self
        }
    }

    impl Injector for RecordingInjector {
        fn absolute_motion(&self) -> bool {
            self.abs
        }
        fn screen_info(&mut self) -> ScreenInfo {
            *self.info.lock().unwrap()
        }
        fn move_cursor(&mut self, x: i32, y: i32) {
            *self.pos.lock().unwrap() = (x, y);
            self.calls.lock().unwrap().push(format!("move {x},{y}"));
        }
        fn move_rel(&mut self, dx: i32, dy: i32) {
            let mut pos = self.pos.lock().unwrap();
            pos.0 += dx;
            pos.1 += dy;
            drop(pos);
            self.calls.lock().unwrap().push(format!("rel {dx},{dy}"));
        }
        fn cursor_position(&mut self) -> (i32, i32) {
            *self.pos.lock().unwrap()
        }
        fn button(&mut self, button: u8, pressed: bool) {
            self.calls.lock().unwrap().push(format!("button {button} {pressed}"));
        }
        fn wheel(&mut self, dx: i32, dy: i32) {
            self.calls.lock().unwrap().push(format!("wheel {dx},{dy}"));
        }
        fn key(&mut self, kind: KeyKind, key: u32) {
            self.calls.lock().unwrap().push(format!("key {kind:?} {key}"));
        }
        fn enter(&mut self) {
            self.calls.lock().unwrap().push("enter".into());
        }
        fn leave(&mut self) {
            self.calls.lock().unwrap().push("leave".into());
        }
    }

    #[test]
    fn client_handshakes_and_applies_messages() {
        let port = 39001;
        let welcome = Message::Welcome {
            server_version: kvmshare_protocol::VERSION,
            layout: Layout { screens: vec![] },
            own_screen_id: 7,
        };
        let script = [Message::MouseMoveAbs { x: 100, y: 200 }, Message::Leave { screen_id: 1 }];
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        fake_server(listener, welcome, &script);

        let mut injector = RecordingInjector::new(ScreenInfo { width: 1920, height: 1080, scale: 1.0 });
        let calls_handle = injector.calls.clone();
        let client = Client::connect(&format!("127.0.0.1:{port}"), "test", injector.screen_info()).unwrap();
        assert_eq!(client.own_id(), 7);
        let (_tx, rx) = mpsc::channel::<Message>();
        client.run(Box::new(injector), Box::new(NoClipboard), &rx).unwrap();

        let calls = calls_handle.lock().unwrap().clone();
        assert!(calls.contains(&"move 100,200".to_string()));
        assert!(calls.contains(&"leave".to_string()));
    }

    #[test]
    fn client_rejects_version_mismatch() {
        let port = 39002;
        let welcome = Message::Welcome {
            server_version: 999,
            layout: Layout { screens: vec![] },
            own_screen_id: 7,
        };
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        fake_server(listener, welcome, &[]);
        let info = ScreenInfo { width: 1920, height: 1080, scale: 1.0 };
        let err = Client::connect(&format!("127.0.0.1:{port}"), "test", info).unwrap_err();
        assert!(err.to_string().contains("v999"));
    }

    #[test]
    fn resolution_change_is_reported() {
        let port = 39003;
        let welcome = Message::Welcome {
            server_version: kvmshare_protocol::VERSION,
            layout: Layout { screens: vec![] },
            own_screen_id: 1,
        };
        let (tx, rx) = mpsc::channel::<String>();
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf).unwrap(); // hello
            stream.write_all(&welcome.encode()).unwrap();
            // Read until the client's ScreenInfo frame arrives, then hang
            // up so the client loop sees EOF.
            loop {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                // SCREEN_INFO (0x03) in a type byte at offset 4 of a frame.
                if buf[..n].windows(5).any(|w| w[0..4] == *b"KVM1" && w[4] == 0x03) {
                    let _ = tx.send("screeninfo seen".into());
                    break;
                }
            }
        });

        let info = ScreenInfo { width: 1920, height: 1080, scale: 1.0 };
        let injector = RecordingInjector::new(info);
        let info_handle = injector.info.clone();
        let client = Client::connect(&format!("127.0.0.1:{port}"), "test", info).unwrap();
        let (_out_tx, out_rx) = mpsc::channel::<Message>();

        // Run the client on its own thread and simulate a display scale
        // change while it is running.
        let client_thread =
            thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx).unwrap());
        thread::sleep(Duration::from_millis(200));
        *info_handle.lock().unwrap() = ScreenInfo { width: 3840, height: 2160, scale: 2.0 };

        client_thread.join().unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok("screeninfo seen".to_string()));
    }

    /// The full UDP cursor path, exactly as the real server provides it:
    /// TCP handshake + control channel, UDP socket on the same port, the
    /// client's registration datagram teaching us where to send motion.
    ///
    /// Under the closed-loop model the frames are *not* injected verbatim:
    /// half is fed forward immediately and the damped corrections deliver
    /// the rest against the read-back real cursor. What must hold is that
    /// the command trajectory is honored **exactly** — total injected
    /// motion equals the sum of the received frames, in order, with the
    /// stale duplicate never re-applied.
    #[test]
    fn udp_motion_stream_reaches_the_command_exactly_and_dedupes() {
        let port = 39004;
        let welcome = Message::Welcome {
            server_version: kvmshare_protocol::VERSION,
            layout: Layout { screens: vec![] },
            own_screen_id: 7,
        };
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        let udp = UdpSocket::bind(("127.0.0.1", port)).unwrap();
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        thread::spawn(move || {
            // Control channel: handshake.
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).unwrap(); // hello
            stream.write_all(&welcome.encode()).unwrap();
            stream.flush().unwrap();
            // The client registers its UDP stream; learn its address.
            let mut reg = [0u8; 512];
            let (n, from) = udp.recv_from(&mut reg).unwrap();
            let d = crate::udp::unpack(&reg[..n]).expect("registration datagram");
            assert_eq!(d.id, 7);
            assert_eq!(d.msg, Message::KeepAlive);
            // Take control, then feed the cursor stream.
            thread::sleep(Duration::from_millis(30));
            stream.write_all(&Message::Enter { screen_id: 7, x: 100, y: 100 }.encode()).unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(80));
            // A burst of three motion frames, then a stale duplicate.
            for (seq, dx) in [(1u32, 10), (2, 20), (3, 30)] {
                let bytes = crate::udp::pack(7, seq, &Message::MouseMoveRel { dx, dy: 0 });
                udp.send_to(&bytes, from).unwrap();
            }
            let dup = crate::udp::pack(7, 2, &Message::MouseMoveRel { dx: 99, dy: 0 });
            udp.send_to(&dup, from).unwrap();
            thread::sleep(Duration::from_millis(200));
            // Tell the recorder we are done; the client keeps running
            // until we close the TCP side.
            drop(stream);
        });

        let mut injector = RecordingInjector::new(ScreenInfo { width: 1920, height: 1080, scale: 1.0 });
        let calls_handle = injector.calls.clone();
        let pos_handle = injector.pos.clone();
        let client = Client::connect(&format!("127.0.0.1:{port}"), "test", injector.screen_info()).unwrap();
        let (_tx, rx) = mpsc::channel::<Message>();
        // The fake server closes the TCP side after ~310ms; run until EOF.
        let _ = client.run(Box::new(injector), Box::new(NoClipboard), &rx);
        let _ = calls;

        let calls = calls_handle.lock().unwrap().clone();
        // The command trajectory is honored exactly: entry at (100,100)
        // plus the received frames (10+20+30, 0) — no frame lost to the
        // wire, none duplicated by the stale re-send, and the closed-loop
        // corrections settle on the command rather than overshooting it.
        assert_eq!(*pos_handle.lock().unwrap(), (160, 100), "command trajectory honored exactly");
        let rels: Vec<&String> = calls.iter().filter(|c| c.starts_with("rel ")).collect();
        let total_x: i64 = rels
            .iter()
            .filter_map(|c| c.split_once(' ').and_then(|(_, r)| r.split_once(',')))
            .map(|(x, _)| x.parse::<i64>().unwrap())
            .sum();
        assert_eq!(total_x, 60, "total injected motion equals the command sum");
        assert!(
            rels.iter().all(|c| !c.contains("99")),
            "stale duplicate (seq 2, dx 99) must never be applied, got {rels:?}"
        );
        assert!(
            rels.iter().all(|c| c.ends_with(",0")),
            "no motion outside the command axis, got {rels:?}"
        );
        assert!(calls.contains(&"enter".to_string()));
        assert!(calls.iter().any(|c| c == "move 100,100"));
    }

    /// An absolute-placement backend (SetCursorPos-style) must land the
    /// cursor **exactly on the command trajectory**: every received frame
    /// advances the command (the stale duplicate never re-applies), and
    /// the per-tick placement puts the cursor at the command — once,
    /// evenly, never one clump per datagram burst.
    #[test]
    fn absolute_backends_land_exactly_on_the_command_via_tick_placement() {
        let port = 39005;
        let welcome = Message::Welcome {
            server_version: kvmshare_protocol::VERSION,
            layout: Layout { screens: vec![] },
            own_screen_id: 7,
        };
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        let udp = UdpSocket::bind(("127.0.0.1", port)).unwrap();

        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).unwrap(); // hello
            stream.write_all(&welcome.encode()).unwrap();
            stream.flush().unwrap();
            let mut reg = [0u8; 512];
            let (n, from) = udp.recv_from(&mut reg).unwrap();
            let d = crate::udp::unpack(&reg[..n]).expect("registration datagram");
            assert_eq!(d.id, 7);
            thread::sleep(Duration::from_millis(30));
            stream.write_all(&Message::Enter { screen_id: 7, x: 100, y: 100 }.encode()).unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(80));
            for (seq, dx) in [(1u32, 10), (2, 20), (3, 30)] {
                let bytes = crate::udp::pack(7, seq, &Message::MouseMoveRel { dx, dy: 0 });
                udp.send_to(&bytes, from).unwrap();
            }
            let dup = crate::udp::pack(7, 2, &Message::MouseMoveRel { dx: 99, dy: 0 });
            udp.send_to(&dup, from).unwrap();
            thread::sleep(Duration::from_millis(200));
            drop(stream);
        });

        let mut injector = RecordingInjector::new(ScreenInfo { width: 1920, height: 1080, scale: 1.0 }).absolute();
        let calls_handle = injector.calls.clone();
        let pos_handle = injector.pos.clone();
        let client = Client::connect(&format!("127.0.0.1:{port}"), "test", injector.screen_info()).unwrap();
        let (_tx, rx) = mpsc::channel::<Message>();
        let _ = client.run(Box::new(injector), Box::new(NoClipboard), &rx);

        let calls = calls_handle.lock().unwrap().clone();
        // Nothing is injected per datagram — the tick places the whole
        // command. The cursor lands exactly on the command trajectory.
        assert!(
            calls.iter().all(|c| !c.starts_with("rel ")),
            "no per-datagram injection for absolute backends, got {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c == "move 160,100"),
            "tick placement lands on the command, got {calls:?}"
        );
        assert_eq!(*pos_handle.lock().unwrap(), (160, 100), "cursor lands exactly on the command");
        assert!(
            calls.iter().all(|c| !c.contains("99")),
            "stale duplicate (seq 2, dx 99) must never be applied, got {calls:?}"
        );
        assert!(calls.contains(&"enter".to_string()));
        assert!(calls.iter().any(|c| c == "move 100,100"));
    }
}