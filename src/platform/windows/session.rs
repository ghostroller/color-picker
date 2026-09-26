//! Main-thread session timer and desktop helpers. No timer callbacks or workers.

use crate::core::geometry::ScreenPointPx;
use std::{
    marker::PhantomData,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
};
use windows::{
    Win32::{
        Foundation::{E_FAIL, E_INVALIDARG, HWND, POINT},
        Graphics::Dwm::DwmFlush,
        UI::WindowsAndMessaging::{GetCursorPos, KillTimer, SetTimer},
    },
    core::{Error, Result},
};

pub const SAMPLE_INTERVAL_MS: u32 = 17;
static NEXT_TIMER_ID: AtomicUsize = AtomicUsize::new(1);

/// Host sampling and clipboard retries share a window, so their timer IDs must
/// come from one non-reusing namespace. Killed timers can still be queued.
pub(crate) fn next_timer_id() -> Result<usize> {
    NEXT_TIMER_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| Error::new(E_FAIL, "Host timer IDs exhausted. Restart the app."))
}

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
        // SAFETY: The host keeps hwnd alive through this UI-thread timer guard; the callback is None
        // and its nonzero, non-reused ID routes only queued WM_TIMER messages.
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
        // SAFETY: This guard owns the timer ID on its live host; cancellation does not dereference application state.
        let _ = unsafe { KillTimer(Some(self.hwnd), self.id) };
    }
}

pub fn cursor_position() -> Result<ScreenPointPx> {
    let mut point = POINT::default();
    // SAFETY: point is a writable initialized POINT; GetCursorPos retains no pointer.
    unsafe { GetCursorPos(&mut point)? };
    Ok(ScreenPointPx {
        x: point.x,
        y: point.y,
    })
}

pub fn flush_composition() -> Result<()> {
    // SAFETY: DwmFlush synchronizes composition for this caller and takes no borrowed memory.
    unsafe { DwmFlush() }
}
