//! Thread-bound GDI ownership. Stock objects are always borrowed.
use std::{marker::PhantomData, rc::Rc};
use windows::{
    Win32::{
        Foundation::{E_FAIL, HWND},
        Graphics::Gdi::*,
    },
    core::{Error, Result},
};

pub(crate) fn failure(message: &'static str) -> Error {
    Error::new(E_FAIL, message)
}
pub(crate) fn invalid_selection(object: HGDIOBJ) -> bool {
    object.is_invalid() || object.0 as isize == -1
}

fn desktop_dc() -> HDC {
    #[cfg(test)]
    if test_support::take_failure(test_support::Failure::Desktop) {
        return HDC::default();
    }
    // SAFETY: acquiring the current thread's desktop DC has no pointer preconditions.
    unsafe { GetDC(None) }
}
fn memory_dc(reference: HDC) -> HDC {
    #[cfg(test)]
    if test_support::take_failure(test_support::Failure::Memory) {
        return HDC::default();
    }
    // SAFETY: reference is borrowed synchronously; the returned DC is newly owned.
    unsafe { CreateCompatibleDC(Some(reference)) }
}
fn compatible_bitmap(reference: HDC, width: i32, height: i32) -> HBITMAP {
    #[cfg(test)]
    if test_support::take_failure(test_support::Failure::Bitmap) {
        return HBITMAP::default();
    }
    // SAFETY: callers validate positive dimensions and synchronously borrow reference.
    unsafe { CreateCompatibleBitmap(reference, width, height) }
}
fn delete_dc(dc: HDC) -> bool {
    #[cfg(test)]
    if test_support::take_failure(test_support::Failure::DeleteDc) {
        return false;
    }
    // SAFETY: only private MemoryDc owners call this with their own handle.
    unsafe { DeleteDC(dc).as_bool() }
}
/// # Safety
/// The caller keeps dc and object alive and owns the selection relationship.
unsafe fn select(dc: HDC, object: HGDIOBJ, restoring: bool) -> HGDIOBJ {
    #[cfg(test)]
    if test_support::take_failure(if restoring {
        test_support::Failure::Restore
    } else {
        test_support::Failure::Select
    }) {
        return HGDIOBJ::default();
    }
    #[cfg(not(test))]
    let _ = restoring;
    // SAFETY: the caller's owning composite/guard maintains both object lifetimes.
    unsafe { SelectObject(dc, object) }
}
fn save_dc(dc: HDC) -> i32 {
    #[cfg(test)]
    if test_support::take_failure(test_support::Failure::Save) {
        return 0;
    }
    // SAFETY: only SavedDc::new calls this under its live-DC contract.
    unsafe { SaveDC(dc) }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::RefCell;
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Failure {
        Desktop,
        Memory,
        Bitmap,
        Select,
        Restore,
        DeleteDc,
        Save,
        RestoreDc,
    }
    thread_local! {
        static NEXT: RefCell<Vec<Failure>> = const { RefCell::new(Vec::new()) };
        static EVENTS: RefCell<Option<Vec<(&'static str, usize)>>> = const { RefCell::new(None) };
    }
    pub(super) fn record(kind: &'static str, handle: usize) {
        EVENTS.with_borrow_mut(|events| {
            if let Some(events) = events {
                events.push((kind, handle));
            }
        });
    }
    pub(crate) fn start_recording() {
        EVENTS.set(Some(Vec::new()));
    }
    pub(crate) fn finish_recording() -> Vec<(&'static str, usize)> {
        EVENTS.take().unwrap()
    }
    pub fn fail_next(failure: Failure) {
        NEXT.set(vec![failure]);
    }
    pub(super) fn fail_sequence(failures: &[Failure]) {
        NEXT.set(failures.to_vec());
    }
    pub(super) fn take_failure(failure: Failure) -> bool {
        NEXT.with_borrow_mut(|next| {
            if next.first() == Some(&failure) {
                next.remove(0);
                true
            } else {
                false
            }
        })
    }
}

pub(crate) struct DesktopDc {
    dc: HDC,
    _thread: PhantomData<Rc<()>>,
}
impl DesktopDc {
    pub fn new() -> Result<Self> {
        // SAFETY: acquire the calling thread's desktop DC; Drop pairs ReleaseDC.
        let dc = desktop_dc();
        if dc.is_invalid() {
            Err(failure("Could not obtain desktop DC"))
        } else {
            #[cfg(test)]
            test_support::record("desktop+", dc.0 as usize);
            Ok(Self {
                dc,
                _thread: PhantomData,
            })
        }
    }
    pub fn raw(&self) -> HDC {
        self.dc
    }
}
impl Drop for DesktopDc {
    fn drop(&mut self) {
        // SAFETY: this owner releases its GetDC(None) result on its original thread.
        if unsafe { ReleaseDC(None, self.dc) } != 0 {
            #[cfg(test)]
            test_support::record("desktop-", self.dc.0 as usize);
        } else {
            crate::app::diagnostics::event(format_args!("gdi.release_desktop_dc_failed"));
        }
    }
}

pub(crate) struct WindowDc {
    hwnd: HWND,
    dc: HDC,
    _thread: PhantomData<Rc<()>>,
}
impl WindowDc {
    /// # Safety
    /// hwnd must remain alive on this thread until the returned guard is dropped.
    pub unsafe fn new(hwnd: HWND) -> Result<Self> {
        // SAFETY: caller keeps the window alive throughout this DC lease.
        let dc = unsafe { GetDC(Some(hwnd)) };
        if dc.is_invalid() {
            Err(failure("Could not obtain window DC"))
        } else {
            Ok(Self {
                hwnd,
                dc,
                _thread: PhantomData,
            })
        }
    }
    pub fn raw(&self) -> HDC {
        self.dc
    }
}
impl Drop for WindowDc {
    fn drop(&mut self) {
        // SAFETY: creation contract keeps hwnd alive; release uses the same pair.
        unsafe {
            ReleaseDC(Some(self.hwnd), self.dc);
        }
    }
}

pub(crate) struct MemoryDc {
    dc: HDC,
    _thread: PhantomData<Rc<()>>,
}
impl MemoryDc {
    pub fn new(reference: HDC) -> Result<Self> {
        // SAFETY: reference is used only synchronously, never retained by GDI.
        let dc = memory_dc(reference);
        if dc.is_invalid() {
            Err(failure("Could not create memory DC"))
        } else {
            #[cfg(test)]
            test_support::record("memory+", dc.0 as usize);
            Ok(Self {
                dc,
                _thread: PhantomData,
            })
        }
    }
    pub fn raw(&self) -> HDC {
        self.dc
    }
    pub fn close(&mut self) -> bool {
        if self.dc.is_invalid() {
            return true;
        }
        // SAFETY: this is our private CreateCompatibleDC result, not a borrowed DC.
        if delete_dc(self.dc) {
            #[cfg(test)]
            test_support::record("memory-", self.dc.0 as usize);
            self.dc = HDC::default();
            true
        } else {
            false
        }
    }
}
impl Drop for MemoryDc {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

pub(crate) struct OwnedBitmap {
    handle: HBITMAP,
    _thread: PhantomData<Rc<()>>,
}
impl OwnedBitmap {
    /// # Safety
    /// Take unique ownership of a newly created, unselected bitmap, never stock.
    pub unsafe fn from_raw(handle: HBITMAP) -> Result<Self> {
        if handle.is_invalid() {
            Err(failure("Could not create bitmap"))
        } else {
            #[cfg(test)]
            test_support::record("bitmap+", handle.0 as usize);
            Ok(Self {
                handle,
                _thread: PhantomData,
            })
        }
    }
    pub fn raw(&self) -> HBITMAP {
        self.handle
    }
    pub fn compatible(reference: HDC, width: i32, height: i32) -> Result<Self> {
        if width <= 0 || height <= 0 {
            return Err(failure("Invalid bitmap dimensions"));
        }
        // SAFETY: creation gives this owner a fresh unselected bitmap.
        unsafe { Self::from_raw(compatible_bitmap(reference, width, height)) }
    }
    fn retain(&mut self) {
        self.handle = HBITMAP::default();
    }
}
impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: our selection owner restores/destroys its DC before this drop.
            if unsafe { DeleteObject(self.handle.into()) }.as_bool() {
                #[cfg(test)]
                test_support::record("bitmap-", self.handle.0 as usize);
            } else {
                crate::app::diagnostics::event(format_args!("gdi.delete_bitmap_failed"));
            }
        }
    }
}

pub(crate) struct OwnedFont {
    handle: HFONT,
    retained: std::cell::Cell<bool>,
    _thread: PhantomData<Rc<()>>,
}
impl OwnedFont {
    /// # Safety
    /// Transfer a fresh CreateFont result exactly once. All controls and DCs must
    /// stop using it before Drop; stock fonts must never be passed here.
    pub unsafe fn from_raw(handle: HFONT) -> Result<Self> {
        if handle.is_invalid() {
            Err(failure("Could not create font"))
        } else {
            Ok(Self {
                handle,
                retained: std::cell::Cell::new(false),
                _thread: PhantomData,
            })
        }
    }
    pub fn raw(&self) -> HFONT {
        self.handle
    }
    pub fn retain(&self) {
        self.retained.set(true);
    }
}
impl Drop for OwnedFont {
    fn drop(&mut self) {
        if !self.retained.get() {
            #[cfg(test)]
            test_support::record("font-delete-attempt", self.handle.0 as usize);
            // SAFETY: the transfer contract requires controls/DCs to release this font first.
            unsafe {
                let _ = DeleteObject(self.handle.into());
            }
        }
    }
}

pub(crate) struct OwnedBrush {
    handle: HBRUSH,
    _thread: PhantomData<Rc<()>>,
}
impl OwnedBrush {
    pub fn solid(color: windows::Win32::Foundation::COLORREF) -> Result<Self> {
        // SAFETY: CreateSolidBrush returns a new unselected brush owned here.
        let handle = unsafe { CreateSolidBrush(color) };
        if handle.is_invalid() {
            Err(failure("Could not create brush"))
        } else {
            Ok(Self {
                handle,
                _thread: PhantomData,
            })
        }
    }
    pub fn raw(&self) -> HBRUSH {
        self.handle
    }
}
impl Drop for OwnedBrush {
    fn drop(&mut self) {
        // SAFETY: callers use this brush synchronously or restore their selection first.
        unsafe {
            let _ = DeleteObject(self.handle.into());
        }
    }
}

pub(crate) struct OwnedPen {
    handle: HPEN,
    _thread: PhantomData<Rc<()>>,
}
impl OwnedPen {
    pub fn new(
        style: PEN_STYLE,
        width: i32,
        color: windows::Win32::Foundation::COLORREF,
    ) -> Result<Self> {
        // SAFETY: CreatePen returns a fresh owned object, never a stock object.
        let handle = unsafe { CreatePen(style, width, color) };
        if handle.is_invalid() {
            Err(failure("Could not create pen"))
        } else {
            Ok(Self {
                handle,
                _thread: PhantomData,
            })
        }
    }
    pub fn raw(&self) -> HPEN {
        self.handle
    }
    pub fn retain(self) {
        std::mem::forget(self);
    }
}
impl Drop for OwnedPen {
    fn drop(&mut self) {
        // SAFETY: caller restores the borrowed DC before the pen owner drops.
        unsafe {
            let _ = DeleteObject(self.handle.into());
        }
    }
}

/// A private memory DC and its persistent bitmap selection; never split ownership.
pub(crate) struct BitmapDc {
    dc: MemoryDc,
    bitmap: OwnedBitmap,
    previous: HGDIOBJ,
    valid: bool,
}
impl BitmapDc {
    pub fn new(reference: HDC, bitmap: OwnedBitmap) -> Result<Self> {
        let dc = MemoryDc::new(reference)?;
        // SAFETY: bitmap is uniquely owned and not selected elsewhere; dc is private.
        let previous = unsafe { select(dc.raw(), bitmap.raw().into(), false) };
        if invalid_selection(previous) {
            return Err(failure("Could not select bitmap"));
        }
        Ok(Self {
            dc,
            bitmap,
            previous,
            valid: true,
        })
    }
    pub fn compatible(reference: HDC, width: i32, height: i32) -> Result<Self> {
        Self::new(
            reference,
            OwnedBitmap::compatible(reference, width, height)?,
        )
    }
    /// # Safety
    /// Do not retain this handle, replace selections or use while CPU pixel views exist.
    pub unsafe fn raw(&self) -> Result<HDC> {
        if self.valid {
            Ok(self.dc.raw())
        } else {
            Err(failure("Bitmap surface is invalid"))
        }
    }
    /// # Safety
    /// temporary is uniquely owned, unselected and remains alive until restore_temporary.
    pub unsafe fn select_temporary(&mut self, temporary: &OwnedBitmap) -> Result<HGDIOBJ> {
        // SAFETY: exclusive surface access and caller's bitmap lifetime cover this selection.
        let previous = unsafe { select(self.raw()?, temporary.raw().into(), false) };
        if invalid_selection(previous) {
            Err(failure("Could not select temporary bitmap"))
        } else {
            Ok(previous)
        }
    }
    /// Restore before temporary destruction. On failure invalidate the surface and
    /// delete our private DC; if that fails, retain both potentially selected objects.
    /// # Safety
    /// temporary and previous must be the exact still-live bitmap/return value
    /// from this surface's outstanding select_temporary, restored exactly once.
    pub unsafe fn restore_temporary(
        &mut self,
        temporary: &mut OwnedBitmap,
        previous: HGDIOBJ,
    ) -> Result<()> {
        // SAFETY: previous came from this private DC; both bitmap owners still exist.
        if !invalid_selection(unsafe { select(self.dc.raw(), previous, true) }) {
            return Ok(());
        }
        self.valid = false;
        if !self.dc.close() {
            temporary.retain();
            self.bitmap.retain();
        }
        Err(failure(
            "Could not restore temporary bitmap; surface invalidated",
        ))
    }
}
impl Drop for BitmapDc {
    fn drop(&mut self) {
        if self.valid {
            // SAFETY: restore the borrowed stock bitmap before deleting owned resources.
            if invalid_selection(unsafe { select(self.dc.raw(), self.previous, true) })
                && !self.dc.close()
            {
                self.bitmap.retain();
                crate::app::diagnostics::event(format_args!(
                    "gdi.restore_failed resources_retained"
                ));
            }
        }
        // DC drops first, followed by bitmap; normal path already restored selection.
    }
}

pub(crate) struct PaintSession {
    hwnd: HWND,
    paint: PAINTSTRUCT,
    pub dc: HDC,
    _thread: PhantomData<Rc<()>>,
}
impl PaintSession {
    /// # Safety
    /// Must run in this live window's WM_PAINT on its UI thread, before borrowing
    /// callback state. BeginPaint may synchronously dispatch WM_ERASEBKGND.
    pub unsafe fn begin(hwnd: HWND) -> Result<Self> {
        let mut paint = PAINTSTRUCT::default();
        // SAFETY: caller establishes a current WM_PAINT and valid output storage.
        let dc = unsafe { BeginPaint(hwnd, &mut paint) };
        let session = Self {
            hwnd,
            paint,
            dc,
            _thread: PhantomData,
        };
        if dc.is_invalid() {
            Err(failure("BeginPaint returned no DC"))
        } else {
            Ok(session)
        }
    }
}
impl Drop for PaintSession {
    fn drop(&mut self) {
        // SAFETY: exactly matches BeginPaint, including its invalid-DC failure path.
        unsafe {
            let _ = EndPaint(self.hwnd, &self.paint);
        }
    }
}

pub(crate) struct SavedDc {
    dc: HDC,
    level: Option<i32>,
    _thread: PhantomData<Rc<()>>,
}
impl SavedDc {
    /// # Safety
    /// dc must remain live until restore/drop; selected owned objects must outlive
    /// the guard. A failed restoration must not be followed by deleting those objects.
    pub unsafe fn new(dc: HDC) -> Result<Self> {
        // SAFETY: caller supplies a live DC on its owning thread.
        let level = save_dc(dc);
        if level == 0 {
            Err(failure("Could not save DC"))
        } else {
            Ok(Self {
                dc,
                level: Some(level),
                _thread: PhantomData,
            })
        }
    }
    pub fn restore(mut self) -> Result<()> {
        self.finish()
    }
    fn finish(&mut self) -> Result<()> {
        if let Some(level) = self.level.take() {
            #[cfg(test)]
            if test_support::take_failure(test_support::Failure::RestoreDc) {
                return Err(failure("Injected RestoreDC failure"));
            }
            // SAFETY: restore exactly our saved level while the DC and objects are alive.
            if !unsafe { RestoreDC(self.dc, level) }.as_bool() {
                return Err(failure("Could not restore DC"));
            }
        }
        Ok(())
    }
}
impl Drop for SavedDc {
    fn drop(&mut self) {
        if self.finish().is_err() {
            crate::app::diagnostics::event(format_args!("gdi.restore_dc_failed"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::{Failure, fail_next};

    #[test]
    fn constructors_and_selection_fail_without_live_owned_objects() {
        test_support::start_recording();
        fail_next(Failure::Desktop);
        assert!(DesktopDc::new().is_err());
        let desktop = DesktopDc::new().unwrap();
        for stage in [Failure::Bitmap, Failure::Memory, Failure::Select] {
            fail_next(stage);
            assert!(BitmapDc::compatible(desktop.raw(), 2, 2).is_err());
        }
        let surface = BitmapDc::compatible(desktop.raw(), 2, 2).unwrap();
        // SAFETY: this test only borrows our private DC synchronously, no pixel view.
        let dc = unsafe { surface.raw().unwrap() };
        fail_next(Failure::Save);
        // SAFETY: surface owns a live DC until the test exits.
        assert!(unsafe { SavedDc::new(dc) }.is_err());
        // SAFETY: the same live DC covers both guard creation and restoration.
        unsafe { SavedDc::new(dc) }.unwrap().restore().unwrap();
        drop(surface);
        drop(desktop);
        let events = test_support::finish_recording();
        let mut owned = std::collections::BTreeSet::new();
        for (event, handle) in events {
            let kind = &event[..event.len() - 1];
            if event.ends_with('+') {
                assert!(
                    owned.insert((kind, handle)),
                    "duplicate owner: {event} {handle}"
                );
            } else {
                assert!(
                    owned.remove(&(kind, handle)),
                    "duplicate or borrowed release: {event} {handle}"
                );
            }
        }
        assert!(
            owned.is_empty(),
            "every acquired resource must be released once"
        );
    }

    #[test]
    fn failed_temporary_restore_invalidates_surface_and_deletes_private_dc() {
        let desktop = DesktopDc::new().unwrap();
        let mut surface = BitmapDc::compatible(desktop.raw(), 1, 1).unwrap();
        let mut temporary = OwnedBitmap::compatible(desktop.raw(), 2, 2).unwrap();
        // SAFETY: both unique owners live until explicit restoration below.
        let previous = unsafe { surface.select_temporary(&temporary).unwrap() };
        fail_next(Failure::Restore);
        // SAFETY: these are the exact owner and previous handle returned above.
        assert!(unsafe { surface.restore_temporary(&mut temporary, previous) }.is_err());
        // SAFETY: merely asks for the invalidated DC and must receive an error.
        assert!(unsafe { surface.raw() }.is_err());
        assert!(surface.dc.raw().is_invalid());
    }

    #[test]
    fn failed_restore_and_dc_delete_retain_uncertain_native_resources() {
        let desktop = DesktopDc::new().unwrap();
        let mut surface = BitmapDc::compatible(desktop.raw(), 1, 1).unwrap();
        let original_raw = surface.bitmap.raw();
        let mut temporary = OwnedBitmap::compatible(desktop.raw(), 2, 2).unwrap();
        let temporary_raw = temporary.raw();
        // SAFETY: both owners remain alive for this exact temporary selection.
        let previous = unsafe { surface.select_temporary(&temporary).unwrap() };
        test_support::fail_sequence(&[Failure::Restore, Failure::DeleteDc]);
        // SAFETY: restore uses the matching bitmap and the returned native object.
        assert!(unsafe { surface.restore_temporary(&mut temporary, previous) }.is_err());
        assert!(!surface.valid);
        assert!(surface.bitmap.raw().is_invalid());
        assert!(temporary.raw().is_invalid());
        // Fault is one-shot: test-only cleanup proves the production fallback
        // retained both objects. No leak is left behind in the test process.
        assert!(surface.dc.close());
        // SAFETY: private DC was successfully deleted; fallback relinquished both
        // handles without deleting them, so this test now has sole cleanup duty.
        unsafe {
            assert!(DeleteObject(original_raw.into()).as_bool());
            assert!(DeleteObject(temporary_raw.into()).as_bool());
        }
    }

    #[test]
    fn failed_saved_dc_restore_is_reported_and_not_retried_by_drop() {
        let desktop = DesktopDc::new().unwrap();
        let surface = BitmapDc::compatible(desktop.raw(), 1, 1).unwrap();
        // SAFETY: private DC remains alive for this scoped save/restore.
        let dc = unsafe { surface.raw().unwrap() };
        // SAFETY: this test has exclusive control of the live private DC state.
        let saved = unsafe { SavedDc::new(dc).unwrap() };
        let level = saved.level.unwrap();
        test_support::fail_next(Failure::RestoreDc);
        assert!(saved.restore().is_err());
        // SAFETY: injection skipped native restore and Drop must not retry; this
        // exact saved level therefore still exists for test-only cleanup.
        assert!(unsafe { RestoreDC(dc, level) }.as_bool());
    }
}
