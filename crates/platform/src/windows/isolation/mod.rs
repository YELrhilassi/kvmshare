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
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::StationsAndDesktops as sd;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

/// A window handle is a raw pointer, so it is not [`Send`] by default —
/// but it is an opaque token that is never dereferenced, so moving it
/// between threads is sound. Used to hand the pump thread's window
/// handle back to its owner for the quit signal.
#[derive(Clone, Copy)]
struct SendHwnd(HWND);
// SAFETY: HWND is an opaque handle value; nothing is dereferenced.
unsafe impl Send for SendHwnd {}

/// True while this machine's cursor is controlled remotely. Read by the
/// hook procedures on every system input event; flipped by the injector
/// on `enter`/`leave`. A single client injector per process makes a
/// static the honest shape for this state.
static ISOLATE: AtomicBool = AtomicBool::new(false);

/// Set when Windows reports this machine resuming from sleep. A resume
/// invalidates the remote-control state (the pre-sleep "cursor on this
/// machine, hardware silenced, cursor hidden" is stale), so the client
/// run loop reads this once per session (via the injector) and ends the
/// session — the reconnect starts fresh and local. See
/// [`core::client::Injector::system_resumed`].
static RESUMED: AtomicBool = AtomicBool::new(false);

/// Mark that the machine resumed from sleep. Called by the pump thread
/// when Windows reports the resume.
pub fn mark_resumed() {
    RESUMED.store(true, Ordering::SeqCst);
}

/// Read-and-clear the resume notice. The client run loop calls this once
/// per session; the clear keeps a fresh session from inheriting a stale
/// resume from a previous one.
pub fn take_resumed() -> bool {
    RESUMED.swap(false, Ordering::SeqCst)
}

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

mod hooks;
mod pump;

/// Set while the Winlogon secure desktop (the UAC consent prompt) is the
/// input desktop and we have already released isolation for this prompt
/// episode — the release and its log line happen once per prompt, not on
/// every 500 ms poll.
static SECURE_RELEASED: AtomicBool = AtomicBool::new(false);

/// Whether the Winlogon secure desktop is currently the input desktop.
/// Windows switches input to it when a UAC consent prompt is shown;
/// nothing can inject into it — not even an elevated process, it is a
/// separate protected desktop — so a machine being driven remotely must
/// hand control back the moment it appears. A **live poll**, not a
/// latched flag: the client run loop asks this each iteration and ends
/// the session for exactly as long as the prompt is up. Anything other
/// than the normal "Default" input desktop (a locked workstation, ...)
/// is treated the same way — it is equally unreachable.
pub fn secure_desktop_active() -> bool {
    // SAFETY: OpenInputDesktop returns a handle this thread owns;
    // GetUserObjectInformationW(UOI_NAME) writes the desktop name into
    // the caller's buffer; CloseDesktop releases the handle. All three
    // are trivial user32 calls with no shared state.
    unsafe {
        let desk = sd::OpenInputDesktop(0, 0, sd::DESKTOP_READOBJECTS);
        if desk.is_null() {
            return false;
        }
        let mut name = [0u16; 64];
        let mut needed: u32 = 0;
        let ok = sd::GetUserObjectInformationW(
            desk,
            sd::UOI_NAME,
            name.as_mut_ptr() as *mut core::ffi::c_void,
            (name.len() * 2) as u32,
            &mut needed,
        );
        sd::CloseDesktop(desk);
        if ok == 0 {
            return false;
        }
        let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        String::from_utf16_lossy(&name[..len]) != "Default"
    }
}

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
        ISOLATION
            .get_or_init(|| Arc::new(NativeIsolation::new()))
            .clone()
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
        Self {
            thread: handle,
            hwnd_rx: std::sync::Mutex::new(Some(hwnd_rx)),
        }
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
