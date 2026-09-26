//! Validated top-down 32-bit BI_RGB storage and exclusive CPU/GDI access.
//! The fourth BGRX byte is initialized storage, never source-image alpha.

use std::{ffi::c_void, ptr::NonNull};

use windows::{
    Win32::{
        Foundation::E_OUTOFMEMORY,
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateDIBSection,
            DIB_RGB_COLORS, GdiFlush, HDC, HGDIOBJ, SRCCOPY,
        },
    },
    core::{Error, Result},
};

use super::gdi::{BitmapDc, DesktopDc, OwnedBitmap, failure};
use crate::core::geometry::ScreenPointPx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Dib32Layout {
    width: i32,
    height: i32,
    stride_bytes: usize,
    len: usize,
}

impl Dib32Layout {
    pub fn new(width: i32, height: i32) -> Result<Self> {
        if width <= 0 || height <= 0 {
            return Err(failure("DIB dimensions must be positive"));
        }
        let stride_bytes = (width as usize)
            .checked_mul(4)
            .ok_or_else(|| failure("DIB stride overflow"))?;
        let len = stride_bytes
            .checked_mul(height as usize)
            .filter(|len| *len <= isize::MAX as usize && u32::try_from(*len).is_ok())
            .ok_or_else(|| failure("DIB allocation exceeds supported byte length"))?;
        Ok(Self {
            width,
            height,
            stride_bytes,
            len,
        })
    }
    pub fn width(self) -> i32 {
        self.width
    }
    pub fn height(self) -> i32 {
        self.height
    }
    pub fn stride_bytes(self) -> usize {
        self.stride_bytes
    }
    pub fn len(self) -> usize {
        self.len
    }
    fn info(self) -> BITMAPINFO {
        BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: self.width,
                biHeight: -self.height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: self.len as u32,
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

/// The only owner of an unselected DIB and its initialized native pixel storage.
pub(crate) struct OwnedDib32 {
    bitmap: OwnedBitmap,
    pixels: PixelStorage,
}

// This address points into the bitmap's native allocation, not into a Rust field.
// It may be moved only together with the unique bitmap owner.
struct PixelStorage {
    address: NonNull<u8>,
    layout: Dib32Layout,
    readable: bool,
}

impl OwnedDib32 {
    pub fn new(reference: HDC, layout: Dib32Layout) -> Result<Self> {
        #[cfg(test)]
        test_support::check(test_support::Failure::Create)?;
        let mut raw: *mut c_void = std::ptr::null_mut();
        // SAFETY: the checked layout describes positive top-down BGRX rows;
        // raw is an output slot and no file-mapping handle is retained.
        let handle = unsafe {
            CreateDIBSection(
                Some(reference),
                &layout.info(),
                DIB_RGB_COLORS,
                &mut raw,
                None,
                0,
            )?
        };
        // SAFETY: this fresh, unselected CreateDIBSection bitmap has one owner.
        let bitmap = unsafe { OwnedBitmap::from_raw(handle)? };
        #[cfg(test)]
        if test_support::take_failure(test_support::Failure::NullStorage) {
            // Exercise the real null-output validation while retaining a real
            // bitmap owner so its partial-construction cleanup is also tested.
            raw = std::ptr::null_mut();
        }
        let address = NonNull::new(raw.cast::<u8>())
            .ok_or_else(|| failure("CreateDIBSection returned no pixel storage"))?;
        // SAFETY: the bitmap owns exactly layout.len writable bytes. It has not
        // been selected or handed to GDI; initialize all bytes before any slice.
        unsafe { address.as_ptr().write_bytes(0, layout.len) };
        Ok(Self {
            bitmap,
            pixels: PixelStorage {
                address,
                layout,
                readable: true,
            },
        })
    }
}

impl PixelStorage {
    fn synchronize(&mut self) -> Result<()> {
        if !self.readable {
            return Err(failure("DIB contains no successfully captured pixels"));
        }
        if let Err(error) = flush() {
            self.readable = false;
            return Err(error);
        }
        Ok(())
    }

    /// # Safety
    /// The corresponding bitmap is alive, exclusively borrowed and its private
    /// DC cannot be accessed until this closure returns. GDI writes have stopped.
    unsafe fn with_pixels<R>(&mut self, read: impl FnOnce(&[u8]) -> R) -> Result<R> {
        self.synchronize()?;
        // SAFETY: creation initialized every byte; synchronization finished GDI
        // access; validated length and bitmap owner cover the entire closure.
        let pixels = unsafe { std::slice::from_raw_parts(self.address.as_ptr(), self.layout.len) };
        Ok(read(pixels))
    }

    /// # Safety
    /// Same owner/exclusive-DC requirements as with_pixels, with no other pixel
    /// references alive. The closure cannot retain a reference beyond this call.
    unsafe fn with_pixels_mut<R>(&mut self, write: impl FnOnce(&mut [u8]) -> R) -> Result<R> {
        self.synchronize()?;
        // SAFETY: exclusive surface borrow excludes CPU/GDI aliases; allocation
        // is initialized, synchronized and lives for the nonescaping closure.
        let pixels =
            unsafe { std::slice::from_raw_parts_mut(self.address.as_ptr(), self.layout.len) };
        Ok(write(pixels))
    }
}

/// A DIB selected into its own thread-bound DC. BitmapDc owns the bitmap;
/// pixels is only metadata, never an independent allocation/deletion owner.
pub(crate) struct Dib32Surface {
    backing: BitmapDc,
    pixels: PixelStorage,
}

impl Dib32Surface {
    pub fn new(reference: HDC, layout: Dib32Layout) -> Result<Self> {
        let OwnedDib32 { bitmap, pixels } = OwnedDib32::new(reference, layout)?;
        Ok(Self {
            backing: BitmapDc::new(reference, bitmap)?,
            pixels,
        })
    }

    pub fn capture_from(&mut self, source: &DesktopDc, origin: ScreenPointPx) -> Result<()> {
        self.pixels.readable = false;
        // SAFETY: exclusive borrow prevents CPU access and temporary selections;
        // the destination DC and selected bitmap remain owned by backing.
        let dc = unsafe { self.backing.raw()? };
        capture(dc, source, origin, self.pixels.layout)?;
        self.pixels.readable = true;
        Ok(())
    }

    pub fn with_pixels<R>(&mut self, read: impl FnOnce(&[u8]) -> R) -> Result<R> {
        // SAFETY: validate the backing before exposing its still-owned storage.
        unsafe { self.backing.raw()? };
        // SAFETY: this exclusive surface borrow covers the owner and private DC;
        // the callback receives no handle and its slice cannot escape.
        unsafe { self.pixels.with_pixels(read) }
    }

    pub fn with_pixels_mut<R>(&mut self, write: impl FnOnce(&mut [u8]) -> R) -> Result<R> {
        // SAFETY: reject surfaces whose failed restoration destroyed the DC.
        unsafe { self.backing.raw()? };
        // SAFETY: the whole surface is exclusively borrowed until callback ends.
        unsafe { self.pixels.with_pixels_mut(write) }
    }

    /// # Safety
    /// target is a live destination DC on this thread, distinct from this private
    /// DC. Caller may not retain or alias this surface's native handles.
    pub unsafe fn blit_to(&mut self, target: HDC, x: i32, y: i32) -> Result<()> {
        if !self.pixels.readable {
            return Err(failure("DIB pixels are invalid"));
        }
        // SAFETY: source remains selected/alive; caller supplies the live target.
        unsafe {
            BitBlt(
                target,
                x,
                y,
                self.pixels.layout.width,
                self.pixels.layout.height,
                Some(self.backing.raw()?),
                0,
                0,
                SRCCOPY,
            )
        }
    }

    /// Temporarily select a DIB into the reusable DC. Restoration is attempted
    /// before returning any result, including allocation/capture/flush errors.
    pub fn with_temporary<R>(
        &mut self,
        temporary: &mut OwnedDib32,
        operation: impl FnOnce(&mut TemporaryDibSelection<'_>) -> Result<R>,
    ) -> Result<R> {
        // SAFETY: temporary's unique bitmap is unselected and both exclusive
        // borrows are held by the guard until explicit restoration or Drop.
        let previous = unsafe { self.backing.select_temporary(&temporary.bitmap)? };
        let mut selected = TemporaryDibSelection {
            surface: self,
            temporary,
            previous: Some(previous),
        };
        let result = operation(&mut selected);
        selected.restore()?;
        result
    }
}

pub(crate) struct TemporaryDibSelection<'a> {
    surface: &'a mut Dib32Surface,
    temporary: &'a mut OwnedDib32,
    previous: Option<HGDIOBJ>,
}

impl TemporaryDibSelection<'_> {
    pub fn capture_from(&mut self, source: &DesktopDc, origin: ScreenPointPx) -> Result<()> {
        self.temporary.pixels.readable = false;
        // SAFETY: guard holds both exclusive owners and prevents surface reuse.
        let dc = unsafe { self.surface.backing.raw()? };
        capture(dc, source, origin, self.temporary.pixels.layout)?;
        self.temporary.pixels.readable = true;
        Ok(())
    }

    pub fn copy_pixels(&mut self) -> Result<Vec<u8>> {
        let copy = |pixels: &[u8]| {
            #[cfg(test)]
            test_support::check(test_support::Failure::CopyAllocation)?;
            let mut copy = Vec::new();
            copy.try_reserve_exact(pixels.len())
                .map_err(|_| Error::new(E_OUTOFMEMORY, "Could not allocate captured pixels"))?;
            copy.extend_from_slice(pixels);
            Ok(copy)
        };
        // SAFETY: the guard exclusively owns the active selection for this call;
        // temporary's bitmap lives through the copy and restoration.
        unsafe { self.temporary.pixels.with_pixels(copy)? }
    }

    fn restore(&mut self) -> Result<()> {
        if let Some(previous) = self.previous.take() {
            // SAFETY: previous came from this guard's select_temporary call;
            // both exclusively borrowed bitmap owners remain alive until return.
            unsafe {
                self.surface
                    .backing
                    .restore_temporary(&mut self.temporary.bitmap, previous)?;
            }
        }
        Ok(())
    }
}

impl Drop for TemporaryDibSelection<'_> {
    fn drop(&mut self) {
        if self.restore().is_err() {
            crate::app::diagnostics::event(format_args!(
                "dib.temporary_restore_failed surface_invalidated"
            ));
        }
    }
}

fn capture(dc: HDC, source: &DesktopDc, origin: ScreenPointPx, layout: Dib32Layout) -> Result<()> {
    #[cfg(test)]
    test_support::check(test_support::Failure::BitBlt)?;
    let source_dc = source.raw();
    #[cfg(test)]
    let source_dc = test_support::source().unwrap_or(source_dc);
    // SAFETY: only exclusive surface/selection methods reach this helper with
    // their private DC; the borrowed desktop DC lives throughout the operation.
    unsafe {
        BitBlt(
            dc,
            0,
            0,
            layout.width,
            layout.height,
            Some(source_dc),
            origin.x,
            origin.y,
            SRCCOPY | CAPTUREBLT,
        )
    }
}

fn flush() -> Result<()> {
    #[cfg(test)]
    test_support::check(test_support::Failure::Flush)?;
    // SAFETY: flush this thread's GDI operations before directly viewing storage.
    if unsafe { GdiFlush() }.as_bool() {
        Ok(())
    } else {
        Err(failure("GdiFlush reported a failed GDI operation"))
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::BitmapDc;
    use super::failure;
    use std::cell::Cell;
    use windows::Win32::Graphics::Gdi::HDC;
    use windows::core::Result;
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Failure {
        Create,
        NullStorage,
        BitBlt,
        Flush,
        CopyAllocation,
    }
    thread_local! { static NEXT: Cell<Option<Failure>> = const { Cell::new(None) }; }
    thread_local! { static SOURCE: Cell<Option<HDC>> = const { Cell::new(None) }; }
    pub(crate) fn with_source<R>(source: &BitmapDc, run: impl FnOnce() -> R) -> R {
        struct RestoreSource(Option<HDC>);
        impl Drop for RestoreSource {
            fn drop(&mut self) {
                SOURCE.with(|source| source.set(self.0));
            }
        }
        // SAFETY: this scoped, thread-local override borrows a real test-owned
        // DC for the entire run. No CPU view or selection change is exposed here.
        let dc = unsafe { source.raw().unwrap() };
        let _restore = RestoreSource(SOURCE.with(|source| source.replace(Some(dc))));
        run()
    }
    pub(super) fn source() -> Option<HDC> {
        SOURCE.with(Cell::get)
    }
    pub(crate) fn fail_next(failure: Failure) {
        NEXT.with(|next| next.set(Some(failure)));
    }
    pub(super) fn check(failure_point: Failure) -> Result<()> {
        if take_failure(failure_point) {
            Err(failure("Injected DIB operation failure"))
        } else {
            Ok(())
        }
    }
    pub(super) fn take_failure(failure_point: Failure) -> bool {
        NEXT.with(|next| {
            if next.get() == Some(failure_point) {
                next.set(None);
                true
            } else {
                false
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_rejects_invalid_dimensions_before_native_allocation() {
        for (width, height) in [
            (0, 1),
            (1, 0),
            (-1, 1),
            (1, i32::MIN),
            (i32::MAX, i32::MAX),
            (i32::MAX, 1),
        ] {
            assert!(Dib32Layout::new(width, height).is_err());
        }
        let layout = Dib32Layout::new(17, 9).unwrap();
        assert_eq!(layout.stride_bytes(), 68);
        assert_eq!(layout.len(), 612);
        assert_eq!(layout.info().bmiHeader.biHeight, -9);
    }

    #[test]
    fn storage_is_initialized_and_failures_do_not_expose_old_pixels() {
        let screen = DesktopDc::new().unwrap();
        let layout = Dib32Layout::new(1, 1).unwrap();
        for fault in [
            test_support::Failure::Create,
            test_support::Failure::NullStorage,
        ] {
            test_support::fail_next(fault);
            assert!(Dib32Surface::new(screen.raw(), layout).is_err());
        }
        let mut surface = Dib32Surface::new(screen.raw(), layout).unwrap();
        assert_eq!(
            surface.with_pixels(|pixels| pixels.to_vec()).unwrap(),
            [0; 4]
        );
        surface
            .with_pixels_mut(|pixels| pixels.copy_from_slice(&[1, 2, 3, 4]))
            .unwrap();
        test_support::fail_next(test_support::Failure::BitBlt);
        assert!(
            surface
                .capture_from(&screen, ScreenPointPx { x: 0, y: 0 })
                .is_err()
        );
        assert!(
            surface
                .with_pixels(|_| panic!("failed capture exposed old pixels"))
                .is_err()
        );
        let mut surface = Dib32Surface::new(screen.raw(), layout).unwrap();
        test_support::fail_next(test_support::Failure::Flush);
        assert!(
            surface
                .with_pixels(|_| panic!("failed flush exposed pixels"))
                .is_err()
        );
        assert!(
            surface
                .with_pixels(|_| panic!("failed flush stayed readable"))
                .is_err()
        );
    }

    #[test]
    fn temporary_selection_restores_after_capture_flush_allocation_errors_and_success() {
        let screen = DesktopDc::new().unwrap();
        let mut surface = Dib32Surface::new(screen.raw(), Dib32Layout::new(1, 1).unwrap()).unwrap();
        surface
            .with_pixels_mut(|pixels| pixels.copy_from_slice(&[7, 8, 9, 10]))
            .unwrap();
        for fault in [
            Some(test_support::Failure::BitBlt),
            Some(test_support::Failure::Flush),
            Some(test_support::Failure::CopyAllocation),
            None,
        ] {
            let mut temporary =
                OwnedDib32::new(screen.raw(), Dib32Layout::new(3, 2).unwrap()).unwrap();
            if let Some(fault) = fault {
                test_support::fail_next(fault);
            }
            let result = surface.with_temporary(&mut temporary, |selected| {
                if fault == Some(test_support::Failure::BitBlt) {
                    selected.capture_from(&screen, ScreenPointPx { x: 0, y: 0 })?;
                }
                selected.copy_pixels()
            });
            assert_eq!(result.is_err(), fault.is_some());
            if let Ok(copy) = result {
                assert_eq!(copy, vec![0; 24]);
            }
            assert_eq!(
                surface.with_pixels(|pixels| pixels.to_vec()).unwrap(),
                [7, 8, 9, 10]
            );
            // Check the native selection as well as the unchanged pointer metadata.
            // SAFETY: no CPU slice is alive and this test only inspects the private DC.
            let selected = unsafe {
                windows::Win32::Graphics::Gdi::GetCurrentObject(
                    surface.backing.raw().unwrap(),
                    windows::Win32::Graphics::Gdi::OBJ_BITMAP,
                )
            };
            assert_ne!(selected, temporary.bitmap.raw().into());
        }
    }

    #[test]
    fn failed_temporary_restore_invalidates_surface_and_rejects_pixel_access() {
        use super::super::gdi::test_support::{Failure, fail_next};
        let screen = DesktopDc::new().unwrap();
        let layout = Dib32Layout::new(1, 1).unwrap();
        let mut surface = Dib32Surface::new(screen.raw(), layout).unwrap();
        let mut temporary = OwnedDib32::new(screen.raw(), layout).unwrap();
        fail_next(Failure::Select);
        assert!(surface.with_temporary(&mut temporary, |_| Ok(())).is_err());
        assert!(surface.with_pixels(|pixels| pixels.len()).is_ok());
        fail_next(Failure::Restore);
        assert!(
            surface
                .with_temporary(&mut temporary, |selected| selected.copy_pixels())
                .is_err()
        );
        assert!(
            surface
                .with_pixels(|_| panic!("invalid surface remained readable"))
                .is_err()
        );
        assert!(
            surface
                .capture_from(&screen, ScreenPointPx { x: 0, y: 0 })
                .is_err()
        );
        assert!(
            surface
                .with_temporary::<()>(&mut temporary, |_| panic!(
                    "invalid surface remained selectable"
                ))
                .is_err()
        );
    }

    #[test]
    fn temporary_selection_unwind_restores_or_invalidates_before_bitmap_release() {
        use super::super::gdi::test_support::{Failure, fail_next};
        use std::{
            cell::Cell,
            panic::{AssertUnwindSafe, catch_unwind},
        };
        use windows::Win32::Graphics::Gdi::{GetCurrentObject, GetObjectType, OBJ_BITMAP};

        let screen = DesktopDc::new().unwrap();
        for fail_restore in [false, true] {
            let mut surface =
                Dib32Surface::new(screen.raw(), Dib32Layout::new(1, 1).unwrap()).unwrap();
            surface
                .with_pixels_mut(|pixels| pixels.copy_from_slice(&[7, 8, 9, 10]))
                .unwrap();
            // SAFETY: inspect the bitmap selected in this owned DC; no CPU view
            // survives the preceding callback and no native selection is changed.
            let original = unsafe { GetCurrentObject(surface.backing.raw().unwrap(), OBJ_BITMAP) };
            let mut temporary =
                OwnedDib32::new(screen.raw(), Dib32Layout::new(2, 2).unwrap()).unwrap();
            let temporary_raw = temporary.bitmap.raw();
            let reached_panic = Cell::new(false);
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                surface
                    .with_temporary::<()>(&mut temporary, |selected| {
                        // SAFETY: the guard holds both owners exclusively; this
                        // query observes its real current bitmap before unwinding.
                        let current = unsafe {
                            GetCurrentObject(selected.surface.backing.raw().unwrap(), OBJ_BITMAP)
                        };
                        assert_eq!(current, temporary_raw.into());
                        if fail_restore {
                            fail_next(Failure::Restore);
                        }
                        reached_panic.set(true);
                        panic!("intentional panic while temporary DIB is selected");
                    })
                    .expect("the intentional panic must not return a capture result");
            }));
            assert!(outcome.is_err());
            assert!(
                reached_panic.get(),
                "must reach the intended panic after native selection"
            );
            if fail_restore {
                assert!(
                    surface
                        .with_pixels(|_| panic!("unwound invalid surface exposed pixels"))
                        .is_err()
                );
                // SAFETY: this only checks invalidation; no DC is returned after
                // the guard's failed restore destroyed its private memory DC.
                assert!(unsafe { surface.backing.raw() }.is_err());
            } else {
                assert_eq!(
                    // SAFETY: unwind completed and the restored private DC is live;
                    // compare its selection before the temporary owner is released.
                    unsafe { GetCurrentObject(surface.backing.raw().unwrap(), OBJ_BITMAP) },
                    original
                );
                assert_eq!(
                    surface.with_pixels(|pixels| pixels.to_vec()).unwrap(),
                    [7, 8, 9, 10]
                );
                surface.with_pixels_mut(|pixels| pixels[0] = 42).unwrap();
                assert_eq!(surface.with_pixels(|pixels| pixels[0]).unwrap(), 42);
            }
            assert_eq!(
                // SAFETY: the temporary owner remains live and either the original
                // selection was restored or its private DC was deleted by the guard.
                unsafe { GetObjectType(temporary_raw.into()) },
                OBJ_BITMAP.0 as u32
            );
            // GDI can cache/recycle a deleted handle, so GetObjectType after
            // deletion is not a release oracle. Record the real successful
            // DeleteObject call made by the unique bitmap owner instead.
            super::super::gdi::test_support::start_recording();
            drop(temporary);
            let events = super::super::gdi::test_support::finish_recording();
            assert_eq!(
                events,
                [("bitmap-", temporary_raw.0 as usize)],
                "the temporary owner must release its bitmap exactly once"
            );
        }
    }
}
