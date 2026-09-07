//! The client side.
//!
//! Connects to a server over **two links** (see [`crate::server`] for
//! the server's half of the contract): a reliable TCP control channel
//! (layout, Enter/Leave, buttons, keys, wheel, clipboard, keepalive) and
//! a loss-tolerant UDP cursor stream (motion in, real-cursor beacons
//! out). The cursor is steered by a **closed loop**
//! ([`PositionFollower`]): every received motion frame advances a
//! commanded position, and the motion thread places the real cursor on
//! the command each tick (for absolute backends) or corrects toward it
//! (relative backends). There is no replay queue, so no backlog can ever
//! form. All OS-specific work lives behind the [`Injector`] trait; this
//! module is plain message dispatch and thread wiring, tested with a
//! fake injector.
//!
//! ## Layout
//!
//! * [`injector`] — the platform hooks (injection + clipboard).
//! * [`shared`] — state shared by the worker threads and the steering
//!   state.
//! * [`supervisor`] — the watchdog that recovers a wedged worker.
//! * [`dispatch`] — applies one server message to the local machine.
//! * [`threads`] — the motion, UDP and sync worker loops.

mod dispatch;
mod injector;
mod shared;
mod supervisor;
mod threads;

use std::collections::VecDeque;
use std::io;
use std::net::{TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use kvmshare_log::{log_error, log_warn};
use kvmshare_protocol::message::{Layout, Message, ScreenInfo};

use crate::motion::{MotionProbe, PositionFollower};
use crate::time::now_ms;
use crate::transport::{RecvResult, Transport};
use crate::udp;

use shared::{MotionState, Shared};

pub use injector::{Clipboard, Injector};

/// How often the client sends a keepalive when idle.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(2);
/// How long the TCP read can block. The control channel is quiet, and
/// nothing on it has a deadline tighter than this: motion lives on its
/// own thread and its own socket.
const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// A connected client. The transport is owned by the TCP thread (the
/// thread that called [`Client::run`]); the cursor stream socket moves
/// into [`Shared`] when `run` starts.
#[derive(Debug)]
pub struct Client {
    transport: Transport,
    /// The id this machine has in the server's layout.
    own_id: u8,
    /// The full layout as last sent by the server (includes the server's
    /// own screen). Clients mostly ignore it today; it exists so future
    /// features have the data they need.
    layout: Layout,
    /// The cursor stream socket; handed to [`Shared`] when `run` starts
    /// (motion and UDP threads share it).
    udp: UdpSocket,
}

impl Client {
    /// Connect to `addr`, say hello with `name`, and wait for the server's
    /// welcome. Opens the UDP cursor stream and registers it with the
    /// server. Returns the client ready to run.
    pub fn connect(addr: &str, name: &str, info: ScreenInfo) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        let mut transport = Transport::with_read_timeout(stream, Some(READ_TIMEOUT))?;
        transport.send(&Message::Hello { version: kvmshare_protocol::VERSION, name: name.to_owned(), info })?;

        let (own_id, layout) = match transport.recv()? {
            RecvResult::Msg(Message::Welcome { server_version, layout, own_screen_id }) => {
                if server_version != kvmshare_protocol::VERSION {
                    return Err(io::Error::other(format!(
                        "server speaks v{server_version}, client speaks v{}",
                        kvmshare_protocol::VERSION
                    )));
                }
                (own_screen_id, layout)
            }
            RecvResult::Msg(Message::Error { code, text }) => Err(io::Error::other(format!(
                "server rejected the connection ({code}): {text}"
            )))?,
            other => return Err(io::Error::other(format!("unexpected first message: {other:?}"))),
        };

        // The cursor stream: one UDP socket, connected to the server's
        // address. The UDP thread blocks on `recv` with a short timeout
        // (frames wake it immediately, so the kernel receive buffer is
        // drained at full speed and only extreme bursts can drop a
        // frame — motion is loss-tolerant, the next frame self-heals).
        let udp = UdpSocket::bind(("0.0.0.0", 0))?;
        udp.set_read_timeout(Some(threads::UDP_RECV_TIMEOUT))?;
        udp.connect(addr)?;
        // First datagram = registration: it carries the client id, so the
        // server learns both who we are and where to send motion.
        udp.send(&udp::pack(own_id, 0, &Message::KeepAlive))?;

        Ok(Self { transport, own_id, layout, udp })
    }

    /// Run the client until the connection closes.
    ///
    /// Spawns the motion, UDP and sync worker threads, then services the
    /// TCP control channel on the calling thread. When the link closes
    /// the workers are stopped and joined, and the cursor is placed once
    /// more at the final command so the last position is exact.
    ///
    /// `outbox` lets the app layer push messages to the server (future
    /// control messages). Drained on the TCP thread.
    pub fn run(
        self,
        injector: Box<dyn Injector>,
        clipboard: Box<dyn Clipboard>,
        outbox: &Receiver<Message>,
    ) -> io::Result<()> {
        // Destructure so each field is owned independently — `udp` moves
        // into [`Shared`] while `transport` stays on this thread.
        let Client { mut transport, own_id, layout, udp } = self;
        let mut layout = layout;
        let shared = Arc::new(Shared {
            injector: Mutex::new(injector),
            clipboard: Mutex::new(clipboard),
            motion: Mutex::new(MotionState {
                follower: PositionFollower::default(),
                probe: MotionProbe::default(),
                frames_win: 0,
                ticks_win: 0,
                win_cmd_px: 0,
                win_real_start: (0, 0),
                pin_windows: 0,
            }),
            active: AtomicBool::new(false),
            wake_lock: Mutex::new(()),
            wake_cv: Condvar::new(),
            udp,
            udp_seq: AtomicU32::new(1),
            stop: AtomicBool::new(false),
            motion_tick_ms: AtomicU64::new(0),
            tcp_tick_ms: AtomicU64::new(0),
            events: Mutex::new(VecDeque::new()),
        });
        // The sync thread hands messages (resolution changes, clipboard
        // uploads) to the TCP thread over this channel.
        let (sync_tx, sync_rx) = mpsc::channel::<Message>();

        let sync = thread::Builder::new()
            .name("kvmshare-client-sync".into())
            .spawn({ let s = shared.clone(); move || threads::sync_loop(s, sync_tx) })
            .expect("cannot spawn client sync thread");
        let udp_thread = thread::Builder::new()
            .name("kvmshare-client-udp".into())
            .spawn({ let s = shared.clone(); move || threads::udp_loop(s, own_id) })
            .expect("cannot spawn client udp thread");
        let motion = thread::Builder::new()
            .name("kvmshare-client-motion".into())
            .spawn({ let s = shared.clone(); move || threads::motion_loop(s, own_id) })
            .expect("cannot spawn client motion thread");
        // The supervisor: a watchdog that can never be blocked by
        // whatever wedges the workers, because it shares nothing with
        // them but two atomics. If a worker stalls while this machine is
        // being controlled, it force-restores local input and ends the
        // session so the reconnect path starts a clean client — a wedged
        // client must never leave this machine's mouse and keyboard
        // trapped. It also logs the stall so the root cause is visible
        // on the next occurrence instead of a mystery freeze.
        let supervisor = thread::Builder::new()
            .name("kvmshare-client-supervisor".into())
            .spawn({ let s = shared.clone(); move || supervisor::supervisor_loop(s) })
            .expect("cannot spawn client supervisor thread");

        // TCP control channel on this thread. The read timeout is
        // constant — motion no longer needs the control loop to wake at
        // motion cadence.
        let mut last_keepalive = Instant::now();
        loop {
            // A recovery path (the supervisor or the cursor-pin detector)
            // can ask the session to end from another thread; the read
            // timeout bounds this loop's wake so the request is seen
            // within ~100 ms even when the link is silent.
            if shared.stop.load(Ordering::Relaxed) {
                break;
            }
            // The machine just woke from sleep: the pre-sleep control
            // state (cursor on this machine, hardware silenced, cursor
            // hidden) is stale — end the session so this machine returns
            // to its user and the reconnect starts fresh and local.
            // (The platform backend clears the flag; a healthy session
            // never sees it.)
            if shared.injector.lock().unwrap().system_resumed() {
                log_warn!("system resumed (sleep/wake) — ending the session so this machine returns to local control");
                break;
            }
            // The UAC secure desktop (Windows): input is being shown a
            // protected desktop no process can inject into, and the
            // person at this machine must be able to answer the prompt.
            // The isolation pump already released local input the moment
            // it appeared; end the session so control returns home and
            // this machine is fully back with its user.
            if shared.injector.lock().unwrap().secure_desktop_active() {
                log_warn!("Windows secure desktop detected (UAC prompt) — ending the session so control returns home and the prompt can be answered");
                break;
            }
            match transport.recv()? {
                RecvResult::Msg(msg) => {
                    shared.tcp_tick_ms.store(now_ms(), Ordering::Relaxed);
                    dispatch::dispatch(&mut layout, &shared, own_id, msg);
                }
                RecvResult::Eof => break,
                RecvResult::NoData => {
                    shared.tcp_tick_ms.store(now_ms(), Ordering::Relaxed);
                    // App-layer outbox, slow-path traffic from the sync
                    // thread, and keepalives to keep the link warm. All
                    // cheap sends; nothing here can block for long.
                    while let Ok(msg) = outbox.try_recv() {
                        transport.send(&msg)?;
                    }
                    while let Ok(msg) = sync_rx.try_recv() {
                        transport.send(&msg)?;
                    }
                    if last_keepalive.elapsed() >= KEEPALIVE_INTERVAL {
                        transport.send(&Message::KeepAlive)?;
                        last_keepalive = Instant::now();
                    }
                }
            }
        }

        // Session over: stop the workers, then place the cursor once more
        // at the final command so the last position is exact. Joins are
        // bounded: a worker wedged on an OS call must not hang the
        // reconnect loop (the supervisor already released local input for
        // that case).
        shared.stop.store(true, Ordering::Relaxed);
        // The motion thread may be blocked in its idle wait — wake it so
        // the join below can complete.
        shared.wake_cv.notify_all();
        Self::join_bounded(motion, "motion");
        Self::join_bounded(udp_thread, "udp");
        Self::join_bounded(sync, "sync");
        let _ = supervisor.join();
        threads::place_at_command(&shared);
        Ok(())
    }

    /// Join a worker thread with a grace period; a thread that has not
    /// exited by then is abandoned (dropping the handle detaches it).
    /// Wedged workers hold only their own locks — the fresh session's
    /// threads do not share them — so abandoning is safe and the machine
    /// is never held hostage by a stuck join.
    fn join_bounded(handle: thread::JoinHandle<()>, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !handle.is_finished() {
            thread::sleep(Duration::from_millis(20));
        }
        if !handle.is_finished() {
            log_error!("client {name} thread did not exit after stop — abandoning it");
            drop(handle);
        } else {
            let _ = handle.join();
        }
    }

    pub fn own_id(&self) -> u8 {
        self.own_id
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;