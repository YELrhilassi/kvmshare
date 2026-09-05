//! A thin, blocking transport: one [`Message`] per frame over a TCP stream.
//!
//! Two modes:
//!
//! * **Blocking** (`Transport::new`) — used by the server. `recv()` waits
//!   for the next message.
//! * **Timed** (`Transport::with_read_timeout`) — used by the client, so
//!   its main loop can also drain the outbox, poll the screen and send
//!   keepalives. A timed-out read yields [`RecvResult::NoData`] instead
//!   of blocking forever.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use kvmshare_protocol::{id::MAGIC, Frame, Message, HEADER_LEN, LEN_OFFSET};

/// What [`Transport::recv`] found.
#[derive(Debug)]
pub enum RecvResult {
    /// A full message arrived.
    Msg(Message),
    /// No message within the read timeout (timed transports only).
    NoData,
    /// Clean end of stream (peer closed).
    Eof,
}

/// A blocking message transport over a TCP stream.
#[derive(Debug)]
pub struct Transport {
    stream: TcpStream,
    read_buf: Vec<u8>,
}

impl Transport {
    pub fn new(stream: TcpStream) -> io::Result<Self> {
        Self::with_read_timeout(stream, None)
    }

    pub fn with_read_timeout(stream: TcpStream, timeout: Option<Duration>) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        stream.set_read_timeout(timeout)?;
        Ok(Self { stream, read_buf: Vec::with_capacity(4096) })
    }

    /// Change the read timeout (used by the client to wake at a finer
    /// cadence while it is pacing motion or being controlled, so buffered
    /// motion and periodic duties are never delayed by a long block).
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    /// A **read-only** handle sharing this connection's socket, with its
    /// own read buffer.
    ///
    /// TCP is full-duplex, so a reader can block on `recv` forever while
    /// another thread sends on the original — no lock needed on the read
    /// side. This is what lets the server's per-client thread read freely
    /// while the main thread writes to the same client.
    pub fn reader(&self) -> io::Result<Self> {
        let stream = self.stream.try_clone()?;
        // A reader never writes; keep any read timeout (None for server).
        Ok(Self { stream, read_buf: Vec::with_capacity(4096) })
    }

    /// Serialize and write one message. The hot path (mouse moves) writes
    /// a single ~16-byte frame; TCP_NODELAY keeps it on the wire at once.
    pub fn send(&mut self, msg: &Message) -> io::Result<()> {
        let bytes = msg.encode();
        self.stream.write_all(&bytes)
    }

    /// Read the next message. Blocks until one arrives, the peer closes,
    /// or (timed transports) the read timeout elapses.
    pub fn recv(&mut self) -> io::Result<RecvResult> {
        let mut scratch = [0u8; 512];
        loop {
            if let Some(msg) = self.try_decode()? {
                return Ok(RecvResult::Msg(msg));
            }
            let n = match self.stream.read(&mut scratch) {
                Ok(0) => {
                    // Only a clean EOF *between* frames is acceptable.
                    return if self.read_buf.is_empty() {
                        Ok(RecvResult::Eof)
                    } else {
                        Err(io::Error::other("eof mid-frame"))
                    };
                }
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
                    return Ok(RecvResult::NoData);
                }
                // A reset is not an error to the peer — it means the peer
                // is gone (its socket closed with data in flight, or the
                // OS tore the connection down). The session outcome is
                // the same as a clean EOF: nothing more will arrive.
                // Treating it as `Eof` keeps the client's reconnect loop
                // quiet (no spurious "session ended: connection reset")
                // and the server's per-client teardown identical for
                // abrupt and graceful disconnects.
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                    ) =>
                {
                    return Ok(RecvResult::Eof);
                }
                Err(e) => return Err(e),
            };
            self.read_buf.extend_from_slice(&scratch[..n]);
        }
    }

    fn try_decode(&mut self) -> io::Result<Option<Message>> {
        // Peek: we need at least the header to know the frame length.
        if self.read_buf.len() < HEADER_LEN {
            return Ok(None);
        }
        if self.read_buf[..MAGIC.len()] != MAGIC {
            // Desync: drop bytes until we find the magic again.
            if let Some(pos) = self.read_buf.windows(MAGIC.len()).position(|w| w == MAGIC) {
                self.read_buf.drain(..pos);
            } else {
                self.read_buf.clear();
            }
            return Ok(None);
        }
        let len = u32::from_be_bytes(
            self.read_buf[LEN_OFFSET..HEADER_LEN].try_into().expect("header slice is 4 bytes"),
        ) as usize;
        let total = HEADER_LEN + len;
        if self.read_buf.len() < total {
            return Ok(None);
        }
        // Decode exactly one frame and keep any trailing bytes: several
        // frames often share one TCP segment, and dropping the rest here
        // would silently lose messages.
        let frame = {
            let mut cur = io::Cursor::new(&self.read_buf[..total]);
            Frame::decode_from(&mut cur)?.expect("frame present because we checked length")
        };
        self.read_buf.drain(..total);
        Ok(Some(Message::from_frame(&frame).map_err(io::Error::other)?))
    }
}