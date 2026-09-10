//! The beacon thread: polls the real pointer position on its own X
//! connection and feeds the session's position beacons.
//!
//! This is the *only* thread that does a `QueryPointer` round-trip. The
//! event thread forwards raw motion and processes X events without ever
//! waiting on a reply, so a busy X server (compositor, a game, heavy
//! load) can delay these round-trips without stalling the cursor
//! stream.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use kvmshare_protocol::message::Message;

use crate::x11::geometry::VisibleDesktop;

use super::thread::{BEACON_IDLE_AFTER, BEACON_IDLE_PERIOD, BEACON_PERIOD};

/// Poll the real pointer position on a dedicated thread with its own X
/// connection, feeding the shared [`InputCapture::real_pos`] and — while
/// the local pointer is free — sending position beacons at
/// [`BEACON_PERIOD`] cadence.
///
/// This is the *only* thread that does a `QueryPointer` round-trip. The
/// event thread forwards raw motion and processes X events without ever
/// waiting on a reply, so a busy X server can delay the beacon thread's
/// round-trips without stalling the cursor stream.
///
/// While the pointer is grabbed, or once the position has not changed
/// for a few consecutive queries, the round-trip backs off to
/// [`BEACON_IDLE_PERIOD`]: there is nothing to report, so the cost drops
/// to a rounding error. The first query after the position changes (or
/// the grab releases) drops straight back to the fast cadence.
pub fn spawn_beacon_thread(
    display: Option<String>,
    visible: VisibleDesktop,
    grabbed: Arc<AtomicBool>,
    real_pos: Arc<Mutex<Option<(i32, i32)>>>,
    tx: Sender<Message>,
) {
    thread::spawn(move || {
        let Ok((conn, screen_num)) = RustConnection::connect(display.as_deref()) else {
            return;
        };
        let root = conn.setup().roots[screen_num].root;
        let mut last: Option<(i32, i32)> = None;
        let mut same_count: u32 = 0;
        let mut interval = BEACON_PERIOD;
        loop {
            // While the local pointer is grabbed (cursor on a client),
            // beacons are meaningless — skip the round-trip entirely and
            // take the idle cadence. The moment the grab releases, the
            // next iteration resumes at the fast cadence.
            if !grabbed.load(Ordering::Relaxed) {
                if let Ok(cookie) = conn.query_pointer(root) {
                    if let Ok(reply) = cookie.reply() {
                        // Root pixels -> visible-local: the session
                        // works in visible pixels (see [`VisibleDesktop`]).
                        let (x, y) = visible.from_root(reply.root_x as i32, reply.root_y as i32);
                        *real_pos.lock().unwrap() = Some((x, y));
                        if last != Some((x, y)) {
                            last = Some((x, y));
                            same_count = 0;
                            interval = BEACON_PERIOD;
                            // The session re-anchors on every beacon; the
                            // ordering guarantee (motion first, then the
                            // position) is the event thread's job — the
                            // poll path is a resync, never a command.
                            if tx.send(Message::MouseMoveAbs { x, y }).is_err() {
                                return; // server gone
                            }
                        } else {
                            // Position unchanged: back off once a few
                            // consecutive queries agree, and stay backed
                            // off until it moves again.
                            same_count += 1;
                            if same_count >= BEACON_IDLE_AFTER {
                                interval = BEACON_IDLE_PERIOD;
                            }
                        }
                    }
                }
            }
            thread::sleep(interval);
        }
    });
}
