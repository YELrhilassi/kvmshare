//! The UDP cursor stream: one receiver thread that learns each client's
//! address, routes real-cursor beacons to the session (dropping stale
//! frames by sequence number), and executes any crossing a beacon fires.
//!
//! Every per-client fact this thread reads or writes — the handshake IP
//! it authenticates against, the stream address, the sequence, the
//! last-heard clock — lives in the [`Peers`] store, so the receiver's
//! update is one operation on one lock instead of a careful dance
//! across four maps.

use std::io;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use kvmshare_log::log_warn;
use kvmshare_protocol::message::Message;

use crate::server::actions::apply_action;
use crate::server::client::ClientCtx;
use crate::session::Action;

/// How long the active client's cursor stream may go silent before the
/// server drops it. The client beacons every ~8 ms while active, so
/// even 5 s of silence is a genuinely wedged client (its motion loop
/// stuck while TCP keepalives still flow) — never a normal gap.
/// The floor is set by radio reality, not by the healthy path: Wi-Fi
/// power-save can blackhole unicast datagrams for one to two seconds
/// (an AP buffering frames for a dozing NIC), and a roam (AP to AP on
/// the same SSID) blacks them out for a few hundred milliseconds to
/// several seconds while the client keeps beaconing into the void. A
/// 1.5 s watchdog fired on exactly those events — the session died
/// *mid-use* whenever the laptop's radio hiccuped, dropping the cursor
/// home while the user was still moving it. 5 s rides out both; the
/// cost of the extra window is a wedged client holding an isolated
/// local machine a few seconds longer before the escape-key/teardown
/// paths (which already existed) recover it.
const ACTIVE_BEACON_TIMEOUT: u64 = 5000;

/// The UDP socket's read timeout: the receiver blocks in `recv_from` and
/// the OS wakes it at this cadence when the stream is idle, purely so
/// the beacon staleness watchdog runs. Datagrams wake it immediately —
/// this is a bound on idle wakes, not a poll.
pub const IDLE_TIMEOUT: Duration = Duration::from_millis(8);

/// Drop the active client when its cursor stream has been silent for
/// [`ACTIVE_BEACON_TIMEOUT`] ms. Called from the UDP receiver whenever
/// the stream is quiet. Mirrors the TCP silence drop in the reader
/// thread — but catches what that cannot: a client whose motion loop is
/// wedged while its control thread (and keepalives) are still alive.
fn check_active_beacon_staleness(ctx: &ClientCtx) {
    let active = *ctx.active.lock().unwrap();
    let Some(id) = active else { return };
    let last = ctx.peers.lock().unwrap().last_heard_of(id);
    let Some(last) = last else { return };
    let now = crate::time::now_ms();
    if now.saturating_sub(last) <= ACTIVE_BEACON_TIMEOUT {
        return;
    }
    log_warn!(
        "client {id}: cursor stream silent for {ACTIVE_BEACON_TIMEOUT} ms while active — dropping so control returns home"
    );
    // The reader thread's normal teardown path does the unregister +
    // return-home; triggering it from here (a forced disconnect) is the
    // same idempotent cleanup. The client's own Arc is passed so
    // teardown's identity check can confirm it is still the registered
    // one (a stale beacon watchdog must never evict a newer connection
    // that took over the id).
    let client = ctx.peers.lock().unwrap().get(id);
    match client {
        Some(client) => ctx.teardown(&client),
        None => {
            // The client is already gone (its teardown raced us, or a
            // crossing latched an id whose client never registered).
            // There is nothing to tear down, but the stale active latch
            // must still be released — otherwise every idle wake of the
            // receiver re-fires this branch forever, spamming the log,
            // while the local machine stays isolated with no client to
            // return to. Clear the latch and hand control home once.
            log_warn!("client {id}: cursor stream silent with no connected client — forcing control home");
            {
                let mut act = ctx.active.lock().unwrap();
                if *act == Some(id) {
                    *act = None;
                }
            }
            let action = ctx.session.lock().unwrap().on_client_disconnected(id);
            if let Action::SwitchToLocal { .. } = action {
                if let Ok(mut engine) = ctx.engine.lock() {
                    let _ = apply_action(action, &ctx.active, &ctx.peers, &mut engine, ctx.events.as_ref());
                }
            }
        }
    }
}

/// The UDP receiver: learns each client's address from its first
/// datagram, routes real-cursor beacons to the session (dropping stale
/// or duplicate frames by sequence number), and executes any crossing a
/// beacon fires — a beacon that parks the real cursor on a wall mid-push
/// must not wait for the next motion frame, which may never come (the
/// user stopped exactly at the wall).
pub fn udp_receiver(udp: Arc<std::net::UdpSocket>, ctx: Arc<ClientCtx>) {
    let mut buf = [0u8; 1500];
    loop {
        match udp.recv_from(&mut buf) {
            Ok((n, from)) => {
                let Some(d) = crate::udp::unpack(&buf[..n]) else { continue };
                // Only datagrams from a known client count — and the
                // client id in a datagram is just a claim; the **source
                // IP** is what the transport authenticates. A datagram is
                // accepted only when it (a) names a connected client and
                // (b) comes from the same IP that client's TCP handshake
                // came from. Anything else (a forged id from another
                // machine, an unconnected peer guessing ids) is dropped
                // before it can refresh liveness, (re)learn an address,
                // or feed a beacon to the session — a forged CursorPos
                // could otherwise drive edge crossings.
                {
                    let mut peers = ctx.peers.lock().unwrap();
                    let Some(known_ip) = peers.tcp_ip_of(d.id) else {
                        continue; // no such connected client
                    };
                    if from.ip() != known_ip {
                        // Not from the handshaked machine: a spoofed id
                        // (or a second NAT leg). Ignore silently — logging
                        // every probe would hand attackers a cheap oracle.
                        continue;
                    }
                    // Any surviving datagram proves this client's cursor
                    // stream is alive: learn/verify the source port (a
                    // new port resets the sequence space — see
                    // [`Peers::hear_from`]) and refresh liveness in the
                    // same operation.
                    peers.hear_from(d.id, from);
                }
                match d.msg {
                    Message::CursorPos { x, y } => {
                        // Stale or duplicate beacons are dropped: a late
                        // "at the wall" report must never arm a crossing.
                        {
                            let mut peers = ctx.peers.lock().unwrap();
                            let last = peers.seq_of(d.id).unwrap_or(0);
                            if !crate::udp::is_newer(d.seq, last) {
                                continue;
                            }
                            peers.advance_seq(d.id, d.seq);
                        }
                        // The client's *real* cursor position drives
                        // remote edge crossings. Session state is updated
                        // here; a crossing fires either on the next
                        // outward delta in the main loop or — when the
                        // beacon parks the cursor on a wall mid-push —
                        // right here, on the park itself.
                        let actions = { ctx.session.lock().unwrap().on_remote_beacon(d.id, x, y) };
                        if !actions.is_empty() {
                            if let Ok(mut engine) = ctx.engine.lock() {
                                for a in actions {
                                    if let Err(e) = apply_action(a, &ctx.active, &ctx.peers, &mut engine, ctx.events.as_ref()) {
                                        log_warn!("beacon crossing for client {}: {e}", d.id);
                                    }
                                }
                            }
                        }
                    }
                    // Registration frames and anything else that happens
                    // to ride UDP are acknowledged by existing; nothing
                    // to do here.
                    _ => {}
                }
            }
            // Blocking socket with an [`IDLE_TIMEOUT`] read timeout: the
            // OS sleeps for us — no busy polling. A timeout is the idle
            // state; run the staleness watchdog and wait again. A parked
            // beacon is answered within one timeout (~8 ms), which is
            // invisible as crossing latency.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
                check_active_beacon_staleness(&ctx);
            }
            Err(e) => {
                log_warn!("udp receiver: {e}");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
