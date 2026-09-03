//! The session: one cursor, one layout, and the rules for moving between
//! screens.
//!
//! The session is pure logic. It owns the *virtual* cursor position and
//! answers "what should happen next?" by returning [`Action`]s. The caller
//! (the server or a test harness) executes the actions.
//!
//! ## Coordinate model
//!
//! * **Virtual coordinates** span the whole desktop layout. The cursor
//!   always has a virtual position, regardless of which screen it is on.
//! * **Local coordinates** are per-screen (`x - screen.rect.x`).
//! * On every switch the virtual cursor is **snapped to the entry point**
//!   of the destination screen, so absolute positions and `Enter` always
//!   agree — no drift, no off-by-one accumulation.
//! * While the cursor is on a remote screen, the *physical* cursor on the
//!   server machine is hidden and parked at the local center so it has
//!   room to roam. When it approaches the physical screen edge it is
//!   warped back to center ([`Action::RecenterLocal`]). Because the
//!   server reads XI2 *raw* motion (see `kvmshare-platform`), a warp is
//!   invisible to the input stream — no phantom deltas, no oscillation.

use kvmshare_protocol::message::{Message, Rect};

use crate::layout::Layout;
use crate::Mode;

/// Something the caller must do in response to an input event.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send this message to the active client.
    Send(Message),
    /// Switch the cursor to client `to`, entering at its local coords
    /// `(x, y)`. The caller must:
    /// 1. send `Leave` to the previous active client,
    /// 2. send `Enter` + `MouseMoveAbs` to the new one,
    /// 3. warp the local cursor to `park` (its center) and hide it.
    SwitchTo { to: u8, x: i32, y: i32, park: (i32, i32) },
    /// Switch back to the local screen, entering at its local coords.
    SwitchToLocal { x: i32, y: i32 },
    /// Warp the hidden local physical cursor to `park` (edge guard). The
    /// virtual cursor is unaffected — raw input has no warp feedback.
    RecenterLocal { park: (i32, i32) },
    /// Nothing to do.
    Nothing,
}

/// How close the parked physical cursor may come to the physical screen
/// edge before it is re-centered.
const PHYSICAL_EDGE_MARGIN: i32 = 8;

/// The cursor model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cursor {
    /// Virtual position.
    x: i32,
    y: i32,
    mode: Mode,
}

/// The switching brain.
pub struct Session {
    layout: Layout,
    cursor: Cursor,
    /// The local screen's rectangle in virtual coordinates.
    local: Rect,
    /// Where the *physical* cursor currently sits on the local screen
    /// while we are on a remote screen (starts at the park point and
    /// follows the raw deltas). Only meaningful in `Remote` mode.
    phys: (i32, i32),
}

impl Session {
    pub fn new(layout: Layout, local_id: u8) -> Self {
        let local = layout.find(local_id).expect("local screen must be in layout").rect;
        let (cx, cy) = local.center();
        Self { cursor: Cursor { x: cx, y: cy, mode: Mode::Local }, layout, local, phys: (cx, cy) }
    }

    pub fn mode(&self) -> Mode {
        self.cursor.mode
    }

    /// The current layout (read-only view).
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The layout screen id for a client that introduced itself with
    /// `name`, if the layout has a remote screen with that name.
    pub fn assign_screen_id(&self, name: &str) -> Option<u8> {
        self.layout.screens.iter().find(|s| s.id != 0 && s.name == name).map(|s| s.id)
    }

    /// A client reported a new screen shape: resize its rect in the
    /// layout (position stays). Used to keep edge math correct after
    /// resolution/scale changes.
    pub fn update_screen_info(&mut self, id: u8, info: kvmshare_protocol::message::ScreenInfo) {
        if let Some(s) = self.layout.screens.iter_mut().find(|s| s.id == id) {
            let w = (info.width as f32 / info.scale.max(0.1)) as i32;
            let h = (info.height as f32 / info.scale.max(0.1)) as i32;
            s.rect.w = w;
            s.rect.h = h;
        }
    }

    /// Adopt a new layout at runtime (config hot-reload).
    ///
    /// If the cursor was on a remote client it is brought home first
    /// (`SwitchToLocal` at the new local center); if it was already local
    /// the virtual cursor is re-anchored to the local center without
    /// moving the physical one. A layout whose local screen (id 0) is
    /// missing is rejected and leaves the session untouched.
    pub fn swap_layout(&mut self, layout: Layout) -> Vec<Action> {
        let local = match layout.find(0) {
            Some(s) => s.rect,
            None => return vec![],
        };
        let was_remote = self.cursor.mode != Mode::Local;
        let (cx, cy) = local.center();
        self.layout = layout;
        self.local = local;
        self.cursor = Cursor { x: cx, y: cy, mode: Mode::Local };
        self.phys = (cx, cy);
        if was_remote {
            vec![Action::SwitchToLocal { x: cx, y: cy }]
        } else {
            vec![]
        }
    }

    /// Process a *local* input event (the user's physical mouse/keyboard
    /// on the server machine). Returns everything the caller must do.
    pub fn on_local_event(&mut self, msg: Message) -> Vec<Action> {
        match msg {
            Message::MouseMoveRel { dx, dy } => self.on_local_motion(dx, dy),
            Message::MouseMoveAbs { .. } => vec![], // absolute local moves carry no delta for clients
            Message::MouseButton { button, pressed } => {
                self.forward_while_remote(Message::MouseButton { button, pressed })
            }
            Message::MouseWheel { dx, dy } => self.forward_while_remote(Message::MouseWheel { dx, dy }),
            Message::Key { kind, key, scan } => self.forward_while_remote(Message::Key { kind, key, scan }),
            _ => vec![],
        }
    }

    fn forward_while_remote(&mut self, msg: Message) -> Vec<Action> {
        if matches!(self.cursor.mode, Mode::Remote(_)) {
            vec![Action::Send(msg)]
        } else {
            vec![]
        }
    }

    /// Relative motion of the physical cursor while the virtual cursor is
    /// anywhere. Returns the actions that keep the virtual cursor on the
    /// right screen.
    fn on_local_motion(&mut self, dx: i32, dy: i32) -> Vec<Action> {
        // Track the virtual position regardless of mode.
        self.cursor.x += dx;
        self.cursor.y += dy;

        match self.cursor.mode {
            Mode::Local => self.handle_local_motion(),
            Mode::Remote(id) => self.handle_remote_motion(id, dx, dy),
        }
    }

    /// Cursor is on the local screen and moving. If it leaves toward a
    /// neighbor, switch.
    fn handle_local_motion(&mut self) -> Vec<Action> {
        match self.layout.exit_direction(0, self.cursor.x, self.cursor.y) {
            None => vec![],
            Some(dir) => match self.layout.neighbor(0, dir, self.cursor.x, self.cursor.y) {
                Some((id, x, y)) => {
                    self.enter_screen(id, x, y);
                    vec![Action::SwitchTo { to: id, x, y, park: self.local.center() }]
                }
                None => {
                    // Dead edge: clamp the virtual cursor to the local
                    // screen boundary.
                    let local = self.local;
                    self.clamp_to(&local);
                    vec![]
                }
            },
        }
    }

    /// Cursor is on a remote screen and the physical mouse keeps moving.
    /// Forward the motion; switch screens (or back home) at edges; keep
    /// the parked physical cursor away from the physical screen edge.
    fn handle_remote_motion(&mut self, id: u8, dx: i32, dy: i32) -> Vec<Action> {
        let mut actions = Vec::with_capacity(2);

        // Edge guard: the hidden physical cursor follows the deltas; once
        // it nears the physical screen edge, warp it back to center. The
        // virtual cursor is untouched (raw input has no warp feedback).
        self.phys.0 += dx;
        self.phys.1 += dy;
        if !inside_margin(self.phys, &self.local, PHYSICAL_EDGE_MARGIN) {
            let park = self.local.center();
            self.phys = park;
            actions.push(Action::RecenterLocal { park });
        }

        let rect = match self.layout.find(id) {
            Some(s) => s.rect,
            None => return actions, // layout changed under us
        };
        let (local_x, local_y) = self.cursor.local_pos(&rect);

        if local_x < 0 || local_x >= rect.w || local_y < 0 || local_y >= rect.h {
            // We left this screen's rect. Which way?
            let dir = match self.layout.exit_direction(id, self.cursor.x, self.cursor.y) {
                Some(d) => d,
                None => {
                    // Shouldn't happen, but stay safe: clamp and resend.
                    self.clamp_to(&rect);
                    actions.push(self.send_local_pos(id));
                    return actions;
                }
            };
            match self.layout.neighbor(id, dir, self.cursor.x, self.cursor.y) {
                Some((next, x, y)) => {
                    if next == 0 {
                        // Back home: enter the local screen at its entry.
                        self.enter_screen(0, x, y);
                        actions.push(Action::SwitchToLocal { x, y });
                    } else {
                        self.enter_screen(next, x, y);
                        actions.push(Action::SwitchTo { to: next, x, y, park: self.local.center() });
                    }
                }
                None => {
                    // Outer edge of the desktop: clamp to the current
                    // screen and keep reporting the pinned position.
                    self.clamp_to(&rect);
                    actions.push(self.send_local_pos(id));
                }
            }
        } else {
            actions.push(Action::Send(Message::MouseMoveAbs { x: local_x, y: local_y }));
        }
        actions
    }

    /// Snap the virtual cursor to the entry point of screen `id` (local
    /// coords `x, y`) and set the mode.
    fn enter_screen(&mut self, id: u8, x: i32, y: i32) {
        let s = self.layout.find(id).expect("entry screen must exist");
        self.cursor.x = s.rect.x + x;
        self.cursor.y = s.rect.y + y;
        self.cursor.mode = if id == 0 { Mode::Local } else { Mode::Remote(id) };
        if id != 0 {
            // The engine parks the hidden physical cursor at the local
            // center; mirror that here so the edge guard starts fresh.
            self.phys = self.local.center();
        }
    }

    /// Send the current cursor position in screen `id`'s local coords.
    fn send_local_pos(&mut self, id: u8) -> Action {
        let rect = self.layout.find(id).expect("active screen must exist").rect;
        let (x, y) = self.cursor.local_pos(&rect);
        Action::Send(Message::MouseMoveAbs { x, y })
    }

    /// Clamp the virtual cursor inside `rect` (in virtual coords).
    fn clamp_to(&mut self, rect: &Rect) {
        self.cursor.x = self.cursor.x.clamp(rect.left(), rect.right() - 1);
        self.cursor.y = self.cursor.y.clamp(rect.top(), rect.bottom() - 1);
    }

    /// The cursor explicitly re-enters the local screen (e.g. after a
    /// layout change or a disconnect). Warp to the local center.
    pub fn force_local(&mut self) -> Action {
        let (x, y) = self.local.center();
        self.enter_screen(0, x, y);
        Action::SwitchToLocal { x, y }
    }

    /// A client with the given id disconnected while active: drop back to
    /// the local screen.
    pub fn on_client_disconnected(&mut self, id: u8) -> Action {
        if self.cursor.mode == Mode::Remote(id) {
            self.force_local()
        } else {
            Action::Nothing
        }
    }

    #[cfg(test)]
    pub fn cursor_pos(&self) -> (i32, i32) {
        (self.cursor.x, self.cursor.y)
    }
}

impl Cursor {
    /// Convert a virtual position to `rect`'s local coordinates.
    fn local_pos(&self, rect: &Rect) -> (i32, i32) {
        (self.x - rect.x, self.y - rect.y)
    }
}

/// Is `(x, y)` inside `rect` with at least `margin` px of slack on every
/// side?
fn inside_margin((x, y): (i32, i32), rect: &Rect, margin: i32) -> bool {
    x >= rect.left() + margin && x < rect.right() - margin && y >= rect.top() + margin && y < rect.bottom() - margin
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvmshare_protocol::message::{KeyKind, Rect, Screen};

    fn two_screens() -> Session {
        let layout = Layout::new(vec![
            Screen { id: 0, name: "pc".into(), rect: Rect { x: 0, y: 0, w: 1920, h: 1080 } },
            Screen { id: 1, name: "hp".into(), rect: Rect { x: -1920, y: 0, w: 1920, h: 1080 } },
        ]);
        Session::new(layout, 0)
    }

    #[test]
    fn assign_screen_id_matches_by_name() {
        let s = two_screens();
        assert_eq!(s.assign_screen_id("hp"), Some(1));
        assert_eq!(s.assign_screen_id("pc"), None); // the server's own screen
        assert_eq!(s.assign_screen_id("nope"), None);
    }

    #[test]
    fn update_screen_info_resizes_rect() {
        let mut s = two_screens();
        s.update_screen_info(1, kvmshare_protocol::message::ScreenInfo { width: 2560, height: 1440, scale: 1.0 });
        let hp = s.layout().find(1).unwrap();
        assert_eq!((hp.rect.w, hp.rect.h), (2560, 1440));
        // Position is untouched.
        assert_eq!(hp.rect.x, -1920);
    }

    #[test]
    fn local_motion_inside_does_nothing() {
        let mut s = two_screens();
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: 10, dy: 10 }), vec![]);
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn crossing_left_edge_switches_to_hp() {
        let mut s = two_screens();
        // From local center (960,540), -1000 puts us at x=-40, past the
        // left edge: switch.
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -1000, dy: 0 });
        match actions.as_slice() {
            [Action::SwitchTo { to, x, y, park }] => {
                assert_eq!(*to, 1);
                assert_eq!(*x, 1919); // hp's right edge, local coords
                assert_eq!(*y, 540);
                assert_eq!(*park, (960, 540));
            }
            other => panic!("expected SwitchTo, got {other:?}"),
        }
        assert_eq!(s.mode(), Mode::Remote(1));
        // Virtual position was snapped to hp's entry point (-1, 540).
        assert_eq!(s.cursor_pos(), (-1, 540));
    }

    #[test]
    fn remote_motion_forwards_absolute_positions() {
        let mut s = two_screens();
        s.on_local_event(Message::MouseMoveRel { dx: -1000, dy: 0 }); // switch to hp
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 });
        // virtual x = -1 - 10 = -11 -> hp-local = -11 + 1920 = 1909
        assert_eq!(actions, vec![Action::Send(Message::MouseMoveAbs { x: 1909, y: 540 })]);
    }

    #[test]
    fn crossing_back_returns_to_local() {
        let mut s = two_screens();
        s.on_local_event(Message::MouseMoveRel { dx: -1000, dy: 0 }); // to hp, virtual (-1,540)
        // One step right: virtual 0 is already past hp's half-open rect
        // ([-1920, 0)), so this crosses back to pc.
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 0, y: 540 }]);
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn buttons_forward_only_when_remote() {
        let mut s = two_screens();
        assert_eq!(s.on_local_event(Message::MouseButton { button: 0, pressed: true }), vec![]);
        s.on_local_event(Message::MouseMoveRel { dx: -1000, dy: 0 });
        let actions = s.on_local_event(Message::MouseButton { button: 0, pressed: true });
        assert_eq!(actions, vec![Action::Send(Message::MouseButton { button: 0, pressed: true })]);
    }

    #[test]
    fn outer_edge_clamps() {
        let mut s = two_screens();
        s.on_local_event(Message::MouseMoveRel { dx: -1000, dy: 0 }); // on hp, virtual (-1,540)
        // Push far left past hp's left edge (virtual -1920).
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -3000, dy: 0 });
        // Clamped to hp's left edge: hp-local x=0. (The physical cursor
        // also drifted 3000px left, so expect a recenter too.)
        assert_eq!(actions[0], Action::RecenterLocal { park: (960, 540) });
        assert_eq!(actions[1], Action::Send(Message::MouseMoveAbs { x: 0, y: 540 }));
    }

    #[test]
    fn disconnect_returns_home() {
        let mut s = two_screens();
        s.on_local_event(Message::MouseMoveRel { dx: -1000, dy: 0 });
        assert_eq!(s.on_client_disconnected(1), Action::SwitchToLocal { x: 960, y: 540 });
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn key_events_forward_when_remote() {
        let mut s = two_screens();
        s.on_local_event(Message::MouseMoveRel { dx: -1000, dy: 0 });
        let actions = s.on_local_event(Message::Key { kind: KeyKind::Down, key: 10, scan: 20 });
        assert_eq!(actions, vec![Action::Send(Message::Key { kind: KeyKind::Down, key: 10, scan: 20 })]);
    }

    #[test]
    fn remote_to_remote_switch_keeps_park_center() {
        let layout = Layout::new(vec![
            Screen { id: 0, name: "pc".into(), rect: Rect { x: 0, y: 0, w: 1920, h: 1080 } },
            Screen { id: 1, name: "hp".into(), rect: Rect { x: -1920, y: 0, w: 1920, h: 1080 } },
            Screen { id: 2, name: "mac".into(), rect: Rect { x: -3840, y: 0, w: 1920, h: 1080 } },
        ]);
        let mut s = Session::new(layout, 0);
        // pc -> hp
        s.on_local_event(Message::MouseMoveRel { dx: -1000, dy: 0 });
        // hp -> mac (keep moving left). A full screen-width of physical
        // travel also trips the edge guard, so a recenter leads.
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 });
        match actions.as_slice() {
            [Action::RecenterLocal { .. }, Action::SwitchTo { to, x, y, park }] => {
                assert_eq!(*to, 2);
                assert_eq!(*x, 1919); // mac's right edge
                assert_eq!(*y, 540);
                assert_eq!(*park, (960, 540));
            }
            other => panic!("expected [Recenter, SwitchTo], got {other:?}"),
        }
    }

    #[test]
    fn recenter_keeps_remote_cursor_untouched() {
        let mut s = two_screens();
        s.on_local_event(Message::MouseMoveRel { dx: -1000, dy: 0 }); // on hp
        // Push left until the parked physical cursor hits the local edge
        // (960 - 968 = -8 <= margin) — recenter fires, hp cursor keeps
        // moving.
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -970, dy: 0 });
        assert_eq!(actions[0], Action::RecenterLocal { park: (960, 540) });
        assert_eq!(actions[1], Action::Send(Message::MouseMoveAbs { x: 949, y: 540 }));
        // A small push right after the recenter must NOT recenter again.
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 5, dy: 0 });
        assert_eq!(actions, vec![Action::Send(Message::MouseMoveAbs { x: 954, y: 540 })]);
    }

    #[test]
    fn swap_layout_while_local_stays_put() {
        let mut s = two_screens();
        assert_eq!(s.mode(), Mode::Local);

        // Same geometry, only names change: nothing to do, cursor stays.
        let new_layout = Layout::new(vec![
            Screen { id: 0, name: "pc".into(), rect: Rect { x: 0, y: 0, w: 1920, h: 1080 } },
            Screen { id: 1, name: "hp".into(), rect: Rect { x: -3840, y: 0, w: 1920, h: 1080 } },
        ]);
        let actions = s.swap_layout(new_layout);
        assert_eq!(actions, vec![]);
        assert_eq!(s.mode(), Mode::Local);
        assert_eq!(s.layout().screens[1].rect.x, -3840);
    }

    #[test]
    fn swap_layout_while_remote_comes_home() {
        let mut s = two_screens();
        s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 }); // now on hp
        assert_eq!(s.mode(), Mode::Remote(1));

        // New layout without hp at all (and a different local size).
        let new_layout = Layout::new(vec![Screen {
            id: 0,
            name: "pc".into(),
            rect: Rect { x: 0, y: 0, w: 2560, h: 1440 },
        }]);
        let actions = s.swap_layout(new_layout);
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 1280, y: 720 }]);
        assert_eq!(s.mode(), Mode::Local);
        assert_eq!(s.cursor_pos(), (1280, 720));
    }

    #[test]
    fn swap_layout_rejects_missing_local_screen() {
        let mut s = two_screens();
        s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 }); // on hp
        let bad = Layout::new(vec![Screen {
            id: 1,
            name: "hp".into(),
            rect: Rect { x: -1920, y: 0, w: 1920, h: 1080 },
        }]);
        let actions = s.swap_layout(bad);
        assert_eq!(actions, vec![]);
        assert_eq!(s.mode(), Mode::Remote(1), "bad layout must not disturb the session");
        assert_eq!(s.layout().screens.len(), 2);
    }
}