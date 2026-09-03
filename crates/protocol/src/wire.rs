//! Small cursor-style readers/writers over byte slices.
//!
//! Hand-rolled instead of pulling in a serde-style dependency: the
//! messages are few and simple, and this keeps encode/decode readable.

use std::fmt;

/// Error produced when decoding malformed bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError {
    pub what: &'static str,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "wire error: {}", self.what)
    }
}

impl std::error::Error for WireError {}

/// A growable writer that serializes values big-endian.
#[derive(Default)]
pub struct WriteBuf {
    bytes: Vec<u8>,
}

impl WriteBuf {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserve room up front for the common hot path (mouse moves).
    pub fn with_capacity(cap: usize) -> Self {
        Self { bytes: Vec::with_capacity(cap) }
    }

    pub fn put_u8(&mut self, v: u8) {
        self.bytes.push(v);
    }

    pub fn put_u16(&mut self, v: u16) {
        self.bytes.extend_from_slice(&v.to_be_bytes());
    }

    pub fn put_u32(&mut self, v: u32) {
        self.bytes.extend_from_slice(&v.to_be_bytes());
    }

    pub fn put_i32(&mut self, v: i32) {
        self.bytes.extend_from_slice(&v.to_be_bytes());
    }

    pub fn put_u64(&mut self, v: u64) {
        self.bytes.extend_from_slice(&v.to_be_bytes());
    }

    pub fn put_bytes(&mut self, b: &[u8]) {
        self.bytes.extend_from_slice(b);
    }

    /// Length-prefixed UTF-8 string.
    pub fn put_str(&mut self, s: &str) {
        self.put_u32(s.len() as u32);
        self.bytes.extend_from_slice(s.as_bytes());
    }

    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// A cursor over a byte slice for decoding.
pub struct ReadBuf<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> ReadBuf<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], WireError> {
        let end = self.pos.checked_add(n).ok_or(WireError { what })?;
        if end > self.bytes.len() {
            return Err(WireError { what });
        }
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub fn get_u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1, "u8")?[0])
    }

    pub fn get_u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_be_bytes(self.take(2, "u16")?.try_into().unwrap()))
    }

    pub fn get_u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_be_bytes(self.take(4, "u32")?.try_into().unwrap()))
    }

    pub fn get_i32(&mut self) -> Result<i32, WireError> {
        Ok(i32::from_be_bytes(self.take(4, "i32")?.try_into().unwrap()))
    }

    pub fn get_u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_be_bytes(self.take(8, "u64")?.try_into().unwrap()))
    }

    pub fn get_bytes(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], WireError> {
        self.take(n, what)
    }

    /// Length-prefixed UTF-8 string.
    pub fn get_str(&mut self) -> Result<&'a str, WireError> {
        let len = self.get_u32()? as usize;
        if len > 64 * 1024 {
            return Err(WireError { what: "string too long" });
        }
        let raw = self.take(len, "string")?;
        std::str::from_utf8(raw).map_err(|_| WireError { what: "invalid utf-8 string" })
    }

    /// Bytes not yet consumed.
    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_primitives() {
        let mut w = WriteBuf::new();
        w.put_u8(1);
        w.put_u16(2);
        w.put_u32(3);
        w.put_i32(-4);
        w.put_u64(5);
        w.put_str("héllo");
        w.put_bytes(&[9, 8, 7]);

        let bytes = w.finish();
        let mut r = ReadBuf::new(&bytes);
        assert_eq!(r.get_u8().unwrap(), 1);
        assert_eq!(r.get_u16().unwrap(), 2);
        assert_eq!(r.get_u32().unwrap(), 3);
        assert_eq!(r.get_i32().unwrap(), -4);
        assert_eq!(r.get_u64().unwrap(), 5);
        assert_eq!(r.get_str().unwrap(), "héllo");
        assert_eq!(r.get_bytes(3, "x").unwrap(), &[9, 8, 7]);
    }

    #[test]
    fn truncated_read_fails() {
        let mut r = ReadBuf::new(&[0u8; 2]);
        assert!(r.get_u32().is_err());
    }

    #[test]
    fn oversized_string_rejected() {
        let mut w = WriteBuf::new();
        w.put_u32(200_000); // lies about length
        let bytes = w.finish();
        let mut r = ReadBuf::new(&bytes);
        assert!(r.get_str().is_err());
    }
}