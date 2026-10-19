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
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT},
        Graphics::{Dwm::*, Gdi::*},
        UI::{
            Controls::*, HiDpi::GetDpiForWindow, Input::KeyboardAndMouse::IsWindowEnabled,
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, PCWSTR, Result, w},
};

use super::drawing::dip;
use crate::app::appearance::{ThemeMode, theme_mode};
use crate::app::i18n::{Language, language, tr};
use crate::platform::windows::gdi::{OwnedBrush, OwnedFont, PaintSession, SavedDc};
use crate::platform::windows::icon::WindowIcons;

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub canvas: COLORREF,
    pub panel: COLORREF,
    pub border: COLORREF,
    pub field: COLORREF,
    pub ink: COLORREF,
    pub muted: COLORREF,
    pub accent: COLORREF,
    success: COLORREF,
    error: COLORREF,
}

pub(super) const fn palette_for(mode: ThemeMode) -> Palette {
    match mode {
        ThemeMode::Light => Palette {
            canvas: rgb(0xf5f7fa),
            panel: rgb(0xffffff),
            border: rgb(0xe2e8f0),
            field: rgb(0xf5f6f8),
            ink: rgb(0x172033),
            muted: rgb(0x627187),
            accent: rgb(0x2563eb),
            success: rgb(0x16815d),
            error: rgb(0xc2413b),
        },
        ThemeMode::Dark => Palette {
            canvas: rgb(0x111827),
            panel: rgb(0x172033),
            border: rgb(0x334155),
            field: rgb(0x222e43),
            ink: rgb(0xf1f5f9),
            muted: rgb(0xa8b8cc),
            accent: rgb(0x93c5fd),
            success: rgb(0x6ee7b7),
            error: rgb(0xfca5a5),
        },
    }
}

pub(super) fn active_palette() -> Palette {
    palette_for(theme_mode())
}

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
    fn color(self, palette: Palette) -> COLORREF {
        match self {
            Self::Text => palette.ink,
            Self::Muted => palette.muted,
            Self::Accent => palette.accent,
            Self::Success => palette.success,
            Self::Error => palette.error,
        }
    }
}

struct Brushes {
    canvas: OwnedBrush,
    panel: OwnedBrush,
    field: OwnedBrush,
}

impl Brushes {
    fn new(palette: Palette) -> Result<Self> {
        Ok(Self {
            canvas: OwnedBrush::solid(palette.canvas)?,
            panel: OwnedBrush::solid(palette.panel)?,
            field: OwnedBrush::solid(palette.field)?,
        })
    }
}

pub(super) struct Theme {
    mode: Cell<ThemeMode>,
    brushes: RefCell<Brushes>,
    // The window owner calls DestroyWindow before dropping its Theme.
    icons: RefCell<Option<WindowIcons>>,
    // A borrowed control DC may retain a WM_GETFONT font after RestoreDC fails.
    // Its window owner checks this sticky flag before every font release.
    font_restore_failed: Cell<bool>,
}

impl Theme {
    pub(super) fn new() -> Result<Self> {
        let mode = theme_mode();
        Ok(Self {
            mode: Cell::new(mode),
            brushes: RefCell::new(Brushes::new(palette_for(mode))?),
            icons: RefCell::new(None),
            font_restore_failed: Cell::new(false),
        })
    }

    pub(super) fn set_mode(&self, mode: ThemeMode) -> Result<()> {
        if mode != self.mode.get() {
            // Allocate the complete replacement before releasing any live brush.
            let brushes = Brushes::new(palette_for(mode))?;
            self.brushes.replace(brushes);
            self.mode.set(mode);
        }
        Ok(())
    }

    pub(super) fn palette(&self) -> Palette {
        palette_for(self.mode.get())
    }

    pub(super) fn mode(&self) -> ThemeMode {
        self.mode.get()
    }

    pub(super) fn is_dark(&self) -> bool {
        self.mode.get() == ThemeMode::Dark
    }

    pub(super) fn fonts_must_be_retained(&self) -> bool {
        self.font_restore_failed.get()
    }

    pub(super) fn control_restore_failed(&self, error: &windows::core::Error) {
        self.font_restore_failed.set(true);
        crate::app::diagnostics::event(format_args!(
            "theme.control_restore_failed font_owners_retained {error}"
        ));
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
        let palette = self.palette();
        // SAFETY: The native control-color callback supplies this live borrowed DC; scalar colors retain no pointers.
        unsafe {
            SetTextColor(hdc, tone.color(palette));
            SetBkColor(hdc, if panel { palette.panel } else { palette.canvas });
            SetBkMode(hdc, TRANSPARENT);
        }
        LRESULT(if panel {
            self.brushes.borrow().panel.raw().0
        } else {
            self.brushes.borrow().canvas.raw().0
        } as isize)
    }

    pub(super) fn field_color(&self, hdc: HDC) -> LRESULT {
        let palette = self.palette();
        // SAFETY: The native control-color callback supplies this borrowed DC;
        // the owning window retains the field brush until child teardown.
        unsafe {
            SetTextColor(hdc, palette.ink);
            SetBkColor(hdc, palette.field);
            SetBkMode(hdc, TRANSPARENT);
        }
        LRESULT(self.brushes.borrow().field.raw().0 as isize)
    }

    pub(super) fn fill_panel(&self, hdc: HDC, rect: &RECT) {
        // SAFETY: The caller's paint session retains this DC; FillRect only borrows
        // the window-owned brush and does not change its selected GDI objects.
        unsafe { FillRect(hdc, rect, self.brushes.borrow().panel.raw()) };
    }

    pub(super) fn paint(&self, hwnd: HWND, panels: &[RECT]) -> LRESULT {
        let palette = self.palette();
        // SAFETY: invoked only by this live window's native WM_PAINT.
        if let Ok(paint) = unsafe { PaintSession::begin(hwnd) } {
            let hdc = paint.dc;
            let mut client = RECT::default();
            // SAFETY: The live paint window and writable RECT slot outlive the native query.
            let _ = unsafe { GetClientRect(hwnd, &mut client) };
            // SAFETY: The DC, rectangle and borrowed/owned brush remain valid for this synchronous fill.
            unsafe { FillRect(hdc, &client, self.brushes.borrow().canvas.raw()) };
            // SAFETY: the paint session outlives this guard; only stock objects are selected.
            if let Ok(saved) = unsafe { SavedDc::new(hdc) } {
                // SAFETY: Query the live window/control handle during its owner or callback lifetime.
                let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
                // SAFETY: SavedDc scopes stock-object and live control-font selections; buffers remain valid throughout drawing.
                unsafe {
                    SelectObject(hdc, GetStockObject(DC_BRUSH));
                    SelectObject(hdc, GetStockObject(DC_PEN));
                    SetDCBrushColor(hdc, palette.panel);
                    SetDCPenColor(hdc, palette.border);
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

/// Restyle native buttons while retaining their input and accessibility behavior.
pub(super) fn custom_draw(
    theme: &Theme,
    draw: &NMCUSTOMDRAW,
    primary_id: usize,
) -> Option<LRESULT> {
    // SAFETY: The decoder validated this live BUTTON sender before the notification.
    let kind = unsafe { GetWindowLongW(draw.hdr.hwndFrom, GWL_STYLE) } & BS_TYPEMASK;
    if matches!(kind, BS_CHECKBOX | BS_AUTOCHECKBOX) {
        return draw_checkbox(theme, draw);
    }
    draw_button(theme, draw, primary_id, false)
}

/// Borderless neutral buttons for the result's softly filled value fields.
pub(super) fn custom_draw_minimal(
    theme: &Theme,
    draw: &NMCUSTOMDRAW,
    primary_id: usize,
) -> Option<LRESULT> {
    draw_button(theme, draw, primary_id, true)
}

fn draw_checkbox(theme: &Theme, draw: &NMCUSTOMDRAW) -> Option<LRESULT> {
    if draw.dwDrawStage != CDDS_PREPAINT {
        return None;
    }
    let hwnd = draw.hdr.hwndFrom;
    let dc = draw.hdc;
    // SAFETY: Native custom draw retains the sender, its font and its DC throughout this call.
    let saved = unsafe { SavedDc::new(dc) }.ok()?;
    // SAFETY: These scalar queries borrow the verified live BUTTON sender only synchronously.
    let (dpi, checked, enabled, font) = unsafe {
        (
            GetDpiForWindow(hwnd).max(96),
            SendMessageW(hwnd, BM_GETCHECK, None, None).0 == BST_CHECKED.0 as isize,
            IsWindowEnabled(hwnd).as_bool(),
            SendMessageW(hwnd, WM_GETFONT, None, None),
        )
    };
    let palette = theme.palette();
    let hot = draw.uItemState.contains(CDIS_HOT) && enabled;
    let pressed = draw.uItemState.contains(CDIS_SELECTED) && enabled;
    let focus = draw.uItemState.contains(CDIS_FOCUS) && enabled;
    let mut label = [0_u16; 256];
    // SAFETY: The control copies into this bounded writable UTF-16 buffer.
    let length = unsafe { GetWindowTextW(hwnd, &mut label) } as usize;
    let mut text_rect = draw.rc;
    text_rect.left += dip(24, dpi);
    let flags = DT_LEFT
        | DT_WORDBREAK
        | if draw.uItemState.contains(CDIS_SHOWKEYBOARDCUES) {
            DRAW_TEXT_FORMAT(0)
        } else {
            DT_HIDEPREFIX
        };
    let mut measured = text_rect;
    let mut metrics = TEXTMETRICW::default();
    // SAFETY: SavedDc restores stock objects and the borrowed control font before return;
    // all UTF-16 text and rectangles are in-bounds local storage for synchronous drawing.
    unsafe {
        if font.0 != 0 {
            SelectObject(dc, HGDIOBJ(font.0 as *mut _));
        }
        SelectObject(dc, GetStockObject(DC_BRUSH));
        SelectObject(dc, GetStockObject(DC_PEN));
        SetDCBrushColor(dc, palette.panel);
        FillRect(dc, &draw.rc, HBRUSH(GetStockObject(DC_BRUSH).0));
        DrawTextW(dc, &mut label[..length], &mut measured, flags | DT_CALCRECT);
        let _ = GetTextMetricsW(dc, &mut metrics);
        text_rect.top +=
            ((draw.rc.bottom - draw.rc.top - (measured.bottom - measured.top)) / 2).max(0);
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, if enabled { palette.ink } else { palette.muted });
        DrawTextW(dc, &mut label[..length], &mut text_rect, flags);
    }
    let side = dip(16, dpi);
    let left = draw.rc.left + dip(2, dpi);
    let top = text_rect.top + ((metrics.tmHeight.max(side) - side) / 2);
    let fill = if checked && enabled {
        if pressed { palette.ink } else { palette.accent }
    } else if hot || pressed {
        palette.field
    } else {
        palette.panel
    };
    // SAFETY: Only stock objects are selected within SavedDc's scope; the checked glyph
    // is an in-bounds local polygon and all coordinates scale with the actual window DPI.
    unsafe {
        SetDCBrushColor(dc, fill);
        SetDCPenColor(
            dc,
            if checked && enabled {
                fill
            } else if hot || focus {
                palette.accent
            } else {
                palette.muted
            },
        );
        let _ = RoundRect(
            dc,
            left,
            top,
            left + side,
            top + side,
            dip(5, dpi),
            dip(5, dpi),
        );
        if checked {
            let tick = [(3, 8), (6, 11), (12, 4), (14, 6), (6, 14), (1, 9)].map(|(x, y)| POINT {
                x: left + dip(x, dpi),
                y: top + dip(y, dpi),
            });
            SelectObject(dc, GetStockObject(NULL_PEN));
            SetDCBrushColor(
                dc,
                if enabled {
                    if theme.is_dark() {
                        palette.canvas
                    } else {
                        palette.panel
                    }
                } else {
                    palette.muted
                },
            );
            let _ = Polygon(dc, &tick);
        }
        if focus {
            SelectObject(dc, GetStockObject(NULL_BRUSH));
            SelectObject(dc, GetStockObject(DC_PEN));
            SetDCPenColor(dc, palette.accent);
            let inset = dip(2, dpi);
            let _ = RoundRect(
                dc,
                left - inset,
                top - inset,
                left + side + inset,
                top + side + inset,
                dip(7, dpi),
                dip(7, dpi),
            );
        }
    }
    if let Err(error) = saved.restore() {
        theme.control_restore_failed(&error);
        return None;
    }
    Some(LRESULT(CDRF_SKIPDEFAULT as isize))
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
    let field_copy = minimal && (100..104).contains(&header.idFrom);
    let palette = theme.palette();
    let dark = theme.is_dark();
    let (fill, border, text) = if disabled {
        (
            if dark { rgb(0x263247) } else { rgb(0xe9edf3) },
            palette.border,
            rgb(0x94a3b8),
        )
    } else if minimal {
        if primary {
            let fill = if pressed {
                if dark { rgb(0xcbd5e1) } else { rgb(0x0b101a) }
            } else if hot {
                if dark { rgb(0xffffff) } else { rgb(0x334155) }
            } else {
                palette.ink
            };
            (fill, fill, palette.panel)
        } else {
            let fill = if pressed {
                if dark { rgb(0x3b4c66) } else { rgb(0xe3e7ed) }
            } else if hot {
                if dark { rgb(0x2e3d55) } else { rgb(0xebedf1) }
            } else if field_copy {
                palette.field
            } else {
                palette.panel
            };
            (
                fill,
                fill,
                if (100..104).contains(&header.idFrom) && !hot {
                    palette.muted
                } else {
                    palette.ink
                },
            )
        }
    } else if primary {
        let fill = if pressed {
            if dark { rgb(0x60a5fa) } else { rgb(0x1e40af) }
        } else if hot {
            if dark { rgb(0xbfdbfe) } else { rgb(0x1d4ed8) }
        } else {
            palette.accent
        };
        (
            fill,
            fill,
            if dark { palette.canvas } else { palette.panel },
        )
    } else {
        let fill = if pressed {
            if dark { rgb(0x3b4c66) } else { rgb(0xe2e8f0) }
        } else if hot {
            if dark { rgb(0x26364f) } else { rgb(0xeff6ff) }
        } else {
            palette.panel
        };
        (
            fill,
            if hot { palette.accent } else { palette.border },
            palette.ink,
        )
    };
    // SAFETY: SavedDc scopes stock-object and live control-font selections; buffers remain valid throughout drawing.
    unsafe {
        SelectObject(hdc, GetStockObject(DC_BRUSH));
        SelectObject(hdc, GetStockObject(DC_PEN));
        // Match each button's surrounding surface, including rounded corners.
        SetDCBrushColor(
            hdc,
            if field_copy {
                palette.field
            } else if minimal || (100..=104).contains(&header.idFrom) {
                palette.panel
            } else {
                palette.canvas
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
            SetDCPenColor(hdc, if primary { text } else { palette.accent });
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
    let palette = theme.palette();
    let dark = BOOL::from(theme.is_dark());
    // SAFETY: This owned live window and the correctly sized attribute value outlive the synchronous call.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            (&palette.canvas as *const COLORREF).cast(),
            size_of::<COLORREF>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TEXT_COLOR,
            (&palette.ink as *const COLORREF).cast(),
            size_of::<COLORREF>() as u32,
        );
        let corners = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark as *const BOOL).cast(),
            size_of::<BOOL>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&corners as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
            size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
        // EnumChildWindows is synchronous and carries only a scalar mode flag.
        let _ = EnumChildWindows(
            Some(hwnd),
            Some(configure_control),
            LPARAM(isize::from(theme.is_dark())),
        );
    }
}

/// Input remains native. Combo surfaces are painted by their local subclass;
/// themed BUTTONs supply custom-draw notifications for our checkbox rendering.
/// Native trackbars must reload their theme when the parent's palette changes.
unsafe extern "system" fn configure_control(hwnd: HWND, _dark: LPARAM) -> BOOL {
    let mut class = [0_u16; 64];
    // SAFETY: EnumChildWindows supplies a live child; class is bounded writable storage.
    let length = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
    let name = String::from_utf16_lossy(&class[..length]);
    // SAFETY: the synchronous enumeration retains this child for the style query.
    let kind = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } & BS_TYPEMASK;
    if name.eq_ignore_ascii_case("combobox") {
        // SAFETY: static strings are read synchronously; this changes only this child's visual style.
        let _ = unsafe { SetWindowTheme(hwnd, w!(""), w!("")) };
    } else if (name.eq_ignore_ascii_case("button") && matches!(kind, BS_CHECKBOX | BS_AUTOCHECKBOX))
        || name.eq_ignore_ascii_case("msctls_trackbar32")
    {
        // SAFETY: null names restore this live child's manifest-provided v6 theme.
        // SetWindowTheme also sends WM_THEMECHANGED, refreshing the trackbar's
        // native background state before the parent invalidates its children.
        let _ = unsafe { SetWindowTheme(hwnd, PCWSTR::null(), PCWSTR::null()) };
    }
    BOOL::from(true)
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

#[cfg(test)]
mod palette_tests {
    use super::*;
    use crate::app::appearance::set_theme_mode;
    use crate::platform::windows::gdi::{BitmapDc, DesktopDc};

    fn luminance(color: COLORREF) -> f64 {
        let linear = |channel: u32| {
            let srgb = f64::from(channel) / 255.0;
            if srgb <= 0.04045 {
                srgb / 12.92
            } else {
                ((srgb + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.0 & 0xff)
            + 0.7152 * linear((color.0 >> 8) & 0xff)
            + 0.0722 * linear((color.0 >> 16) & 0xff)
    }

    fn assert_readable(mode: ThemeMode, foreground: COLORREF, background: COLORREF) {
        let foreground_luminance = luminance(foreground);
        let background_luminance = luminance(background);
        let contrast = (foreground_luminance.max(background_luminance) + 0.05)
            / (foreground_luminance.min(background_luminance) + 0.05);
        assert!(
            contrast >= 4.5,
            "{mode:?}: foreground {foreground:?} on {background:?} has contrast {contrast:.2}"
        );
    }

    #[test]
    fn palettes_keep_text_and_status_readable_on_their_ui_surfaces() {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let palette = palette_for(mode);
            // Body text appears on every surface. Muted text includes the
            // settings subtitle on canvas and the result's inline copy fields.
            for background in [palette.canvas, palette.panel, palette.field] {
                for foreground in [palette.ink, palette.muted, palette.accent] {
                    assert_readable(mode, foreground, background);
                }
            }
            // Status text appears in the settings footer and result sheet;
            // no success/error text is placed inside a filled value field.
            for background in [palette.canvas, palette.panel] {
                for foreground in [palette.success, palette.error] {
                    assert_readable(mode, foreground, background);
                }
            }
        }
    }

    #[test]
    fn theme_changes_refresh_control_brushes_and_keep_font_retention_sticky() {
        let previous = theme_mode();
        set_theme_mode(ThemeMode::Light);
        let theme = Theme::new().unwrap();
        set_theme_mode(previous);
        assert_eq!(theme.mode(), ThemeMode::Light);
        assert!(!theme.fonts_must_be_retained());
        theme.font_restore_failed.set(true);

        let desktop = DesktopDc::new().unwrap();
        let surface = BitmapDc::compatible(desktop.raw(), 4, 4).unwrap();
        // SAFETY: The private bitmap surface has no CPU views and remains live
        // through every scalar query and synchronous brush fill below.
        let dc = unsafe { surface.raw() }.unwrap();
        let rect = RECT {
            right: 4,
            bottom: 4,
            ..Default::default()
        };
        for mode in [ThemeMode::Dark, ThemeMode::Dark, ThemeMode::Light].repeat(8) {
            theme.set_mode(mode).unwrap();
            assert_eq!(theme.mode(), mode);
            assert_eq!(theme.is_dark(), mode == ThemeMode::Dark);
            assert!(theme.fonts_must_be_retained());
            let palette = palette_for(mode);
            let field = HBRUSH(theme.field_color(dc).0 as *mut _);
            // SAFETY: The Theme retains this brush throughout FillRect. The
            // private DC and in-bounds rectangle stay live for all native calls.
            unsafe {
                assert_ne!(FillRect(dc, &rect, field), 0);
                assert_eq!(GetPixel(dc, 1, 1), palette.field);
                assert_eq!(GetBkColor(dc), palette.field);
                assert_eq!(GetTextColor(dc), palette.ink);
            }
            for panel in [false, true] {
                let background = if panel { palette.panel } else { palette.canvas };
                for tone in [
                    Tone::Text,
                    Tone::Muted,
                    Tone::Accent,
                    Tone::Success,
                    Tone::Error,
                ] {
                    let brush = HBRUSH(theme.control_color(dc, panel, tone).0 as *mut _);
                    // SAFETY: The Theme owns the returned brush until the next
                    // iteration; FillRect borrows it without selecting it.
                    unsafe {
                        assert_ne!(FillRect(dc, &rect, brush), 0);
                        assert_eq!(GetPixel(dc, 1, 1), background);
                        assert_eq!(GetBkColor(dc), background);
                        assert_eq!(GetTextColor(dc), tone.color(palette));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod checkbox_tests {
    use super::super::messages;
    use super::*;
    use crate::platform::windows::gdi::{BitmapDc, DesktopDc};
    use windows::Win32::{
        Foundation::WPARAM,
        UI::{
            Input::KeyboardAndMouse::{EnableWindow, VK_SPACE},
            Shell::{DefSubclassProc, SetWindowSubclass},
        },
    };

    struct DrawState {
        theme: Theme,
        notifications: Cell<usize>,
        handled: Cell<usize>,
        clicks: Cell<usize>,
    }

    struct Fixture {
        parent: HWND,
        checkbox: HWND,
        state: Box<DrawState>,
        _font: Font,
    }

    impl Fixture {
        fn new(mode: ThemeMode) -> Self {
            let theme = Theme::new().unwrap();
            theme.set_mode(mode).unwrap();
            let state = Box::new(DrawState {
                theme,
                notifications: Cell::new(0),
                handled: Cell::new(0),
                clicks: Cell::new(0),
            });
            // SAFETY: Built-in windows retain no temporary strings. The parent
            // stays hidden and owns its child until Fixture::drop.
            let parent = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("STATIC"),
                    w!(""),
                    WS_OVERLAPPED,
                    0,
                    0,
                    480,
                    120,
                    None,
                    None,
                    None,
                    None,
                )
            }
            .unwrap();
            assert!(
                // SAFETY: The stable Box outlives the subclass and complete child
                // tree; synchronous notifications borrow its immutable contents.
                unsafe {
                    SetWindowSubclass(
                        parent,
                        Some(parent_proc),
                        1,
                        &*state as *const DrawState as usize,
                    )
                }
                .as_bool()
            );
            // SAFETY: The retained parent owns this native automatic checkbox;
            // WS_VISIBLE never displays it because its parent remains hidden.
            let checkbox = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("BUTTON"),
                    w!("Checkbox label"),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_TABSTOP
                        | WINDOW_STYLE((BS_AUTOCHECKBOX | BS_MULTILINE) as u32),
                    0,
                    0,
                    320,
                    80,
                    Some(parent),
                    Some(HMENU(7_usize as *mut _)),
                    None,
                    None,
                )
            }
            .unwrap();
            // SAFETY: Both queries and style application borrow this live child.
            let dpi = unsafe { GetDpiForWindow(checkbox) }.max(96);
            let font = Font::for_language(14, dpi, 400, false, Language::English).unwrap();
            // SAFETY: The owned font and child remain live through teardown;
            // style configuration is the production theme-enumeration path.
            unsafe {
                SendMessageW(
                    checkbox,
                    WM_SETFONT,
                    Some(WPARAM(font.raw().0 as usize)),
                    None,
                );
                let _ = configure_control(checkbox, LPARAM(isize::from(mode == ThemeMode::Dark)));
                SetWindowPos(
                    checkbox,
                    None,
                    0,
                    0,
                    dip(160, dpi),
                    dip(40, dpi),
                    SWP_NOACTIVATE | SWP_NOZORDER,
                )
                .unwrap();
            }
            Self {
                parent,
                checkbox,
                state,
                _font: font,
            }
        }

        fn checked(&self) -> bool {
            // SAFETY: The fixture retains this checkbox for the scalar query.
            unsafe { SendMessageW(self.checkbox, BM_GETCHECK, None, None) }.0
                == BST_CHECKED.0 as isize
        }

        fn send(&self, message: u32, key: usize) {
            // SAFETY: These fixture messages have scalar-only parameters and
            // synchronously target a live native checkbox on this UI thread.
            unsafe { SendMessageW(self.checkbox, message, Some(WPARAM(key)), None) };
        }

        fn pixels(&self) -> (Vec<COLORREF>, i32, i32, u32) {
            // SAFETY: The fixture owns the child and the local RECT output.
            let dpi = unsafe { GetDpiForWindow(self.checkbox) }.max(96);
            let (width, height) = (dip(160, dpi), dip(40, dpi));
            let desktop = DesktopDc::new().unwrap();
            let surface = BitmapDc::compatible(desktop.raw(), width, height).unwrap();
            // SAFETY: This private bitmap DC stays live until all scalar pixel
            // reads finish and has no simultaneous CPU or paint references.
            let dc = unsafe { surface.raw() }.unwrap();
            let before = self.state.handled.get();
            // SAFETY: WM_PRINTCLIENT borrows the private HDC synchronously;
            // the real BUTTON procedure generates the custom-draw notification.
            unsafe {
                SendMessageW(
                    self.checkbox,
                    WM_PRINTCLIENT,
                    Some(WPARAM(dc.0 as usize)),
                    Some(LPARAM(PRF_CLIENT as isize)),
                );
            }
            assert!(
                self.state.notifications.get() > before,
                "native checkbox did not send NM_CUSTOMDRAW"
            );
            assert!(
                self.state.handled.get() > before,
                "native checkbox custom drawing was not handled"
            );
            let mut pixels = Vec::with_capacity((width * height) as usize);
            for y in 0..height {
                for x in 0..width {
                    // SAFETY: All coordinates are within the private bitmap.
                    pixels.push(unsafe { GetPixel(dc, x, y) });
                }
            }
            (pixels, width, height, dpi)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // SAFETY: Destroy the entire tree while the subclass Box and font
            // are still owned; neither callback nor font reference survives.
            unsafe { DestroyWindow(self.parent) }.unwrap();
        }
    }

    /// # Safety
    /// Installed only on Fixture's retained parent. reference points to its
    /// stable DrawState Box through the parent's synchronous destruction.
    unsafe extern "system" fn parent_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        reference: usize,
    ) -> LRESULT {
        // SAFETY: Fixture owns this stable Box until after parent destruction.
        let state = unsafe { &*(reference as *const DrawState) };
        if message == WM_NOTIFY {
            // SAFETY: Only native notification storage is decoded here; the
            // decoder validates sender class, ownership and notification code.
            let result = unsafe {
                messages::with_button_custom_draw(hwnd, lparam, |draw| {
                    state.notifications.set(state.notifications.get() + 1);
                    let result = custom_draw(&state.theme, draw, usize::MAX);
                    if result.is_some() {
                        state.handled.set(state.handled.get() + 1);
                    }
                    result
                })
            }
            .flatten();
            if let Some(result) = result {
                return result;
            }
        } else if message == WM_COMMAND && (wparam.0 >> 16) as u32 == BN_CLICKED {
            state.clicks.set(state.clicks.get() + 1);
        } else if message == WM_CTLCOLORBTN || message == WM_CTLCOLORSTATIC {
            return state
                .theme
                .control_color(HDC(wparam.0 as *mut _), true, Tone::Text);
        }
        // SAFETY: Forward the unchanged native message synchronously to the
        // original STATIC parent procedure without retaining message storage.
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }

    #[test]
    fn checkbox_native_print_draws_themed_panel_and_checked_glyph() {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let fixture = Fixture::new(mode);
            let palette = fixture.state.theme.palette();
            let (empty, width, height, dpi) = fixture.pixels();
            assert_eq!(empty[0], palette.panel);
            assert_ne!(empty[0], palette.canvas);
            assert!(
                empty
                    .iter()
                    .enumerate()
                    .any(|(i, color)| i as i32 % width >= dip(24, dpi) && *color == palette.ink),
                "checkbox label was not painted"
            );
            fixture.send(BM_CLICK, 0);
            assert!(fixture.checked());
            let (checked, _, _, _) = fixture.pixels();
            let mut accent_bounds = RECT {
                left: width,
                top: height,
                ..Default::default()
            };
            let mut accent_pixels = 0;
            for y in 0..height {
                for x in 0..dip(20, dpi) {
                    if checked[(y * width + x) as usize] == palette.accent {
                        accent_pixels += 1;
                        accent_bounds.left = accent_bounds.left.min(x);
                        accent_bounds.right = accent_bounds.right.max(x);
                        accent_bounds.top = accent_bounds.top.min(y);
                        accent_bounds.bottom = accent_bounds.bottom.max(y);
                    }
                }
            }
            assert!(
                accent_pixels >= 40,
                "checked checkbox did not paint its accent fill"
            );
            let tick = if mode == ThemeMode::Dark {
                palette.canvas
            } else {
                palette.panel
            };
            let inset = dip(3, dpi);
            assert!(
                (accent_bounds.top + inset..accent_bounds.bottom - inset).any(|y| {
                    (accent_bounds.left + inset..accent_bounds.right - inset)
                        .any(|x| checked[(y * width + x) as usize] == tick)
                }),
                "checked checkbox did not paint its tick inside the accent fill"
            );
            assert_ne!(empty, checked);
        }
    }

    #[test]
    fn checkbox_native_click_and_space_preserve_automatic_check_state() {
        let fixture = Fixture::new(ThemeMode::Dark);
        assert!(!fixture.checked());
        fixture.send(BM_CLICK, 0);
        assert!(fixture.checked());
        assert_eq!(fixture.state.clicks.get(), 1);
        fixture.send(WM_KEYDOWN, VK_SPACE.0 as usize);
        fixture.send(WM_KEYUP, VK_SPACE.0 as usize);
        assert!(!fixture.checked());
        assert_eq!(fixture.state.clicks.get(), 2);
        // SAFETY: This enables/disables only the fixture-owned native child.
        let _ = unsafe { EnableWindow(fixture.checkbox, false) };
        // SAFETY: Scalar state query for the retained native checkbox.
        assert!(!unsafe { IsWindowEnabled(fixture.checkbox) }.as_bool());
        assert!(!fixture.checked());
        assert_eq!(fixture.state.clicks.get(), 2);
        let (pixels, width, _, dpi) = fixture.pixels();
        let palette = fixture.state.theme.palette();
        assert!(!pixels.contains(&palette.accent));
        assert!(
            pixels
                .iter()
                .enumerate()
                .any(|(i, color)| { i as i32 % width >= dip(24, dpi) && *color == palette.muted }),
            "disabled checkbox label was not painted in the muted color"
        );
    }

    #[test]
    fn checkbox_restore_failure_marks_fonts_for_retention() {
        for fail_restore in [false, true] {
            let fixture = Fixture::new(ThemeMode::Dark);
            restore_tests::draw_button(
                fixture.parent,
                fixture.checkbox,
                7,
                &fixture.state.theme,
                fail_restore,
            );
            assert_eq!(fixture.state.theme.fonts_must_be_retained(), fail_restore);
        }
    }
}
