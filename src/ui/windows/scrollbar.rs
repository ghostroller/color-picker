//! A local skin for the viewport's standard vertical scrollbar. USER32 retains
//! the range, geometry, hit testing and accessibility; captured dragging avoids
//! the system scrollbar's unthemed painting inside its modal tracking loop.

use std::{cell::Cell, mem::size_of};

use windows::Win32::{
    Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::*,
    UI::{
        HiDpi::GetDpiForWindow,
        Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture},
        WindowsAndMessaging::*,
    },
};

use super::super::{drawing::dip, theme::Theme};
use crate::platform::windows::gdi::{SavedDc, WindowDc};

#[derive(Clone, Copy)]
struct Drag {
    grab_offset: i32,
    track_top: i32,
    travel: i32,
    min: i32,
    max: i32,
}

const REPEAT_TIMER: usize = 0x5343;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    Thumb,
}

#[derive(Clone, Copy)]
struct Press {
    part: Part,
    point: POINT,
}

fn hit_part(bar: &SCROLLBARINFO, point: POINT) -> Option<Part> {
    let rect = bar.rcScrollBar;
    if point.x < rect.left || point.x >= rect.right || point.y < rect.top || point.y >= rect.bottom
    {
        None
    } else if point.y < rect.top + bar.dxyLineButton {
        Some(Part::LineUp)
    } else if point.y >= rect.bottom - bar.dxyLineButton {
        Some(Part::LineDown)
    } else if point.y < rect.top + bar.xyThumbTop {
        Some(Part::PageUp)
    } else if point.y >= rect.top + bar.xyThumbBottom {
        Some(Part::PageDown)
    } else {
        Some(Part::Thumb)
    }
}

#[derive(Default)]
pub(super) struct Scrollbar {
    drag: Cell<Option<Drag>>,
    press: Cell<Option<Press>>,
}

impl Scrollbar {
    pub(super) fn paint(&self, hwnd: HWND, theme: &Theme) {
        paint(hwnd, theme, self.drag.get().is_some());
    }

    pub(super) fn cancel(&self, hwnd: HWND) {
        let was_dragging = self.drag.take().is_some();
        let was_pressed = self.press.take().is_some();
        if was_dragging || was_pressed {
            // Clear state before ReleaseCapture can synchronously reenter.
            // SAFETY: this viewport and its capture belong to the calling UI thread.
            unsafe {
                let _ = KillTimer(Some(hwnd), REPEAT_TIMER);
                if GetCapture() == hwnd {
                    let _ = ReleaseCapture();
                }
            }
        }
    }

    fn repeat_step(&self, hwnd: HWND, state: &super::CallbackState) {
        let Some(press) = self.press.get() else {
            return;
        };
        let mut bar = SCROLLBARINFO {
            cbSize: size_of::<SCROLLBARINFO>() as u32,
            ..Default::default()
        };
        let mut info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_ALL,
            ..Default::default()
        };
        // SAFETY: the viewport remains owned while native geometry and scalar
        // range values are copied into these initialized local structures.
        if unsafe {
            GetScrollBarInfo(hwnd, OBJID_VSCROLL, &mut bar).is_err()
                || GetScrollInfo(hwnd, SB_VERT, &mut info).is_err()
        } || hit_part(&bar, press.point) != Some(press.part)
        {
            return;
        }
        // SAFETY: read only the scalar DPI of this live viewport.
        let line = dip(28, unsafe { GetDpiForWindow(hwnd) }.max(96));
        let delta = match press.part {
            Part::LineUp => -line,
            Part::LineDown => line,
            Part::PageUp => -(info.nPage as i32),
            Part::PageDown => info.nPage as i32,
            Part::Thumb => return,
        };
        if let Err(error) = state.scroll_to(info.nPos.saturating_add(delta)) {
            crate::app::diagnostics::event(format_args!("settings.scroll_failed {error}"));
        }
    }

    fn move_to(&self, hwnd: HWND, lparam: LPARAM, state: &super::CallbackState) {
        let mut point = POINT {
            x: lparam.0 as u16 as i16 as i32,
            y: (lparam.0 >> 16) as u16 as i16 as i32,
        };
        // SAFETY: captured mouse coordinates are relative to this live viewport;
        // ClientToScreen only transforms the local writable POINT.
        if !unsafe { ClientToScreen(hwnd, &mut point) }.as_bool() {
            return;
        }
        if let Some(mut press) = self.press.get() {
            press.point = point;
            self.press.set(Some(press));
        }
        let Some(drag) = self.drag.get() else { return };
        if drag.travel <= 0 {
            return;
        }
        let offset = (point.y - drag.grab_offset - drag.track_top).clamp(0, drag.travel);
        let range = i64::from(drag.max) - i64::from(drag.min);
        let position = drag.min
            + ((i64::from(offset) * range + i64::from(drag.travel) / 2) / i64::from(drag.travel))
                as i32;
        if let Err(error) = state.scroll_to(position) {
            crate::app::diagnostics::event(format_args!("settings.scroll_failed {error}"));
        }
        self.paint(hwnd, &state.theme);
    }
}

fn paint(hwnd: HWND, theme: &Theme, pressed: bool) {
    // SAFETY: settings owns its viewport on this UI thread throughout this call.
    if let Ok(dc) = unsafe { WindowDc::including_nonclient(hwnd) } {
        paint_to(hwnd, dc.raw(), theme, pressed);
    }
}

fn paint_to(hwnd: HWND, dc: HDC, theme: &Theme, pressed: bool) {
    let mut info = SCROLLBARINFO {
        cbSize: size_of::<SCROLLBARINFO>() as u32,
        ..Default::default()
    };
    let mut window = RECT::default();
    // SAFETY: the live viewport owns its scroll state; both outputs are valid
    // writable native structures and no reference is retained by USER32.
    if unsafe { GetScrollBarInfo(hwnd, OBJID_VSCROLL, &mut info) }.is_err()
        // SAFETY: the viewport remains live and this local RECT is writable.
        || unsafe { GetWindowRect(hwnd, &mut window) }.is_err()
        // Hidden test/print windows still need their nonclient rendering. The
        // style bit, rather than ancestor visibility, identifies a real bar.
        // SAFETY: read only a scalar style from this live viewport.
        || unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32 & WS_VSCROLL.0 == 0
    {
        return;
    }
    let rect = RECT {
        left: info.rcScrollBar.left - window.left,
        top: info.rcScrollBar.top - window.top,
        right: info.rcScrollBar.right - window.left,
        bottom: info.rcScrollBar.bottom - window.top,
    };
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return;
    }
    // SAFETY: the caller retains this synchronous drawing DC; only stock GDI
    // objects are selected, and SavedDc restores all borrowed DC attributes.
    let Ok(saved) = (unsafe { SavedDc::new(dc) }) else {
        return;
    };
    let palette = theme.palette();
    // SAFETY: a current viewport HWND provides the DPI; the owned drawing guard
    // retains the DC and scopes stock-object selection and clipping.
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96);
        let _ = IntersectClipRect(dc, rect.left, rect.top, rect.right, rect.bottom);
        let brush = GetStockObject(DC_BRUSH);
        SetDCBrushColor(dc, palette.canvas);
        FillRect(dc, &rect, HBRUSH(brush.0));
        SelectObject(dc, brush);
        SelectObject(dc, GetStockObject(NULL_PEN));
        // Keep the native hit target wide, while the visible thumb is quiet and
        // rounded. Arrow hit regions retain their usual line-scroll behavior.
        let width = dip(6, dpi).min((rect.right - rect.left).max(1));
        let left = rect.left + (rect.right - rect.left - width) / 2;
        let top = rect.top + info.xyThumbTop;
        let bottom = rect.top + info.xyThumbBottom;
        if bottom > top {
            SetDCBrushColor(dc, thumb_color(palette.canvas, palette.muted, pressed));
            let _ = RoundRect(dc, left, top, left + width, bottom, width, width);
        }
    }
    if let Err(error) = saved.restore() {
        crate::app::diagnostics::event(format_args!("settings.scrollbar_restore_failed {error}"));
    }
}

fn thumb_color(canvas: COLORREF, muted: COLORREF, pressed: bool) -> COLORREF {
    if pressed {
        return muted;
    }
    let channel = |shift| {
        let background = (canvas.0 >> shift) & 0xff_u32;
        let foreground = (muted.0 >> shift) & 0xff_u32;
        (background * 2_u32 + foreground * 3_u32) / 5_u32
    };
    COLORREF(channel(0) | (channel(8) << 8) | (channel(16) << 16))
}

/// # Safety
/// Only route messages for the live settings viewport with native payloads.
pub(super) unsafe fn handle_message(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    state: &super::CallbackState,
) -> Option<LRESULT> {
    let scrollbar = &state.scrollbar;
    let theme = &state.theme;
    match message {
        WM_NCPAINT => {
            scrollbar.paint(hwnd, theme);
            Some(LRESULT(0))
        }
        WM_NCMOUSEMOVE | WM_NCMOUSELEAVE => {
            scrollbar.paint(hwnd, theme);
            Some(LRESULT(0))
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if wparam.0 == HTVSCROLL as usize => {
            scrollbar.cancel(hwnd);
            let mut bar = SCROLLBARINFO {
                cbSize: size_of::<SCROLLBARINFO>() as u32,
                ..Default::default()
            };
            let mut info = SCROLLINFO {
                cbSize: size_of::<SCROLLINFO>() as u32,
                fMask: SIF_ALL,
                ..Default::default()
            };
            // SAFETY: query only this owned viewport's native scrollbar into
            // initialized synchronous output structures.
            if unsafe {
                GetScrollBarInfo(hwnd, OBJID_VSCROLL, &mut bar).is_ok()
                    && GetScrollInfo(hwnd, SB_VERT, &mut info).is_ok()
            } {
                let point = POINT {
                    x: lparam.0 as u16 as i16 as i32,
                    y: (lparam.0 >> 16) as u16 as i16 as i32,
                };
                let y = point.y;
                let thumb_top = bar.rcScrollBar.top + bar.xyThumbTop;
                let thumb_bottom = bar.rcScrollBar.top + bar.xyThumbBottom;
                if y >= thumb_top && y < thumb_bottom {
                    let track_top = bar.rcScrollBar.top + bar.dxyLineButton;
                    let travel = (bar.rcScrollBar.bottom
                        - bar.dxyLineButton
                        - track_top
                        - (thumb_bottom - thumb_top))
                        .max(0);
                    scrollbar.drag.set(Some(Drag {
                        grab_offset: y - thumb_top,
                        track_top,
                        travel,
                        min: info.nMin,
                        max: (info.nMax - (info.nPage as i32 - 1).max(0)).max(info.nMin),
                    }));
                } else if let Some(part) = hit_part(&bar, point) {
                    scrollbar.press.set(Some(Press { part, point }));
                }
                // SAFETY: capture is local to this UI thread's owned viewport.
                unsafe { SetCapture(hwnd) };
                // SAFETY: scalar capture query cannot retain native references.
                if unsafe { GetCapture() } != hwnd {
                    scrollbar.cancel(hwnd);
                } else if scrollbar.press.get().is_some() {
                    scrollbar.repeat_step(hwnd, state);
                    // This timer drives held-button input only. The first repeat
                    // waits longer; subsequent repeats use the ordinary cadence.
                    // SAFETY: the owned viewport retains the scalar timer identity.
                    unsafe { SetTimer(Some(hwnd), REPEAT_TIMER, 350, None) };
                }
            }
            scrollbar.paint(hwnd, theme);
            Some(LRESULT(0))
        }
        WM_MOUSEMOVE if scrollbar.drag.get().is_some() || scrollbar.press.get().is_some() => {
            scrollbar.move_to(hwnd, lparam, state);
            Some(LRESULT(0))
        }
        WM_LBUTTONUP if scrollbar.drag.get().is_some() || scrollbar.press.get().is_some() => {
            scrollbar.move_to(hwnd, lparam, state);
            scrollbar.cancel(hwnd);
            scrollbar.paint(hwnd, theme);
            Some(LRESULT(0))
        }
        WM_TIMER if wparam.0 == REPEAT_TIMER => {
            // SAFETY: only read this UI thread's capture ownership.
            if scrollbar.press.get().is_some() && unsafe { GetCapture() } == hwnd {
                scrollbar.repeat_step(hwnd, state);
                // SAFETY: repeat exists only while this owned viewport has capture.
                unsafe { SetTimer(Some(hwnd), REPEAT_TIMER, 60, None) };
            } else {
                scrollbar.cancel(hwnd);
            }
            Some(LRESULT(0))
        }
        WM_CAPTURECHANGED | WM_CANCELMODE | WM_NCDESTROY => {
            scrollbar.cancel(hwnd);
            if message != WM_NCDESTROY {
                scrollbar.paint(hwnd, theme);
            }
            None
        }
        WM_SHOWWINDOW if wparam.0 == 0 => {
            scrollbar.cancel(hwnd);
            None
        }
        WM_PRINT => {
            // SAFETY: WM_PRINT supplies the borrowed live DC and native flags.
            let result = unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
            if lparam.0 as i32 & PRF_NONCLIENT != 0 {
                paint_to(
                    hwnd,
                    HDC(wparam.0 as *mut _),
                    theme,
                    scrollbar.drag.get().is_some(),
                );
            }
            Some(result)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::appearance::ThemeMode,
        platform::windows::gdi::{BitmapDc, DesktopDc},
    };
    use windows::{Win32::UI::Controls::SetScrollInfo, core::w};

    #[test]
    fn both_palettes_paint_native_geometry_without_changing_scroll_state() {
        struct Window(HWND);
        impl Drop for Window {
            fn drop(&mut self) {
                // SAFETY: this test alone owns the hidden native fixture.
                let _ = unsafe { DestroyWindow(self.0) };
            }
        }
        // SAFETY: static class/title strings remain terminated and valid during
        // synchronous creation. The test never shows or activates this window.
        let window = Window(unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Color Picker scrollbar palette fixture"),
                WS_OVERLAPPED | WS_VSCROLL,
                0,
                0,
                220,
                220,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        });
        let initial = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: 999,
            nPage: 180,
            nPos: 100,
            ..Default::default()
        };
        // SAFETY: the fixture retains its scrollbar; USER32 copies scalar state.
        unsafe { SetScrollInfo(window.0, SB_VERT, &initial, false) };
        let mut info = SCROLLBARINFO {
            cbSize: size_of::<SCROLLBARINFO>() as u32,
            ..Default::default()
        };
        let mut bounds = RECT::default();
        // SAFETY: live fixture and initialized native output structures.
        unsafe {
            GetScrollBarInfo(window.0, OBJID_VSCROLL, &mut info).unwrap();
            GetWindowRect(window.0, &mut bounds).unwrap();
        }
        assert!(info.xyThumbBottom > info.xyThumbTop);
        let desktop = DesktopDc::new().unwrap();
        let bitmap = BitmapDc::compatible(desktop.raw(), 220, 220).unwrap();
        // SAFETY: bitmap owns its private drawing DC throughout this test.
        let dc = unsafe { bitmap.raw().unwrap() };
        let theme = Theme::new().unwrap();
        let left = info.rcScrollBar.left - bounds.left;
        let right = info.rcScrollBar.right - bounds.left;
        let top = info.rcScrollBar.top - bounds.top;
        let middle_y = top + (info.xyThumbTop + info.xyThumbBottom) / 2;
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            theme.set_mode(mode).unwrap();
            paint_to(window.0, dc, &theme, false);
            // SAFETY: pixel reads borrow the completed private bitmap DC only.
            unsafe {
                assert_eq!(GetPixel(dc, left, middle_y), theme.palette().canvas);
                assert_eq!(
                    GetPixel(dc, (left + right) / 2, middle_y),
                    thumb_color(theme.palette().canvas, theme.palette().muted, false)
                );
                assert_eq!(
                    GetPixel(dc, (left + right) / 2, top),
                    theme.palette().canvas
                );
            }
        }
        let mut after = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_ALL,
            ..Default::default()
        };
        // SAFETY: the fixture remains live through its final read-only query.
        unsafe { GetScrollInfo(window.0, SB_VERT, &mut after).unwrap() };
        assert_eq!(
            (after.nMin, after.nMax, after.nPage, after.nPos),
            (0, 999, 180, 100)
        );
    }
}
