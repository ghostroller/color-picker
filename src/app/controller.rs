//! M2's preview-only session. No input hooks exist yet; M3 will connect the
//! full input/state protocol. HWND callbacks never borrow this controller.

#![forbid(unsafe_code)]

use crate::{
    app::diagnostics,
    core::state::{PickedColor, SampleKind, SessionId},
    platform::windows::{
        capture::GdiSampler,
        monitors::Monitors,
        session::{SessionTimer, cursor_position, flush_composition},
    },
    ui::windows::preview::PreviewWindow,
};
use windows::{
    Win32::Foundation::{E_FAIL, HWND},
    core::{Error, Result},
};

const MAX_CONSECUTIVE_FAILURES: u32 = 3;

pub struct PreviewController {
    host: HWND,
    generations: Generations,
    live: Option<LivePreview>,
    sample_attempts: u64,
}

struct LivePreview {
    session: SessionId,
    // Drop order is significant even during an error or early return.
    timer: Option<SessionTimer>,
    preview: PreviewWindow,
    sampler: GdiSampler,
    monitors: Monitors,
    failures: u32,
    last_sample: Option<PickedColor>,
}

impl PreviewController {
    pub fn new(host: HWND) -> Self {
        Self {
            host,
            generations: Generations::default(),
            live: None,
            sample_attempts: 0,
        }
    }

    /// A second activation during preview is ignored, not a nested session.
    pub fn start(&mut self) -> Result<bool> {
        if self.live.is_some() {
            return Ok(false);
        }
        let session = SessionId(self.generations.next_session()?);
        let monitors = Monitors::enumerate()?;
        let sampler = GdiSampler::new().map_err(capture_error)?;
        let preview = PreviewWindow::new()?;
        let timer_id = self.generations.next_timer()?;
        let timer = SessionTimer::start(self.host, timer_id)?;
        self.live = Some(LivePreview {
            session,
            timer: Some(timer),
            preview,
            sampler,
            monitors,
            failures: 0,
            last_sample: None,
        });
        // The first frame needn't wait for the first 17ms timer message.
        if let Err(error) = self.sample_once() {
            self.stop("startup_failure");
            return Err(error);
        }
        diagnostics::event(format_args!(
            "preview.started session={} timer={} interval_ms=17",
            session.0, timer_id
        ));
        Ok(true)
    }

    pub fn active(&self) -> bool {
        self.live.is_some()
    }
    pub fn timer_id(&self) -> usize {
        self.live
            .as_ref()
            .and_then(|live| live.timer.as_ref())
            .map_or(0, SessionTimer::id)
    }
    pub fn session_id(&self) -> u64 {
        self.live.as_ref().map_or(0, |live| live.session.0)
    }
    pub fn sample_attempts(&self) -> u64 {
        self.sample_attempts
    }
    pub fn last_sample(&self) -> Option<PickedColor> {
        self.live.as_ref().and_then(|live| live.last_sample)
    }

    /// IDs are never reused. Queued WM_TIMER messages from a stopped/paused
    /// session cannot sample in another generation, even with an equal HWND.
    pub fn on_timer(&mut self, id: usize) -> Result<()> {
        if id == 0 || id != self.timer_id() {
            return Ok(());
        }
        if let Err(error) = self.sample_once() {
            self.stop("sample_failure");
            return Err(error);
        }
        Ok(())
    }

    pub fn stop(&mut self, reason: &str) {
        if let Some(live) = self.live.take() {
            let session = live.session;
            drop(live);
            diagnostics::event(format_args!(
                "preview.stopped session={} reason={reason} timer=0 resources_released=true",
                session.0
            ));
        }
    }

    pub fn pause_for_menu(&mut self) -> Result<()> {
        if let Some(live) = self.live.as_mut() {
            live.timer.take();
            live.preview.hide();
            flush_composition()?;
        }
        Ok(())
    }

    pub fn resume_after_menu(&mut self) -> Result<()> {
        if let Some(live) = self.live.as_mut()
            && live.timer.is_none()
        {
            // The menu itself was another owned visible surface. Ensure it has
            // left the composed image before sampling its former location.
            flush_composition()?;
            let id = self.generations.next_timer()?;
            live.timer = Some(SessionTimer::start(self.host, id)?);
        }
        Ok(())
    }

    fn sample_once(&mut self) -> Result<()> {
        let Some(live) = self.live.as_mut() else {
            return Ok(());
        };
        let point = match cursor_position() {
            Ok(point) => point,
            Err(error) => {
                live.last_sample = None;
                live.preview.hide();
                return failure(live, error);
            }
        };
        let Some(monitor) = live.monitors.at(point).copied() else {
            live.last_sample = None;
            live.preview.hide();
            return failure(live, Error::new(E_FAIL, "光标没有位于有效显示器，无法采样"));
        };
        if live.preview.rect().is_some_and(|rect| rect.contains(point)) {
            live.preview.hide();
            // This transition is essential even when capture exclusion was
            // accepted: it is the correctness fallback for our own overlay.
            flush_composition()?;
        }
        self.sample_attempts = self.sample_attempts.saturating_add(1);
        match live.sampler.sample_pixel(point) {
            Ok(rgb) => {
                live.failures = 0;
                live.last_sample = Some(PickedColor {
                    rgb,
                    source: point,
                    kind: SampleKind::Live,
                });
                live.preview.update(point, Some(rgb), monitor.work_area)?;
                Ok(())
            }
            Err(error) => {
                live.last_sample = None;
                live.preview.update(point, None, monitor.work_area)?;
                failure(live, capture_error(error))
            }
        }
    }
}

impl Drop for PreviewController {
    fn drop(&mut self) {
        self.stop("host_exit");
    }
}

fn failure(live: &mut LivePreview, error: Error) -> Result<()> {
    live.failures = live.failures.saturating_add(1);
    if live.failures == 1 {
        diagnostics::event(format_args!(
            "preview.sample_unavailable session={} error={error}",
            live.session.0
        ));
    }
    if live.failures >= MAX_CONSECUTIVE_FAILURES {
        Err(error)
    } else {
        Ok(())
    }
}

fn capture_error(error: impl std::fmt::Display) -> Error {
    Error::new(E_FAIL, error.to_string())
}

#[derive(Default)]
struct Generations {
    session: u64,
    timer: usize,
}
impl Generations {
    fn next_session(&mut self) -> Result<u64> {
        self.session = self
            .session
            .checked_add(1)
            .ok_or_else(|| Error::new(E_FAIL, "Session generation exhausted"))?;
        Ok(self.session)
    }
    fn next_timer(&mut self) -> Result<usize> {
        self.timer = self
            .timer
            .checked_add(1)
            .ok_or_else(|| Error::new(E_FAIL, "Timer generation exhausted"))?;
        Ok(self.timer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_cookies_are_unique_across_menu_pauses_and_new_sessions() {
        let mut generations = Generations::default();
        assert_eq!(generations.next_session().unwrap(), 1);
        assert_eq!(generations.next_timer().unwrap(), 1);
        assert_eq!(generations.next_timer().unwrap(), 2);
        assert_eq!(generations.next_session().unwrap(), 2);
        assert_eq!(generations.next_timer().unwrap(), 3);
    }

    #[test]
    fn generation_exhaustion_never_reuses_a_stale_cookie() {
        let mut generations = Generations {
            session: u64::MAX,
            timer: usize::MAX,
        };
        assert!(generations.next_session().is_err());
        assert!(generations.next_timer().is_err());
        assert_eq!(generations.session, u64::MAX);
        assert_eq!(generations.timer, usize::MAX);
    }
}
