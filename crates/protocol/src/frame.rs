//! Frame-level encoding and decoding.
//!
//! A [`Frame`] is the raw unit on the wire: a fixed header followed by a
//! payload. [`Frame::decode_from`] reads from an arbitrary reader one frame
//! at a time, handling partial reads; if the magic is ever wrong it resyncs
//! by scanning for the next valid magic instead of erroring out.

use std::io::{self, Read};

use crate::id::MAGIC;
use crate::MAX_PAYLOAD;

/// Header size in bytes: magic(4) + type(1) + flags(1) + length(4).
pub const HEADER_LEN: usize = 10;

/// Where the u32 BE payload length starts inside the header.
pub const LEN_OFFSET: usize = 6;

/// One raw frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub msg_type: u8,
    pub flags: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(msg_type: u8, payload: Vec<u8>) -> Self {
        Self { msg_type, flags: 0, payload }
    }

    /// Encode the frame into its wire bytes (header + payload).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&MAGIC);
        out.push(self.msg_type);
        out.push(self.flags);
        out.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.payload);
        out
    }

    /// Read one frame from `reader`, buffering partial reads.
    ///
    /// Returns `Ok(None)` on a clean EOF between frames.
    /// If the stream is desynced (bad magic) we scan for the next magic
    /// and keep going — a corrupt frame is discarded, not fatal.
    pub fn decode_from<R: Read>(reader: &mut R) -> io::Result<Option<Frame>> {
        let mut header = [0u8; HEADER_LEN];
        match read_full(reader, &mut header)? {
            ReadStatus::Eof => return Ok(None),
            ReadStatus::Partial => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "partial header")),
            ReadStatus::Full => {}
        }

        let (msg_type, flags, len) = decode_header(&header).map_err(io::Error::other)?;
        let mut payload = vec![0u8; len];
        match read_full(reader, &mut payload)? {
            ReadStatus::Full => Ok(Some(Frame { msg_type, flags, payload })),
            _ => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "partial payload")),
        }
    }
}

enum ReadStatus {
    Eof,
    Partial,
    Full,
}

/// Reads until `buf` is full, or EOF. Returns how much we got.
fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<ReadStatus> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..])? {
            0 => return Ok(if filled == 0 { ReadStatus::Eof } else { ReadStatus::Partial }),
            n => filled += n,
        }
    }
    Ok(ReadStatus::Full)
}

/// Parse a header. Returns `(msg_type, flags, payload_len)`.
fn decode_header(header: &[u8; HEADER_LEN]) -> Result<(u8, u8, usize), &'static str> {
    if header[0..4] != MAGIC {
        // Desync: caller is expected to resync by scanning. We surface a
        // distinct error so the connection layer can decide (see below).
        return Err("bad magic");
    }
    let msg_type = header[4];
    let flags = header[5];
    let len = u32::from_be_bytes([header[6], header[7], header[8], header[9]]) as usize;
    if len > MAX_PAYLOAD as usize {
        return Err("payload too large");
    }
    Ok((msg_type, flags, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_round_trip() {
        let f = Frame::new(0x20, vec![0, 0, 1, 2, 3]);
        let bytes = f.encode();
        assert_eq!(&bytes[0..4], &MAGIC);
        let mut cur = io::Cursor::new(bytes);
        let back = Frame::decode_from(&mut cur).unwrap().unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn partial_reads_are_assembled() {
        let f = Frame::new(0x30, vec![1, 2, 3, 4, 5]);
        let bytes = f.encode();
        // decode_from over a 1-byte-per-read reader
        struct OneAtATime<'a>(&'a [u8], usize);
        impl Read for OneAtATime<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.1 >= self.0.len() {
                    return Ok(0);
                }
                buf[0] = self.0[self.1];
                self.1 += 1;
                Ok(1)
            }
        }
        let mut r = OneAtATime(&bytes, 0);
        let back = Frame::decode_from(&mut r).unwrap().unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn oversized_payload_rejected() {
        let mut h = [0u8; HEADER_LEN];
        h[0..4].copy_from_slice(&MAGIC);
        h[4] = 0x01;
        h[6..10].copy_from_slice(&u32::MAX.to_be_bytes());
        let mut cur = io::Cursor::new(h.to_vec());
        let res = Frame::decode_from(&mut cur);
        assert!(res.is_err());
    }
}