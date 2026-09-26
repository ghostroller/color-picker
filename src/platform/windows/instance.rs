//! One resident process per logon and Windows session.

use std::mem::size_of;

use windows::Win32::Foundation::{
    CloseHandle, E_UNEXPECTED, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError, HANDLE,
    SetLastError,
};
use windows::Win32::Security::{
    GetTokenInformation, TOKEN_QUERY, TOKEN_STATISTICS, TokenStatistics,
};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken};
use windows::core::{Error, PCWSTR, Result};

pub enum InstanceStatus {
    Primary(SingleInstance),
    Existing,
}

/// Keep this guard alive until the host has destroyed its window and exited.
/// The mutex is only an existence marker: this process never acquires ownership.
pub struct SingleInstance {
    mutex: HANDLE,
}

impl SingleInstance {
    pub fn acquire() -> Result<InstanceStatus> {
        let name: Vec<u16> = format!("Local\\{}.Instance", instance_key()?)
            .encode_utf16()
            .chain(Some(0))
            .collect();

        // Inspect last-error immediately after creation, before any other Win32 call.
        // Clear it first so stale errors cannot classify a new mutex as an old one.
        // SAFETY: name is terminated UTF-16 for this synchronous call; the fresh mutex handle is owned below.
        // LastError is inspected before any intervening Win32 operation.
        let (mutex, already_exists) = unsafe {
            SetLastError(ERROR_SUCCESS);
            let mutex = CreateMutexW(None, false, PCWSTR(name.as_ptr()))?;
            let already_exists = GetLastError() == ERROR_ALREADY_EXISTS;
            (mutex, already_exists)
        };
        let instance = Self { mutex };
        if already_exists {
            // CreateMutexW opened a second handle; close it before returning.
            drop(instance);
            Ok(InstanceStatus::Existing)
        } else {
            Ok(InstanceStatus::Primary(instance))
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // No ReleaseMutex: initial ownership was false, and we never wait on it.
        // SAFETY: This is our CreateMutexW handle, not a pseudo-handle; no mutex ownership was acquired.
        let _ = unsafe { CloseHandle(self.mutex) };
    }
}

/// A stable name for processes in this logon, suitable for the hidden host's
/// window name or class name. It is not a persistent user/configuration ID.
/// Using AuthenticationId instead of TokenId lets newly started processes find
/// the existing host while preventing another user's host from matching.
pub fn instance_key() -> Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: The process pseudo-handle is borrowed; token is a writable output slot for a new owned handle.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)? };
    let token = ProcessToken(token);
    let mut statistics = TOKEN_STATISTICS::default();
    let mut returned_bytes = 0;
    let expected_bytes = size_of::<TOKEN_STATISTICS>() as u32;

    // SAFETY: token remains open; statistics is writable TOKEN_STATISTICS storage of exactly expected_bytes.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenStatistics,
            Some((&mut statistics as *mut TOKEN_STATISTICS).cast()),
            expected_bytes,
            &mut returned_bytes,
        )?;
    }
    if returned_bytes != expected_bytes {
        return Err(Error::new(
            E_UNEXPECTED,
            "GetTokenInformation returned an unexpected TOKEN_STATISTICS size",
        ));
    }

    let authentication = statistics.AuthenticationId;
    Ok(format!(
        "color-picker.{:08x}.{:08x}",
        authentication.HighPart as u32, authentication.LowPart,
    ))
}

/// Only owns the real handle returned by OpenProcessToken, never a pseudo-handle.
struct ProcessToken(HANDLE);

impl Drop for ProcessToken {
    fn drop(&mut self) {
        // SAFETY: Only the real OpenProcessToken handle is closed by this unique guard.
        let _ = unsafe { CloseHandle(self.0) };
    }
}
