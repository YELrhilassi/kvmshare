//! Native-input isolation while this machine's cursor is controlled
//! remotely (the Windows client's equivalent of the server's device
//! grab).
//!
//! A KVM client shows the *server's* desktop on this screen and injects
//! the server's cursor motion into it — but nothing stops this machine's
//! own hardware from moving the same cursor. A laptop's touchpad sitting
//! next to the shared mouse is a second, uncontrolled input source: a
//! brush of it while the cursor is being driven remotely shows up as
//! exactly the symptom that cursor-side fixes cannot cure — the cursor
//! stopping, stuttering and jumping to places the injected stream never
//! pointed at, because the native events reach the cursor outside our
//! loop with zero latency.
//!
//! The server never has this problem: it isolates its devices at the
//! kernel (evdev grab) while remote, so its own hardware cannot fight
//! the injected stream. Windows has no device grab, but it has the next
//! best thing — **low-level hooks** ([`SetWindowsHookExW`] with
//! `WH_MOUSE_LL` / `WH_KEYBOARD_LL`), which see every system mouse and
//! keyboard event before delivery and can veto it.
//!
//! ## The filter: hardware yes, software no
//!
//! Windows marks every event that travelled through [`SendInput`] with
//! the `LLMHF_INJECTED` / `LLKHF_INJECTED` flags. The hooks swallow
//! events *without* those flags — hardware-origin input — and pass
//! everything injected. That is the precise boundary:
//!
//! * our own injection (the shared cursor, forwarded keys) passes;
//! * software the user runs on this machine keeps working (an app
//!   synthesizing a click, the on-screen keyboard, automation) — those
//!   are injected too, and blocking them would break legitimate flows;
//! * only this machine's physical devices are silenced while it is
//!   being driven remotely — the same contract the server's grab
//!   enforces on its own hardware.
//!
//! ## Structure and cost
//!
//! A low-level hook procedure runs on the thread that installed it, so
//! that thread must pump messages. A dedicated background thread installs
//! both hooks once at startup and then parks in a message loop for the
//! life of the process. Control state is a single atomic flag flipped by
//! the injector's `enter`/`leave` — a crossing costs one store, and the
//! hooks themselves are always installed, so while control is *away*
//! (this machine used as a normal laptop) every input event costs the
//! hook thread one load and a pass-through call. That is invisible; the
//! alternative — installing and uninstalling hooks on every crossing —
//! would put work and failure modes at the exact moment that must stay
//! frictionless.
//!
//! [`SetWindowsHookExW`]: windows_sys::Win32::UI::WindowsAndMessaging::SetWindowsHookExW
//! [`SendInput`]: windows_sys::Win32::UI::Input::KeyboardAndMouse::SendInput

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

/// A window handle is a raw pointer, so it is not [`Send`] by default —
/// but it is an opaque token that is never dereferenced, so moving it
/// between threads is sound. Used to hand the pump thread's window
/// handle back to its owner for the quit signal.
#[derive(Clone, Copy)]
struct SendHwnd(HWND);
// SAFETY: HWND is an opaque handle value; nothing is dereferenced.
unsafe impl Send for SendHwnd {}

use kvmshare_log::{log_error, log_info, log_warn};

/// True while this machine's cursor is controlled remotely. Read by the
/// hook procedures on every system input event; flipped by the injector
/// on `enter`/`leave`. A single client injector per process makes a
/// static the honest shape for this state.
static ISOLATE: AtomicBool = AtomicBool::new(false);

/// The liveness heartbeat (ms since boot): bumped by the injector on
/// every motion tick while this machine is controlled. The watchdog
/// releases isolation when it goes stale — a wedged client (a blocked
/// lock, a stuck thread) must never leave this machine's hardware
/// silenced, or the user would be trapped on a screen that cannot move.
static LAST_STEER: AtomicU64 = AtomicU64::new(0);
/// Steering must stay fresh within this window (ms) while isolating,
/// or the watchdog releases local input.
const WATCHDOG_TIMEOUT_MS: u64 = 2000;
/// How often the pump thread re-checks the heartbeat (ms).
const WATCHDOG_PERIOD_MS: u32 = 500;

/// Milliseconds since the process started (monotonic, immune to clock
/// changes — `Instant::elapsed` on Windows uses the same tick source
/// `GetTickCount64` does). Shared by the injector (heartbeat) and the
/// pump thread (watchdog), so both must agree on the epoch; a process
/// static gives them exactly that.
static BOOT: OnceLock<std::time::Instant> = OnceLock::new();

fn ms_now() -> u64 {
    let boot = *BOOT.get_or_init(std::time::Instant::now);
    boot.elapsed().as_millis() as u64
}

/// Mouse events marked as injected (travelled through SendInput). Covers
/// both same-integrity events and lower-integrity-injected ones (which
/// set `LLMHF_LOWER_IL_INJECTED` in addition).
const LLMHF_INJECTED: u32 = 1;
/// Keyboard events marked as injected.
const LLKHF_INJECTED: u32 = 16;

/// Custom message posted to the pump thread's window to end its loop.
const QUIT_MSG: u32 = wm::WM_APP + 1;
/// Window class of the pump thread's hidden message-only window.
const ISOLATION_CLASS: &[u16] = &[
    'K' as u16, 'V' as u16, 'M' as u16, 'I' as u16, 'S' as u16, 'O' as u16, 0,
];

/// Owns the background hook thread. The hooks are installed once in
/// [`NativeIsolation::new`] and then stay live (passing everything
/// through) until the process exits; [`NativeIsolation::set_isolating`]
/// flips what they do as control enters and leaves this machine.
///
/// A **process singleton**: one pump thread and one hook pair for the
/// life of the process, shared by every injector. The client recreates
/// its injector on each reconnect — without the singleton every
/// reconnect would register a second hook window (whose class is already
/// registered), stack another pump thread, and chain another hook pair.
pub struct NativeIsolation {
    /// The pump thread, joined on drop after it is told to quit.
    thread: Option<JoinHandle<()>>,
    /// Receives the pump thread's window handle once it is ready. In a
    /// `Mutex` so the singleton can be shared (`Receiver` is not `Sync`).
    hwnd_rx: std::sync::Mutex<Option<Receiver<SendHwnd>>>,
}

/// The one isolation instance for this process (see the struct docs).
static ISOLATION: OnceLock<Arc<NativeIsolation>> = OnceLock::new();

impl NativeIsolation {
    /// The process-wide instance. Every injector shares it; control
    /// state is a process-global atomic anyway ([`ISOLATE`]), so the
    /// shared instance is the honest shape.
    pub fn global() -> Arc<Self> {
        ISOLATION.get_or_init(|| Arc::new(NativeIsolation::new())).clone()
    }

    /// Start the pump thread and install the low-level hooks. Never
    /// fails the caller: if hooks cannot be installed (an unusual
    /// session), the machine simply runs without isolation and the
    /// injector's other guarantees still hold.
    pub fn new() -> Self {
        let (hwnd_tx, hwnd_rx) = mpsc::channel::<SendHwnd>();
        let handle = thread::Builder::new()
            .name("input-isolation".into())
            .spawn(move || Self::pump(hwnd_tx))
            .ok();
        Self { thread: handle, hwnd_rx: std::sync::Mutex::new(Some(hwnd_rx)) }
    }

    /// Flip the gate. `true` silences this machine's hardware input
    /// (control is on this screen); `false` restores it (control is
    /// home). A single store — nothing blocks and nothing can fail on
    /// the crossing path.
    pub fn set_isolating(&self, isolating: bool) {
        ISOLATE.store(isolating, Ordering::SeqCst);
        if isolating {
            // Arm the heartbeat at the moment control arrives, so the
            // watchdog's staleness check can never trip between `enter`
            // and the first motion tick.
            Self::heartbeat();
        }
    }

    /// Bump the steering heartbeat. Called by the injector on every
    /// motion tick; the watchdog releases isolation if this goes stale
    /// while isolating (see the [`LAST_STEER`] docs).
    pub fn heartbeat() {
        LAST_STEER.store(ms_now(), Ordering::Relaxed);
    }

    /// The watchdog, run on the pump thread at [`WATCHDOG_PERIOD_MS`]:
    /// if isolation is active but steering has been silent for
    /// [`WATCHDOG_TIMEOUT_MS`], release local input and restore the
    /// cursor. This is the last line of defense — a client that cannot
    /// steer must never hold this machine's hardware hostage.
    fn watchdog() {
        if !ISOLATE.load(Ordering::SeqCst) {
            return;
        }
        let now = ms_now();
        let last = LAST_STEER.load(Ordering::Relaxed);
        if last == 0 {
            // Control just entered before the first steering tick: arm
            // the heartbeat now so the check below has a baseline.
            LAST_STEER.store(now, Ordering::Relaxed);
            return;
        }
        if now.saturating_sub(last) > WATCHDOG_TIMEOUT_MS {
            log_warn!(
                "isolation watchdog: client steering stalled — releasing local input so this machine is never trapped"
            );
            ISOLATE.store(false, Ordering::SeqCst);
            // Restore the cursor the injector hid. Best effort: the
            // injector's own `leave` cannot run (it is wedged too), and
            // ShowCursor's per-thread count is a cosmetic detail next to
            // the machine being usable again.
            // SAFETY: ShowCursor is a trivial user32 call.
            unsafe {
                wm::ShowCursor(1);
            }
        }
    }

    /// The pump thread's body: a hidden message-only window, both hooks
    /// installed, then a message loop. Low-level hook procedures run on
    /// the installing thread, so this thread must keep pumping for the
    /// hooks to fire — it parks in `GetMessageW` for the life of the
    /// process.
    fn pump(hwnd_tx: Sender<SendHwnd>) {
        // The message-only window gives the owner a handle to post the
        // quit message to (see Drop).
        let hwnd = Self::create_window();
        if hwnd.is_null() {
            log_error!("input isolation: could not create the hook window");
            return;
        }
        // SAFETY: GetModuleHandleW(null) returns this module's instance
        // handle; low-level hooks may point at a procedure in the current
        // module (the hook is called in this thread's context, not
        // injected into other processes).
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        // SAFETY: both procs are `extern "system"` statics matching the
        // low-level-hook procedure shape the API requires.
        let mouse =
            unsafe { wm::SetWindowsHookExW(wm::WH_MOUSE_LL, Some(mouse_proc), hinstance, 0) };
        let keyboard =
            unsafe { wm::SetWindowsHookExW(wm::WH_KEYBOARD_LL, Some(keyboard_proc), hinstance, 0) };
        if mouse.is_null() || keyboard.is_null() {
            log_error!(
                "input isolation hooks failed to install (mouse: {}, keyboard: {})",
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
            return;
        }
        log_info!("input isolation hooks installed");
        let _ = hwnd_tx.send(SendHwnd(hwnd));
        // A periodic timer so the pump wakes and runs the watchdog even
        // when no input events flow (the whole point is to catch a
        // machine that has gone quiet).
        // SAFETY: SetTimer with a period and no callback posts WM_TIMER
        // to this thread's queue; the window is ours.
        unsafe {
            wm::SetTimer(hwnd, 1, WATCHDOG_PERIOD_MS, None);
        }

        // Pump until the owner posts QUIT_MSG. WM_TIMER wakes the loop
        // for the watchdog; everything else is discarded (nothing
        // dispatches messages; the window exists only as a message
        // target).
        // SAFETY: msg is a valid out-parameter; GetMessageW blocks until
        // a message arrives and returns 0 only on WM_QUIT, -1 on error.
        unsafe {
            let mut msg = std::mem::zeroed::<wm::MSG>();
            loop {
                let ret = wm::GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                if ret == 0 || ret == -1 {
                    break;
                }
                if msg.message == QUIT_MSG {
                    break;
                }
                if msg.message == wm::WM_TIMER {
                    Self::watchdog();
                }
            }
        }
        // SAFETY: unhooking the handles this thread installed, then
        // destroying the window it created.
        unsafe {
            wm::KillTimer(hwnd, 1);
            wm::UnhookWindowsHookEx(mouse);
            wm::UnhookWindowsHookEx(keyboard);
            wm::DestroyWindow(hwnd);
        }
        log_info!("input isolation stopped");
    }

    /// Create the hidden message-only window that anchors the pump loop.
    fn create_window() -> HWND {
        // SAFETY: trivial kernel32 module lookup for class registration.
        let hinstance =
            unsafe { GetModuleHandleW(std::ptr::null()) };
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
            lpszClassName: ISOLATION_CLASS.as_ptr(),
        };
        // SAFETY: the class is fully initialized above.
        let atom = unsafe { wm::RegisterClassW(&class) };
        if atom == 0 {
            // The class may already be registered by an earlier instance
            // (harmless — it is ours); any other failure is fatal.
            // SAFETY: GetLastError reads the thread's last-error slot.
            let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            if err != windows_sys::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS {
                return HWND::default();
            }
        }
        // SAFETY: creating a message-only window (parent HWND_MESSAGE).
        unsafe {
            wm::CreateWindowExW(
                0,
                ISOLATION_CLASS.as_ptr(),
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
        }
    }
}

impl Drop for NativeIsolation {
    fn drop(&mut self) {
        // Ask the pump thread to exit (it unhooks its own hooks and
        // destroys its window on the way out), then join it so no hook
        // procedure can outlive this module.
        if let Some(rx) = self.hwnd_rx.lock().unwrap().take() {
            if let Ok(hwnd) = rx.recv_timeout(std::time::Duration::from_secs(1)) {
                // SAFETY: hwnd belongs to the pump thread and stays valid
                // until that thread exits.
                unsafe {
                    wm::PostMessageW(hwnd.0, QUIT_MSG, 0, 0);
                }
            }
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// Low-level mouse hook: swallow hardware-origin input while this
/// machine's cursor is controlled remotely; pass everything else through
/// untouched.
///
/// # Safety
///
/// `lparam` points at a `MSLLHOOKSTRUCT` owned by the system for the
/// duration of the call — the standard low-level-hook contract.
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && ISOLATE.load(Ordering::SeqCst) {
        // SAFETY: lparam is a valid MSLLHOOKSTRUCT pointer while the
        // hook procedure runs (documented contract of WH_MOUSE_LL).
        let info = unsafe { &*(lparam as *const wm::MSLLHOOKSTRUCT) };
        if info.flags & LLMHF_INJECTED == 0 {
            // Nonzero return: the event is not delivered. The hardware
            // cursor never moves, no click lands, no wheel turns.
            return 1;
        }
    }
    wm::CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

/// Low-level keyboard hook: the same contract as [`mouse_proc`] for
/// keys — hardware keys are silenced while controlled, injected keys
/// (the forwarded stream) pass.
///
/// # Safety
///
/// `lparam` points at a `KBDLLHOOKSTRUCT` owned by the system for the
/// duration of the call.
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && ISOLATE.load(Ordering::SeqCst) {
        // SAFETY: lparam is a valid KBDLLHOOKSTRUCT pointer while the
        // hook procedure runs (documented contract of WH_KEYBOARD_LL).
        let info = unsafe { &*(lparam as *const wm::KBDLLHOOKSTRUCT) };
        if info.flags & LLKHF_INJECTED == 0 {
            return 1;
        }
    }
    wm::CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}
