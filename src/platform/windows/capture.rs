//! Main-thread GDI sampling for the composed, eight-bit SDR desktop.
//! A sampler owns one reusable pixel surface for the duration of Live mode.

use std::{ffi::c_void, marker::PhantomData, ptr::NonNull, rc::Rc};

use windows::{
    Win32::{
        Foundation::{E_FAIL, E_OUTOFMEMORY, POINT},
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC,
            CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC,
            GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, MONITOR_DEFAULTTONULL, MONITORINFO,
            MonitorFromPoint, ReleaseDC, SRCCOPY, SelectObject,
        },
    },
    core::Error,
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
    // Drop first restores previous_bitmap, then Rust drops these fields in
    // declaration order: bitmap -> memory DC -> screen DC.
    _bitmap: OwnedBitmap,
    memory: MemoryDc,
    screen: ScreenDc,
    previous_bitmap: HGDIOBJ,
    pixels: NonNull<u8>,
    _main_thread_only: PhantomData<Rc<()>>,
}

impl GdiSampler {
    pub fn new() -> Result<Self, CaptureError> {
        let screen = ScreenDc::new()?;
        let memory = MemoryDc::new(screen.0)?;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: 1,
                biHeight: -1, // Negative height means top-down scanline order.
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: 4,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut raw_pixels: *mut c_void = std::ptr::null_mut();
        // SAFETY: BITMAPINFO describes one 32-bit pixel; ppvbits is a valid
        // output slot. The returned bitmap guard owns the allocation thereafter.
        let bitmap = OwnedBitmap(unsafe {
            CreateDIBSection(
                Some(screen.0),
                &info,
                DIB_RGB_COLORS,
                &mut raw_pixels,
                None,
                0,
            )?
        });
        let pixels = NonNull::new(raw_pixels.cast::<u8>())
            .ok_or_else(|| api_failure("CreateDIBSection returned null pixel storage"))?;
        // SAFETY: this newly created bitmap is not selected into any other DC.
        // Remember the shared default bitmap but never assume ownership of it.
        let previous_bitmap = unsafe { SelectObject(memory.0, bitmap.0.into()) };
        if previous_bitmap.is_invalid() {
            return Err(api_failure(
                "SelectObject could not select the sampling bitmap",
            ));
        }

        Ok(Self {
            _bitmap: bitmap,
            memory,
            screen,
            previous_bitmap,
            pixels,
            _main_thread_only: PhantomData,
        })
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

        // SAFETY: both DCs and the selected 1x1 destination remain owned by this
        // sampler, on their creating thread. The source point was just checked.
        unsafe {
            BitBlt(
                self.memory.0,
                0,
                0,
                1,
                1,
                Some(self.screen.0),
                point.x,
                point.y,
                SRCCOPY | CAPTUREBLT,
            )?;
            // Flush GDI's current-thread batch before direct CPU access to DIB
            // storage. DwmFlush is a separate overlay-composition concern.
            if !GdiFlush().as_bool() {
                return Err(api_failure("GdiFlush reported a failed GDI operation"));
            }
            // The 32-bit BI_RGB layout is B,G,R,X. Only read the color channels;
            // the fourth byte does not describe an original alpha value.
            let b = self.pixels.as_ptr().read();
            let g = self.pixels.as_ptr().add(1).read();
            let r = self.pixels.as_ptr().add(2).read();
            Ok(Rgb8::new(r, g, b))
        }
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
        // The preceding per-axis limit makes these dimensions and sizes small,
        // positive and representable in both GDI's i32 and usize arithmetic.
        let stride_bytes = width as usize * 4;
        let length = stride_bytes * height as usize;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: length as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut raw_pixels: *mut c_void = std::ptr::null_mut();
        let bitmap = OwnedBitmap(unsafe {
            CreateDIBSection(
                Some(self.screen.0),
                &info,
                DIB_RGB_COLORS,
                &mut raw_pixels,
                None,
                0,
            )?
        });
        let pixels = NonNull::new(raw_pixels.cast::<u8>())
            .ok_or_else(|| api_failure("CreateDIBSection returned null freeze storage"))?;
        let previous = unsafe { SelectObject(self.memory.0, bitmap.0.into()) };
        if previous.is_invalid() {
            return Err(api_failure("Could not select the freeze bitmap"));
        }
        // Declared after bitmap: restore the reusable 1x1 surface before the
        // temporary bitmap is deleted, on both successful and failed captures.
        let _selection = BitmapSelection {
            dc: self.memory.0,
            previous,
        };
        unsafe {
            BitBlt(
                self.memory.0,
                0,
                0,
                width as i32,
                height as i32,
                Some(self.screen.0),
                rect.left,
                rect.top,
                SRCCOPY | CAPTUREBLT,
            )?;
            if !GdiFlush().as_bool() {
                return Err(api_failure("GdiFlush failed while freezing the screen"));
            }
        }
        let mut bgrx = Vec::new();
        bgrx.try_reserve_exact(length).map_err(|_| {
            CaptureError::Api(Error::new(
                E_OUTOFMEMORY,
                "Could not allocate the frozen pixels",
            ))
        })?;
        // The selected DIB owns this exact byte span until the guards above drop.
        bgrx.extend_from_slice(unsafe { std::slice::from_raw_parts(pixels.as_ptr(), length) });
        Ok(FrozenImage {
            origin: ScreenPointPx {
                x: rect.left,
                y: rect.top,
            },
            width,
            height,
            stride_bytes,
            bgrx,
        })
    }
}

struct BitmapSelection {
    dc: HDC,
    previous: HGDIOBJ,
}

impl Drop for BitmapSelection {
    fn drop(&mut self) {
        let _ = unsafe { SelectObject(self.dc, self.previous) };
    }
}

impl Drop for GdiSampler {
    fn drop(&mut self) {
        // SAFETY: memory still exists and owns the selection. Restore before
        // field destructors delete the bitmap and then its DC. The original
        // bitmap belongs to GDI and is never deleted by this sampler.
        let _ = unsafe { SelectObject(self.memory.0, self.previous_bitmap) };
    }
}

fn api_failure(message: &'static str) -> CaptureError {
    // These handle/BOOL APIs do not guarantee meaningful GetLastError values;
    // do not report a stale unrelated last-error as their cause.
    CaptureError::Api(Error::new(E_FAIL, message))
}

struct ScreenDc(HDC);

impl ScreenDc {
    fn new() -> Result<Self, CaptureError> {
        let handle = unsafe { GetDC(None) };
        if handle.is_invalid() {
            Err(CaptureError::DesktopUnavailable)
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        // Paired with GetDC(None), never DeleteDC or CloseHandle.
        let _ = unsafe { ReleaseDC(None, self.0) };
    }
}

struct MemoryDc(HDC);

impl MemoryDc {
    fn new(screen: HDC) -> Result<Self, CaptureError> {
        let handle = unsafe { CreateCompatibleDC(Some(screen)) };
        if handle.is_invalid() {
            Err(api_failure(
                "CreateCompatibleDC could not allocate a memory DC",
            ))
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for MemoryDc {
    fn drop(&mut self) {
        let _ = unsafe { DeleteDC(self.0) };
    }
}

struct OwnedBitmap(HBITMAP);

impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        let _ = unsafe { DeleteObject(self.0.into()) };
    }
}
