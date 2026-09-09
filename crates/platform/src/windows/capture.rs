//! Local input capture via Win32 **low-level hooks** (`WH_MOUSE_LL` /
//! `WH_KEYBOARD_LL`).
//!
//! A hidden message-only window anchors the capture thread's message
//! loop; the two hooks are installed on that same thread, so their
//! procedures run on the pump thread and every system mouse/keyboard
//! event crosses them. The hook procedures forward what matters as
//! protocol [`Message`]s — and, while the cursor is on a client,
//! swallow the events so the local desktop never sees them.
//!
//! ## Why hooks, not Raw Input + `BlockInput`
//!
//! The server's isolation and its capture must work **at the same
//! time**: while the cursor is on a client, the session forwards the
//! user's physical input to the client *and* the local desktop must not
//! react to it. Two candidate mechanisms fail that requirement, for
//! opposite reasons, and both were proven on real hardware:
//!
//! * **`BlockInput`** (the earlier design) discards physical input at
//!   the system level — and with it the raw input (`WM_INPUT`) the
//!   capture depended on. The crossing fired, isolation armed, and the
//!   capture went deaf: the client cursor froze at its entry point and
//!   nothing could bring it home (the local cursor was pinned and the
//!   raw stream that carries the return push never arrived).
//! * **Hooks that swallow** suppress raw input the same way (a probe
//!   counting `WM_INPUT` while a hook returned 1 saw 2 messages for 25
//!   injected moves). But the hook procedures themselves still see
//!   every event — 25/25 — and the hook's `pt` field carries the
//!   position the cursor *would* move to, unclamped. So a wall-pinned
//!   or swallowed cursor keeps reporting its push: `delta = pt −
//!   GetCursorPos()` preserves the full magnitude of every move,
//!   including the outward pushes that fire a crossing and the motion
//!   that steers a client.
//!
//! So the capture is hook-driven: one path that works identically
//! whether the events pass through (cursor home) or are swallowed
//! (cursor on a client). The deltas it forwards are the *effective*
//! movement the cursor would have made (post-acceleration), which is
//! exactly what the session's pointer-gain model measures and forwards
//! (the measured gain converges to 1.0 and the client cursor moves like
//! the server's visible cursor would).
//!
//! ## Position beacons
//!
//! Hooks carry only *deltas* — the session's boundary model needs the
//! **real** cursor position too (see `core::session`: a crossing is
//! armed by a beacon placing the visible cursor on a screen wall, and
//! deltas alone must never fire one). The capture therefore polls
//! `GetCursorPos` on a timer ([`BEACON_MS`]) and forwards each *changed*
//! position as a [`Message::MouseMoveAbs`] beacon. While the cursor is
//! on a client it is hidden and pinned at the wall, so the poll reports
//! the same position every time and goes quiet — exactly the mirror of
//! the X11 backend, where the pointer pinned at a wall stops producing
//! motion events.
//!
//! ## Isolation and the escape hatch
//!
//! Isolation is a single atomic flag flipped by the engine on crossing:
//! while set, the hook procedures return 1 (swallow) for every event,
//! so the local desktop is inert but the capture still sees and
//! forwards everything. While the cursor is away, **Scroll Lock** is the
//! universal "come home" key: a press is consumed and turned into
//! [`Message::Escape`] (the session forces control home); its release is
//! swallowed too, so no stray key-up reaches the client. At home the key
//! passes through untouched.
//!
//! The hooks die with the process or the installing thread — a crash or
//! a wedged capture (which the server supervisor detects and exits from)
//! releases local input automatically, so this machine can never be left
//! input-trapped.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::thread;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use kvmshare_log::{log_error, log_info};
use kvmshare_protocol::message::{KeyKind, Message};

use super::buttons;

/// Window class name for the hidden capture window.
const CAPTURE_CLASS: &[u16] = &[
    'K' as u16, 'V' as u16, 'M' as u16, 'S' as u16, 'H' as u16, 'A' as u16, 'R' as u16, 'E' as u16, 0,
];

/// How often the position beacon polls `GetCursorPos` (ms). Same order
/// as the X11 capture's coalesced beacons (~6 ms + poll slack): tight
/// enough that edge crossings arm promptly, sparse enough to stay
/// negligible. Beacons are only forwarded when the position *changed*, so
/// an idle desktop sends nothing.
const BEACON_MS: usize = 8;
/// The timer id for the position beacon poll.
const BEACON_TIMER: usize = 1;

/// `VK_PAUSE`. Pause arrives as scan code 0x45 (Num Lock's make code)
/// via the E1 prefix; the low-level hook exposes no E1 flag, so the key
/// is dropped by its virtual key code instead — it must never be
/// forwarded as Num Lock.
const VK_PAUSE: u32 = 0x13;

/// The set-1 scan code (and extended flag) of Scroll Lock — the escape
/// key, shared by every capture backend (`keys::ESCAPE_KEY_HID`).
const ESCAPE_SCAN: u16 = 0x46;
const ESCAPE_EXTENDED: bool = false;

// ---- state shared with the hook procedures --------------------------
//
// The hook procedures are free `extern "system"` functions (the shape
// `SetWindowsHookExW` requires), so the state they need lives in
// process statics. All of it is written before the hooks can fire (the
// capture thread installs the hooks only after the statics are
// populated) and the procedures run on exactly one thread — the capture
// thread that installed them — so no cross-thread contention exists in
// the steady state; the `Mutex` on the key set is future-proofing, not
// a hot path.

/// True while the cursor is on a client: the hook procedures swallow
/// every event so the local desktop is inert. Flipped by the engine on
/// crossing; read by the procedures on every event.
static ISOLATE: OnceLock<Arc<AtomicBool>> = OnceLock::new();

/// The outbound channel the hook procedures forward messages on.
static TX: OnceLock<Sender<Message>> = OnceLock::new();

/// Keys currently held down, as (scan code, extended flag). Auto-repeat
/// presses are deduplicated against it: only true down/up transitions
/// are forwarded, so a stuck key can never be left on the receiving
/// machine.
static KEYS_DOWN: LazyLock<Mutex<HashSet<(u16, bool)>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Set when an escape (Scroll Lock) press was consumed while the cursor
/// was away; the matching release is swallowed too (it was never
/// forwarded, so forwarding its release would leave the client with a
/// phantom key-up).
static ESCAPE_CONSUMED: AtomicBool = AtomicBool::new(false);

fn isolate() -> bool {
    ISOLATE.get().is_some_and(|f| f.load(Ordering::SeqCst))
}

fn hook_send(msg: Message) {
    if let Some(tx) = TX.get() {
        let _ = tx.send(msg);
    }
}

/// The mouse hook: capture motion/buttons/wheel, and swallow everything
/// while the cursor is on a client.
///
/// # Safety
///
/// `lparam` points at a `MSLLHOOKSTRUCT` owned by the system for the
/// duration of the call — the standard low-level-hook contract.
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: valid MSLLHOOKSTRUCT pointer while the hook runs.
        let info = unsafe { &*(lparam as *const wm::MSLLHOOKSTRUCT) };
        let msg = wparam as u32;
        if msg == wm::WM_MOUSEMOVE {
            // Effective motion. The hook's `pt` is where the cursor
            // WOULD be for this event (unclamped); `GetCursorPos` is
            // where it actually is (the event is not applied yet). The
            // difference is the movement the hand caused — exact even
            // when the cursor is pinned at a wall (a crossing push) or
            // being swallowed (isolation), where the real cursor never
            // moves but `pt` keeps advancing with the push.
            // SAFETY: GetCursorPos writes one POINT into a valid buffer.
            let mut actual = POINT { x: 0, y: 0 };
            if unsafe { wm::GetCursorPos(&mut actual) } != 0 {
                let dx = info.pt.x - actual.x;
                let dy = info.pt.y - actual.y;
                if dx != 0 || dy != 0 {
                    hook_send(Message::MouseMoveRel { dx, dy });
                }
            }
        } else if let Some((button, pressed)) = buttons::from_wparam(msg, info.mouseData) {
            hook_send(Message::MouseButton { button, pressed });
        } else if msg == wm::WM_MOUSEWHEEL || msg == wm::WM_MOUSEHWHEEL {
            let notches = wheel_notches(info.mouseData);
            if notches != 0 {
                if msg == wm::WM_MOUSEWHEEL {
                    hook_send(Message::MouseWheel { dx: 0, dy: notches });
                } else {
                    hook_send(Message::MouseWheel { dx: notches, dy: 0 });
                }
            }
        }
        if isolate() {
            // Isolation: swallow. The local desktop must not act on
            // input the session is forwarding to a client.
            return 1;
        }
    }
    // SAFETY: standard hook chaining; a null handle is accepted.
    unsafe { wm::CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// The keyboard hook: capture key transitions (deduplicating auto-
/// repeat), consume the escape key while away, and swallow everything
/// while the cursor is on a client.
///
/// # Safety
///
/// `lparam` points at a `KBDLLHOOKSTRUCT` owned by the system for the
/// duration of the call.
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: valid KBDLLHOOKSTRUCT pointer while the hook runs.
        let info = unsafe { &*(lparam as *const wm::KBDLLHOOKSTRUCT) };
        let msg = wparam as u32;
        // Pause's E1 prefix is invisible to the hook; drop it by
        // virtual key code so it can never be mis-forwarded as Num Lock.
        if info.vkCode != VK_PAUSE {
            let extended = info.flags & wm::LLKHF_EXTENDED != 0;
            let is_down = matches!(msg, wm::WM_KEYDOWN | wm::WM_SYSKEYDOWN);
            let is_up = matches!(msg, wm::WM_KEYUP | wm::WM_SYSKEYUP);
            if is_down || is_up {
                let id = (info.scanCode as u16, extended);
                let mut down = KEYS_DOWN.lock().unwrap();
                let was_down = down.contains(&id);
                if is_down && !was_down {
                    down.insert(id);
                    if let Some(key) = crate::keys::hid_from_scancode(id.0, id.1) {
                        if isolate() && key == crate::keys::ESCAPE_KEY_HID {
                            // Scroll Lock while away: come home, consume
                            // the key (never forwarded).
                            log_info!("escape (Scroll Lock) pressed while remote — returning control home");
                            hook_send(Message::Escape);
                            ESCAPE_CONSUMED.store(true, Ordering::SeqCst);
                            drop(down);
                            return 1;
                        }
                        hook_send(Message::Key { kind: KeyKind::Down, key });
                    }
                } else if is_up && was_down {
                    down.remove(&id);
                    if id == (ESCAPE_SCAN, ESCAPE_EXTENDED) && ESCAPE_CONSUMED.swap(false, Ordering::SeqCst) {
                        // Release of a consumed escape: swallow it too.
                        drop(down);
                        return 1;
                    }
                    if let Some(key) = crate::keys::hid_from_scancode(id.0, id.1) {
                        hook_send(Message::Key { kind: KeyKind::Up, key });
                    }
                }
            }
        }
        if isolate() {
            return 1;
        }
    }
    // SAFETY: standard hook chaining.
    unsafe { wm::CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Create the hidden message-only capture window. `DefWindowProcW` is
/// the procedure; nothing needs a custom window proc (hook events and
/// `WM_TIMER` are handled directly in the message loop).
fn create_capture_window() -> Result<HWND, String> {
    // SAFETY: trivial kernel32 module lookup for the class registration.
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = wm::WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(wm::DefWindowProcW),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: CAPTURE_CLASS.as_ptr(),
    };
    // SAFETY: the class is fully initialized above.
    let atom = unsafe { wm::RegisterClassW(&class) };
    if atom == 0 {
        return Err("RegisterClassW failed".into());
    }
    // SAFETY: creating a message-only window (parent HWND_MESSAGE).
    let hwnd = unsafe {
        wm::CreateWindowExW(
            0,
            CAPTURE_CLASS.as_ptr(),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            wm::HWND_MESSAGE,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err("CreateWindowExW failed".into());
    }
    // Start the position-beacon poller (see the module docs).
    // SAFETY: hwnd is valid and owned by this thread's message loop.
    if unsafe { wm::SetTimer(hwnd, BEACON_TIMER as usize, BEACON_MS as u32, None) } == 0 {
        return Err("SetTimer failed".into());
    }
    Ok(hwnd)
}

/// Open input capture and start the background message loop.
///
/// Returns the channel the server's main loop reads local input from,
/// the capture liveness tick, and the shared isolation flag (the engine
/// flips it on crossing; the hook procedures read it on every event).
///
/// The hidden capture window, the timer and both hooks are created
/// **on the capture thread itself**, not on the caller's thread:
/// Windows posts window messages to the queue of the thread that *owns*
/// the window, and low-level hook procedures run on the thread that
/// installed them — both the pump thread here. Creating the window or
/// installing the hooks on the caller's thread would leave every
/// message and every hook call stranded in a thread that never pumps,
/// and the capture would block in `GetMessageW` forever, deaf to the
/// mouse. A one-shot handshake keeps startup errors synchronous.
pub fn start() -> Result<(Receiver<Message>, Arc<AtomicU64>, Arc<AtomicBool>), String> {
    let (tx, rx) = mpsc::channel();
    let (init_tx, init_rx) = mpsc::channel();
    let capture_tick = Arc::new(AtomicU64::new(0));
    let isolate = Arc::new(AtomicBool::new(false));
    // Populate the statics before the thread can run. `set` fails only
    // if a capture already exists in this process — one server per
    // process, so that never happens.
    let _ = TX.set(tx);
    let _ = ISOLATE.set(isolate.clone());
    let tick = capture_tick.clone();
    thread::spawn(move || {
        if let Err(e) = run_forever(init_tx, tick) {
            log_error!("input capture stopped: {e}");
        }
    });
    // Wait for the capture thread's create/install verdict. The window
    // is created immediately; a real desktop never delays this.
    match init_rx.recv() {
        Ok(Ok(())) => Ok((rx, capture_tick, isolate)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("input capture thread died before start".into()),
    }
}

/// The capture loop: create the hidden window and install both hooks
/// **on this thread** (see [`start`] for why the owner thread matters),
/// report the outcome over the start handshake, then block on the
/// message queue — which services the hooks — forwarding what matters.
/// Runs forever; returns only on a fatal error.
fn run_forever(init: Sender<Result<(), String>>, capture_tick: Arc<AtomicU64>) -> Result<(), String> {
    let hwnd = match create_capture_window() {
        Ok(h) => h,
        Err(e) => {
            let _ = init.send(Err(e.clone()));
            return Err(e);
        }
    };
    // SAFETY: GetModuleHandleW(null) returns this module's instance
    // handle; low-level hooks may point at a procedure in the current
    // module (the hook is called in this thread's context, not injected
    // into other processes).
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
    // SAFETY: both procs are `extern "system"` statics matching the
    // low-level-hook procedure shape the API requires.
    let mouse = unsafe { wm::SetWindowsHookExW(wm::WH_MOUSE_LL, Some(mouse_proc), hinstance, 0) };
    let keyboard = unsafe { wm::SetWindowsHookExW(wm::WH_KEYBOARD_LL, Some(keyboard_proc), hinstance, 0) };
    if mouse.is_null() || keyboard.is_null() {
        let err = format!(
            "input hooks failed to install (mouse: {}, keyboard: {})",
            mouse.is_null(),
            keyboard.is_null()
        );
        if !mouse.is_null() {
            // SAFETY: unhooking the handle this thread installed.
            unsafe { wm::UnhookWindowsHookEx(mouse) };
        }
        if !keyboard.is_null() {
            // SAFETY: unhooking the handle this thread installed.
            unsafe { wm::UnhookWindowsHookEx(keyboard) };
        }
        // SAFETY: destroying the window this thread created.
        unsafe { wm::DestroyWindow(hwnd) };
        let _ = init.send(Err(err.clone()));
        return Err(err);
    }
    log_info!("input capture started (low-level hooks)");
    let _ = init.send(Ok(()));
    // SAFETY: msg is a valid out-parameter; GetMessageW blocks until a
    // message arrives and returns 0 only on WM_QUIT (never posted
    // here), -1 on error. The hook procedures run inside these calls.
    let ret = unsafe {
        let mut msg = std::mem::zeroed::<wm::MSG>();
        loop {
            capture_tick.store(
                std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
                Ordering::Relaxed,
            );
            let r = wm::GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
            if r == 0 || r == -1 {
                break r;
            }
            if msg.message == wm::WM_TIMER && msg.wParam as usize == BEACON_TIMER {
                on_timer();
            } else {
                wm::TranslateMessage(&msg);
                wm::DispatchMessageW(&msg);
            }
        }
    };
    // SAFETY: unhooking the handles this thread installed, then
    // destroying the window it created.
    unsafe {
        wm::UnhookWindowsHookEx(mouse);
        wm::UnhookWindowsHookEx(keyboard);
        wm::DestroyWindow(hwnd);
    }
    if ret == -1 {
        return Err("GetMessageW failed".into());
    }
    Ok(())
}

/// The last beaconed cursor position (`None` = none yet). Only changes
/// are forwarded, so a still (or pinned) cursor goes quiet.
static LAST_BEACON: Mutex<Option<(i32, i32)>> = Mutex::new(None);

/// One beacon poll: forward the real cursor position when it moved
/// since the last poll. See the module docs for why the session needs
/// these and why changes only. Skipped while the cursor is on a client:
/// it is hidden and pinned, its position is meaningless to the session
/// (which ignores local beacons in Remote mode anyway), and the 
/// re-anchoring would fight the forwarded motion.
fn on_timer() {
    if isolate() {
        return;
    }
    let mut pt = POINT { x: 0, y: 0 };
    // SAFETY: GetCursorPos writes one POINT into a valid buffer.
    let got = unsafe { wm::GetCursorPos(&mut pt) } != 0;
    if !got {
        return;
    }
    let pos = (pt.x, pt.y);
    let mut last = LAST_BEACON.lock().unwrap();
    if *last == Some(pos) {
        return; // still (or pinned): quiet, exactly like X11 at a wall
    }
    *last = Some(pos);
    drop(last);
    hook_send(Message::MouseMoveAbs { x: pos.0, y: pos.1 });
}

/// Convert a wheel `mouseData` to protocol notch count. Windows reports
/// multiples of `WHEEL_DELTA` (120); the protocol uses 1 notch per
/// unit, matching the X11 backend.
///
/// The delta is the **signed high-order word** of the hook's
/// `mouseData` (the low-order word is reserved): masking the low word —
/// the natural first guess — reads the reserved zeroes and silently
/// drops every wheel event (the Windows-server wheel was dead in the
/// field; the Linux-server direction worked because its wheel travels as
/// button events, not a masked delta).
fn wheel_notches(data: u32) -> i32 {
    ((data >> 16) as u16 as i16 as i32) / 120
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;