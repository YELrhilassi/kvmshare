//! The geometry and layout types carried by messages: screen shape,
//! rectangles, screens and the desktop layout.

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
    pub(crate) fn encode(&self, w: &mut WriteBuf) {
        w.put_u32(self.width);
        w.put_u32(self.height);
        w.put_u32(self.scale.to_bits());
    }

    pub(crate) fn decode(r: &mut ReadBuf<'_>) -> Result<Self, WireError> {
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
    pub(crate) fn to_id(self) -> u8 {
        match self {
            KeyKind::Down => crate::id::keys::DOWN,
            KeyKind::Up => crate::id::keys::UP,
            KeyKind::Repeat => crate::id::keys::REPEAT,
        }
    }

    pub(crate) fn from_id(id: u8) -> Option<Self> {
        match id {
            crate::id::keys::DOWN => Some(KeyKind::Down),
            crate::id::keys::UP => Some(KeyKind::Up),
            crate::id::keys::REPEAT => Some(KeyKind::Repeat),
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

    pub(crate) fn encode(&self, w: &mut WriteBuf) {
        w.put_i32(self.x);
        w.put_i32(self.y);
        w.put_i32(self.w);
        w.put_i32(self.h);
    }

    pub(crate) fn decode(r: &mut ReadBuf<'_>) -> Result<Self, WireError> {
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

    pub(crate) fn encode(&self, w: &mut WriteBuf) {
        w.put_u8(self.id);
        w.put_str(&self.name);
        self.rect.encode(w);
    }

    pub(crate) fn decode(r: &mut ReadBuf<'_>) -> Result<Self, WireError> {
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
    pub(crate) fn encode(&self, w: &mut WriteBuf) {
        w.put_u8(self.screens.len() as u8);
        for s in &self.screens {
            s.encode(w);
        }
    }

    pub(crate) fn decode(r: &mut ReadBuf<'_>) -> Result<Self, WireError> {
        let n = r.get_u8()? as usize;
        let mut screens = Vec::with_capacity(n);
        for _ in 0..n {
            screens.push(Screen::decode(r)?);
        }
        Ok(Self { screens })
    }
}