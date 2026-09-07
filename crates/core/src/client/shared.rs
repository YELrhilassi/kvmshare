//! State shared by the client's worker threads, the injection event
//! queue, and the cursor-side steering state (`MotionState`).

use std::collections::VecDeque;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::Mutex;

use kvmshare_log::log_error;
use kvmshare_protocol::message::{KeyKind, ScreenInfo};

use crate::client::injector::{Clipboard, Injector};
use crate::motion::{MotionProbe, PositionFollower};

/// Consecutive 100 ms probe windows where the cursor was commanded to
/// move but did not (away from a screen edge) before the client treats
/// injected input as blocked and recovers. ~600 ms: long enough that a
/// single hiccup never trips it, short enough that a genuinely blocked
/// cursor recovers in about a second.
const PIN_WINDOWS_TO_TRIP: u32 = 6;
/// Minimum commanded motion (pixels) within one probe window for the
/// cursor to count as "commanded to move".
const PIN_MIN_COMMANDED_PX: i64 = 60;
/// Maximum real travel (pixels) within one probe window for the cursor
/// to still count as pinned (not following the command).
const PIN_MAX_TRAVEL_PX: i64 = 6;

/// State shared by the client's worker threads. Every thread touches
/// only the fields it needs, and every critical section is short —
/// microsecond-scale lock holds on [`Shared::motion`] and
/// [`Shared::injector`] (always in that order).
pub(crate) struct Shared {
    /// The platform injector. Touched by the motion thread (placement),
    /// the TCP thread (events, enter/leave) and the sync thread (screen
    /// info). Never held across a slow call.
    pub(crate) injector: Mutex<Box<dyn Injector>>,
    /// The platform clipboard, on its own lock. A clipboard read or
    /// write can block (another process holding the clipboard open), so
    /// it must never serialize with the cursor — a stalled clipboard
    /// delays only clipboard sync.
    pub(crate) clipboard: Mutex<Box<dyn Clipboard>>,
    /// The cursor follower (command trajectory), telemetry probe, and
    /// per-window wire counters.
    pub(crate) motion: Mutex<MotionState>,
    /// Whether control is currently on this machine. Flipped by the TCP
    /// thread on Enter/Leave; read every tick by the motion and UDP
    /// threads.
    pub(crate) active: AtomicBool,
    /// The UDP cursor stream: motion in, beacons out.
    pub(crate) udp: UdpSocket,
    /// Sequence for outgoing beacon datagrams (registration used 0).
    pub(crate) udp_seq: AtomicU32,
    /// Set when the session ends; worker threads exit at their next wake.
    pub(crate) stop: AtomicBool,
    /// Monotonic millis of the motion thread's last loop wake. Bumped
    /// unconditionally every iteration, so a stalled value means the
    /// motion thread is genuinely wedged (not merely idle). Read by the
    /// supervisor to detect the stall and by the teardown path to decide
    /// whether joining would hang.
    pub(crate) motion_tick_ms: AtomicU64,
    /// Monotonic millis of the TCP thread's last wake (a dispatch or an
    /// idle NoData cycle). A stalled value while the link is healthy
    /// means the control loop is wedged inside a dispatch.
    pub(crate) tcp_tick_ms: AtomicU64,
    /// Injection events (buttons, keys, wheel) queued by the TCP thread
    /// and executed by the motion thread on its own cadence.
    ///
    /// Why: a button/key/wheel is an OS call (`SendInput` and friends)
    /// that can *block* — Windows UI-protection (UIPI) and elevated
    /// windows can stall it indefinitely. If the TCP thread executed
    /// injection directly while holding the injector lock, such a stall
    /// wedges the whole control loop permanently: the thread that must
    /// act on the supervisor's `stop` is the thread stuck inside the OS
    /// call, so no recovery is possible. Executing injection on the
    /// motion thread confines a block to the motion loop — which the
    /// supervisor, the isolation watchdog, and the server's beacon
    /// watchdog all recover.
    pub(crate) events: Mutex<VecDeque<InjectEvent>>,
}

/// One queued injection event (see [`Shared::events`] for why events are
/// queued instead of injected from the control thread).
#[derive(Debug)]
pub(crate) enum InjectEvent {
    Button { button: u8, pressed: bool },
    Wheel { dx: i32, dy: i32 },
    Key { kind: KeyKind, key: u32 },
}

/// How many events the queue may hold before the newest is dropped.
/// Bounded so a wedged motion thread cannot grow memory without limit;
/// dropping input is preferable to freezing the machine. Far above any
/// realistic burst (a drag streams a few events per second).
const EVENT_QUEUE_CAP: usize = 512;

impl Shared {
    /// Queue one injection event for the motion thread. Bounded: when
    /// the queue is full the event is dropped (the motion loop is
    /// wedged, and the recovery paths will restart the session anyway).
    pub(crate) fn enqueue_event(&self, event: InjectEvent) {
        let mut q = self.events.lock().unwrap();
        if q.len() < EVENT_QUEUE_CAP {
            q.push_back(event);
        }
    }

    /// Drain and execute every queued event, in order. Called by the
    /// motion loop after placing the cursor, with the injector lock
    /// already held (the loop holds it for placement). A blocking OS
    /// call inside an event stalls this tick — and only this tick: the
    /// motion loop is exactly the thread the supervisor and watchdogs
    /// recover.
    pub(crate) fn drain_events(&self, inj: &mut Box<dyn Injector>) {
        let pending: Vec<InjectEvent> = {
            let mut q = self.events.lock().unwrap();
            q.drain(..).collect()
        };
        for event in pending {
            match event {
                InjectEvent::Button { button, pressed } => inj.button(button, pressed),
                InjectEvent::Wheel { dx, dy } => inj.wheel(dx, dy),
                InjectEvent::Key { kind, key } => inj.key(kind, key),
            }
        }
    }
}

/// The cursor-side steering state, guarded by [`Shared::motion`].
pub(crate) struct MotionState {
    pub(crate) follower: PositionFollower,
    pub(crate) probe: MotionProbe,
    /// Motion frames accepted since the last telemetry window (wire
    /// health — a stall shows as a window with few frames).
    pub(crate) frames_win: u32,
    /// Steering ticks since the last telemetry window (loop health).
    pub(crate) ticks_win: u32,
    /// Pixels commanded since the last probe window (accumulated in
    /// `apply_motion_frame`). Compared against the real cursor's travel
    /// to detect a cursor the OS refuses to move.
    pub(crate) win_cmd_px: i64,
    /// Real cursor position when the current probe window opened.
    pub(crate) win_real_start: (i32, i32),
    /// Consecutive windows where the cursor was commanded to move but
    /// did not. Enough of them, away from a screen edge, means injected
    /// motion is being eaten by the OS — the recovery path releases
    /// local input and restarts the session (see the probe block).
    pub(crate) pin_windows: u32,
}

/// Commands issued during a probe window: how much motion was commanded
/// (compared with real travel) and how many windows the cursor has been
/// pinned for. Returned by [`MotionState::probe_window`] for the caller
/// to act on (release + restart) after the locks are dropped.
pub(crate) struct ProbeReport {
    /// Trace line describing the window, if the probe sample is due.
    pub(crate) trace: Option<String>,
    /// True when injected motion is being eaten by the OS: the cursor
    /// was commanded well away from any screen edge yet did not travel.
    pub(crate) pinned: bool,
}

impl MotionState {
    /// Collect one telemetry window and decide whether the cursor is
    /// pinned (commanded to move but not moving, away from any edge).
    /// Pure computation on this state plus the real position: the caller
    /// holds the locks and releases them before logging, so a slow log
    /// sink can never hold up the next placement.
    ///
    /// `screen` is queried lazily — only when the pin threshold is
    /// reached — because on Windows it is a user32 call that can stall
    /// on a busy desktop. It must never run on every tick of the cursor
    /// hot path.
    pub(crate) fn probe_window(
        &mut self,
        rx: i32,
        ry: i32,
        screen: impl FnOnce() -> ScreenInfo,
    ) -> ProbeReport {
        let mut trace = None;
        let mut pinned = false;
        if self.probe.due() {
            let (ex, ey) = self.follower.error((rx, ry));
            let (frames, ticks) = (self.frames_win, self.ticks_win);
            self.frames_win = 0;
            self.ticks_win = 0;
            // Cursor-pin detection: this window commanded real motion
            // (`win_cmd_px`, accumulated as UDP frames arrived) yet the
            // real cursor did not travel. That means the OS is silently
            // eating injected motion — the supervisor cannot see it (the
            // motion thread is healthy; the cursor just never moves). A
            // few such windows away from a screen edge is the signature
            // of blocked input.
            let travel = ((rx - self.win_real_start.0).abs()
                + (ry - self.win_real_start.1).abs()) as i64;
            let is_pinned = self.win_cmd_px > PIN_MIN_COMMANDED_PX && travel < PIN_MAX_TRAVEL_PX;
            self.pin_windows = if is_pinned { self.pin_windows + 1 } else { 0 };
            let pin_age_ms = self.pin_windows as u64 * 100;
            self.win_cmd_px = 0;
            self.win_real_start = (rx, ry);
            if self.pin_windows >= PIN_WINDOWS_TO_TRIP {
                // A cursor legitimately parked at a screen edge is a
                // wall push (the clamp holds the command), not a
                // block — only trip away from the edges.
                let screen = screen();
                let at_edge = rx <= 1
                    || ry <= 1
                    || rx as i64 >= screen.width as i64 - 2
                    || ry as i64 >= screen.height as i64 - 2;
                if !at_edge {
                    log_error!(
                        "cursor pinned ~{pin_age_ms} ms while {frames} frames/tick commanded motion — injected input is being eaten; releasing local input and restarting the session"
                    );
                    pinned = true;
                }
            }
            self.probe.sample((rx, ry), &mut |rx, ry, ax, ay, _exp_x, _exp_y, gx, gy| {
                trace = Some(format!(
                    "motion req=({rx},{ry}) act=({ax},{ay}) err=({ex},{ey}) real=({gx},{gy}) frames={frames} ticks={ticks}"
                ));
            });
        }
        ProbeReport { trace, pinned }
    }
}