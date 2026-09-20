//! GDI drawing resources owned by a single preview window on the UI thread.

use windows::Win32::Foundation::{COLORREF, E_FAIL, HWND, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::core::{Error, PCWSTR, Result, w};

use crate::core::color::Rgb8;

pub(super) fn dip(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi) + 48) / 96) as i32
}

/// The sampling overlays deliberately stay dark, so they remain distinct from
/// both the sampled pixels and the normal result/settings windows.
pub(super) mod palette {
    use windows::Win32::Foundation::COLORREF;

    const fn rgb(value: u32) -> COLORREF {
        COLORREF(((value & 0xff) << 16) | (value & 0xff00) | ((value >> 16) & 0xff))
    }

    pub const BACKGROUND: COLORREF = rgb(0x111827);
    pub const PANEL: COLORREF = rgb(0x172033);
    pub const BORDER: COLORREF = rgb(0x334155);
    pub const SWATCH_BORDER: COLORREF = rgb(0x64748b);
    pub const TEXT: COLORREF = rgb(0xf8fafc);
    pub const SECONDARY: COLORREF = rgb(0xcbd5e1);
    pub const MUTED: COLORREF = rgb(0x94a3b8);
    pub const ACCENT: COLORREF = rgb(0x93c5fd);
    pub const EMPTY: COLORREF = rgb(0x475569);
}

pub(super) struct Content {
    pub rgb: Option<Rgb8>,
    pub color_text: Vec<u16>,
    pub coordinates: Vec<u16>,
    pub help: Vec<u16>,
}

struct MemoryDc(HDC);

impl Drop for MemoryDc {
    fn drop(&mut self) {
        let _ = unsafe { DeleteDC(self.0) };
    }
}

struct OwnedBitmap(HBITMAP);

impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
    }
}

pub(super) struct OwnedFont(HFONT);

impl OwnedFont {
    pub fn new(size: i32, weight: i32, dpi: u32) -> Result<Self> {
        let font = Self(unsafe {
            CreateFontW(
                -dip(size, dpi),
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
                0,
                w!("Microsoft YaHei UI"),
            )
        });
        if font.0.is_invalid() {
            Err(Error::new(E_FAIL, "Could not create an overlay font"))
        } else {
            Ok(font)
        }
    }
}

impl Drop for OwnedFont {
    fn drop(&mut self) {
        let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
    }
}

struct ScreenDc(HDC);

impl Drop for ScreenDc {
    fn drop(&mut self) {
        let _ = unsafe { ReleaseDC(None, self.0) };
    }
}

pub(super) struct Surface {
    dc: MemoryDc,
    _bitmap: OwnedBitmap,
    heading_font: OwnedFont,
    body_font: OwnedFont,
    help_font: OwnedFont,
    old_bitmap: HGDIOBJ,
    pub width: i32,
    pub height: i32,
    pub dpi: u32,
}

impl Surface {
    pub fn new(width: i32, height: i32, dpi: u32) -> Result<Self> {
        let screen = ScreenDc(unsafe { GetDC(None) });
        if screen.0.0.is_null() {
            return Err(Error::new(E_FAIL, "Could not obtain a drawing DC"));
        }
        let dc = MemoryDc(unsafe { CreateCompatibleDC(Some(screen.0)) });
        if dc.0.0.is_null() {
            return Err(Error::new(E_FAIL, "Could not create a preview memory DC"));
        }
        let bitmap = OwnedBitmap(unsafe { CreateCompatibleBitmap(screen.0, width, height) });
        if bitmap.0.0.is_null() {
            return Err(Error::new(
                E_FAIL,
                "Could not create the preview back buffer",
            ));
        }
        let heading_font = OwnedFont::new(18, 600, dpi)?;
        let body_font = OwnedFont::new(12, 400, dpi)?;
        let help_font = OwnedFont::new(11, 400, dpi)?;
        let old_bitmap = unsafe { SelectObject(dc.0, HGDIOBJ(bitmap.0.0)) };
        if invalid_selection(old_bitmap) {
            return Err(Error::new(E_FAIL, "Could not select the preview bitmap"));
        }
        Ok(Self {
            dc,
            _bitmap: bitmap,
            heading_font,
            body_font,
            help_font,
            old_bitmap,
            width,
            height,
            dpi,
        })
    }

    pub fn draw(&self, target: HDC, content: &Content) -> Result<()> {
        let rect = RECT {
            left: 0,
            top: 0,
            right: self.width,
            bottom: self.height,
        };
        // DC_BRUSH is a shared stock object and must never be deleted. Its
        // per-DC color avoids allocating a new brush for each sampled color.
        let swatch_brush = HBRUSH(unsafe { GetStockObject(DC_BRUSH) }.0);
        if swatch_brush.0.is_null() {
            return Err(Error::new(
                E_FAIL,
                "Could not obtain the shared drawing brush",
            ));
        }
        self.fill(swatch_brush, palette::PANEL, rect)?;
        if unsafe { SetDCBrushColor(self.dc.0, palette::BORDER) }.0 == CLR_INVALID
            || unsafe { FrameRect(self.dc.0, &rect, swatch_brush) } == 0
        {
            return Err(Error::new(E_FAIL, "Could not paint the preview border"));
        }
        let color = content.rgb.map_or(palette::EMPTY, |rgb| {
            COLORREF(u32::from(rgb.r) | (u32::from(rgb.g) << 8) | (u32::from(rgb.b) << 16))
        });
        self.swatch(
            swatch_brush,
            palette::SWATCH_BORDER,
            RECT {
                left: 16,
                top: 16,
                right: 72,
                bottom: 72,
            },
        )?;
        self.swatch(
            swatch_brush,
            color,
            RECT {
                left: 18,
                top: 18,
                right: 70,
                bottom: 70,
            },
        )?;
        self.text(
            &self.heading_font,
            RECT {
                left: 88,
                top: 19,
                right: 264,
                bottom: 45,
            },
            palette::TEXT,
            &content.color_text,
        )?;
        self.text(
            &self.body_font,
            RECT {
                left: 88,
                top: 49,
                right: 264,
                bottom: 70,
            },
            palette::SECONDARY,
            &content.coordinates,
        )?;
        self.swatch(
            swatch_brush,
            palette::BORDER,
            RECT {
                left: 16,
                top: 80,
                right: 264,
                bottom: 81,
            },
        )?;
        self.text(
            &self.help_font,
            RECT {
                left: 16,
                top: 89,
                right: 264,
                bottom: 104,
            },
            palette::MUTED,
            &content.help,
        )?;
        unsafe {
            BitBlt(
                target,
                0,
                0,
                self.width,
                self.height,
                Some(self.dc.0),
                0,
                0,
                SRCCOPY,
            )
        }
    }

    fn swatch(&self, brush: HBRUSH, color: COLORREF, rect_dip: RECT) -> Result<()> {
        self.fill(brush, color, self.scale_rect(rect_dip))
    }

    fn fill(&self, brush: HBRUSH, color: COLORREF, rect: RECT) -> Result<()> {
        if unsafe { SetDCBrushColor(self.dc.0, color) }.0 == CLR_INVALID {
            return Err(Error::new(E_FAIL, "Could not set the preview swatch color"));
        }
        if unsafe { FillRect(self.dc.0, &rect, brush) } == 0 {
            return Err(Error::new(E_FAIL, "Could not paint the preview swatch"));
        }
        Ok(())
    }

    fn scale_rect(&self, rect: RECT) -> RECT {
        RECT {
            left: dip(rect.left, self.dpi),
            top: dip(rect.top, self.dpi),
            right: dip(rect.right, self.dpi),
            bottom: dip(rect.bottom, self.dpi),
        }
    }

    fn text(&self, font: &OwnedFont, rect: RECT, color: COLORREF, text: &[u16]) -> Result<()> {
        draw_text(self.dc.0, font, self.scale_rect(rect), color, text)
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // Restore borrowed stock objects before fields delete the DC/resources.
        unsafe {
            SelectObject(self.dc.0, self.old_bitmap);
        }
    }
}

fn invalid_selection(object: HGDIOBJ) -> bool {
    object.0.is_null() || object.0 as isize == -1
}

/// Text uses cached fonts and an explicit clip rectangle. Even unusually long
/// negative coordinates cannot paint over the color swatch or the outer border.
pub(super) fn draw_text(
    dc: HDC,
    font: &OwnedFont,
    rect: RECT,
    color: COLORREF,
    text: &[u16],
) -> Result<()> {
    if rect.right <= rect.left || rect.bottom <= rect.top || text.is_empty() {
        return Ok(());
    }
    if unsafe { SetBkMode(dc, TRANSPARENT) } == 0
        || unsafe { SetTextColor(dc, color) }.0 == CLR_INVALID
    {
        return Err(Error::new(E_FAIL, "Could not configure overlay text"));
    }
    let old_font = unsafe { SelectObject(dc, font.0.into()) };
    if invalid_selection(old_font) {
        return Err(Error::new(E_FAIL, "Could not select an overlay font"));
    }
    let result = unsafe {
        ExtTextOutW(
            dc,
            rect.left,
            rect.top,
            ETO_CLIPPED,
            Some(&rect),
            PCWSTR(text.as_ptr()),
            text.len() as u32,
            None,
        )
    };
    // Restore the borrowed previous font even when text drawing failed.
    if invalid_selection(unsafe { SelectObject(dc, old_font) }) {
        return Err(Error::new(E_FAIL, "Could not restore the overlay font"));
    }
    if result.as_bool() {
        Ok(())
    } else {
        Err(Error::new(E_FAIL, "Could not draw overlay text"))
    }
}

pub(super) struct PaintSession {
    hwnd: HWND,
    paint: PAINTSTRUCT,
    pub dc: HDC,
}

impl PaintSession {
    pub fn begin(hwnd: HWND) -> Self {
        let mut paint = PAINTSTRUCT::default();
        let dc = unsafe { BeginPaint(hwnd, &mut paint) };
        Self { hwnd, paint, dc }
    }
}

impl Drop for PaintSession {
    fn drop(&mut self) {
        let _ = unsafe { EndPaint(self.hwnd, &self.paint) };
    }
}
