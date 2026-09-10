//! The isolation pump thread: installs the low-level hooks, then parks
//! in a message loop running the watchdog, secure-desktop release and
//! power handling. Split from the gate ([`super::isolation`]) so the
//! state machine and its background machinery read separately.

use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power as power;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use kvmshare_log::{log_error, log_info, log_warn};

use super::hooks::{keyboard_proc, mouse_proc};
use super::{
    mark_resumed, ms_now, secure_desktop_active, NativeIsolation, SendHwnd, ISOLATE,
    ISOLATION_CLASS, LAST_STEER, QUIT_MSG, SECURE_RELEASED, WATCHDOG_TIMEOUT_MS,
    WATCHDOG_PERIOD_MS,
};

impl NativeIsolation {
    /// The pump thread's body: a hidden message-only window, both hooks
    /// installed, then a message loop. Low-level hook procedures run on
    /// the installing thread, so this thread must keep pumping for the
    /// hooks to fire — it parks in `GetMessageW` for the life of the
    /// process.
    pub(super) fn pump(hwnd_tx: Sender<SendHwnd>) {
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
        // Register for suspend/resume notifications (delivered to this
        // window as WM_POWERBROADCAST). Sleep is the one event the
        // watchdog cannot see in time: the process itself suspends, so
        // on resume the pre-sleep control state (cursor on this machine,
        // hardware silenced, cursor hidden) is stale — the session must
        // end so the machine returns to its user. Best effort: without
        // the registration the watchdog still releases the input gate
        // ~2 s after resume via the stale-steering check.
        // SAFETY: RegisterSuspendResumeNotification takes an HWND and
        // returns a handle to keep; both are opaque values here.
        let power_notify = unsafe {
            power::RegisterSuspendResumeNotification(hwnd as _, wm::DEVICE_NOTIFY_WINDOW_HANDLE)
        };

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
                if msg.message == wm::WM_POWERBROADCAST {
                    Self::on_power_event(msg.wParam as u32);
                }
            }
        }
        // SAFETY: unregistering the power notification if we got one
        // (HPOWERNOTIFY is 0 on failure).
        unsafe {
            if power_notify != 0 {
                power::UnregisterSuspendResumeNotification(power_notify);
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

    /// The watchdog, run on the pump thread at [`WATCHDOG_PERIOD_MS`]:
    /// if isolation is active but steering has been silent for
    /// [`WATCHDOG_TIMEOUT_MS`], release local input and restore the
    /// cursor. This is the last line of defense — a client that cannot
    /// steer must never hold this machine's hardware hostage.
    /// The UAC secure desktop, checked while isolating: Windows switches
    /// input to the protected Winlogon desktop when a consent prompt is
    /// shown, and no process can inject into it. A machine being driven
    /// remotely would be dead to the user *and* dead to the server, so
    /// local input is released the moment it appears — the person at the
    /// machine can answer the prompt, and the client run loop (which
    /// polls [`secure_desktop_active`] itself) hands control back.
    fn on_secure_desktop() -> bool {
        if !secure_desktop_active() {
            SECURE_RELEASED.store(false, Ordering::SeqCst);
            return false;
        }
        if SECURE_RELEASED.swap(true, Ordering::SeqCst) {
            return true; // already released for this prompt episode
        }
        log_warn!(
            "Windows secure desktop detected (UAC prompt) — releasing local input so it can be answered"
        );
        ISOLATE.store(false, Ordering::SeqCst);
        // SAFETY: ShowCursor is a trivial user32 call.
        unsafe {
            wm::ShowCursor(1);
        }
        true
    }

    fn watchdog() {
        if !ISOLATE.load(Ordering::SeqCst) {
            return;
        }
        // A prompt must win over everything else: check it before the
        // steering staleness so a healthy-but-blocked stream still
        // releases the machine the instant the secure desktop appears.
        if Self::on_secure_desktop() {
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

    /// Handle a power broadcast on the pump thread. A resume (the machine
    /// waking from sleep) is the one event the watchdog cannot catch
    /// promptly — the process was suspended with it — so on resume the
    /// input gate is released immediately and the session is told to end
    /// (via [`mark_resumed`]).
    fn on_power_event(event: u32) {
        match event {
            wm::PBT_APMRESUMEAUTOMATIC | wm::PBT_APMRESUMESUSPEND => {
                log_warn!("system resumed from sleep — releasing local input so this machine is never left trapped");
                // The pre-sleep control state is stale: release the gate
                // now (the watchdog would take ~2 s) and show the cursor
                // (best effort; the injector's own restore balances the
                // count exactly when the session ends).
                ISOLATE.store(false, Ordering::SeqCst);
                mark_resumed();
                // SAFETY: ShowCursor is a trivial user32 call.
                unsafe {
                    wm::ShowCursor(1);
                }
            }
            _ => {}
        }
    }

}
