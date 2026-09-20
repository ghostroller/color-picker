use std::{marker::PhantomData, rc::Rc};

use windows::{
    Win32::{
        Foundation::{E_INVALIDARG, HWND},
        UI::Input::KeyboardAndMouse::{
            HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey,
            UnregisterHotKey, VK_C,
        },
    },
    core::{Error, Result},
};

use crate::app::config::HotkeyConfig;

pub const DEFAULT_HOTKEY_ID: i32 = 1;

/// A hotkey registration owned by the host's UI thread.
///
/// The host window must outlive this guard. Each live registration must have a
/// distinct ID for that window, including temporary registrations used by a
/// future settings transaction.
#[derive(Debug)]
pub struct HotkeyGuard {
    hwnd: HWND,
    id: i32,
    // Do not allow registration ownership to move to another thread, even if
    // the underlying binding changes its handle auto-traits in the future.
    _main_thread_only: PhantomData<Rc<()>>,
}

impl HotkeyGuard {
    pub fn register_config(hwnd: HWND, id: i32, config: &HotkeyConfig) -> Result<Self> {
        let key = config
            .virtual_key()
            .map_err(|error| Error::new(E_INVALIDARG, error.to_string()))?;
        let mut modifiers = MOD_NOREPEAT;
        if config.ctrl {
            modifiers |= MOD_CONTROL;
        }
        if config.alt {
            modifiers |= MOD_ALT;
        }
        if config.shift {
            modifiers |= MOD_SHIFT;
        }
        Self::register(hwnd, id, modifiers, key)
    }

    pub fn register(hwnd: HWND, id: i32, modifiers: HOT_KEY_MODIFIERS, vk: u32) -> Result<Self> {
        // SAFETY: registration only passes the window handle and scalar values
        // to Win32; failure (including a conflict) is returned to the caller.
        unsafe { RegisterHotKey(Some(hwnd), id, modifiers, vk)? };
        Ok(Self {
            hwnd,
            id,
            _main_thread_only: PhantomData,
        })
    }

    pub const fn id(&self) -> i32 {
        self.id
    }
}

impl Drop for HotkeyGuard {
    fn drop(&mut self) {
        // SAFETY: this guard is created only after successful registration and
        // remains on its owning thread. The host drops it before destroying
        // the window. Drop cannot report an OS cleanup failure to its caller.
        let _ = unsafe { UnregisterHotKey(Some(self.hwnd), self.id) };
    }
}

pub fn register_default(hwnd: HWND) -> Result<HotkeyGuard> {
    HotkeyGuard::register(
        hwnd,
        DEFAULT_HOTKEY_ID,
        MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
        u32::from(VK_C.0),
    )
}
