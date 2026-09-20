//! Eager UTF-16 clipboard writes with explicit movable-memory ownership.

use std::mem::size_of;

use windows::{
    Win32::{
        Foundation::{
            E_INVALIDARG, E_OUTOFMEMORY, ERROR_ACCESS_DENIED, ERROR_BUSY, ERROR_SUCCESS,
            GetLastError, GlobalFree, HANDLE, HGLOBAL, HWND, SetLastError,
        },
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Ole::CF_UNICODETEXT,
        },
        UI::WindowsAndMessaging::IsWindow,
    },
    core::{Error, HRESULT},
};

#[derive(Debug)]
pub enum ClipboardError {
    Busy,
    Other(Error),
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("剪贴板正被其他程序占用"),
            Self::Other(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ClipboardError {}

impl From<Error> for ClipboardError {
    fn from(error: Error) -> Self {
        Self::Other(error)
    }
}

pub struct Clipboard;

impl Clipboard {
    /// No retries here: the result UI owns its bounded, cancelable retry timer.
    pub fn copy_text(owner: HWND, text: &str) -> Result<(), ClipboardError> {
        if !unsafe { IsWindow(Some(owner)) }.as_bool() || text.contains('\0') {
            return Err(Error::new(E_INVALIDARG, "Clipboard owner or text is invalid").into());
        }
        let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let bytes = text.len().checked_mul(size_of::<u16>()).ok_or_else(|| {
            Error::new(E_OUTOFMEMORY, "Clipboard text exceeds addressable memory")
        })?;
        let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes)? };
        let mut memory = MovableMemory(Some(handle));
        let destination = unsafe { GlobalLock(handle) };
        if destination.is_null() {
            return Err(Error::from_thread().into());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), destination.cast::<u16>(), text.len());
        }
        unlock_memory(handle)?;

        if let Err(error) = unsafe { OpenClipboard(Some(owner)) } {
            if matches!(
                error.code(),
                code if code == HRESULT::from_win32(ERROR_ACCESS_DENIED.0)
                    || code == HRESULT::from_win32(ERROR_BUSY.0)
            ) {
                return Err(ClipboardError::Busy);
            }
            return Err(error.into());
        }
        let mut opened = OpenClipboardGuard(true);
        unsafe {
            EmptyClipboard()?;
            SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(handle.0)))?;
        }
        // Ownership transfers only after SetClipboardData succeeds, even if a
        // later CloseClipboard reports a failure. Never free that memory again.
        memory.0 = None;
        opened.close()?;
        Ok(())
    }
}

fn unlock_memory(handle: HGLOBAL) -> windows::core::Result<()> {
    // Final unlock returns zero even on success. Inspect the native error code
    // immediately; do not depend on how windows-rs represents Err for S_OK.
    unsafe { SetLastError(ERROR_SUCCESS) };
    let unlocked = unsafe { GlobalUnlock(handle) };
    let last_error = unsafe { GetLastError() };
    if unlocked.is_err() && last_error != ERROR_SUCCESS {
        Err(Error::from_hresult(HRESULT::from_win32(last_error.0)))
    } else {
        Ok(())
    }
}

struct MovableMemory(Option<HGLOBAL>);

impl Drop for MovableMemory {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            let _ = unsafe { GlobalFree(Some(handle)) };
        }
    }
}

struct OpenClipboardGuard(bool);

impl OpenClipboardGuard {
    fn close(&mut self) -> windows::core::Result<()> {
        unsafe { CloseClipboard()? };
        self.0 = false;
        Ok(())
    }
}

impl Drop for OpenClipboardGuard {
    fn drop(&mut self) {
        if self.0 {
            let _ = unsafe { CloseClipboard() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_movable_memory_unlock_succeeds_without_touching_clipboard() {
        let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, 16) }.unwrap();
        let _memory = MovableMemory(Some(handle));
        assert!(!unsafe { GlobalLock(handle) }.is_null());
        unsafe { SetLastError(ERROR_ACCESS_DENIED) };
        unlock_memory(handle).unwrap();
    }
}
