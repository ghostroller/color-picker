//! GDI drawing resources owned by a single preview window on the UI thread.

use windows::Win32::Foundation::{COLORREF, E_FAIL, HWND, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::core::{Error, Result, w};

use crate::core::color::Rgb8;

pub(super) fn dip(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi) + 48) / 96) as i32
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

struct OwnedFont(HFONT);

impl Drop for OwnedFont {
    fn drop(&mut self) {
        let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
    }
}

struct OwnedBrush(HBRUSH);

impl Drop for OwnedBrush {
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
    _font: OwnedFont,
    background: OwnedBrush,
    old_bitmap: HGDIOBJ,
    old_font: HGDIOBJ,
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
        let font = OwnedFont(unsafe {
            CreateFontW(
                -dip(14, dpi),
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            )
        });
        if font.0.0.is_null() {
            return Err(Error::new(E_FAIL, "Could not create the preview font"));
        }
        let background = OwnedBrush(unsafe { CreateSolidBrush(COLORREF(0x00202020)) });
        if background.0.0.is_null() {
            return Err(Error::new(E_FAIL, "Could not create the preview brush"));
        }
        let old_bitmap = unsafe { SelectObject(dc.0, HGDIOBJ(bitmap.0.0)) };
        if invalid_selection(old_bitmap) {
            return Err(Error::new(E_FAIL, "Could not select the preview bitmap"));
        }
        let old_font = unsafe { SelectObject(dc.0, HGDIOBJ(font.0.0)) };
        if invalid_selection(old_font) {
            let _ = unsafe { SelectObject(dc.0, old_bitmap) };
            return Err(Error::new(E_FAIL, "Could not select the preview font"));
        }
        Ok(Self {
            dc,
            _bitmap: bitmap,
            _font: font,
            background,
            old_bitmap,
            old_font,
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
        if unsafe { FillRect(self.dc.0, &rect, self.background.0) } == 0 {
            return Err(Error::new(E_FAIL, "Could not paint the preview background"));
        }
        // DC_BRUSH is a shared stock object and must never be deleted. Its
        // per-DC color avoids allocating a new brush for each sampled color.
        let swatch_brush = HBRUSH(unsafe { GetStockObject(DC_BRUSH) }.0);
        if swatch_brush.0.is_null() {
            return Err(Error::new(
                E_FAIL,
                "Could not obtain the shared drawing brush",
            ));
        }
        let rgb = content.rgb.unwrap_or(Rgb8 {
            r: 96,
            g: 96,
            b: 96,
        });
        let color = COLORREF(u32::from(rgb.r) | (u32::from(rgb.g) << 8) | (u32::from(rgb.b) << 16));
        self.swatch(
            swatch_brush,
            COLORREF(0x00aaaaaa),
            RECT {
                left: 12,
                top: 12,
                right: 52,
                bottom: 52,
            },
        )?;
        self.swatch(
            swatch_brush,
            color,
            RECT {
                left: 13,
                top: 13,
                right: 51,
                bottom: 51,
            },
        )?;
        if unsafe { SetBkMode(self.dc.0, TRANSPARENT) } == 0 {
            return Err(Error::new(
                E_FAIL,
                "Could not set transparent text background",
            ));
        }
        self.text_color(COLORREF(0x00ffffff))?;
        self.text(64, 23, &content.color_text)?;
        self.text(12, 59, &content.coordinates)?;
        self.text_color(COLORREF(0x00bbbbbb))?;
        self.text(12, 81, &content.help)?;
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
        if unsafe { SetDCBrushColor(self.dc.0, color) }.0 == CLR_INVALID {
            return Err(Error::new(E_FAIL, "Could not set the preview swatch color"));
        }
        let rect = RECT {
            left: dip(rect_dip.left, self.dpi),
            top: dip(rect_dip.top, self.dpi),
            right: dip(rect_dip.right, self.dpi),
            bottom: dip(rect_dip.bottom, self.dpi),
        };
        if unsafe { FillRect(self.dc.0, &rect, brush) } == 0 {
            return Err(Error::new(E_FAIL, "Could not paint the preview swatch"));
        }
        Ok(())
    }

    fn text_color(&self, color: COLORREF) -> Result<()> {
        if unsafe { SetTextColor(self.dc.0, color) }.0 == CLR_INVALID {
            Err(Error::new(E_FAIL, "Could not set the preview text color"))
        } else {
            Ok(())
        }
    }

    fn text(&self, x: i32, y: i32, text: &[u16]) -> Result<()> {
        if unsafe { TextOutW(self.dc.0, dip(x, self.dpi), dip(y, self.dpi), text) }.as_bool() {
            Ok(())
        } else {
            Err(Error::new(E_FAIL, "Could not draw preview text"))
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // Restore borrowed stock objects before fields delete the DC/resources.
        unsafe {
            SelectObject(self.dc.0, self.old_font);
            SelectObject(self.dc.0, self.old_bitmap);
        }
    }
}

fn invalid_selection(object: HGDIOBJ) -> bool {
    object.0.is_null() || object.0 as isize == -1
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
