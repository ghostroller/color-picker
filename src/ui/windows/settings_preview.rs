//! Settings-only example: every background pixel is generated locally. It uses
//! the overlay's real blur/tint and border helpers without reading the desktop.

use windows::{
    Win32::{
        Foundation::{E_FAIL, RECT},
        Graphics::Gdi::*,
    },
    core::{Error, Result},
};

use super::super::drawing::{
    OwnedFont, dip, draw_bottom_right_border, draw_text, live_preview_height_dip, palette,
};
use crate::app::{config::AppearanceConfig, i18n::tr};
use crate::platform::windows::gdi::SavedDc;
use crate::ui::pixel_effects::blur_and_tint;

// Fill the widest settings content area (500 DIP minus two 40 DIP insets).
// Narrower panes crop this generated background while keeping the overlay centered.
pub(super) const WIDTH: i32 = 420;
pub(super) const HEIGHT: i32 = 72;
const OVERLAY_WIDTH: i32 = 168;

pub(super) struct AppearancePreview {
    dpi: u32,
    appearance: AppearanceConfig,
    pixels: Vec<u8>,
    heading: OwnedFont,
    body: OwnedFont,
}

impl AppearancePreview {
    pub(super) fn new(dpi: u32, appearance: AppearanceConfig) -> Result<Self> {
        Ok(Self {
            dpi,
            appearance,
            pixels: example_pixels(dpi, appearance),
            heading: OwnedFont::new(13, 600, dpi)?,
            body: OwnedFont::new(10, 400, dpi)?,
        })
    }

    pub(super) fn matches(&self, dpi: u32, appearance: AppearanceConfig) -> bool {
        self.dpi == dpi && self.appearance == appearance
    }

    pub(super) fn paint(&self, dc: HDC, bounds: RECT) -> Result<()> {
        let width = dip(WIDTH, self.dpi);
        let height = dip(HEIGHT, self.dpi);
        let visible_width = (bounds.right - bounds.left).min(width);
        let overlay_width = dip(OVERLAY_WIDTH, self.dpi);
        let source_x = (width - overlay_width) / 2 - (visible_width - overlay_width) / 2;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: this synchronous owner-draw DC remains live until paint returns.
        let saved = unsafe { SavedDc::new(dc)? };
        let result = (|| {
            // SAFETY: The local exact BGRX fixture covers BITMAPINFO dimensions and remains borrowed for the native copy.
            if unsafe {
                SetDIBitsToDevice(
                    dc,
                    bounds.left,
                    bounds.top,
                    visible_width as u32,
                    height as u32,
                    source_x,
                    0,
                    0,
                    height as u32,
                    self.pixels.as_ptr().cast(),
                    &info,
                    DIB_RGB_COLORS,
                )
            } == 0
            {
                return Err(Error::new(
                    E_FAIL,
                    tr("无法绘制外观示例", "Could not draw appearance example"),
                ));
            }
            // Crop only the generated background on narrow panes. The example
            // overlay keeps its real DPI font size and stays centered/visible.
            let x = bounds.left + (visible_width - dip(OVERLAY_WIDTH, self.dpi)) / 2;
            let overlay_height = dip(
                live_preview_height_dip(self.appearance.border_width_dip),
                self.dpi,
            );
            let y = bounds.top + (height - overlay_height) / 2;
            // SAFETY: The borrowed owner-draw DC remains live and SavedDc restores the viewport before return.
            unsafe {
                SetViewportOrgEx(dc, x, y, None).ok()?;
            }
            let rect = |left, top, right, bottom| RECT {
                left: dip(left, self.dpi),
                top: dip(top, self.dpi),
                right: dip(right, self.dpi),
                bottom: dip(bottom, self.dpi),
            };
            draw_text(
                dc,
                &self.heading,
                rect(44, 2, 164, 21),
                palette::TEXT,
                &"#49A7C6".encode_utf16().collect::<Vec<_>>(),
            )?;
            draw_text(
                dc,
                &self.body,
                rect(44, 21, 164, 37),
                palette::SECONDARY,
                &"X 1280  Y 720".encode_utf16().collect::<Vec<_>>(),
            )?;
            draw_bottom_right_border(
                dc,
                dip(OVERLAY_WIDTH, self.dpi),
                overlay_height,
                self.dpi,
                self.appearance.border_width_dip,
            )
        })();
        saved.restore()?;
        result
    }
}

fn example_pixels(dpi: u32, appearance: AppearanceConfig) -> Vec<u8> {
    let width = dip(WIDTH, dpi) as usize;
    let height = dip(HEIGHT, dpi) as usize;
    let mut pixels = vec![0xff; width * height * 4];
    let tile = dip(24, dpi).max(1) as usize;
    for y in 0..height {
        for x in 0..width {
            // Broad pastel tiles make the effect visible without visual noise.
            let color = if (x / tile + y / tile).is_multiple_of(2) {
                [239, 225, 199]
            } else {
                [220, 204, 163]
            };
            pixels[(y * width + x) * 4..][..3].copy_from_slice(&color);
        }
    }
    let overlay_width = dip(OVERLAY_WIDTH, dpi) as usize;
    let overlay_height = dip(live_preview_height_dip(appearance.border_width_dip), dpi) as usize;
    let left = (width - overlay_width) / 2;
    let top = (height - overlay_height) / 2;
    let swatch_width = dip(38, dpi) as usize;
    let panel_width = overlay_width - swatch_width;
    let mut panel = vec![0xff; panel_width * overlay_height * 4];
    for y in 0..overlay_height {
        let source = ((top + y) * width + left + swatch_width) * 4;
        panel[y * panel_width * 4..(y + 1) * panel_width * 4]
            .copy_from_slice(&pixels[source..source + panel_width * 4]);
    }
    let mut scratch = vec![0; panel.len()];
    blur_and_tint(
        &mut panel,
        &mut scratch,
        panel_width,
        overlay_height,
        dip(8, dpi).max(1) as usize,
        appearance.background_transparency_percent,
    )
    .expect("validated appearance and locally allocated exact BGRX fixture dimensions");
    for y in 0..overlay_height {
        for x in 0..swatch_width {
            let position = ((top + y) * width + left + x) * 4;
            pixels[position..position + 3].copy_from_slice(&[0xc6, 0xa7, 0x49]);
        }
        let target = ((top + y) * width + left + swatch_width) * 4;
        pixels[target..target + panel_width * 4]
            .copy_from_slice(&panel[y * panel_width * 4..(y + 1) * panel_width * 4]);
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_transparency_only_changes_the_information_panel() {
        for dpi in [96, 144, 192] {
            let opaque = example_pixels(
                dpi,
                AppearanceConfig {
                    background_transparency_percent: 0,
                    ..AppearanceConfig::default()
                },
            );
            let transparent = example_pixels(
                dpi,
                AppearanceConfig {
                    background_transparency_percent: 80,
                    ..AppearanceConfig::default()
                },
            );
            let width = dip(WIDTH, dpi) as usize;
            let height = dip(HEIGHT, dpi) as usize;
            let left = (width - dip(OVERLAY_WIDTH, dpi) as usize) / 2;
            let top = (height
                - dip(
                    live_preview_height_dip(AppearanceConfig::default().border_width_dip),
                    dpi,
                ) as usize)
                / 2;
            let at = |x, y| (y * width + x) * 4;
            let swatch = at(left + dip(10, dpi) as usize, top + dip(10, dpi) as usize);
            assert_eq!(&opaque[swatch..swatch + 4], &[0xc6, 0xa7, 0x49, 255]);
            assert_eq!(
                &opaque[swatch..swatch + 4],
                &transparent[swatch..swatch + 4]
            );
            assert_eq!(&opaque[..width * 4], &transparent[..width * 4]);
            let info = at(left + dip(50, dpi) as usize, top + dip(10, dpi) as usize);
            assert_eq!(&opaque[info..info + 4], &[51, 32, 23, 255]);
            assert_ne!(&opaque[info..info + 4], &transparent[info..info + 4]);
        }
    }
}
