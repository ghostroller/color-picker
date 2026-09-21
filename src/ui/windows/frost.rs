//! Small, reusable backdrop surface for the information strip only.
//!
//! Used only when the overlay has WDA_EXCLUDEFROMCAPTURE. SRCCOPY alone does
//! not omit layered windows on modern Windows. Refresh only when moved, never
//! with an extra timer; a frozen window keeps one cached backdrop.

use std::{cell::RefCell, ffi::c_void, ptr::NonNull};

use windows::{
    Win32::{
        Foundation::{E_FAIL, E_INVALIDARG, RECT},
        Graphics::Gdi::*,
    },
    core::{Error, Result},
};

use crate::{app::config::MAX_BACKGROUND_TRANSPARENCY_PERCENT, core::geometry::ScreenPointPx};

pub(super) struct FrostedPanel {
    bitmap: HBITMAP,
    dc: HDC,
    screen: HDC,
    previous: HGDIOBJ,
    pixels: NonNull<u8>,
    cache: RefCell<Cache>,
    width: i32,
    height: i32,
    radius: usize,
    transparency_percent: u8,
}

struct Cache {
    scratch: Vec<u8>,
    origin: Option<ScreenPointPx>,
}

impl FrostedPanel {
    pub fn new(width: i32, height: i32, radius: i32, transparency_percent: u8) -> Result<Self> {
        if transparency_percent > MAX_BACKGROUND_TRANSPARENCY_PERCENT {
            return Err(Error::new(E_INVALIDARG, "Invalid backdrop transparency"));
        }
        if width <= 0
            || height <= 0
            || width
                .checked_mul(height)
                .and_then(|n| n.checked_mul(4))
                .is_none()
        {
            return Err(Error::new(E_FAIL, "Invalid backdrop dimensions"));
        }
        let screen = unsafe { GetDC(None) };
        if screen.is_invalid() {
            return Err(Error::new(E_FAIL, "Could not obtain backdrop DC"));
        }
        let dc = unsafe { CreateCompatibleDC(Some(screen)) };
        if dc.is_invalid() {
            unsafe { ReleaseDC(None, screen) };
            return Err(Error::new(E_FAIL, "Could not create backdrop DC"));
        }
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
        let mut raw: *mut c_void = std::ptr::null_mut();
        let bitmap = match unsafe {
            CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut raw, None, 0)
        } {
            Ok(bitmap) => bitmap,
            Err(error) => {
                unsafe {
                    let _ = DeleteDC(dc);
                    ReleaseDC(None, screen);
                }
                return Err(error);
            }
        };
        let previous = unsafe { SelectObject(dc, bitmap.into()) };
        let Some(pixels) = NonNull::new(raw.cast()) else {
            unsafe {
                SelectObject(dc, previous);
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(dc);
                ReleaseDC(None, screen);
            }
            return Err(Error::new(E_FAIL, "Backdrop bitmap has no pixel storage"));
        };
        let panel = Self {
            bitmap,
            dc,
            screen,
            previous,
            pixels,
            cache: RefCell::new(Cache {
                scratch: vec![0; (width * height * 4) as usize],
                origin: None,
            }),
            width,
            height,
            radius: radius.max(1) as usize,
            transparency_percent,
        };
        if previous.is_invalid() {
            return Err(Error::new(E_FAIL, "Could not select backdrop bitmap"));
        }
        Ok(panel)
    }

    pub fn paint(&self, target: HDC, rect: RECT, origin: ScreenPointPx) -> Result<()> {
        let mut cache = self.cache.borrow_mut();
        let origin = ScreenPointPx {
            x: origin.x + rect.left,
            y: origin.y + rect.top,
        };
        unsafe {
            if cache.origin != Some(origin) {
                cache.origin = None;
                // The caller enabled capture exclusion before constructing us.
                // Keep other applications' layered content in the decorative image.
                BitBlt(
                    self.dc,
                    0,
                    0,
                    self.width,
                    self.height,
                    Some(self.screen),
                    origin.x,
                    origin.y,
                    SRCCOPY | CAPTUREBLT,
                )?;
                if !GdiFlush().as_bool() {
                    return Err(Error::new(E_FAIL, "Could not flush backdrop capture"));
                }
                let pixels = std::slice::from_raw_parts_mut(
                    self.pixels.as_ptr(),
                    (self.width * self.height * 4) as usize,
                );
                blur_and_tint(
                    pixels,
                    &mut cache.scratch,
                    self.width as usize,
                    self.height as usize,
                    self.radius,
                    self.transparency_percent,
                );
                cache.origin = Some(origin);
            }
            BitBlt(
                target,
                rect.left,
                rect.top,
                self.width,
                self.height,
                Some(self.dc),
                0,
                0,
                SRCCOPY,
            )
        }
    }
}

impl Drop for FrostedPanel {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.is_invalid() {
                SelectObject(self.dc, self.previous);
            }
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
            ReleaseDC(None, self.screen);
        }
    }
}

pub(super) fn blur_and_tint(
    pixels: &mut [u8],
    scratch: &mut [u8],
    width: usize,
    height: usize,
    radius: usize,
    transparency_percent: u8,
) {
    let backdrop_opacity = (u32::from(transparency_percent) * 255 + 50) / 100;
    let tint_opacity = 255 - backdrop_opacity;
    // Sliding box filters keep the cost linear in this small information strip.
    let count = (2 * radius + 1) as u32;
    for y in 0..height {
        for channel in 0..3 {
            let at = |x: usize| pixels[(y * width + x) * 4 + channel] as u32;
            let mut sum = (radius as u32 + 1) * at(0);
            for x in 1..=radius {
                sum += at(x.min(width - 1));
            }
            for x in 0..width {
                scratch[(y * width + x) * 4 + channel] = (sum / count) as u8;
                sum -= at(x.saturating_sub(radius));
                sum += at((x + radius + 1).min(width - 1));
            }
        }
    }
    for x in 0..width {
        for (channel, tint) in [51_u32, 32, 23].into_iter().enumerate() {
            let at = |y: usize| scratch[(y * width + x) * 4 + channel] as u32;
            let mut sum = (radius as u32 + 1) * at(0);
            for y in 1..=radius {
                sum += at(y.min(height - 1));
            }
            for y in 0..height {
                // Transparency controls only the cached information backdrop;
                // sampled image pixels, swatches and text remain fully opaque.
                let value = ((sum / count) * backdrop_opacity + tint * tint_opacity + 127) / 255;
                pixels[(y * width + x) * 4 + channel] = value as u8;
                sum -= at(y.saturating_sub(radius));
                sum += at((y + radius + 1).min(height - 1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::blur_and_tint;

    #[test]
    fn frosted_background_softens_edges_without_losing_tint_or_small_image_support() {
        for (percent, expected) in [
            (0, [51, 32, 23, 255]),
            (35, [122, 110, 104, 255]),
            (80, [214, 210, 209, 255]),
        ] {
            for (width, height) in [(1, 1), (1, 7), (7, 1), (12, 8)] {
                let mut pixels = vec![255; width * height * 4];
                let mut scratch = vec![0; pixels.len()];
                blur_and_tint(&mut pixels, &mut scratch, width, height, 9, percent);
                for pixel in pixels.chunks_exact(4) {
                    assert_eq!(pixel, &expected);
                }
            }
        }
        let mut pixels = vec![0; 12 * 4];
        pixels[6 * 4..].fill(255);
        let mut scratch = vec![0; pixels.len()];
        blur_and_tint(&mut pixels, &mut scratch, 12, 1, 3, 35);
        assert!(pixels[5 * 4] > pixels[0]);
        assert!(pixels[6 * 4] < pixels[11 * 4]);
        assert!(pixels[5 * 4] <= pixels[6 * 4]);
    }
}
