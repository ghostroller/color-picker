//! Shared physical-pixel window constraints. Fonts retain their actual DPI;
//! a smaller work area changes the viewport, never the text scale.

use std::mem::size_of;

use windows::{
    Win32::{
        Foundation::{E_FAIL, RECT},
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect},
    },
    core::{Error, Result},
};

use crate::app::i18n::tr;

pub(super) fn unavailable() -> Error {
    Error::new(
        E_FAIL,
        tr(
            "当前显示器工作区太小，无法显示操作按钮。请增大可用工作区或调整系统显示缩放。",
            "The display work area is too small for the window controls. Increase the available work area or adjust system display scaling.",
        ),
    )
}

pub(super) fn fit_to_work_area(request: RECT, minimum: (i32, i32)) -> Result<RECT> {
    fit_rect(request, work_area(request)?, minimum).ok_or_else(unavailable)
}

pub(super) fn work_area(request: RECT) -> Result<RECT> {
    // SAFETY: request is initialized RECT storage, borrowed only for this monitor lookup.
    let monitor = unsafe { MonitorFromRect(&request, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: the queried monitor handle and initialized MONITORINFO size match this output buffer.
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return Err(Error::from_thread());
    }
    Ok(info.rcWork)
}

pub(super) fn fit_rect(request: RECT, work: RECT, minimum: (i32, i32)) -> Option<RECT> {
    let width = request
        .right
        .saturating_sub(request.left)
        .min(work.right.saturating_sub(work.left));
    let height = request
        .bottom
        .saturating_sub(request.top)
        .min(work.bottom.saturating_sub(work.top));
    if width < minimum.0 || height < minimum.1 {
        return None;
    }
    let left = request.left.clamp(work.left, work.right - width);
    let top = request.top.clamp(work.top, work.bottom - height);
    Some(RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    })
}

#[derive(Debug)]
pub(super) struct SheetLayout {
    pub viewport: RECT,
    pub actions: RECT,
}

impl SheetLayout {
    pub fn new(
        width: i32,
        height: i32,
        header: i32,
        footer: i32,
        minimum_content: i32,
    ) -> Result<Self> {
        let bottom = height - footer;
        if width <= 0 || bottom - header < minimum_content {
            return Err(unavailable());
        }
        Ok(Self {
            viewport: RECT {
                left: 0,
                top: header,
                right: width,
                bottom,
            },
            actions: RECT {
                left: 0,
                top: bottom,
                right: width,
                bottom: height,
            },
        })
    }
}

pub(super) fn reveal_offset(
    offset: i32,
    top: i32,
    bottom: i32,
    viewport: i32,
    content: i32,
    padding: i32,
) -> i32 {
    // A control taller than the viewport is aligned at its top. Every focusable
    // control is shorter than the supported minimum viewport.
    let next = if top < padding {
        offset + top - padding
    } else if bottom > viewport - padding {
        offset + (bottom - viewport + padding).min(top - padding)
    } else {
        offset
    };
    next.clamp(0, (content - viewport).max(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::windows::drawing::dip;

    #[test]
    fn sheets_fit_small_negative_origin_work_areas_at_each_supported_scale() {
        for dpi in [96, 120, 144, 192, 240, 288] {
            for (width, height) in [(900, 650), (800, 650), (1920, 1040)] {
                let work = RECT {
                    left: -width,
                    top: -height,
                    right: 0,
                    bottom: 0,
                };
                for (desired_width, desired_height, minimum_width) in
                    [(420, 364, 240), (420, 420, 240), (500, 750, 280)]
                {
                    let request = RECT {
                        left: -20,
                        top: -20,
                        right: -20 + dip(desired_width, dpi),
                        bottom: -20 + dip(desired_height, dpi),
                    };
                    let minimum = (dip(minimum_width, dpi), dip(212, dpi));
                    let fitted = fit_rect(request, work, minimum);
                    if width < minimum.0 || height < minimum.1 {
                        assert!(fitted.is_none());
                        continue;
                    }
                    let fitted = fitted.unwrap();
                    assert!(fitted.left >= work.left && fitted.right <= work.right);
                    assert!(fitted.top >= work.top && fitted.bottom <= work.bottom);
                    let panes = SheetLayout::new(
                        fitted.right - fitted.left,
                        fitted.bottom - fitted.top,
                        dip(48, dpi),
                        dip(100, dpi),
                        dip(64, dpi),
                    )
                    .unwrap();
                    assert_eq!(panes.viewport.bottom, panes.actions.top);
                    assert!(panes.actions.bottom <= fitted.bottom - fitted.top);
                }
            }
        }
    }

    #[test]
    fn focused_content_is_reachable_and_extreme_work_areas_fail_explicitly() {
        for top in (0..1000).step_by(40) {
            let offset = reveal_offset(0, top, top + 32, 80, 1040, 8);
            assert!(top - offset >= 0 && top + 32 - offset <= 80);
        }
        assert!(
            fit_rect(
                RECT {
                    right: 800,
                    bottom: 700,
                    ..Default::default()
                },
                RECT {
                    right: 1,
                    bottom: 1,
                    ..Default::default()
                },
                (240, 212)
            )
            .is_none()
        );
    }

    #[test]
    fn initial_placement_keeps_the_selected_monitors_dpi_at_shared_edges() {
        // A neighbouring monitor must not be selected by the oversized request
        // rectangle. It may have a completely different DPI from this monitor.
        for (work, dpi) in [
            (
                RECT {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1040,
                },
                96,
            ),
            (
                RECT {
                    left: -1920,
                    top: -1080,
                    right: 0,
                    bottom: -40,
                },
                192,
            ),
        ] {
            for (x, y) in [
                (work.right - 20, work.top + 40),
                (work.left + 20, work.bottom - 20),
            ] {
                let request = RECT {
                    left: x,
                    top: y,
                    right: x + dip(420, dpi),
                    bottom: y + dip(390, dpi),
                };
                let fitted = fit_rect(request, work, (dip(240, dpi), dip(262, dpi))).unwrap();
                assert!(fitted.left >= work.left && fitted.right <= work.right);
                assert!(fitted.top >= work.top && fitted.bottom <= work.bottom);
                assert_eq!(fitted.right - fitted.left, dip(420, dpi));
                assert_eq!(fitted.bottom - fitted.top, dip(390, dpi));
            }
        }
    }
}
