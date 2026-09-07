//! The client's worker threads: motion steering, UDP cursor stream, and
//! the slow sync duties. Each loop is deliberately single-purpose so
//! nothing can make the cursor wait.

use std::io;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use kvmshare_log::{log_trace, log_warn};
use kvmshare_protocol::message::Message;

use crate::client::shared::Shared;
use crate::motion::MOTION_PERIOD;
use crate::time::now_ms;
use crate::udp;

/// How often the client reports its real cursor position to the server
/// while being controlled. The server anchors its virtual cursor and
/// edge decisions on these — a tight cadence keeps crossings exact
/// without flooding the wire. Beacons ride UDP; a lost one is replaced
/// by the next.
const CURSOR_BEACON_INTERVAL: Duration = Duration::from_millis(8);
/// UDP socket read timeout: the cursor stream thread blocks on `recv`
/// and wakes at this cadence when idle (to notice shutdown). Frames
/// themselves wake it immediately — this is not a poll.
pub(crate) const UDP_RECV_TIMEOUT: Duration = Duration::from_millis(8);
/// How often the sync thread re-checks the display geometry (rare
/// event; the poll exists so a resolution change is noticed without
/// restarting). Kept long so this thread rarely touches the injector
/// lock — a display query must never contend with cursor placement.
const SCREEN_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How often the sync thread polls the local clipboard to push changes
/// up. The poll lives on the sync thread, so even a clipboard read that
/// stalls for tens of milliseconds delays only the clipboard sync.
const CLIPBOARD_INTERVAL: Duration = Duration::from_millis(500);

/// The motion thread: a fixed-cadence steering loop. Each tick it places
/// the real cursor on the commanded position (absolute backends) or
/// corrects toward it (relative backends), beacons the real position
/// back at [`CURSOR_BEACON_INTERVAL`], and samples telemetry. It does
/// nothing else — no network reads, no clipboard, no screen queries — so
/// nothing can make the cursor wait.
pub(crate) fn motion_loop(shared: Arc<Shared>, own_id: u8) {
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
            // Execute queued injection events (buttons, keys, wheel) at
            // the placed position, on this thread's cadence — see
            // [`Shared::events`] for why injection never happens on the
            // control thread. A block here stalls only the motion loop,
            // which the supervisor and watchdogs recover.
            shared.drain_events(&mut inj);
            // Telemetry is collected under the locks but logged only
            // after they are released — a slow log sink must never hold
            // up the next placement. The screen query stays lazy: it is
            // only needed to disambiguate a pin, and it is a user32 call
            // that can stall on a busy desktop — never on the hot path.
            let report = m.probe_window(rx, ry, || inj.screen_info());
            if report.pinned {
                inj.emergency_release();
                shared.stop.store(true, Ordering::Relaxed);
                recover = true;
            }
            drop(m);
            drop(inj);
            if let Some(line) = report.trace {
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
pub(crate) fn udp_loop(shared: Arc<Shared>, own_id: u8) {
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
pub(crate) fn sync_loop(shared: Arc<Shared>, tx: Sender<Message>) {
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
pub(crate) fn apply_motion_frame(shared: &Shared, dx: i32, dy: i32) {
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

/// A final placement at the last command (used when the session ends).
/// Absolute backends land exactly; relative backends have already
/// converged (the follower only corrects toward the command).
pub(crate) fn place_at_command(shared: &Shared) {
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
pub(crate) fn send_beacon(shared: &Shared, own_id: u8, x: i32, y: i32) -> io::Result<()> {
    let seq = shared.udp_seq.fetch_add(1, Ordering::Relaxed);
    let bytes = udp::pack(own_id, seq, &Message::CursorPos { x, y });
    shared.udp.send(&bytes).map(|_| ())
}