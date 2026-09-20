//! Main-thread session timer and desktop helpers. No timer callbacks or workers.

use crate::core::geometry::ScreenPointPx;
use std::{marker::PhantomData, rc::Rc};
use windows::{
    Win32::{
        Foundation::{E_INVALIDARG, HWND, POINT},
        Graphics::Dwm::DwmFlush,
        UI::WindowsAndMessaging::{GetCursorPos, KillTimer, SetTimer},
    },
    core::{Error, Result},
};

pub const SAMPLE_INTERVAL_MS: u32 = 17;

pub struct SessionTimer {
    hwnd: HWND,
    id: usize,
    _thread: PhantomData<Rc<()>>,
}

impl SessionTimer {
    pub fn start(hwnd: HWND, id: usize) -> Result<Self> {
        if id == 0 {
            return Err(Error::new(
                E_INVALIDARG,
                "Session timer IDs must be nonzero",
            ));
        }
        if unsafe { SetTimer(Some(hwnd), id, SAMPLE_INTERVAL_MS, None) } == 0 {
            return Err(Error::from_thread());
        }
        // With a non-NULL HWND, nIDEvent (not the return value) identifies it.
        Ok(Self {
            hwnd,
            id,
            _thread: PhantomData,
        })
    }

    pub fn id(&self) -> usize {
        self.id
    }
}

impl Drop for SessionTimer {
    fn drop(&mut self) {
        let _ = unsafe { KillTimer(Some(self.hwnd), self.id) };
    }
}

pub fn cursor_position() -> Result<ScreenPointPx> {
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point)? };
    Ok(ScreenPointPx {
        x: point.x,
        y: point.y,
    })
}

pub fn flush_composition() -> Result<()> {
    unsafe { DwmFlush() }
}
