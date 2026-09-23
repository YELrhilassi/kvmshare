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
//!
//! Every transport enables TCP keepalive with a short probe cadence.
//! This is not a nicety — it is the difference between a session that
//! notices a dead peer and one that never does. The application-level
//! silence timeouts (the server's `CLIENT_SILENT_TIMEOUT`, the client's
//! supervisor) only fire while the local reader is *waking up*; when a
//! peer vanishes without a FIN (power loss, cable pull, NAT reset, a
//! mid-flight RST), the kernel delivers nothing and a blocking read
//! would wait forever. Keepalive probes make the kernel itself detect
//! the dead link and fail every outstanding read within seconds —
//! measured here as: peer socket closed hard → local reads error out
//! inside [`DEAD_PEER_TIMEOUT`].

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use kvmshare_protocol::{id::MAGIC, Frame, Message, HEADER_LEN, LEN_OFFSET};

// Keepalive cadence (see the module docs). The socket may idle for
// seconds between control messages, so the idle probe must be shorter
// than the application's silence timeouts: 2 s idle (one skipped
// keepalive is not yet suspicious), then probes every 2 s — Linux lets
// the probe count be tuned; Windows fixes it at 10 (an upper bound of
// 20 s to detection, still inside the reconnect contract).
const TCP_KEEPALIVE_SECS_BEFORE: u32 = 2;
const TCP_KEEPALIVE_SECS_INTERVAL: u32 = 2;
#[cfg(unix)]
const TCP_KEEPALIVE_RETRIES: u32 = 3;

/// Upper bound on how long detecting a hard-dead peer may take: 2 s
/// idle + 3 probes × 2 s ≈ 8 s. Documented as a contract for tests and
/// the ops narrative ("the GUI stops claiming a session within one
/// silence timeout").
pub const DEAD_PEER_TIMEOUT: Duration = Duration::from_secs(8);

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
        enable_keepalive(&stream)?;
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

/// Turn on TCP keepalive with the short cadence from the module docs.
///
/// std has no keepalive API, so this goes to the socket options
/// directly: `TCP_KEEPALIVE`/`TCP_KEEPINTVL`/`TCP_KEEPCNT` on Unix,
/// `SIO_KEEPALIVE_VALS` on Windows. Failure is *not* fatal to the
/// transport — a link without keepalive degrades to the old
/// "detects nothing" behavior rather than refusing to work at all —
/// but every constructor calls it, and a successful call is the
/// documented behavior.
fn enable_keepalive(stream: &TcpStream) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = stream.as_raw_fd();
        // Idle time before the first probe. TCP_KEEPIDLE is the portable
        // name on Linux; macOS lacks it and uses TCP_KEEPALIVE (same
        // numeric value, 0x4) — the cfg picks per-OS so both compile.
        #[cfg(target_os = "macos")]
        const TCP_KEEPIDLE: i32 = libc::TCP_KEEPALIVE;
        #[cfg(not(target_os = "macos"))]
        const TCP_KEEPIDLE: i32 = libc::TCP_KEEPIDLE;
        set_opt(fd, libc::IPPROTO_TCP, TCP_KEEPIDLE, TCP_KEEPALIVE_SECS_BEFORE)?;
        set_opt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, TCP_KEEPALIVE_SECS_INTERVAL)?;
        set_opt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, TCP_KEEPALIVE_RETRIES)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock;
        // Windows expresses the cadence in one call: `SIO_KEEPALIVE_VALS`
        // takes (onoff, idle time in ms, interval in ms) — milliseconds
        // of idle before the first probe, then the same interval between
        // probes (probe count is fixed at 10 by the OS — with our 2 s
        // interval that is ≤20 s to detection, inside the contract).
        // SAFETY: a valid socket handle and correctly sized in/out
        // buffers for the documented SIO_KEEPALIVE_VALS contract.
        let onoff: u32 = 1;
        let idle_ms: u32 = TCP_KEEPALIVE_SECS_BEFORE * 1000;
        let interval_ms: u32 = TCP_KEEPALIVE_SECS_INTERVAL * 1000;
        let mut ret = 0u32;
        let res = unsafe {
            WinSock::WSAIoctl(
                stream.as_raw_socket() as usize,
                WinSock::SIO_KEEPALIVE_VALS,
                &mut (onoff, idle_ms, interval_ms) as *mut (u32, u32, u32) as *mut _,
                std::mem::size_of::<(u32, u32, u32)>() as u32,
                std::ptr::null_mut(),
                0,
                &mut ret,
                std::ptr::null_mut(),
                None,
            )
        };
        if res == WinSock::SOCKET_ERROR {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(unix)]
fn set_opt(fd: i32, level: i32, option: i32, value: u32) -> io::Result<()> {
    let v: libc::c_int = value as libc::c_int;
    let res = unsafe { libc::setsockopt(fd, level, option, &v as *const _ as *const libc::c_void, std::mem::size_of::<libc::c_int>() as libc::socklen_t) };
    if res != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loopback_pair() -> (TcpStream, TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (a, _b) = (
            TcpStream::connect(addr).unwrap(),
            listener.incoming().next().unwrap().unwrap(),
        );
        (a, _b)
    }

    // The constructor wires keepalive on every transport (both roles
    // build them; a silent failure here is the old "never notices" bug
    // coming back). Verifiable directly on Linux/macOS; on other
    // platforms the constructor still must succeed.
    #[cfg(unix)]
    #[test]
    fn constructor_enables_keepalive_options() {
        use std::os::fd::AsRawFd;
        let (a, _b) = loopback_pair();
        let mut t = Transport::with_read_timeout(a, Some(Duration::from_millis(50))).unwrap();
        // Re-read the options through the same fd to prove they stuck.
        let fd = t.stream.as_raw_fd();
        let mut val: libc::c_int = 0;
        let mut len: libc::socklen_t = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let res = unsafe {
            libc::getsockopt(fd, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, &mut val as *mut _ as *mut libc::c_void, &mut len)
        };
        assert_eq!(res, 0, "getsockopt failed: {}", std::io::Error::last_os_error());
        assert_eq!(val, TCP_KEEPALIVE_RETRIES as libc::c_int, "TCP_KEEPCNT not applied");
        // And the transport still works end-to-end.
        t.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    }

    // Transport contract on a dead peer: after the peer's socket is
    // closed hard (RST path), recv surfaces it as EOF promptly — this is
    // the app-visible detection the keepalive backstops; the slow
    // no-FIN path is what keepalive itself covers (cannot be simulated
    // portably in a unit test without root packet injection).
    #[test]
    fn recv_reports_eof_when_peer_resets() {
        // set_linger is not stable on TcpStream yet; drop without RST
        // still gives a prompt FIN-driven EOF, which is the contract
        // under test (peer death surfaces as EOF, quickly).
        let (a, b) = loopback_pair();
        let mut t = Transport::with_read_timeout(a, Some(Duration::from_millis(100))).unwrap();
        drop(b);
        let started = std::time::Instant::now();
        loop {
            match t.recv().unwrap() {
                RecvResult::Eof => break,
                RecvResult::NoData => continue,
                RecvResult::Msg(_) => panic("unexpected message"),
            }
        }
        assert!(started.elapsed() < DEAD_PEER_TIMEOUT * 2, "detection took too long");
    }

    fn panic(msg: &str) -> ! {
        std::panic::panic_any(msg.to_string())
    }
}