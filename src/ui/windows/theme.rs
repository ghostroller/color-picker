//! Shared, lightweight styling for native result and settings controls.
//!
//! Controls keep their native keyboard, selection and accessibility behavior.
//! Brushes/fonts are owned by the window; paint uses stock pens and brushes.

use std::{cell::RefCell, mem::size_of};

use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT},
        Graphics::{Dwm::*, Gdi::*},
        UI::{
            Controls::*, HiDpi::GetDpiForWindow, Input::KeyboardAndMouse::IsWindowEnabled,
            WindowsAndMessaging::*,
        },
    },
    core::{Error, Result, w},
};

use super::drawing::dip;
use crate::platform::windows::icon::WindowIcons;

const CANVAS: COLORREF = rgb(0xf5f7fa);
const PANEL: COLORREF = rgb(0xffffff);
const BORDER: COLORREF = rgb(0xe2e8f0);
const INK: COLORREF = rgb(0x172033);
const MUTED: COLORREF = rgb(0x64748b);
const ACCENT: COLORREF = rgb(0x2563eb);

const fn rgb(value: u32) -> COLORREF {
    COLORREF(((value & 0xff) << 16) | (value & 0xff00) | ((value >> 16) & 0xff))
}

#[derive(Clone, Copy)]
pub(super) enum Tone {
    Text,
    Muted,
    Accent,
    Success,
    Error,
}

impl Tone {
    fn color(self) -> COLORREF {
        match self {
            Self::Text => INK,
            Self::Muted => MUTED,
            Self::Accent => ACCENT,
            Self::Success => rgb(0x16815d),
            Self::Error => rgb(0xc2413b),
        }
    }
}

pub(super) struct Theme {
    canvas: HBRUSH,
    panel: HBRUSH,
    // The window owner calls DestroyWindow before dropping its Theme.
    icons: RefCell<Option<WindowIcons>>,
}

impl Theme {
    pub(super) fn new() -> Result<Self> {
        let canvas = unsafe { CreateSolidBrush(CANVAS) };
        if canvas.is_invalid() {
            return Err(Error::from_thread());
        }
        let panel = unsafe { CreateSolidBrush(PANEL) };
        if panel.is_invalid() {
            let _ = unsafe { DeleteObject(HGDIOBJ(canvas.0)) };
            return Err(Error::from_thread());
        }
        Ok(Self {
            canvas,
            panel,
            icons: RefCell::new(None),
        })
    }

    pub(super) fn update_window_icons(&self, hwnd: HWND) {
        if self
            .icons
            .borrow()
            .as_ref()
            .is_some_and(|icons| icons.matches_window_dpi(hwnd))
        {
            return;
        }
        // Test executables that construct UI windows directly may not embed
        // app.rc. Missing optional artwork must not prevent using the window.
        if let Ok(icons) = WindowIcons::for_window(hwnd) {
            icons.apply(hwnd);
            // Replace only after Windows no longer borrows either old icon.
            self.icons.replace(Some(icons));
        }
    }

    pub(super) fn control_color(&self, hdc: HDC, panel: bool, tone: Tone) -> LRESULT {
        unsafe {
            SetTextColor(hdc, tone.color());
            SetBkColor(hdc, if panel { PANEL } else { CANVAS });
            SetBkMode(hdc, TRANSPARENT);
        }
        LRESULT(if panel { self.panel.0 } else { self.canvas.0 } as isize)
    }

    pub(super) fn paint(&self, hwnd: HWND, panels: &[RECT]) -> LRESULT {
        let mut paint = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
        if !hdc.is_invalid() {
            let mut client = RECT::default();
            let _ = unsafe { GetClientRect(hwnd, &mut client) };
            unsafe { FillRect(hdc, &client, self.canvas) };
            let saved = unsafe { SaveDC(hdc) };
            if saved != 0 {
                let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
                unsafe {
                    SelectObject(hdc, GetStockObject(DC_BRUSH));
                    SelectObject(hdc, GetStockObject(DC_PEN));
                    SetDCBrushColor(hdc, PANEL);
                    SetDCPenColor(hdc, BORDER);
                }
                for panel in panels {
                    let _ = unsafe {
                        RoundRect(
                            hdc,
                            dip(panel.left, dpi),
                            dip(panel.top, dpi),
                            dip(panel.right, dpi),
                            dip(panel.bottom, dpi),
                            dip(16, dpi),
                            dip(16, dpi),
                        )
                    };
                }
                let _ = unsafe { RestoreDC(hdc, saved) };
            }
        }
        let _ = unsafe { EndPaint(hwnd, &paint) };
        LRESULT(0)
    }
}

impl Drop for Theme {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.canvas.0));
            let _ = DeleteObject(HGDIOBJ(self.panel.0));
        }
    }
}

pub(super) struct Font(pub HFONT);

impl Font {
    pub(super) fn new(size_dip: i32, dpi: u32, weight: i32, mono: bool) -> Result<Self> {
        let font = unsafe {
            CreateFontW(
                -dip(size_dip, dpi),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                DEFAULT_PITCH.0 as u32,
                if mono {
                    w!("Consolas")
                } else {
                    w!("Microsoft YaHei UI")
                },
            )
        };
        if font.is_invalid() {
            Err(Error::from_thread())
        } else {
            Ok(Self(font))
        }
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
    }
}

/// Draw only native push buttons. Checkboxes keep Windows' themed rendering.
pub(super) fn custom_draw(lparam: LPARAM, primary_id: usize) -> Option<LRESULT> {
    draw_button(lparam, primary_id, false)
}

/// Borderless neutral buttons for the result's plain white value list.
pub(super) fn custom_draw_minimal(lparam: LPARAM, primary_id: usize) -> Option<LRESULT> {
    draw_button(lparam, primary_id, true)
}

fn draw_button(lparam: LPARAM, primary_id: usize, minimal: bool) -> Option<LRESULT> {
    if lparam.0 == 0 {
        return None;
    }
    let header = unsafe { &*(lparam.0 as *const NMHDR) };
    if header.code != NM_CUSTOMDRAW {
        return None;
    }
    let mut class = [0_u16; 32];
    let length = unsafe { GetClassNameW(header.hwndFrom, &mut class) } as usize;
    if !String::from_utf16_lossy(&class[..length]).eq_ignore_ascii_case("button") {
        return None;
    }
    let kind = unsafe { GetWindowLongW(header.hwndFrom, GWL_STYLE) } & BS_TYPEMASK;
    if kind != BS_PUSHBUTTON && kind != BS_DEFPUSHBUTTON {
        return None;
    }
    let draw = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
    if draw.dwDrawStage != CDDS_PREPAINT {
        return None;
    }
    let hdc = draw.hdc;
    let saved = unsafe { SaveDC(hdc) };
    if saved == 0 {
        return None;
    }
    let dpi = unsafe { GetDpiForWindow(header.hwndFrom) }.max(96);
    let disabled = (draw.uItemState.0 & (CDIS_DISABLED.0 | CDIS_GRAYED.0)) != 0
        || !unsafe { IsWindowEnabled(header.hwndFrom) }.as_bool();
    let pressed = draw.uItemState.contains(CDIS_SELECTED);
    let hot = draw.uItemState.contains(CDIS_HOT);
    let primary = header.idFrom == primary_id;
    let (fill, border, text) = if disabled {
        (rgb(0xe9edf3), BORDER, rgb(0x94a3b8))
    } else if minimal {
        if primary {
            let fill = if pressed {
                rgb(0x0b101a)
            } else if hot {
                rgb(0x334155)
            } else {
                INK
            };
            (fill, fill, PANEL)
        } else {
            let fill = if pressed {
                rgb(0xe9edf2)
            } else if hot {
                rgb(0xf4f6f8)
            } else {
                PANEL
            };
            (
                fill,
                fill,
                if (100..104).contains(&header.idFrom) && !hot {
                    MUTED
                } else {
                    INK
                },
            )
        }
    } else if primary {
        let fill = if pressed {
            rgb(0x1e40af)
        } else if hot {
            rgb(0x1d4ed8)
        } else {
            ACCENT
        };
        (fill, fill, PANEL)
    } else {
        let fill = if pressed {
            rgb(0xe2e8f0)
        } else if hot {
            rgb(0xeff6ff)
        } else {
            PANEL
        };
        (fill, if hot { rgb(0x93b4f3) } else { rgb(0xd6dee9) }, INK)
    };
    unsafe {
        SelectObject(hdc, GetStockObject(DC_BRUSH));
        SelectObject(hdc, GetStockObject(DC_PEN));
        // Row copy / key-capture buttons sit on white cards; footer on canvas.
        SetDCBrushColor(
            hdc,
            if minimal || (100..=104).contains(&header.idFrom) {
                PANEL
            } else {
                CANVAS
            },
        );
        FillRect(hdc, &draw.rc, HBRUSH(GetStockObject(DC_BRUSH).0));
        SetDCBrushColor(hdc, fill);
        SetDCPenColor(hdc, border);
        let _ = RoundRect(
            hdc,
            draw.rc.left,
            draw.rc.top,
            draw.rc.right,
            draw.rc.bottom,
            dip(10, dpi),
            dip(10, dpi),
        );
        let font = SendMessageW(header.hwndFrom, WM_GETFONT, None, None);
        if font.0 != 0 {
            SelectObject(hdc, HGDIOBJ(font.0 as *mut _));
        }
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, text);
        let mut label = [0_u16; 128];
        let mut length = GetWindowTextW(header.hwndFrom, &mut label) as usize;
        if (100..104).contains(&header.idFrom) {
            // Keep the full native name (e.g. "复制 CSS RGB") for screen readers,
            // while the visible row already identifies the target format.
            label[..2].copy_from_slice(&[0x590d, 0x5236]);
            length = 2;
        }
        let mut rect = draw.rc;
        let flags = DT_CENTER
            | DT_VCENTER
            | DT_SINGLELINE
            | if draw.uItemState.contains(CDIS_SHOWKEYBOARDCUES) {
                DRAW_TEXT_FORMAT(0)
            } else {
                DT_HIDEPREFIX
            };
        DrawTextW(hdc, &mut label[..length], &mut rect, flags);
        if draw.uItemState.contains(CDIS_FOCUS) && !disabled {
            let inset = dip(4, dpi);
            rect.left += inset;
            rect.right -= inset;
            rect.top += inset;
            rect.bottom -= inset;
            SelectObject(hdc, GetStockObject(NULL_BRUSH));
            SetDCPenColor(hdc, if primary { PANEL } else { ACCENT });
            let _ = RoundRect(
                hdc,
                rect.left,
                rect.top,
                rect.right,
                rect.bottom,
                dip(6, dpi),
                dip(6, dpi),
            );
        }
        let _ = RestoreDC(hdc, saved);
    }
    Some(LRESULT(CDRF_SKIPDEFAULT as isize))
}

/// Optional caption polish; unsupported attributes are harmless on Windows 10.
pub(super) fn configure_window(hwnd: HWND, theme: &Theme) {
    theme.update_window_icons(hwnd);
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            (&CANVAS as *const COLORREF).cast(),
            size_of::<COLORREF>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TEXT_COLOR,
            (&INK as *const COLORREF).cast(),
            size_of::<COLORREF>() as u32,
        );
        let corners = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&corners as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
            size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
    }
}
