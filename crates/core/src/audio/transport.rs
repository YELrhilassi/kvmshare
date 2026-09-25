//! The dedicated UDP socket audio rides on.
//!
//! One socket per machine, bound to its own ephemeral port and announced to
//! the peer in the handshake. It is deliberately **not** the cursor
//! stream's socket: that one is a low-volume, high-priority, bidirectional
//! cursor channel, and putting a 1.5 Mbit/s audio payload in the same queue
//! would make the cursor wait behind audio and make audio jitter with cursor
//! traffic. Separate sockets mean the two are independently paced, and
//! either can be tuned without touching the other.
//!
//! # Security
//!
//! Every datagram from an address other than the authenticated peer is
//! dropped **before** it is parsed. This mirrors the hardening the cursor
//! stream received (`ClientCtx::tcp_ips`): the UDP port is reachable by
//! anything on the LAN, and a datagram is only trusted once the TCP
//! handshake has established who the peer is. The port itself is ephemeral
//! and unguessable, which is defence in depth rather than the defence.
//!
//! # Blocking
//!
//! The socket is used in blocking mode with a read timeout. That is what
//! lets the playout loop both wait for audio and notice a stop request
//! without spinning — the failure mode that previously pinned a core in the
//! GUI's discovery loops.

use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Mutex;
use std::time::Duration;

/// Receive buffer for one datagram: the largest packet the format allows,
/// plus its header. Sized once and reused for the life of the stream rather
/// than allocated per datagram.
pub const RECV_BUFFER: usize = super::packet::HEADER_LEN + super::packet::MAX_PAYLOAD;

/// The machine's audio socket.
#[derive(Debug)]
pub struct AudioSocket {
    socket: UdpSocket,
    /// The authenticated peer's IP, from the TCP connection. `None` until
    /// the handshake completes, during which every datagram is rejected —
    /// fail closed.
    peer_ip: Mutex<Option<IpAddr>>,
}

impl AudioSocket {
    /// Bind an ephemeral port on every interface.
    ///
    /// The bind must be unspecified rather than loopback: the peer is on
    /// another machine. Reachability is governed by the peer-IP filter and
    /// by the platform firewall (which the installer already configures),
    /// not by the bind address.
    pub fn bind() -> io::Result<Self> {
        let socket = UdpSocket::bind((IpAddr::from([0, 0, 0, 0]), 0))?;
        Ok(Self { socket, peer_ip: Mutex::new(None) })
    }

    /// The port to announce to the peer.
    pub fn local_port(&self) -> io::Result<u16> {
        Ok(self.socket.local_addr()?.port())
    }

    /// Trust datagrams from this address — called once the TCP handshake
    /// has proven who the peer is. Until then nothing is accepted.
    pub fn allow_peer(&self, ip: IpAddr) {
        *self.peer_ip.lock().unwrap() = Some(ip);
    }

    /// Stop trusting any peer (session ended). Rejects everything again.
    pub fn clear_peer(&self) {
        *self.peer_ip.lock().unwrap() = None;
    }

    /// Send one datagram to the peer's audio port.
    ///
    /// `ErrorKind::WouldBlock` is reported as success-by-omission by the
    /// caller's judgement: audio is loss-tolerant, so a full send buffer
    /// means the network is behind and dropping this packet is exactly the
    /// right response. Returning the error lets the caller count it.
    pub fn send(&self, datagram: &[u8], peer_port: u16) -> io::Result<()> {
        let ip = self.peer_ip().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "no authenticated audio peer yet")
        })?;
        let addr = SocketAddr::new(ip, peer_port);
        self.socket.send_to(datagram, addr)?;
        Ok(())
    }

    /// Receive one datagram from the peer, or `None` on timeout.
    ///
    /// Datagrams from any other address are discarded silently and the read
    /// is retried: the port is on a shared network, so unrelated traffic is
    /// expected rather than exceptional, and it must never be parsed as
    /// audio or surfaced as an error the user has to think about.
    pub fn recv(&self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        loop {
            match self.socket.recv_from(buf) {
                Ok((len, from)) => {
                    let trusted = self.peer_ip().map(|ip| ip == from.ip()).unwrap_or(false);
                    if trusted {
                        return Ok(Some(len));
                    }
                    // Not the peer: drop it and keep waiting. Logged once
                    // per event at debug level, because on a busy network
                    // this could be frequent and a warning per packet would
                    // be noise.
                    kvmshare_log::log_debug!(
                        "audio: dropped a datagram from {} (not the authenticated peer)",
                        from
                    );
                }
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    return Ok(None);
                }
                // A datagram larger than the buffer is truncated by the OS
                // and useless; skip it rather than playing a partial frame.
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                    // ICMP port-unreachable from a previous send: on a
                    // connectionless socket this is a stale report about a
                    // datagram already sent, not a problem with this read.
                    kvmshare_log::log_debug!("audio: peer refused a datagram (stale ICMP)");
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// How long `recv` waits before returning `None`. Set to roughly one
    /// frame so the playout loop wakes at the audio cadence.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.socket.set_read_timeout(timeout)
    }

    fn peer_ip(&self) -> Option<IpAddr> {
        *self.peer_ip.lock().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    /// An unauthenticated socket accepts nothing and sends nothing: fail
    /// closed, so a datagram that arrives before the handshake completes is
    /// never treated as audio.
    #[test]
    fn nothing_is_accepted_before_the_handshake() {
        let socket = AudioSocket::bind().unwrap();
        assert!(socket.send(&[0u8; 8], 1234).is_err(), "no peer to send to");

        // Put a datagram in the socket's own receive queue, then prove it
        // is rejected because no peer has been authenticated.
        let other = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        other
            .send_to(&[0u8; 8], (Ipv4Addr::LOCALHOST, socket.local_port().unwrap()))
            .unwrap();
        socket.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        let mut buf = [0u8; 64];
        assert_eq!(socket.recv(&mut buf).unwrap(), None, "rejected before authentication");
    }

    /// Once the peer is authenticated, its datagrams are delivered.
    #[test]
    fn an_authenticated_peer_is_accepted() {
        let receiver = AudioSocket::bind().unwrap();
        receiver.allow_peer(IpAddr::from(Ipv4Addr::LOCALHOST));
        receiver.set_read_timeout(Some(Duration::from_millis(500))).unwrap();

        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let payload = [7u8; 32];
        sender
            .send_to(&payload, (Ipv4Addr::LOCALHOST, receiver.local_port().unwrap()))
            .unwrap();

        let mut buf = [0u8; 128];
        let len = receiver.recv(&mut buf).unwrap().expect("datagram delivered");
        assert_eq!(len, payload.len());
        assert_eq!(&buf[..len], &payload);
    }

    /// A datagram from anyone else is discarded, and the read keeps
    /// waiting rather than reporting an error or handing up foreign bytes.
    #[test]
    fn a_datagram_from_another_host_is_discarded() {
        let receiver = AudioSocket::bind().unwrap();
        // Trust a different address than the one that will send.
        receiver.allow_peer(IpAddr::from(Ipv4Addr::new(10, 0, 0, 1)));
        receiver.set_read_timeout(Some(Duration::from_millis(80))).unwrap();

        let stranger = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        stranger
            .send_to(&[9u8; 16], (Ipv4Addr::LOCALHOST, receiver.local_port().unwrap()))
            .unwrap();

        let mut buf = [0u8; 64];
        assert_eq!(receiver.recv(&mut buf).unwrap(), None, "foreign datagram dropped");
    }

    /// Clearing the peer rejects everything again, so an ended session
    /// cannot keep streaming audio.
    #[test]
    fn clearing_the_peer_closes_the_stream() {
        let receiver = AudioSocket::bind().unwrap();
        receiver.allow_peer(IpAddr::from(Ipv4Addr::LOCALHOST));
        assert!(receiver.send(&[0u8; 4], 1).is_ok());
        receiver.clear_peer();
        assert!(receiver.send(&[0u8; 4], 1).is_err());
    }

    /// The receive buffer must be able to hold the largest legal datagram,
    /// or a valid packet would be truncated into nonsense.
    #[test]
    fn the_receive_buffer_holds_a_maximum_datagram() {
        assert!(RECV_BUFFER > super::super::packet::MAX_PAYLOAD);
        assert_eq!(
            RECV_BUFFER,
            super::super::packet::HEADER_LEN + super::super::packet::MAX_PAYLOAD
        );
    }
}
