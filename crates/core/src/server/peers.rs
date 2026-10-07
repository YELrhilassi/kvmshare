//! Per-client side state — one store.
//!
//! The server used to keep five `Arc<Mutex<HashMap<u8, _>>>` beside each
//! other: the client map itself, every client's UDP address,
//! cursor-stream sequence, stream-silence timestamp and handshake IP.
//! Every mutation had to hit all of them in the same order from every
//! call site, and "the same order" was exactly where the bugs lived — a
//! teardown that checked identity under one lock and removed rows under
//! the others could unregister a replacement connection that took over
//! the id meanwhile, and a registration racing a teardown could orphan a
//! row in one map.
//!
//! Those maps describe one thing — the connected client — so they are
//! one store now. [`Peers`] owns the client map plus a per-id
//! [`PeerRecord`] of everything the transport layers know about the
//! client; [`Peers::register`] and [`Peers::unregister`] are the only
//! writers of a record's lifetime, both identity-checked under the
//! single lock, so the supersede races the old comments had to guard
//! against become unrepresentable rather than avoided-by-discipline.
//!
//! Everything else is a point lookup or mutation under that one lock:
//! short and uncontended in practice (the writer thread, the UDP
//! receiver and the input loop touch different rows), and impossible to
//! half-complete — there is no second map to forget.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use kvmshare_log::{log_debug, log_warn};
use kvmshare_protocol::message::Message;

use crate::server::client::{enqueue_client, Client};

/// What the transport layers record about one connected client, beside
/// its [`Client`] handle. All of it dies with the client: there is no
/// state here that outlives a registration.
#[derive(Debug, Clone)]
pub struct PeerRecord {
    /// The IP the client's TCP handshake came from. Datagrams naming
    /// this client's id are accepted only from it — the id inside a
    /// datagram is a client-supplied claim, the handshake's source IP is
    /// not. One field, because there is exactly one authenticated
    /// identity per connection (see the wire docs).
    pub tcp_ip: IpAddr,
    /// Source address of the client's cursor stream, learned from its
    /// first datagram and re-learned when it reconnects from a new port.
    pub udp_addr: Option<SocketAddr>,
    /// Highest applied cursor-stream sequence for stale/duplicate
    /// rejection. Reset when the stream re-registers from a new address,
    /// because the old sequence space belongs to the old address (a late
    /// frame from the dead session would otherwise deafen the live one).
    pub seq: u32,
    /// Monotonic ms when the cursor stream was last heard. The active
    /// client beacons every few ms; silence is the signature of a wedged
    /// client — see the beacon watchdog in `server::udp`.
    pub last_heard: Option<u64>,
}

impl PeerRecord {
    /// A record at handshake time: identity proven, stream not yet
    /// registered.
    fn new(tcp_ip: IpAddr) -> Self {
        Self { tcp_ip, udp_addr: None, seq: 0, last_heard: None }
    }
}

/// The connected clients, with their transport records, behind one lock.
#[derive(Default)]
pub struct Peers {
    by_id: HashMap<u8, Arc<Client>>,
    records: HashMap<u8, PeerRecord>,
}

impl Peers {
    /// Is `id` currently held by exactly this client handle? The
    /// identity check every mutation funnels through: a stale
    /// connection (the machine reconnected and a fresh one took over
    /// the id) must never touch the fresh registration's state.
    fn is_current(&self, id: u8, client: &Client) -> bool {
        // Pointer identity of the `Client` the Arc wraps: callers hold
        // `Arc<Client>` and pass it as `&Client` (deref), so comparing
        // the inner allocation is the same identity test `Arc::ptr_eq`
        // performs, in the form the signature admits.
        match self.by_id.get(&id) {
            Some(c) => std::ptr::eq(Arc::as_ptr(c), client as *const Client),
            None => false,
        }
    }

    /// A client by id, if connected.
    pub fn get(&self, id: u8) -> Option<Arc<Client>> {
        self.by_id.get(&id).cloned()
    }

    /// All clients, in unspecified order (for broadcasts and
    /// reconciliation).
    pub fn all(&self) -> Vec<Arc<Client>> {
        self.by_id.values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Which connected client is `machine_id`? A short form is accepted
    /// as a prefix in either direction — the same forgiveness the trust
    /// policy applies to machine ids (users paste what they copied).
    /// This is the media router's pinned-target lookup.
    pub fn solve_machine_id(&self, machine_id: &str) -> Option<u8> {
        self.by_id
            .values()
            .find(|c| {
                let id = c.machine_id.as_str();
                id == machine_id || id.starts_with(machine_id) || machine_id.starts_with(id)
            })
            .map(|c| c.id)
    }

    /// Which ids does `predicate` accept? (Disconnect sweeps: revoked,
    /// stale layout.)
    pub fn ids_where(&self, predicate: impl Fn(&Client) -> bool) -> Vec<u8> {
        self.by_id.values().filter(|c| predicate(c)).map(|c| c.id).collect()
    }

    /// Register a fully handshaked client, replacing any stale
    /// connection that still holds its id.
    ///
    /// The registration and its transport record land together, so no
    /// other thread can observe the client in one map and not the
    /// other. The replaced client, if any, is returned so the caller
    /// can send its farewell *after* this returns — the lock is never
    /// held across a send.
    pub fn register(&mut self, id: u8, client: Arc<Client>, tcp_ip: IpAddr) -> Option<Arc<Client>> {
        self.records.insert(id, PeerRecord::new(tcp_ip));
        self.by_id.insert(id, client)
    }

    /// Remove `id` when — and only when — it is still `client`'s.
    ///
    /// Returns whether this caller owns the id and must run the rest of
    /// teardown (the session's return-home, the app-layer event). A
    /// superseded caller gets `false` and touches nothing: the fresh
    /// registration owns the id now, and the old reader simply finishes.
    pub fn unregister(&mut self, id: u8, client: &Client) -> bool {
        if !self.is_current(id, client) {
            return false;
        }
        self.by_id.remove(&id);
        self.records.remove(&id);
        true
    }

    /// Unconditionally drop client `id` and its record. The operator
    /// disconnect path uses this: it just resolved the client by id, so
    /// an identity check cannot add information there.
    pub fn remove(&mut self, id: u8) {
        self.by_id.remove(&id);
        self.records.remove(&id);
    }

    /// Send a superseded connection its farewell, in the order the
    /// teardown path established: `Leave` restores its input at once,
    /// `disconnect` ends its process — a replacement that lingers traps
    /// nobody (the fresh connection already owns the id).
    pub fn farewell_superseded(client: &Arc<Client>) {
        enqueue_client(client, Message::Leave { screen_id: client.id });
        enqueue_client(client, Message::Control { command: kvmshare_protocol::id::control::DISCONNECT });
    }

    /// Note one cursor-stream datagram from `from` for client `id`.
    ///
    /// Learns or verifies the datagram's source **port** (the IP was
    /// already matched against the handshake by the receiver): the port
    /// is learned from the client's first datagram and can legitimately
    /// change when the client reconnects. Either way the old sequence
    /// space belongs to the old address, so it is reset and the new
    /// source is adopted — without this, a late frame from a dead
    /// session re-created the tracker at its old high value and every
    /// fresh beacon (starting at 1) was judged stale.
    pub fn hear_from(&mut self, id: u8, from: SocketAddr) {
        let Some(record) = self.records.get_mut(&id) else {
            // No record means no client: the handshake's identity check
            // upstream makes this unreachable; ignore rather than invent
            // state for a client that does not exist.
            return;
        };
        match record.udp_addr {
            None => {
                record.udp_addr = Some(from);
                log_debug!("client {id} registered UDP stream from {from}");
            }
            Some(addr) if addr != from => {
                record.seq = 0;
                record.udp_addr = Some(from);
                log_debug!("client {id} re-registered UDP stream from {from}");
            }
            Some(_) => {}
        }
        record.last_heard = Some(crate::time::now_ms());
    }

    /// The cursor-stream target address of client `id`, when its stream
    /// has registered (the writer sends motion frames here).
    pub fn addr_of(&self, id: u8) -> Option<SocketAddr> {
        self.records.get(&id).and_then(|r| r.udp_addr)
    }

    /// The TCP peer IP the handshake came from, for datagram
    /// authentication.
    pub fn tcp_ip_of(&self, id: u8) -> Option<IpAddr> {
        self.records.get(&id).map(|r| r.tcp_ip)
    }

    /// The highest applied cursor-stream sequence, for stale rejection.
    pub fn seq_of(&self, id: u8) -> Option<u32> {
        self.records.get(&id).map(|r| r.seq)
    }

    /// Record a cursor-stream frame as applied.
    pub fn advance_seq(&mut self, id: u8, seq: u32) {
        if let Some(record) = self.records.get_mut(&id) {
            record.seq = seq;
        }
    }

    /// When the cursor stream was last heard (monotonic ms), if at all.
    pub fn last_heard_of(&self, id: u8) -> Option<u64> {
        self.records.get(&id).and_then(|r| r.last_heard)
    }

    /// Reset the silence watchdog's clock, called the moment the session
    /// activates a client. Its beacons only flow while it is active, so
    /// without this the watchdog would judge it dead before the first
    /// beacon could arrive — dropping every crossing that followed an
    /// idle stretch. A stream that goes silent after this is a real
    /// wedge and still gets caught.
    pub fn mark_heard(&mut self, id: u8) {
        match self.records.get_mut(&id) {
            Some(record) => record.last_heard = Some(crate::time::now_ms()),
            None => log_warn!("peer {id} activated without a transport record"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::client::Client;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::mpsc::sync_channel;
    use std::sync::Mutex;

    fn test_client(id: u8, machine_id: &str) -> (Arc<Client>, std::sync::mpsc::Receiver<crate::server::client::Outbound>) {
        let (tx, rx) = sync_channel(16);
        (
            Arc::new(Client {
                id,
                name: format!("client-{id}"),
                machine_id: machine_id.into(),
                since_ms: 0,
                out: tx,
                audio: Mutex::new(None),
            }),
            rx,
        )
    }

    /// Registration replaces a stale connection atomically and hands the
    /// old handle back for the farewell.
    #[test]
    fn registration_replaces_the_stale_holder_of_an_id() {
        let (old, _old_rx) = test_client(1, "a");
        let (fresh, _rx) = test_client(1, "a");
        let mut peers = Peers::default();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        peers.register(1, old.clone(), ip);
        let replaced = peers.register(1, fresh, ip).unwrap();
        assert!(Arc::ptr_eq(&replaced, &old));

        // The stale handle may no longer unregister or effect anything:
        // the fresh registration owns the id.
        assert!(!peers.unregister(1, &old));
        assert!(peers.get(1).is_some());
        assert_eq!(peers.len(), 1);
    }

    /// Only the current holder unregisters.
    #[test]
    fn only_the_current_holder_unregisters() {
        let (client, _rx) = test_client(2, "b");
        let mut peers = Peers::default();
        peers.register(2, client.clone(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(peers.unregister(2, &client));
        assert!(peers.is_empty());
        // A second unregister is a no-op, not a panic and not a
        // different client's rows.
        assert!(!peers.unregister(2, &client));
    }

    /// The record's fields live and die with the client.
    #[test]
    fn the_record_lives_and_dies_with_the_client() {
        let (client, _rx) = test_client(3, "c");
        let mut peers = Peers::default();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        peers.register(3, client.clone(), ip);
        assert_eq!(peers.tcp_ip_of(3), Some(ip));

        let from: SocketAddr = "127.0.0.1:5555".parse().unwrap();
        peers.hear_from(3, from);
        assert_eq!(peers.last_heard_of(3), Some(peers.last_heard_of(3).unwrap()));
        peers.advance_seq(3, 7);
        assert_eq!(peers.seq_of(3), Some(7));

        assert!(peers.unregister(3, &client));
        assert_eq!(peers.tcp_ip_of(3), None);
        assert_eq!(peers.seq_of(3), None);
        assert_eq!(peers.last_heard_of(3), None);
    }

    /// A changed stream address resets the sequence space; the same
    /// address just updates liveness.
    #[test]
    fn a_new_stream_address_resets_the_sequence() {
        let (client, _rx) = test_client(4, "d");
        let mut peers = Peers::default();
        peers.register(4, client, IpAddr::V4(Ipv4Addr::LOCALHOST));
        let a: SocketAddr = "127.0.0.1:1111".parse().unwrap();
        let b: SocketAddr = "127.0.0.1:2222".parse().unwrap();
        peers.hear_from(4, a);
        peers.advance_seq(4, 50);
        peers.hear_from(4, b);
        assert_eq!(peers.seq_of(4), Some(0), "the old sequence space belongs to the old address");
        peers.hear_from(4, b);
        // Still registered, still tracking.
        assert!(peers.seq_of(4).is_some());
    }

    /// The machine-id lookup accepts the short form in both directions,
    /// like the trust policy.
    #[test]
    fn machine_ids_match_by_prefix() {
        let (client, _rx) = test_client(5, "98980a4d9afac273");
        let mut peers = Peers::default();
        peers.register(5, client, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(peers.solve_machine_id("98980a4d"), Some(5));
        assert_eq!(peers.solve_machine_id("98980a4d9afac273"), Some(5));
        assert_eq!(peers.solve_machine_id("nope"), None);
    }

    /// The watchdog clock is armed on activation, present or not in the
    /// record yet (an activated client always has a record — but the
    /// missing-record path must not panic).
    #[test]
    fn mark_heard_is_safe_without_a_record() {
        let mut peers = Peers::default();
        peers.mark_heard(9); // no client registered: warn, no panic
        assert_eq!(peers.last_heard_of(9), None);
    }
}
