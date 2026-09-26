//! Reusable decorative backdrop; refreshed only when moved, without timers.
//! The owning overlay must use WDA_EXCLUDEFROMCAPTURE before constructing this.

use std::cell::RefCell;
use windows::{
    Win32::{
        Foundation::{E_INVALIDARG, E_OUTOFMEMORY, RECT},
        Graphics::Gdi::HDC,
    },
    core::{Error, Result},
};

use crate::{
    app::config::MAX_BACKGROUND_TRANSPARENCY_PERCENT,
    core::geometry::ScreenPointPx,
    platform::windows::{
        dib::{Dib32Layout, Dib32Surface},
        gdi::DesktopDc,
    },
    ui::pixel_effects::blur_and_tint,
};

pub(super) struct FrostedPanel {
    state: RefCell<BackdropState>,
    screen: DesktopDc,
    layout: Dib32Layout,
    radius: usize,
    transparency_percent: u8,
}

struct BackdropState {
    surface: Dib32Surface,
    scratch: Vec<u8>,
    origin: Option<ScreenPointPx>,
}

impl FrostedPanel {
    pub fn new(width: i32, height: i32, radius: i32, transparency_percent: u8) -> Result<Self> {
        if transparency_percent > MAX_BACKGROUND_TRANSPARENCY_PERCENT {
            return Err(Error::new(E_INVALIDARG, "Invalid backdrop transparency"));
        }
        let layout = Dib32Layout::new(width, height)?;
        let screen = DesktopDc::new()?;
        let surface = Dib32Surface::new(screen.raw(), layout)?;
        let mut scratch = Vec::new();
        scratch.try_reserve_exact(layout.len()).map_err(|_| {
            Error::new(E_OUTOFMEMORY, "Could not allocate backdrop scratch storage")
        })?;
        scratch.resize(layout.len(), 0);
        Ok(Self {
            state: RefCell::new(BackdropState {
                surface,
                scratch,
                origin: None,
            }),
            screen,
            layout,
            radius: radius.max(1) as usize,
            transparency_percent,
        })
    }

    pub fn paint(&self, target: HDC, rect: RECT, origin: ScreenPointPx) -> Result<()> {
        // One borrow protects both native storage and cache validity. No USER32
        // call that could reenter this window is made while this borrow is held.
        let mut state = self.state.borrow_mut();
        let Some((x, y)) = origin
            .x
            .checked_add(rect.left)
            .zip(origin.y.checked_add(rect.top))
        else {
            state.origin = None;
            return Err(Error::new(
                E_INVALIDARG,
                "Backdrop origin overflows screen coordinates",
            ));
        };
        let origin = ScreenPointPx { x, y };
        if state.origin != Some(origin) {
            state.origin = None;
            let BackdropState {
                surface, scratch, ..
            } = &mut *state;
            surface.capture_from(&self.screen, origin)?;
            surface
                .with_pixels_mut(|pixels| {
                    blur_and_tint(
                        pixels,
                        scratch,
                        self.layout.width() as usize,
                        self.layout.height() as usize,
                        self.radius,
                        self.transparency_percent,
                    )
                })?
                .map_err(|_| Error::new(E_INVALIDARG, "Invalid backdrop effect parameters"))?;
            state.origin = Some(origin);
        }
        // SAFETY: the caller supplies its live paint/backbuffer DC, distinct from
        // the private backdrop DC. The exclusive state borrow prevents CPU aliases.
        unsafe { state.surface.blit_to(target, rect.left, rect.top) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::windows::{
        dib::test_support::{Failure, fail_next},
        gdi::BitmapDc,
    };

    #[test]
    fn cache_reuses_same_origin_and_retries_after_failed_refresh() {
        let panel = FrostedPanel::new(2, 2, 3, 35).unwrap();
        let target = BitmapDc::compatible(panel.screen.raw(), 4, 4).unwrap();
        // SAFETY: this private target stays alive for all paints; its handle is
        // not exposed to any CPU pixel view or modified selection.
        let target_dc = unsafe { target.raw().unwrap() };
        let rect = RECT {
            left: 0,
            top: 0,
            right: 2,
            bottom: 2,
        };
        let origin = ScreenPointPx { x: 0, y: 0 };
        let source = BitmapDc::compatible(panel.screen.raw(), 4, 4).unwrap();
        crate::platform::windows::dib::test_support::with_source(&source, || {
            panel.paint(target_dc, rect, origin).unwrap();
            let scratch_address = panel.state.borrow().scratch.as_ptr();
            fail_next(Failure::BitBlt);
            // The injected capture failure is not consumed when origin is unchanged.
            panel.paint(target_dc, rect, origin).unwrap();
            let moved = ScreenPointPx { x: 1, y: 1 };
            assert!(panel.paint(target_dc, rect, moved).is_err());
            assert_eq!(panel.state.borrow().origin, None);
            panel.paint(target_dc, rect, moved).unwrap();
            assert_eq!(panel.state.borrow().origin, Some(moved));
            assert_eq!(panel.state.borrow().scratch.as_ptr(), scratch_address);
            fail_next(Failure::Flush);
            assert!(panel.paint(target_dc, rect, origin).is_err());
            assert_eq!(panel.state.borrow().origin, None);
            panel.paint(target_dc, rect, origin).unwrap();
        });
    }

    #[test]
    fn ui_transparency_limit_and_invalid_layout_are_preserved() {
        assert!(FrostedPanel::new(1, 1, 1, 81).is_err());
        assert!(FrostedPanel::new(-1, 1, 1, 35).is_err());
        assert!(FrostedPanel::new(1, i32::MIN, 1, 35).is_err());
    }
}
