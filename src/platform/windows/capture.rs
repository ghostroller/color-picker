//! Main-thread GDI sampling for the composed, eight-bit SDR desktop.
//! A sampler owns one reusable pixel surface for the duration of Live mode.

use windows::{
    Win32::{
        Foundation::{E_FAIL, POINT},
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint},
    },
    core::Error,
};

use super::{
    dib::{Dib32Layout, Dib32Surface, OwnedDib32},
    gdi::DesktopDc,
};

use crate::core::{
    color::Rgb8,
    geometry::{MAX_FREEZE_SIDE_PX, ScreenPointPx, ScreenRectPx},
    zoom::FrozenImage,
};

#[derive(Debug)]
pub enum CaptureError {
    /// GetDC could not obtain the desktop DC. No black/previous pixel is returned.
    DesktopUnavailable,
    /// The physical screen point is outside every display, including display gaps.
    NoMonitor,
    /// Freeze capture must be nonempty and bounded by `MAX_FREEZE_SIDE_PX` per axis.
    InvalidRectangle,
    Api(Error),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DesktopUnavailable => formatter.write_str("the desktop DC is unavailable"),
            Self::NoMonitor => {
                formatter.write_str("the source is not contained in one actual monitor")
            }
            Self::InvalidRectangle => write!(
                formatter,
                "freeze capture requires a nonempty area no larger than {MAX_FREEZE_SIDE_PX} by {MAX_FREEZE_SIDE_PX} pixels",
            ),
            Self::Api(error) => write!(formatter, "screen capture failed: {error}"),
        }
    }
}

impl std::error::Error for CaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Api(error) => Some(error),
            Self::DesktopUnavailable | Self::NoMonitor | Self::InvalidRectangle => None,
        }
    }
}

impl From<Error> for CaptureError {
    fn from(error: Error) -> Self {
        Self::Api(error)
    }
}

/// Owns thread-bound GDI resources. Create and use on the application's UI thread.
/// No image/DC allocation or color formatting occurs in sample_pixel.
pub struct GdiSampler {
    surface: Dib32Surface,
    screen: DesktopDc,
}

impl GdiSampler {
    pub fn new() -> Result<Self, CaptureError> {
        let screen = DesktopDc::new().map_err(|_| CaptureError::DesktopUnavailable)?;
        let surface = Dib32Surface::new(screen.raw(), Dib32Layout::new(1, 1)?)?;
        Ok(Self { surface, screen })
    }

    /// Returns a fresh pixel or an error. The caller must hide any intersecting
    /// owned overlay before sampling and keep coordinates in physical pixels.
    pub fn sample_pixel(&mut self, point: ScreenPointPx) -> Result<Rgb8, CaptureError> {
        // SAFETY: MonitorFromPoint accepts physical signed desktop coordinates;
        // DEFAULTTONULL deliberately does not snap gaps to a nearby monitor.
        if unsafe {
            MonitorFromPoint(
                POINT {
                    x: point.x,
                    y: point.y,
                },
                MONITOR_DEFAULTTONULL,
            )
        }
        .is_invalid()
        {
            return Err(CaptureError::NoMonitor);
        }

        self.surface.capture_from(&self.screen, point)?;
        // CPU access checks GdiFlush and borrows the reusable surface exclusively.
        Ok(self
            .surface
            .with_pixels(|pixels| Rgb8::new(pixels[2], pixels[1], pixels[0]))?)
    }

    /// Copy one small physical-pixel area into an immutable top-down snapshot.
    /// The caller clips to a monitor and hides/flushed owned overlays first.
    /// This validation rejects crossing monitors even if their edges touch.
    pub fn capture_rect(&mut self, rect: ScreenRectPx) -> Result<FrozenImage, CaptureError> {
        if rect.is_empty()
            || rect.width() > MAX_FREEZE_SIDE_PX
            || rect.height() > MAX_FREEZE_SIDE_PX
        {
            return Err(CaptureError::InvalidRectangle);
        }
        // SAFETY: physical signed coordinates are values; no pointer is retained.
        let monitor = unsafe {
            MonitorFromPoint(
                POINT {
                    x: rect.left,
                    y: rect.top,
                },
                MONITOR_DEFAULTTONULL,
            )
        };
        if monitor.is_invalid() {
            return Err(CaptureError::NoMonitor);
        }
        let mut monitor_info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: monitor was returned above and monitor_info has the API-required size.
        if !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
            return Err(api_failure("Could not validate the freeze monitor"));
        }
        let monitor = monitor_info.rcMonitor;
        if rect.left < monitor.left
            || rect.top < monitor.top
            || rect.right > monitor.right
            || rect.bottom > monitor.bottom
        {
            return Err(CaptureError::NoMonitor);
        }
        let width = rect.width();
        let height = rect.height();
        // Freeze policy above remains stricter than the shared storage limits.
        let layout = Dib32Layout::new(width as i32, height as i32)?;
        let mut temporary = OwnedDib32::new(self.screen.raw(), layout)?;
        let origin = ScreenPointPx {
            x: rect.left,
            y: rect.top,
        };
        let bgrx = self.surface.with_temporary(&mut temporary, |selection| {
            selection.capture_from(&self.screen, origin)?;
            selection.copy_pixels()
        })?;
        Ok(FrozenImage {
            origin,
            width,
            height,
            stride_bytes: layout.stride_bytes(),
            bgrx,
        })
    }
}

fn api_failure(message: &'static str) -> CaptureError {
    // These handle/BOOL APIs do not guarantee meaningful GetLastError values;
    // do not report a stale unrelated last-error as their cause.
    CaptureError::Api(Error::new(E_FAIL, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::windows::{
        dib::test_support as dib_test,
        gdi::{BitmapDc, test_support as gdi_test},
    };
    use windows::Win32::{Foundation::COLORREF, Graphics::Gdi::SetPixel};

    #[test]
    fn sampling_reuses_one_pixel_and_freeze_reuses_its_dc() {
        let screen = DesktopDc::new().unwrap();
        let source = BitmapDc::compatible(screen.raw(), 4, 4).unwrap();
        // SAFETY: the synthetic source owns this private DC/bitmap and no CPU
        // slices exist; write one known color for every capture in this test.
        let color = unsafe { SetPixel(source.raw().unwrap(), 0, 0, COLORREF(0x00563412)) };
        assert_eq!(color, COLORREF(0x00563412));
        dib_test::with_source(&source, || {
            let mut sampler = GdiSampler::new().unwrap();
            let point = ScreenPointPx { x: 0, y: 0 };
            let rect = ScreenRectPx {
                left: 0,
                top: 0,
                right: 2,
                bottom: 2,
            };
            let expected = Rgb8::new(0x12, 0x34, 0x56);
            gdi_test::fail_next(gdi_test::Failure::Memory);
            for _ in 0..32 {
                assert_eq!(sampler.sample_pixel(point).unwrap(), expected);
            }
            assert_eq!(
                sampler.capture_rect(rect).unwrap().pixel_at(0, 0),
                Some(expected)
            );
            // Neither live samples nor a temporary freeze created an extra DC:
            // the injected failure is still waiting for this explicit creation.
            assert!(BitmapDc::compatible(screen.raw(), 1, 1).is_err());
            dib_test::fail_next(dib_test::Failure::Create);
            assert_eq!(sampler.sample_pixel(point).unwrap(), expected);
            assert!(sampler.capture_rect(rect).is_err());
            for fault in [dib_test::Failure::BitBlt, dib_test::Failure::Flush] {
                dib_test::fail_next(fault);
                assert!(sampler.sample_pixel(point).is_err());
                assert_eq!(sampler.sample_pixel(point).unwrap(), expected);
            }
        });
    }
}
