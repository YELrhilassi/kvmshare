//! Typed messages carried by frames.
//!
//! [`Message`] is the single source of truth for what peers can say to
//! each other. Encoding is a straightforward `match` over the variants;
//! decoding is its inverse. There is no macro magic — if you can read a
//! match statement you can read this file.

use crate::frame::Frame;
use crate::id::{keys, types};
use crate::wire::{ReadBuf, WireError, WriteBuf};

/// A screen's shape as reported by a client or used in a layout.
///
/// Not `Eq` on purpose: `scale` is an `f32`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenInfo {
    pub width: u32,
    pub height: u32,
    /// Logical scale factor (1.0 = 100%). Used to normalize coordinates
    /// across machines with different display scaling.
    pub scale: f32,
}

impl Default for ScreenInfo {
    fn default() -> Self {
        Self { width: 1920, height: 1080, scale: 1.0 }
    }
}

impl ScreenInfo {
    fn encode(&self, w: &mut WriteBuf) {
        w.put_u32(self.width);
        w.put_u32(self.height);
        w.put_u32(self.scale.to_bits());
    }

    fn decode(r: &mut ReadBuf<'_>) -> Result<Self, WireError> {
        Ok(Self {
            width: r.get_u32()?,
            height: r.get_u32()?,
            scale: f32::from_bits(r.get_u32()?),
        })
    }
}

/// Key kind for [`Message::Key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Down,
    Up,
    Repeat,
}

impl KeyKind {
    fn to_id(self) -> u8 {
        match self {
            KeyKind::Down => keys::DOWN,
            KeyKind::Up => keys::UP,
            KeyKind::Repeat => keys::REPEAT,
        }
    }

    fn from_id(id: u8) -> Option<Self> {
        match id {
            keys::DOWN => Some(KeyKind::Down),
            keys::UP => Some(KeyKind::Up),
            keys::REPEAT => Some(KeyKind::Repeat),
            _ => None,
        }
    }
}

/// A rectangle in the shared virtual desktop (screen coordinates, +y down).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn left(&self) -> i32 {
        self.x
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn top(&self) -> i32 {
        self.y
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    /// Inclusive test — the cursor at the last pixel of a screen still
    /// belongs to it (mirrors how real desktops behave).
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    fn encode(&self, w: &mut WriteBuf) {
        w.put_i32(self.x);
        w.put_i32(self.y);
        w.put_i32(self.w);
        w.put_i32(self.h);
    }

    fn decode(r: &mut ReadBuf<'_>) -> Result<Self, WireError> {
        Ok(Self { x: r.get_i32()?, y: r.get_i32()?, w: r.get_i32()?, h: r.get_i32()? })
    }
}

/// One screen in a layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// Stable id assigned by the server.
    pub id: u8,
    /// Host name / friendly name.
    pub name: String,
    pub rect: Rect,
}

impl Screen {
    /// `true` if the vertical spans of `self` and `other` overlap
    /// (required for left/right adjacency).
    pub fn overlaps_vertically(&self, other: &Screen) -> bool {
        self.rect.top() < other.rect.bottom() && other.rect.top() < self.rect.bottom()
    }

    /// `true` if the horizontal spans overlap (required for top/bottom
    /// adjacency).
    pub fn overlaps_horizontally(&self, other: &Screen) -> bool {
        self.rect.left() < other.rect.right() && other.rect.left() < self.rect.right()
    }

    fn encode(&self, w: &mut WriteBuf) {
        w.put_u8(self.id);
        w.put_str(&self.name);
        self.rect.encode(w);
    }

    fn decode(r: &mut ReadBuf<'_>) -> Result<Self, WireError> {
        Ok(Self {
            id: r.get_u8()?,
            name: r.get_str()?.to_owned(),
            rect: Rect::decode(r)?,
        })
    }
}

/// The full desktop layout (server → client).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub screens: Vec<Screen>,
}

impl Layout {
    fn encode(&self, w: &mut WriteBuf) {
        w.put_u8(self.screens.len() as u8);
        for s in &self.screens {
            s.encode(w);
        }
    }

    fn decode(r: &mut ReadBuf<'_>) -> Result<Self, WireError> {
        let n = r.get_u8()? as usize;
        let mut screens = Vec::with_capacity(n);
        for _ in 0..n {
            screens.push(Screen::decode(r)?);
        }
        Ok(Self { screens })
    }
}

/// Every message peers can exchange.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Client → server, first message.
    Hello { version: u16, name: String, info: ScreenInfo },
    /// Server → client, acknowledges the hello and sends the layout.
    Welcome { server_version: u16, layout: Layout, own_screen_id: u8 },
    /// Client → server: its screen shape changed (resolution/scale).
    ScreenInfo { info: ScreenInfo },
    /// Server → client: layout changed.
    Layout { layout: Layout },
    /// Server → client: the cursor is entering this screen at (x, y).
    Enter { screen_id: u8, x: i32, y: i32 },
    /// Server → client: the cursor is leaving this screen.
    Leave { screen_id: u8 },
    /// Absolute cursor position (client local pixels).
    MouseMoveAbs { x: i32, y: i32 },
    /// Relative cursor motion.
    MouseMoveRel { dx: i32, dy: i32 },
    /// Client → server: where the client's *real* cursor currently is
    /// (local pixels). Reported on a cadence while the client is being
    /// controlled. See [`crate::id::types::CURSOR_POS`].
    CursorPos { x: i32, y: i32 },
    MouseButton { button: u8, pressed: bool },
    MouseWheel { dx: i32, dy: i32 },
    /// Keyboard event. `key` is a **canonical USB HID usage id**, which is
    /// what makes a Windows machine talk to a Unix machine: every platform
    /// backend converts its native key identity (X11 keycode / Wayland
    /// evdev code / Windows scan code) to and from this id, so the wire is
    /// identical no matter which pair of OSes is connected.
    Key { kind: KeyKind, key: u32 },
    /// Clipboard content; `mime` describes the format ("text/plain", ...).
    Clipboard { mime: String, data: Vec<u8> },
    KeepAlive,
    Error { code: u8, text: String },
    /// Local only, never serialized to a peer: the user pressed the
    /// escape key (e.g. Scroll Lock) while the cursor was on a client
    /// and wants control back on the server machine now. The capture
    /// emits it; the session answers with a switch home.
    Escape,
}

impl Message {
    /// The frame type id for this message.
    pub fn msg_type(&self) -> u8 {
        match self {
            Message::Hello { .. } => types::HELLO,
            Message::Welcome { .. } => types::WELCOME,
            Message::ScreenInfo { .. } => types::SCREEN_INFO,
            Message::Layout { .. } => types::LAYOUT,
            Message::Enter { .. } => types::ENTER,
            Message::Leave { .. } => types::LEAVE,
            Message::MouseMoveAbs { .. } => types::MOUSE_MOVE_ABS,
            Message::MouseMoveRel { .. } => types::MOUSE_MOVE_REL,
            Message::CursorPos { .. } => types::CURSOR_POS,
            Message::MouseButton { .. } => types::MOUSE_BUTTON,
            Message::MouseWheel { .. } => types::MOUSE_WHEEL,
            Message::Key { .. } => types::KEY,
            Message::Clipboard { .. } => types::CLIPBOARD,
            Message::KeepAlive => types::KEEPALIVE,
            Message::Error { .. } => types::ERROR,
            Message::Escape => types::ESCAPE,
        }
    }

    /// Encode to a wire [`Frame`].
    pub fn to_frame(&self) -> Frame {
        let mut w = WriteBuf::with_capacity(32);
        match self {
            Message::Hello { version, name, info } => {
                w.put_u16(*version);
                w.put_str(name);
                info.encode(&mut w);
            }
            Message::Welcome { server_version, layout, own_screen_id } => {
                w.put_u16(*server_version);
                layout.encode(&mut w);
                w.put_u8(*own_screen_id);
            }
            Message::ScreenInfo { info } => info.encode(&mut w),
            Message::Layout { layout } => layout.encode(&mut w),
            Message::Enter { screen_id, x, y } => {
                w.put_u8(*screen_id);
                w.put_i32(*x);
                w.put_i32(*y);
            }
            Message::Leave { screen_id } => w.put_u8(*screen_id),
            Message::MouseMoveAbs { x, y } => {
                w.put_i32(*x);
                w.put_i32(*y);
            }
            Message::MouseMoveRel { dx, dy } => {
                w.put_i32(*dx);
                w.put_i32(*dy);
            }
            Message::CursorPos { x, y } => {
                w.put_i32(*x);
                w.put_i32(*y);
            }
            Message::MouseButton { button, pressed } => {
                w.put_u8(*button);
                w.put_u8(*pressed as u8);
            }
            Message::MouseWheel { dx, dy } => {
                w.put_i32(*dx);
                w.put_i32(*dy);
            }
            Message::Key { kind, key } => {
                w.put_u8(kind.to_id());
                w.put_u32(*key);
            }
            Message::Clipboard { mime, data } => {
                w.put_str(mime);
                w.put_u32(data.len() as u32);
                w.put_bytes(data);
            }
            Message::KeepAlive => {}
            Message::Error { code, text } => {
                w.put_u8(*code);
                w.put_str(text);
            }
            Message::Escape => {}
        }
        Frame::new(self.msg_type(), w.finish())
    }

    /// Decode from a raw frame. Returns an `Err` on malformed payloads.
    pub fn from_frame(frame: &Frame) -> Result<Self, WireError> {
        let mut r = ReadBuf::new(&frame.payload);
        let msg = match frame.msg_type {
            types::HELLO => Message::Hello {
                version: r.get_u16()?,
                name: r.get_str()?.to_owned(),
                info: ScreenInfo::decode(&mut r)?,
            },
            types::WELCOME => Message::Welcome {
                server_version: r.get_u16()?,
                layout: Layout::decode(&mut r)?,
                own_screen_id: r.get_u8()?,
            },
            types::SCREEN_INFO => Message::ScreenInfo { info: ScreenInfo::decode(&mut r)? },
            types::LAYOUT => Message::Layout { layout: Layout::decode(&mut r)? },
            types::ENTER => Message::Enter {
                screen_id: r.get_u8()?,
                x: r.get_i32()?,
                y: r.get_i32()?,
            },
            types::LEAVE => Message::Leave { screen_id: r.get_u8()? },
            types::MOUSE_MOVE_ABS => Message::MouseMoveAbs { x: r.get_i32()?, y: r.get_i32()? },
            types::MOUSE_MOVE_REL => Message::MouseMoveRel { dx: r.get_i32()?, dy: r.get_i32()? },
            types::CURSOR_POS => Message::CursorPos { x: r.get_i32()?, y: r.get_i32()? },
            types::MOUSE_BUTTON => Message::MouseButton {
                button: r.get_u8()?,
                pressed: r.get_u8()? != 0,
            },
            types::MOUSE_WHEEL => Message::MouseWheel { dx: r.get_i32()?, dy: r.get_i32()? },
            types::KEY => Message::Key {
                kind: KeyKind::from_id(r.get_u8()?).ok_or(WireError { what: "bad key kind" })?,
                key: r.get_u32()?,
            },
            types::CLIPBOARD => {
                let mime = r.get_str()?.to_owned();
                let len = r.get_u32()? as usize;
                let data = r.get_bytes(len, "clipboard data")?.to_vec();
                Message::Clipboard { mime, data }
            }
            types::KEEPALIVE => Message::KeepAlive,
            types::ERROR => Message::Error { code: r.get_u8()?, text: r.get_str()?.to_owned() },
            types::ESCAPE => Message::Escape,
            _other => return Err(WireError { what: "unknown message type" }),
        };
        // All payload bytes must be consumed — trailing bytes mean a bug
        // somewhere in encode/decode, so fail loudly.
        if r.remaining() != 0 {
            return Err(WireError { what: "trailing bytes in payload" });
        }
        Ok(msg)
    }

    /// Convenience: build the wire bytes directly.
    pub fn encode(&self) -> Vec<u8> {
        self.to_frame().encode()
    }

    /// Convenience: decode a single message from wire bytes. Rejects any
    /// bytes after the frame — this is a single-message API, and trailing
    /// bytes mean the caller mis-sliced.
    pub fn decode(bytes: &[u8]) -> Result<Self, WireError> {
        let mut cur = std::io::Cursor::new(bytes);
        let frame = match crate::frame::Frame::decode_from(&mut cur).map_err(|_| WireError { what: "bad frame" })? {
            Some(f) => f,
            None => return Err(WireError { what: "no frame" }),
        };
        if cur.position() as usize != bytes.len() {
            return Err(WireError { what: "trailing bytes after frame" });
        }
        Self::from_frame(&frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::errors;

    fn roundtrip(m: Message) -> Message {
        let bytes = m.encode();
        let back = Message::decode(&bytes).unwrap();
        assert_eq!(m, back);
        back
    }

    #[test]
    fn hello_roundtrip() {
        roundtrip(Message::Hello {
            version: 1,
            name: "hp".into(),
            info: ScreenInfo { width: 1920, height: 1080, scale: 1.0 },
        });
    }

    #[test]
    fn welcome_with_layout_roundtrip() {
        roundtrip(Message::Welcome {
            server_version: 1,
            layout: Layout {
                screens: vec![
                    Screen { id: 0, name: "pc".into(), rect: Rect { x: 0, y: 0, w: 1920, h: 1080 } },
                    Screen { id: 1, name: "hp".into(), rect: Rect { x: -1920, y: 0, w: 1920, h: 1080 } },
                ],
            },
            own_screen_id: 1,
        });
    }

    #[test]
    fn mouse_hot_path_roundtrip() {
        roundtrip(Message::MouseMoveAbs { x: 1234, y: -56 });
        roundtrip(Message::MouseMoveRel { dx: 12, dy: -3 });
        roundtrip(Message::CursorPos { x: 1919, y: 540 });
        roundtrip(Message::MouseButton { button: 0, pressed: true });
        roundtrip(Message::MouseWheel { dx: 0, dy: 120 });
    }

    #[test]
    fn key_and_clipboard_roundtrip() {
        roundtrip(Message::Key { kind: KeyKind::Repeat, key: 0x14 }); // HID usage: Q
        roundtrip(Message::Clipboard { mime: "text/plain".into(), data: b"hello".to_vec() });
    }

    #[test]
    fn keepalive_and_error_roundtrip() {
        roundtrip(Message::KeepAlive);
        roundtrip(Message::Error { code: errors::PROTOCOL, text: "nope".into() });
        roundtrip(Message::Escape);
    }

    #[test]
    fn trailing_bytes_rejected() {
        let mut bytes = Message::KeepAlive.encode();
        bytes.push(0xff);
        assert!(Message::decode(&bytes).is_err());
    }

    #[test]
    fn unknown_type_rejected() {
        let mut bytes = Message::KeepAlive.encode();
        bytes[4] = 0x99; // corrupt the type byte
        assert!(Message::decode(&bytes).is_err());
    }
}