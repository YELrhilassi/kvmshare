//! System-wide cursor hiding for the server: while the shared cursor is
//! away on a client, this machine must show **no** cursor at all.
//!
//! ## Why not `ShowCursor`
//!
//! `ShowCursor` maintains a per-thread display count that is honored
//! only over windows of the calling thread. The engine thread owns no
//! window, so hiding from it leaves the cursor fully visible over every
//! real application — measured, reproducible, and the root cause of the
//! "the old cursor stays visible after crossing" report.
//!
//! ## The mechanism that actually works
//!
//! `SetSystemCursor` replaces a **system** cursor (arrow, I-beam, …)
//! globally. While hidden, we swap every shape the user can realistically
//! hit for a fully transparent cursor; while shown, we tell the system
//! to reload the current scheme (`SystemParametersInfoW(SPI_SETCURSORS)`)
//! which restores the user's real cursors from their settings — including
//! any custom scheme they had, because the reload reads the live registry
//! values. No window, no message pump, no per-thread state: the cursor is
//! gone over every application, including elevated ones.
//!
//! ## Why `SPI_SETCURSORS` for restore (and not cached `HCURSOR`s)
//!
//! Caching and re-`SetSystemCursor`ing the handles we replaced breaks
//! two ways in the field: the same handle must not be re-applied twice
//! (system cursors are reloaded between our hide and show in some apps),
//! and a cached handle can dangle after a display-scheme change.
//! `SPI_SETCURSORS` re-reads the user's scheme — the one source of truth
//! — and re-propagates `WM_SETTINGCHANGE`, so every window refreshes.

use std::sync::atomic::{AtomicU32, Ordering};

use windows_sys::Win32::UI::WindowsAndMessaging as wm;

/// The system cursor shapes that matter while the machine is being
/// driven remotely. A window can legitimately ask for any of these
/// (text fields want I-beam, busy apps want WAIT), and any one left
/// unswapped shows a ghost cursor over that window. `OCR_NORMAL` (the
/// arrow) is the common case but never the only one.
const HIDDEN_SHAPES: [wm::SYSTEM_CURSOR_ID; 12] = [
    wm::OCR_NORMAL,
    wm::OCR_IBEAM,
    wm::OCR_WAIT,
    wm::OCR_CROSS,
    wm::OCR_UP,
    wm::OCR_SIZENWSE,
    wm::OCR_SIZENESW,
    wm::OCR_SIZENS,
    wm::OCR_SIZEWE,
    wm::OCR_NO,
    wm::OCR_HAND,
    wm::OCR_APPSTARTING,
];

/// 0 = shown, 1 = hide requested, 2 = hidden (system cursors swapped).
static STATE: AtomicU32 = AtomicU32::new(0);

/// An invisible cursor: an all-transparent AND mask with a zero XOR
/// mask, so every pixel of the shape is blank. Size comes from the
/// system metrics so it matches whatever cursor size the user runs
/// (standard, large, extra large accessibility settings).
fn create_invisible_cursor() -> wm::HCURSOR {
    // SAFETY: GetSystemMetrics is a pure query.
    let (w, h) = unsafe { (wm::GetSystemMetrics(wm::SM_CXCURSOR), wm::GetSystemMetrics(wm::SM_CYCURSOR)) };
    let (w, h) = (w.max(1) as usize, h.max(1) as usize);
    // AND mask = all 1s (transparent), XOR mask = all 0s: every pixel
    // renders fully transparent for a monochrome cursor.
    let and_plane = vec![0xFFu8; (w + 7) / 8 * h];
    let xor_plane = vec![0u8; and_plane.len()];
    // SAFETY: the masks are the exact size CreateCursor expects for
    // (w, h); the returned handle is a system cursor resource we keep
    // alive for the process lifetime (it is leaked once, below).
    unsafe {
        wm::CreateCursor(
            std::ptr::null_mut(),
            0,
            0,
            w as i32,
            h as i32,
            and_plane.as_ptr().cast(),
            xor_plane.as_ptr().cast(),
        )
    }
}

/// Hide every system cursor shape. Idempotent: repeated calls while
/// hidden do nothing (unlike `ShowCursor`, there is no count to balance
/// — a state flag is enough).
pub fn hide() {
    if STATE
        .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return; // already hiding or hidden
    }
    let invisible = create_invisible_cursor();
    if invisible.is_null() {
        STATE.store(0, Ordering::SeqCst);
        return;
    }
    // The handle is intentionally kept for the process lifetime: hide
    // and show alternate many times and re-creating per cycle would
    // churn GDI resources. HCURSOR is a raw pointer (no Send/Sync), so
    // the static wraps it in a newtype asserting what the Win32 docs
    // guarantee: HCURSOR is a process-wide handle, usable from any
    // thread (engine, capture, whatever calls us).
    struct SendCursor(wm::HCURSOR);
    // SAFETY: HCURSOR is documented as usable from any thread of the
    // process (GDI object handles are process-wide, not thread-bound).
    unsafe impl Send for SendCursor {}
    unsafe impl Sync for SendCursor {}
    static INVISIBLE: std::sync::OnceLock<SendCursor> = std::sync::OnceLock::new();
    let cursor = INVISIBLE.get_or_init(|| SendCursor(invisible)).0;
    let mut ok = true;
    // SAFETY: SetSystemCursor with a valid HCURSOR and a documented
    // OCR_* id; a false return for one shape is non-fatal (some shapes
    // can be missing from the active scheme).
    for &shape in &HIDDEN_SHAPES {
        unsafe {
            if wm::SetSystemCursor(cursor, shape) == 0 {
                ok = false;
            }
        }
    }
    if !ok {
        // At least one shape failed: rather than a half-hidden cursor,
        // restore immediately and report the hide as not taken.
        restore();
        STATE.store(0, Ordering::SeqCst);
    } else {
        STATE.store(2, Ordering::SeqCst);
    }
}

/// Restore the user's real cursor scheme. Idempotent and safe to call
/// when not hidden. Reads the live scheme from the user's settings, so
/// custom cursor schemes survive a kvmshare session untouched.
pub fn restore() {
    // SAFETY: SPI_SETCURSORS with a null param is the documented
    // "reload the cursor scheme" call; SPIF_SENDCHANGE broadcasts
    // WM_SETTINGCHANGE so every window repaints its cursor immediately.
    unsafe {
        wm::SystemParametersInfoW(wm::SPI_SETCURSORS, 0, std::ptr::null_mut(), wm::SPIF_SENDCHANGE);
    }
    STATE.store(0, Ordering::SeqCst);
}

/// Current hide state (true while the system cursors are swapped away).
#[allow(dead_code)]
pub fn is_hidden() -> bool {
    STATE.load(Ordering::SeqCst) == 2
}

/// Process-exit safety net: restore the user's cursors no matter how we
/// exit (normal drop, supervisor abort, panic). Registered once from the
/// engine constructor.
pub fn install_exit_hook() {
    // A panic hook runs before unwinding continues; the libc atexit
    // path covers a normal exit. Both restore the scheme; a hard kill
    // (TerminateProcess) cannot run code — and there Windows reloads the
    // scheme from its own registry values on the next user/settings
    // change, so nothing is permanently altered.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}
