//! The device enumerator: opens `/dev/input` on demand and hands fresh
//! device lists to the reader over a channel.
//!
//! The enumerator is **event-driven**: it watches `/dev/input` with
//! inotify and only re-opens the devices when something actually changed
//! (a device plugged in or removed). A long fallback scan (every
//! [`FALLBACK_PERIOD`]) catches what inotify cannot see — a permission
//! change on an existing device (the installer's access grant), an
//! inotify queue overflow, or an environment without inotify — at a
//! cadence far too slow to matter (one ~150 ms pass per 30 s).
//!
//! Everything that can block (the device opens themselves) happens on
//! this thread. The reader never waits on it: it applies fresh lists
//! when woken, and a slow open (wedged driver) delays new devices,
//! never the cursor stream.

use std::collections::HashSet;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use kvmshare_log::{log_debug, log_warn};

use crate::evdev::device::{open_devices, Opened};

/// How long the enumerator blocks between scans when inotify reports
/// nothing. Long by design: hot-plug is signalled by inotify
/// immediately, so this timer only exists as a fallback for changes
/// inotify cannot see. One open pass per 30 s is negligible.
const FALLBACK_PERIOD: Duration = Duration::from_secs(30);
/// inotify events that mean the device set changed and a re-scan is
/// warranted. `IN_Q_OVERFLOW` (0x4000) is handled separately — its
/// struct has no name and the flag alone means \"something happened\".
const WATCH_MASK: u32 = libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO;

/// Start the enumerator thread. Wakes the reader (via `wake`, the shared
/// wake pipe) after every hand-off, so a fresh list is applied exactly
/// when it arrives. The thread is detached: if a device open wedges for
/// a long time, the reader simply keeps its current devices until the
/// next pass completes.
pub(crate) fn spawn_enumerator(
    enum_tx: Sender<(Vec<Opened>, bool)>,
    known_paths: Arc<Mutex<HashSet<std::path::PathBuf>>>,
    wake: UnixStream,
) {
    thread::Builder::new()
        .name("kvmshare-evdev-enum".into())
        .spawn(move || {
            let mut inotify: Option<OwnedFd> = None;
            let mut watching = false;
            let mut buf = [0u8; 4096];
            loop {
                // (Re-)establish the watch when it is missing: the fd
                // init or the directory watch can fail when /dev/input
                // does not exist yet, and both are retried here.
                if inotify.is_none() || !watching {
                    match init_watch(&mut inotify) {
                        Ok(()) => watching = true,
                        Err(()) => {
                            // No watch yet: fall back to the timer and
                            // try again next cycle.
                            thread::sleep(FALLBACK_PERIOD);
                            scan_and_send(&enum_tx, &known_paths, &wake);
                            continue;
                        }
                    }
                }
                let fd = inotify.as_ref().expect("watch established").as_raw_fd();
                let mut pfd = [libc::pollfd { fd, events: libc::POLLIN, revents: 0 }];
                // SAFETY: pfd is a single valid pollfd backed by the
                // inotify fd, which lives for the call.
                let ready = unsafe { libc::poll(pfd.as_mut_ptr(), 1, FALLBACK_PERIOD.as_millis() as i32) };
                if ready < 0 {
                    // EINTR or a transient error: pause and retry.
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                if ready == 0 {
                    // Fallback timer: something may have changed that
                    // inotify cannot see (permissions). Scan.
                    scan_and_send(&enum_tx, &known_paths, &wake);
                    continue;
                }
                if pfd[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
                    continue;
                }
                // Drain every queued inotify event; any that means
                // \"devices changed\" warrants a scan.
                let mut changed = false;
                loop {
                    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
                    if n <= 0 {
                        if n < 0 {
                            let err = std::io::Error::last_os_error();
                            if err.kind() == std::io::ErrorKind::WouldBlock {
                                break;
                            }
                            // A real read error: be safe and scan.
                            changed = true;
                        }
                        break;
                    }
                    changed |= parse_inotify(&buf[..n as usize]);
                }
                if changed {
                    scan_and_send(&enum_tx, &known_paths, &wake);
                }
            }
        })
        .ok();
}

/// Create the inotify fd and watch `/dev/input` for device changes.
/// Returns Ok(()) once both are in place. Failures (no inotify, no
/// `/dev/input` yet) are the caller's signal to fall back to the timer.
fn init_watch(inotify: &mut Option<OwnedFd>) -> Result<(), ()> {
    if inotify.is_none() {
        // SAFETY: inotify_init1 with valid flags; the result is checked
        // and wrapped in OwnedFd immediately.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(());
        }
        // SAFETY: fd is a valid, owned, nonblocking inotify fd.
        *inotify = Some(unsafe { OwnedFd::from_raw_fd(fd) });
    }
    let fd = inotify.as_ref().expect("just initialized").as_raw_fd();
    let path = std::ffi::CString::new("/dev/input").map_err(|_| ())?;
    // SAFETY: a NUL-terminated path and the inotify fd; the return is
    // checked.
    let wd = unsafe { libc::inotify_add_watch(fd, path.as_ptr(), WATCH_MASK) };
    if wd < 0 {
        return Err(());
    }
    Ok(())
}

/// Parse a buffer of inotify events; returns whether any of them means
/// \"devices changed\" (a create/delete/move, or a queue overflow).
fn parse_inotify(buf: &[u8]) -> bool {
    // struct inotify_event { wd: i32, mask: u32, cookie: u32, len: u32, name: u8[] }
    let mut off = 0usize;
    let mut changed = false;
    while off + 16 <= buf.len() {
        let mask = u32::from_ne_bytes([buf[off + 4], buf[off + 5], buf[off + 6], buf[off + 7]]);
        let len = u32::from_ne_bytes([buf[off + 12], buf[off + 13], buf[off + 14], buf[off + 15]]) as usize;
        if mask & (WATCH_MASK | libc::IN_Q_OVERFLOW) != 0 {
            changed = true;
        }
        // The kernel pads the name to 4-byte alignment; `len` includes
        // that padding.
        off += 16 + len;
    }
    changed
}

/// One full scan: open every pointer/keyboard in `/dev/input` and hand
/// the fresh list to the reader, then wake it so the list is applied
/// promptly. Runs on this thread only — never on the reader's.
fn scan_and_send(
    enum_tx: &Sender<(Vec<Opened>, bool)>,
    known_paths: &Mutex<HashSet<std::path::PathBuf>>,
    wake: &UnixStream,
) {
    // Diagnostic: a slow pass is now harmless to the cursor stream, but
    // still worth knowing about.
    let t0 = Instant::now();
    let known = known_paths.lock().unwrap().clone();
    let (fresh, denied) = open_devices(&known);
    let took = t0.elapsed();
    // A pass is normally 100-200 ms (the device opens) and runs off the
    // motion thread, so it is not worth a WARN on every cadence — that
    // was log spam. Only a genuinely pathological pass (a wedged device
    // open) rises to WARN.
    if took > Duration::from_millis(400) {
        log_warn!("evdev: re-enumeration took {took:?} (off the motion thread)");
    } else if took > Duration::from_millis(20) {
        log_debug!("evdev: re-enumeration took {took:?}");
    }
    if enum_tx.send((fresh, denied)).is_err() {
        return; // reader gone
    }
    // Nudge the reader out of poll so the fresh list is applied now.
    let mut w = wake;
    let _ = w.write(&[1]);
}