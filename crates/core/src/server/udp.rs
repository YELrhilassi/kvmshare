//! The UDP cursor stream: one receiver thread that learns each client's
//! address, routes real-cursor beacons to the session (dropping stale
//! frames by sequence number), and executes any crossing a beacon fires.

use std::io;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use kvmshare_log::{log_debug, log_warn};
use kvmshare_protocol::message::Message;

use crate::server::actions::apply_action;
use crate::server::client::ClientCtx;
use crate::time::now_ms;
use crate::udp;

/// How long the active client's cursor stream may go silent before the
/// server drops it. The client beacons every ~8 ms while active, so this
/// is generous — a healthy stream can never trip it, and a genuinely
/// wedged client (its motion loop stuck, even though TCP keepalives
/// still flow) is caught in about a second. The drop returns control
/// home instead of leaving the cursor stranded on a client that cannot
/// move it.
const ACTIVE_BEACON_TIMEOUT: Duration = Duration::from_millis(1500);

/// Drop the active client when its cursor stream has been silent for
/// [`ACTIVE_BEACON_TIMEOUT`]. Called from the UDP receiver whenever the
/// stream is quiet. Mirrors the TCP silence drop in the reader thread —
/// but catches what that cannot: a client whose motion loop is wedged
/// while its control thread (and keepalives) are still alive.
fn check_active_beacon_staleness(ctx: &ClientCtx) {
    let active = *ctx.active.lock().unwrap();
    let Some(id) = active else { return };
    let now = now_ms();
    let last = ctx.last_heard.lock().unwrap().get(&id).copied();
    let Some(last) = last else { return };
    if now.saturating_sub(last) <= ACTIVE_BEACON_TIMEOUT.as_millis() as u64 {
        return;
    }
    log_warn!(
        "client {id}: cursor stream silent for {ACTIVE_BEACON_TIMEOUT:?} while active — dropping so control returns home"
    );
    // The reader thread's normal teardown path does the unregister +
    // return-home; triggering it from here (a forced disconnect) is the
    // same idempotent cleanup.
    let name = {
        let clients = ctx.clients.lock().unwrap();
        clients.get(&id).map(|c| c.name.clone()).unwrap_or_default()
    };
    ctx.teardown(id, &name);
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
                let Some(d) = udp::unpack(&buf[..n]) else { continue };
                // Only datagrams from a known client count. Learning the
                // address happens here too — the first datagram is the
                // registration the client sends right after the
                // handshake.
                if !ctx.clients.lock().unwrap().contains_key(&d.id) {
                    continue;
                }
                // Any datagram from a known client proves its cursor
                // stream is alive (beacons flow continuously while it is
                // active). Tracked for the staleness watchdog above.
                ctx.last_heard.lock().unwrap().insert(d.id, now_ms());
                // Learn or verify the datagram's source. The first
                // datagram from a client teaches us its address; a
                // datagram from a *different* address is either a stale
                // frame from a previous session (still draining the
                // socket buffer after a disconnect) or a fresh
                // registration from a reconnect. Either way the old
                // sequence space belongs to the old address — reset it
                // and adopt the new source. Without this, a late frame
                // from a dead session re-creates the seq tracker at its
                // high value and every fresh beacon (starting at 1) is
                // judged stale: the live session is deafened.
                {
                    let mut addrs = ctx.addrs.lock().unwrap();
                    match addrs.get(&d.id) {
                        None => {
                            addrs.insert(d.id, from);
                            log_debug!("client {} registered UDP stream from {from}", d.id);
                        }
                        Some(addr) if *addr != from => {
                            ctx.seqs.lock().unwrap().remove(&d.id);
                            addrs.insert(d.id, from);
                            log_debug!("client {} re-registered UDP stream from {from}", d.id);
                        }
                        Some(_) => {}
                    }
                }
                match d.msg {
                    Message::CursorPos { x, y } => {
                        // Stale or duplicate beacons are dropped: a late
                        // "at the wall" report must never arm a crossing.
                        {
                            let mut seqs = ctx.seqs.lock().unwrap();
                            let last = seqs.entry(d.id).or_default();
                            if !udp::is_newer(d.seq, *last) {
                                continue;
                            }
                            *last = d.seq;
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
                                    if let Err(e) =
                                        apply_action(a, &ctx.active, &ctx.clients, &ctx.last_heard, &mut engine)
                                    {
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
            // Non-blocking socket: WouldBlock is the normal idle state,
            // not an error — yield briefly and check again. The sleep is
            // kept short so a beacon parked at a wall is answered within
            // ~1 ms (crossing latency is invisible at that scale) while
            // the thread still yields the CPU when nothing is flowing.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                check_active_beacon_staleness(&ctx);
                thread::sleep(Duration::from_millis(1));
            }
            Err(e) => {
                log_warn!("udp receiver: {e}");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}