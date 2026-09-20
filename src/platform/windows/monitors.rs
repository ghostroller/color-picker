//! Enumerate actual monitor rectangles, preserving gaps in the virtual desktop.

use std::{mem::size_of, panic::AssertUnwindSafe};

use windows::{
    Win32::{
        Foundation::{
            E_FAIL, E_OUTOFMEMORY, ERROR_SUCCESS, GetLastError, LPARAM, RECT, SetLastError,
            WIN32_ERROR,
        },
        Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO},
    },
    core::{BOOL, Error, HRESULT, Result},
};

use crate::core::geometry::{ScreenPointPx, ScreenRectPx};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorInfo {
    pub bounds: ScreenRectPx,
    pub work_area: ScreenRectPx,
}

#[derive(Debug)]
pub struct Monitors {
    items: Vec<MonitorInfo>,
}

impl Monitors {
    /// The calling process must already have its PerMonitorV2 manifest active.
    /// Keep this snapshot only until the next display/work-area change.
    pub fn enumerate() -> Result<Self> {
        let mut enumeration = Enumeration::default();
        // Enumeration is synchronous. Only a raw pointer, not a live Rust borrow,
        // crosses the API call that invokes our callback on this thread.
        let (completed, error) = unsafe {
            SetLastError(ERROR_SUCCESS);
            let completed = EnumDisplayMonitors(
                None,
                None,
                Some(collect_monitor),
                LPARAM((&raw mut enumeration) as isize),
            );
            (completed.as_bool(), GetLastError())
        };
        if let Some(failure) = enumeration.failure {
            return Err(failure.into_error());
        }
        if !completed {
            return Err(api_error(error, "EnumDisplayMonitors failed"));
        }
        if enumeration.items.is_empty() {
            return Err(Error::new(E_FAIL, "No desktop monitors are available"));
        }
        Ok(Self {
            items: enumeration.items,
        })
    }

    /// Return no monitor for gaps, rather than treating the virtual bounding box
    /// as one continuous screen or choosing an unrelated nearest monitor.
    pub fn at(&self, point: ScreenPointPx) -> Option<&MonitorInfo> {
        self.items
            .iter()
            .find(|monitor| monitor.bounds.contains(point))
    }
}

#[derive(Default)]
struct Enumeration {
    items: Vec<MonitorInfo>,
    failure: Option<EnumerationFailure>,
}

enum EnumerationFailure {
    MonitorInfo(WIN32_ERROR),
    InvalidBounds,
    Allocation,
    CallbackPanicked,
}

impl EnumerationFailure {
    fn into_error(self) -> Error {
        match self {
            Self::MonitorInfo(error) => api_error(error, "GetMonitorInfoW failed"),
            Self::InvalidBounds => Error::new(E_FAIL, "A monitor has invalid desktop bounds"),
            Self::Allocation => Error::new(E_OUTOFMEMORY, "Could not store the monitor list"),
            Self::CallbackPanicked => Error::new(E_FAIL, "The monitor enumeration callback failed"),
        }
    }
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _dc: HDC,
    _clip: *mut RECT,
    context: LPARAM,
) -> BOOL {
    // This pointer is supplied by enumerate and is valid for the synchronous call.
    let Some(enumeration) = (unsafe { (context.0 as *mut Enumeration).as_mut() }) else {
        return false.into();
    };
    // Do not unwind across Win32, and do not build formatted errors inside the callback.
    let collected = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut information = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let (success, error) = unsafe {
            SetLastError(ERROR_SUCCESS);
            let success = GetMonitorInfoW(monitor, &mut information).as_bool();
            (success, GetLastError())
        };
        if !success {
            return Err(EnumerationFailure::MonitorInfo(error));
        }
        let information = MonitorInfo {
            bounds: screen_rect(information.rcMonitor),
            work_area: screen_rect(information.rcWork),
        };
        if information.bounds.is_empty() {
            return Err(EnumerationFailure::InvalidBounds);
        }
        enumeration
            .items
            .try_reserve(1)
            .map_err(|_| EnumerationFailure::Allocation)?;
        enumeration.items.push(information);
        Ok(())
    }));
    match collected {
        Ok(Ok(())) => true.into(),
        Ok(Err(failure)) => {
            enumeration.failure = Some(failure);
            false.into()
        }
        Err(_) => {
            enumeration.failure = Some(EnumerationFailure::CallbackPanicked);
            false.into()
        }
    }
}

fn screen_rect(rect: RECT) -> ScreenRectPx {
    ScreenRectPx {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

fn api_error(error: WIN32_ERROR, fallback: &str) -> Error {
    if error == ERROR_SUCCESS {
        Error::new(E_FAIL, fallback)
    } else {
        Error::from_hresult(HRESULT::from_win32(error.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_preserves_monitor_gaps_and_half_open_edges() {
        let left = ScreenRectPx {
            left: -120,
            top: -80,
            right: -20,
            bottom: 20,
        };
        let right = ScreenRectPx {
            left: 0,
            top: 0,
            right: 100,
            bottom: 100,
        };
        let monitors = Monitors {
            items: vec![
                MonitorInfo {
                    bounds: left,
                    work_area: left,
                },
                MonitorInfo {
                    bounds: right,
                    work_area: ScreenRectPx {
                        bottom: 90,
                        ..right
                    },
                },
            ],
        };
        assert_eq!(
            monitors
                .at(ScreenPointPx { x: -120, y: -80 })
                .unwrap()
                .bounds,
            left
        );
        assert!(monitors.at(ScreenPointPx { x: -20, y: 0 }).is_none());
        assert!(monitors.at(ScreenPointPx { x: -10, y: 10 }).is_none());
        assert!(monitors.at(ScreenPointPx { x: 0, y: -1 }).is_none());
        assert!(monitors.at(ScreenPointPx { x: 100, y: 99 }).is_none());
        // The taskbar is sampleable even though previews must stay in rcWork.
        let taskbar = monitors.at(ScreenPointPx { x: 99, y: 99 }).unwrap();
        assert_eq!(taskbar.bounds, right);
        assert_eq!(taskbar.work_area.bottom, 90);
    }
}
