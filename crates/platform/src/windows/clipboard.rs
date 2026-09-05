//! Win32 clipboard access for the Windows backend.
//!
//! The protocol carries clipboard content as UTF-8 bytes with a mime type
//! (`text/plain` in v1). Windows stores text on the clipboard as UTF-16
//! (`CF_UNICODETEXT`), so this module converts at the boundary. Pure-Rust
//! UTF-8 <-> UTF-16 conversion keeps us off the `MultiByteToWideChar`
//! surface entirely.
//!
//! One instance is shared by the engine and the injector, mirroring the
//! `arboard`-based clipboard on the X11 backend — same contract, same
//! `last_remote` echo-guard.

use kvmshare_log::log_warn;
use windows_sys::Win32::System::DataExchange as dx;
use windows_sys::Win32::System::Memory as mem;
use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};

/// `CF_UNICODETEXT` (13). Not exported by windows-sys; standard value.
const CF_UNICODETEXT: u32 = 13;

/// Clipboard state shared by the Windows engine/injector.
#[derive(Default)]
pub struct Clipboard {
    /// Last content applied from a *remote* source; pollers compare
    /// against this so content that arrived from a peer is never echoed
    /// back to it.
    last_remote: Option<(String, Vec<u8>)>,
}

impl Clipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Put `data` (as UTF-8 text) into the system clipboard.
    pub fn set_text(&mut self, mime: &str, data: &[u8]) {
        if mime != "text/plain" {
            log_warn!("clipboard: ignoring non-text mime {mime:?}");
            return;
        }
        let Ok(text) = std::str::from_utf8(data) else {
            log_warn!("clipboard: incoming data is not valid UTF-8");
            return;
        };
        // Copy into a moveable global block; the clipboard takes
        // ownership until EmptyClipboard or CloseClipboard.
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: plain Win32 clipboard access. The handle is a raw
        // allocation we size exactly; OpenClipboard serializes access.
        unsafe {
            if dx::OpenClipboard(std::ptr::null_mut()) == 0 {
                log_warn!("clipboard: OpenClipboard failed");
                return;
            }
            let h = mem::GlobalAlloc(mem::GMEM_MOVEABLE, wide.len() * 2);
            if h.is_null() {
                log_warn!("clipboard: GlobalAlloc failed");
                dx::CloseClipboard();
                return;
            }
            let p = mem::GlobalLock(h);
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), p as *mut u16, wide.len());
                mem::GlobalUnlock(h);
            } else {
                log_warn!("clipboard: GlobalLock failed");
                GlobalFree(h);
                dx::CloseClipboard();
                return;
            }
            dx::EmptyClipboard();
            // On failure the system did not take ownership — free the
            // block so nothing leaks.
            if dx::SetClipboardData(CF_UNICODETEXT, h).is_null() {
                log_warn!("clipboard: SetClipboardData failed");
                GlobalFree(h);
            }
            dx::CloseClipboard();
        }
        self.last_remote = Some((mime.to_owned(), data.to_vec()));
    }

    /// Read the current clipboard text, if any, as UTF-8 bytes.
    pub fn get_text(&mut self) -> Option<(String, Vec<u8>)> {
        // SAFETY: standard clipboard read; the locked block is copied
        // into an owned Vec before the clipboard is closed.
        unsafe {
            if dx::OpenClipboard(std::ptr::null_mut()) == 0 {
                return None;
            }
            let h = dx::GetClipboardData(CF_UNICODETEXT);
            if h.is_null() {
                dx::CloseClipboard();
                return None;
            }
            let p = mem::GlobalLock(h as HGLOBAL);
            if p.is_null() {
                dx::CloseClipboard();
                return None;
            }
            // CF_UNICODETEXT memory is NUL-terminated; scan for the end.
            let mut len = 0usize;
            while *((p as *const u16).add(len)) != 0 {
                len += 1;
            }
            let wide = std::slice::from_raw_parts(p as *const u16, len);
            let text = String::from_utf16_lossy(wide);
            mem::GlobalUnlock(h as HGLOBAL);
            dx::CloseClipboard();
            Some(("text/plain".into(), text.into_bytes()))
        }
    }

    /// The last content applied from a remote source (echo guard).
    pub fn last_injected(&self) -> Option<(String, Vec<u8>)> {
        self.last_remote.clone()
    }
}

/// The shared clipboard contract ([`kvmshare_core::clipboard::Clipboard`]),
/// so this same object serves both the client role and the server role.
impl kvmshare_core::client::Clipboard for Clipboard {
    fn set(&mut self, mime: &str, data: &[u8]) {
        self.set_text(mime, data);
    }
    fn get(&mut self) -> Option<(String, Vec<u8>)> {
        self.get_text()
    }
    fn last_injected(&mut self) -> Option<(String, Vec<u8>)> {
        // Same module: touch the field directly rather than the
        // same-named inherent method (which would recurse through the
        // trait dispatch).
        self.last_remote.clone()
    }
}

#[cfg(test)]
#[path = "clipboard_tests.rs"]
mod tests;
