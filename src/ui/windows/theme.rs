//! Shared, lightweight styling for native result and settings controls.
//!
//! Controls keep their native keyboard, selection and accessibility behavior.
//! Brushes/fonts are owned by the window; paint uses stock pens and brushes.

use std::{
    cell::{Cell, RefCell},
    mem::size_of,
};

use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LRESULT, RECT},
        Graphics::{Dwm::*, Gdi::*},
        UI::{
            Controls::*, HiDpi::GetDpiForWindow, Input::KeyboardAndMouse::IsWindowEnabled,
            WindowsAndMessaging::*,
        },
    },
    core::{Result, w},
};

use super::drawing::dip;
use crate::app::i18n::{Language, language, tr};
use crate::platform::windows::gdi::{OwnedBrush, OwnedFont, PaintSession, SavedDc};
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
    canvas: OwnedBrush,
    panel: OwnedBrush,
    // The window owner calls DestroyWindow before dropping its Theme.
    icons: RefCell<Option<WindowIcons>>,
    // A borrowed control DC may retain a WM_GETFONT font after RestoreDC fails.
    // Its window owner checks this sticky flag before every font release.
    font_restore_failed: Cell<bool>,
}

impl Theme {
    pub(super) fn new() -> Result<Self> {
        let canvas = OwnedBrush::solid(CANVAS)?;
        let panel = OwnedBrush::solid(PANEL)?;
        Ok(Self {
            canvas,
            panel,
            icons: RefCell::new(None),
            font_restore_failed: Cell::new(false),
        })
    }

    pub(super) fn fonts_must_be_retained(&self) -> bool {
        self.font_restore_failed.get()
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
        // SAFETY: The native control-color callback supplies this live borrowed DC; scalar colors retain no pointers.
        unsafe {
            SetTextColor(hdc, tone.color());
            SetBkColor(hdc, if panel { PANEL } else { CANVAS });
            SetBkMode(hdc, TRANSPARENT);
        }
        LRESULT(if panel {
            self.panel.raw().0
        } else {
            self.canvas.raw().0
        } as isize)
    }

    pub(super) fn paint(&self, hwnd: HWND, panels: &[RECT]) -> LRESULT {
        // SAFETY: invoked only by this live window's native WM_PAINT.
        if let Ok(paint) = unsafe { PaintSession::begin(hwnd) } {
            let hdc = paint.dc;
            let mut client = RECT::default();
            // SAFETY: The live paint window and writable RECT slot outlive the native query.
            let _ = unsafe { GetClientRect(hwnd, &mut client) };
            // SAFETY: The DC, rectangle and borrowed/owned brush remain valid for this synchronous fill.
            unsafe { FillRect(hdc, &client, self.canvas.raw()) };
            // SAFETY: the paint session outlives this guard; only stock objects are selected.
            if let Ok(saved) = unsafe { SavedDc::new(hdc) } {
                // SAFETY: Query the live window/control handle during its owner or callback lifetime.
                let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
                // SAFETY: SavedDc scopes stock-object and live control-font selections; buffers remain valid throughout drawing.
                unsafe {
                    SelectObject(hdc, GetStockObject(DC_BRUSH));
                    SelectObject(hdc, GetStockObject(DC_PEN));
                    SetDCBrushColor(hdc, PANEL);
                    SetDCPenColor(hdc, BORDER);
                }
                for panel in panels {
                    // SAFETY: The active paint DC is live and its selected stock objects are restored by SavedDc.
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
                if let Err(error) = saved.restore() {
                    crate::app::diagnostics::event(format_args!(
                        "theme.paint_restore_failed {error}"
                    ));
                }
            }
        }
        LRESULT(0)
    }
}

pub(super) struct Font(OwnedFont);

impl Font {
    pub(super) fn raw(&self) -> HFONT {
        self.0.raw()
    }
    pub(super) fn retain(&self) {
        self.0.retain();
    }

    pub(super) fn new(size_dip: i32, dpi: u32, weight: i32, mono: bool) -> Result<Self> {
        Self::for_language(size_dip, dpi, weight, mono, language())
    }

    pub(super) fn for_language(
        size_dip: i32,
        dpi: u32,
        weight: i32,
        mono: bool,
        language: Language,
    ) -> Result<Self> {
        // SAFETY: Create a fresh unselected font with a static face name; transfer it immediately to its unique owner.
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
                    match language {
                        Language::SimplifiedChinese => w!("Microsoft YaHei UI"),
                        Language::English => w!("Segoe UI"),
                    }
                },
            )
        };
        // SAFETY: CreateFontW produces a fresh font; its window owner keeps
        // the font alive until controls release it or the root tree terminates.
        unsafe { OwnedFont::from_raw(font) }.map(Self)
    }
}

/// Draw only native push buttons. Checkboxes keep Windows' themed rendering.
pub(super) fn custom_draw(
    theme: &Theme,
    draw: &NMCUSTOMDRAW,
    primary_id: usize,
) -> Option<LRESULT> {
    draw_button(theme, draw, primary_id, false)
}

/// Borderless neutral buttons for the result's plain white value list.
pub(super) fn custom_draw_minimal(
    theme: &Theme,
    draw: &NMCUSTOMDRAW,
    primary_id: usize,
) -> Option<LRESULT> {
    draw_button(theme, draw, primary_id, true)
}

fn draw_button(
    theme: &Theme,
    draw: &NMCUSTOMDRAW,
    primary_id: usize,
    minimal: bool,
) -> Option<LRESULT> {
    let header = &draw.hdr;
    if draw.dwDrawStage != CDDS_PREPAINT {
        return None;
    }
    let hdc = draw.hdc;
    // SAFETY: the native draw callback keeps this DC and its control font alive.
    let saved = unsafe { SavedDc::new(hdc) }.ok()?;
    // SAFETY: Query the live window/control handle during its owner or callback lifetime.
    let dpi = unsafe { GetDpiForWindow(header.hwndFrom) }.max(96);
    let disabled = (draw.uItemState.0 & (CDIS_DISABLED.0 | CDIS_GRAYED.0)) != 0
        // SAFETY: The verified custom-draw sender remains live during this synchronous notification.
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
    // SAFETY: SavedDc scopes stock-object and live control-font selections; buffers remain valid throughout drawing.
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
    }
    let mut label = [0_u16; 128];
    // SAFETY: the verified live control fills this bounded UTF-16 output buffer.
    let mut length = unsafe { GetWindowTextW(header.hwndFrom, &mut label) } as usize;
    let copied_label = tr("已复制", "Copied");
    let copied_length = copied_label.encode_utf16().count();
    if minimal
        && (primary || (100..104).contains(&header.idFrom))
        && label[..length]
            .iter()
            .copied()
            .take(copied_length)
            .eq(copied_label.encode_utf16())
    {
        // The accessible native name still includes the copied format.
        length = copied_length;
    } else if (100..104).contains(&header.idFrom) {
        // Keep the full native name (e.g. "复制 CSS RGB") for screen readers,
        // while the visible row already identifies the target format.
        let copy_label = tr("复制", "Copy");
        for (slot, character) in label.iter_mut().zip(copy_label.encode_utf16()) {
            *slot = character;
        }
        length = copy_label.encode_utf16().count();
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
    // SAFETY: label is an in-bounds UTF-16 slice and the draw DC remains live.
    unsafe {
        DrawTextW(hdc, &mut label[..length], &mut rect, flags);
    }
    if draw.uItemState.contains(CDIS_FOCUS) && !disabled {
        let inset = dip(4, dpi);
        rect.left += inset;
        rect.right -= inset;
        rect.top += inset;
        rect.bottom -= inset;
        // SAFETY: focus decoration uses borrowed stock objects in SavedDc's scope.
        unsafe {
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
    }
    if let Err(error) = saved.restore() {
        // The borrowed paint/control DC cannot be destroyed here. Its selected
        // WM_GETFONT handle may outlive the current font set or root window.
        // Mark all window font owners for retention before any later release.
        theme.font_restore_failed.set(true);
        crate::app::diagnostics::event(format_args!(
            "theme.button_restore_failed font_owners_retained {error}"
        ));
        return None;
    }
    Some(LRESULT(CDRF_SKIPDEFAULT as isize))
}

/// Optional caption polish; unsupported attributes are harmless on Windows 10.
pub(super) fn configure_window(hwnd: HWND, theme: &Theme) {
    theme.update_window_icons(hwnd);
    // SAFETY: This owned live window and the correctly sized attribute value outlive the synchronous call.
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

#[cfg(test)]
pub(super) mod restore_tests {
    use super::*;
    use crate::platform::windows::gdi::{
        BitmapDc, DesktopDc,
        test_support::{self, Failure},
    };
    use windows::Win32::Foundation::{LPARAM, WPARAM};

    /// Exercise the native WM_NOTIFY boundary with a real owned button and
    /// private bitmap DC. Recover the injected native state before dropping font
    /// owners, so failed DeleteObject on a selected font cannot fake retention.
    pub fn draw_button(parent: HWND, button: HWND, id: usize, theme: &Theme, fail_restore: bool) {
        let desktop = DesktopDc::new().unwrap();
        let surface = BitmapDc::compatible(desktop.raw(), 160, 40).unwrap();
        // SAFETY: the private surface has no CPU views and stays live throughout this test.
        let dc = unsafe { surface.raw() }.unwrap();
        // SAFETY: query the private live DC's currently selected stock font.
        let previous = unsafe { GetCurrentObject(dc, OBJ_FONT) };
        let draw = NMCUSTOMDRAW {
            hdr: NMHDR {
                hwndFrom: button,
                idFrom: id,
                code: NM_CUSTOMDRAW,
            },
            dwDrawStage: CDDS_PREPAINT,
            hdc: dc,
            rc: RECT {
                left: 0,
                top: 0,
                right: 160,
                bottom: 40,
            },
            ..Default::default()
        };
        if fail_restore {
            test_support::fail_next(Failure::RestoreDc);
        }
        // SAFETY: this initialized notification fixture, parent, button and DC
        // stay live for SendMessage's synchronous decoder and drawing callback.
        unsafe {
            SendMessageW(
                parent,
                WM_NOTIFY,
                Some(WPARAM(id)),
                Some(LPARAM(&draw as *const NMCUSTOMDRAW as isize)),
            );
        }
        assert_eq!(theme.fonts_must_be_retained(), fail_restore);
        if fail_restore {
            // SAFETY: this test owns the isolated DC; the one-shot injected
            // failure skipped native RestoreDC, leaving exactly the latest save.
            assert!(unsafe { RestoreDC(dc, -1) }.as_bool());
        }
        // SAFETY: restored live DC state must no longer select a window-owned font.
        assert_eq!(unsafe { GetCurrentObject(dc, OBJ_FONT) }, previous);
    }

    pub fn begin_font_delete_tracking() {
        test_support::start_recording();
    }

    pub fn assert_font_delete_attempts(fonts: &[HFONT], deleted: bool) {
        let events = test_support::finish_recording();
        for font in fonts {
            let attempts = events
                .iter()
                .filter(|(event, handle)| {
                    *event == "font-delete-attempt" && *handle == font.0 as usize
                })
                .count();
            assert_eq!(
                attempts,
                usize::from(deleted),
                "font {:?}: deletion must be exactly once normally, never after uncertain restoration",
                font
            );
        }
    }

    pub fn cleanup_retained_fonts(fonts: &[HFONT]) {
        for font in fonts {
            // SAFETY: test callers destroyed both the native window tree and
            // private drawing DC, and each retained owner has already dropped.
            assert!(unsafe { DeleteObject((*font).into()) }.as_bool());
        }
    }
}
