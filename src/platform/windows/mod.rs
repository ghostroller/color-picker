use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    GetThreadDpiAwarenessContext,
};

pub mod capture;
pub mod clipboard;
pub mod config_path;
pub(crate) mod copy_job;
pub mod host;
pub mod hotkey;
pub(crate) mod icon;
pub mod input;
pub mod instance;
pub mod monitors;
mod onboarding;
pub mod session;
pub mod settings;
pub mod tray;

pub fn check_environment() -> windows::core::Result<()> {
    // No DPI override: this checks the context supplied by the embedded manifest.
    let is_pmv2 = unsafe {
        AreDpiAwarenessContextsEqual(
            GetThreadDpiAwarenessContext(),
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
        .as_bool()
    };
    if !is_pmv2 {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "The embedded PerMonitorV2 DPI manifest is not active",
        ));
    }
    Ok(())
}
