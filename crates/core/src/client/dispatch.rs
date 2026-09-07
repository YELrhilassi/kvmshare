//! Applies one server message to the local machine.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use kvmshare_log::{log_debug, log_trace, log_warn};
use kvmshare_protocol::message::{Layout, Message};

use crate::client::shared::{InjectEvent, Shared};
use crate::client::threads::send_beacon;

/// Apply one server message to the local machine. Ordering-critical
/// events (button, key, wheel) first place the cursor on the command
/// point; Enter/Leave flip control ownership; everything else is
/// dispatch.
pub(crate) fn dispatch(layout: &mut Layout, shared: &Arc<Shared>, own_id: u8, msg: Message) {
    match msg {
        Message::MouseMoveRel { dx, dy } => {
            // Defensive: motion normally arrives over UDP (drained by
            // the UDP thread); a frame on the control channel follows
            // the same path.
            crate::client::threads::apply_motion_frame(shared, dx, dy);
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
        // must sit on the command point before the event fires. The
        // motion loop places the cursor on every tick, so executing the
        // event on that same cadence (via the queue) guarantees the
        // ordering. Injection is never done from this thread: it is an
        // OS call that can block (UIPI / elevated windows), and a block
        // here would wedge the whole control loop unrecoverably — see
        // [`Shared::events`].
        Message::MouseButton { button, pressed } => {
            log_trace!("button {button} {}", if pressed { "down" } else { "up" });
            shared.enqueue_event(InjectEvent::Button { button, pressed });
        }
        Message::MouseWheel { dx, dy } => {
            log_trace!("wheel {dx},{dy}");
            shared.enqueue_event(InjectEvent::Wheel { dx, dy });
        }
        Message::Key { kind, key } => {
            log_trace!("key {kind:?} {key}");
            shared.enqueue_event(InjectEvent::Key { kind, key });
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