//! Owned icons from the executable's multi-resolution application resource.
//! Windows borrows HICONs supplied to WM_SETICON and Shell_NotifyIconW: keep
//! these owners alive until the window or notification registration is gone.

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi},
            WindowsAndMessaging::{
                DestroyIcon, HICON, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW,
                SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON, SendMessageW, WM_SETICON,
            },
        },
    },
    core::{PCWSTR, Result},
};

// Keep in sync with resources/app.rc. MAKEINTRESOURCEW encodes an integer in
// the low word of a resource-name pointer; Windows never dereferences it.
const APP_ICON: PCWSTR = PCWSTR(101_usize as *const u16);

pub(crate) struct AppIcon(HICON);

impl AppIcon {
    fn load(width: i32, height: i32) -> Result<Self> {
        // SAFETY: Borrow the current module; the executable resource remains loaded for process lifetime.
        let instance = unsafe { GetModuleHandleW(None)? }.into();
        // SAFETY: APP_ICON is a MAKEINTRESOURCE identifier; LR_SHARED is absent, so the result is uniquely owned.
        let image = unsafe {
            LoadImageW(
                Some(instance),
                APP_ICON,
                IMAGE_ICON,
                width,
                height,
                LR_DEFAULTCOLOR,
            )?
        };
        // Do not use LR_SHARED: its cache can reuse the first loaded size for
        // later requests, and these handles have an explicit Rust owner.
        Ok(Self(HICON(image.0)))
    }

    pub(crate) fn small_for_window(hwnd: HWND) -> Result<Self> {
        // SAFETY: Query only the caller's live window; no Rust memory is passed or retained.
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        Self::load(
            // SAFETY: This scalar DPI/metric query does not borrow application storage.
            unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) },
            // SAFETY: This scalar DPI/metric query does not borrow application storage.
            unsafe { GetSystemMetricsForDpi(SM_CYSMICON, dpi) },
        )
    }

    pub(crate) fn handle(&self) -> HICON {
        self.0
    }
}

impl Drop for AppIcon {
    fn drop(&mut self) {
        // SAFETY: This uniquely owned LoadImageW icon is released only after window/tray borrowers are gone.
        let _ = unsafe { DestroyIcon(self.0) };
    }
}

pub(crate) struct WindowIcons {
    small: AppIcon,
    large: AppIcon,
    dpi: u32,
}

impl WindowIcons {
    pub(crate) fn for_window(hwnd: HWND) -> Result<Self> {
        // SAFETY: The caller keeps this window alive during the DPI query.
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        Ok(Self {
            small: AppIcon::small_for_window(hwnd)?,
            // SAFETY: Both metric calls take scalar dimensions only; load immediately owns the resulting new icon.
            large: AppIcon::load(unsafe { GetSystemMetricsForDpi(SM_CXICON, dpi) }, unsafe {
                GetSystemMetricsForDpi(SM_CYICON, dpi)
            })?,
            dpi,
        })
    }

    pub(crate) fn matches_window_dpi(&self, hwnd: HWND) -> bool {
        // SAFETY: The window is borrowed only for this synchronous DPI query.
        self.dpi == unsafe { GetDpiForWindow(hwnd) }.max(96)
    }

    /// The caller retains this owner until after the window is destroyed, or
    /// until another live WindowIcons has replaced both borrowed handles.
    pub(crate) fn apply(&self, hwnd: HWND) {
        for (kind, icon) in [(ICON_SMALL, &self.small), (ICON_BIG, &self.large)] {
            // SAFETY: The owner retains both icons until replaced or the window tree terminates; WM_SETICON
            // receives native handles only and no RefCell borrow crosses the synchronous callback.
            unsafe {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(kind as usize)),
                    Some(LPARAM(icon.handle().0 as isize)),
                )
            };
        }
    }
}
