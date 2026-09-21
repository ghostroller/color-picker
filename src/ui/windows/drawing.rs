//! GDI drawing resources owned by a single preview window on the UI thread.

use windows::Win32::Foundation::{COLORREF, E_FAIL, HWND, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::core::{Error, PCWSTR, Result, w};

use super::frost::FrostedPanel;
use crate::app::config::AppearanceConfig;
use crate::app::i18n::{Language, language};
use crate::core::color::Rgb8;
use crate::core::geometry::ScreenPointPx;

pub(super) fn dip(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi) + 48) / 96) as i32
}

/// Preserve the default 38 DIP footprint while reserving space for wider
/// bottom borders. The text keeps its original baseline and readable area.
pub(super) fn live_preview_height_dip(border_width_dip: u8) -> i32 {
    38 + i32::from(border_width_dip.saturating_sub(2))
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
    pub const TEXT: COLORREF = rgb(0xf8fafc);
    pub const SECONDARY: COLORREF = rgb(0xcbd5e1);
    pub const ACCENT: COLORREF = rgb(0x93c5fd);
    pub const EMPTY: COLORREF = rgb(0x475569);
}

pub(super) struct Content {
    pub rgb: Option<Rgb8>,
    pub color_text: Vec<u16>,
    pub coordinates: Vec<u16>,
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
    pub(super) fn measure(
        &self,
        dc: HDC,
        text: &[u16],
    ) -> Result<windows::Win32::Foundation::SIZE> {
        let old = unsafe { SelectObject(dc, self.0.into()) };
        if invalid_selection(old) {
            return Err(Error::from_thread());
        }
        let mut size = windows::Win32::Foundation::SIZE::default();
        let measured = unsafe { GetTextExtentPoint32W(dc, text, &mut size) };
        unsafe {
            SelectObject(dc, old);
        }
        measured.ok()?;
        Ok(size)
    }

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
                match language() {
                    Language::SimplifiedChinese => w!("Microsoft YaHei UI"),
                    Language::English => w!("Segoe UI"),
                },
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
    frost: Option<FrostedPanel>,
    appearance: AppearanceConfig,
    old_bitmap: HGDIOBJ,
    pub width: i32,
    pub height: i32,
    pub dpi: u32,
}

impl Surface {
    pub fn new(
        width: i32,
        height: i32,
        dpi: u32,
        capture_excluded: bool,
        appearance: AppearanceConfig,
    ) -> Result<Self> {
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
        let heading_font = OwnedFont::new(13, 600, dpi)?;
        let body_font = OwnedFont::new(10, 400, dpi)?;
        let frost = if capture_excluded && appearance.background_transparency_percent != 0 {
            match FrostedPanel::new(
                width - dip(38, dpi),
                height,
                dip(8, dpi),
                appearance.background_transparency_percent,
            ) {
                Ok(panel) => Some(panel),
                Err(error) => {
                    crate::app::diagnostics::event(format_args!(
                        "preview.frost_unavailable {error}"
                    ));
                    None
                }
            }
        } else {
            None
        };
        let old_bitmap = unsafe { SelectObject(dc.0, HGDIOBJ(bitmap.0.0)) };
        if invalid_selection(old_bitmap) {
            return Err(Error::new(E_FAIL, "Could not select the preview bitmap"));
        }
        Ok(Self {
            dc,
            _bitmap: bitmap,
            heading_font,
            body_font,
            frost,
            appearance,
            old_bitmap,
            width,
            height,
            dpi,
        })
    }

    pub fn draw(&self, target: HDC, content: &Content, origin: ScreenPointPx) -> Result<()> {
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
        if let Some(frost) = &self.frost {
            // A decorative backdrop failure keeps the opaque base below it.
            let panel = RECT {
                left: dip(38, self.dpi),
                ..rect
            };
            if frost.paint(self.dc.0, panel, origin).is_err() {
                self.fill(swatch_brush, palette::PANEL, panel)?;
            }
        }
        let color = content.rgb.map_or(palette::EMPTY, |rgb| {
            COLORREF(u32::from(rgb.r) | (u32::from(rgb.g) << 8) | (u32::from(rgb.b) << 16))
        });
        // The swatch remains flush to the window edges, without inset padding.
        self.fill(
            swatch_brush,
            color,
            RECT {
                left: 0,
                top: 0,
                right: dip(38, self.dpi),
                bottom: self.height,
            },
        )?;
        self.text(
            &self.heading_font,
            RECT {
                left: 44,
                top: 2,
                right: 164,
                bottom: 21,
            },
            palette::TEXT,
            &content.color_text,
        )?;
        self.text(
            &self.body_font,
            RECT {
                left: 44,
                top: 21,
                right: 164,
                bottom: 37,
            },
            palette::SECONDARY,
            &content.coordinates,
        )?;
        draw_bottom_right_border(
            self.dc.0,
            self.width,
            self.height,
            self.dpi,
            self.appearance.border_width_dip,
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

/// A configurable edge, painted inside the existing window bounds.
/// The top and left stay open, and neither layout nor pixel hit mapping changes.
pub(super) fn draw_bottom_right_border(
    dc: HDC,
    width: i32,
    height: i32,
    dpi: u32,
    width_dip: u8,
) -> Result<()> {
    let thickness = border_thickness(width, height, dpi, width_dip);
    if thickness == 0 {
        return Ok(());
    }
    let brush = HBRUSH(unsafe { GetStockObject(DC_BRUSH) }.0);
    if brush.is_invalid() || unsafe { SetDCBrushColor(dc, palette::BORDER) }.0 == CLR_INVALID {
        return Err(Error::new(E_FAIL, "Could not configure the overlay border"));
    }
    for edge in [
        RECT {
            left: 0,
            top: height - thickness,
            right: width,
            bottom: height,
        },
        RECT {
            left: width - thickness,
            top: 0,
            right: width,
            bottom: height,
        },
    ] {
        if unsafe { FillRect(dc, &edge, brush) } == 0 {
            return Err(Error::new(E_FAIL, "Could not paint the overlay border"));
        }
    }
    Ok(())
}

pub(super) fn border_thickness(width: i32, height: i32, dpi: u32, width_dip: u8) -> i32 {
    if width_dip == 0 || width <= 0 || height <= 0 {
        0
    } else {
        dip(i32::from(width_dip), dpi).max(1).min(width).min(height)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_live_borders_leave_both_text_lines_intact_at_common_dpis() {
        let content = Content {
            rgb: Some(Rgb8::new(0x49, 0xa7, 0xc6)),
            color_text: "#49A7C6".encode_utf16().collect(),
            coordinates: "X -12345  Y -67890".encode_utf16().collect(),
        };
        assert_eq!(live_preview_height_dip(2), 38);
        for dpi in [96, 120, 144, 168, 192] {
            let width = dip(168, dpi);
            let reference = Surface::new(
                width,
                dip(44, dpi),
                dpi,
                false,
                AppearanceConfig {
                    border_width_dip: 0,
                    background_transparency_percent: 0,
                },
            )
            .unwrap();
            // Draw directly into its own offscreen bitmap: no window, screen
            // capture, focus change or clipboard access is involved.
            reference
                .draw(reference.dc.0, &content, ScreenPointPx { x: 0, y: 0 })
                .unwrap();
            for border in 0..=6 {
                let height = dip(live_preview_height_dip(border), dpi);
                let actual = Surface::new(
                    width,
                    height,
                    dpi,
                    false,
                    AppearanceConfig {
                        border_width_dip: border,
                        background_transparency_percent: 0,
                    },
                )
                .unwrap();
                actual
                    .draw(actual.dc.0, &content, ScreenPointPx { x: 0, y: 0 })
                    .unwrap();
                let mut text_pixels = [0, 0];
                for y in dip(2, dpi)..dip(37, dpi) {
                    for x in dip(44, dpi)..dip(164, dpi) {
                        let expected = unsafe { GetPixel(reference.dc.0, x, y) };
                        if expected != palette::PANEL {
                            let line = usize::from(y >= dip(21, dpi));
                            text_pixels[line] += 1;
                            assert_eq!(
                                unsafe { GetPixel(actual.dc.0, x, y) },
                                expected,
                                "border {border} at {dpi} DPI covered text at ({x}, {y})"
                            );
                        }
                    }
                }
                assert!(
                    text_pixels.into_iter().all(|count| count > 20),
                    "both lines must render"
                );
                if border > 0 {
                    assert_eq!(
                        unsafe { GetPixel(actual.dc.0, width - 1, height / 2) },
                        palette::BORDER
                    );
                    assert_eq!(
                        unsafe { GetPixel(actual.dc.0, width / 2, height - 1) },
                        palette::BORDER
                    );
                }
            }
        }
    }
}
