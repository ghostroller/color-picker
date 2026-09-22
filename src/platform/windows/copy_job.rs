//! Host-owned quick copying. Success does not allocate or activate result UI.

use std::{marker::PhantomData, rc::Rc};

use windows::{
    Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{KillTimer, SetTimer},
    },
    core::{Error, Result},
};

use crate::{
    app::i18n::tr,
    core::{
        format::{ColorFormat, format_color},
        state::PickedColor,
    },
};

use super::{
    clipboard::{Clipboard, ClipboardError},
    session::next_timer_id,
};

const RETRY_MS: u32 = 50;
const MAX_RETRIES: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CopyTimer {
    pub id: usize,
    pub request: usize,
}

pub(crate) enum CopyProgress {
    Pending,
    Copied,
    Failed {
        picked: PickedColor,
        format: ColorFormat,
        message: String,
    },
}

struct Request {
    token: usize,
    picked: PickedColor,
    format: ColorFormat,
    text: String,
    retries_left: u8,
}

pub(crate) trait CopyPlatform {
    fn copy(&mut self, owner: HWND, text: &str) -> std::result::Result<(), ClipboardError>;
    fn arm(&mut self, owner: HWND, timer: usize, delay_ms: u32) -> Result<()>;
    fn disarm(&mut self, owner: HWND, timer: usize);
}

pub(crate) struct NativeCopyPlatform;

impl CopyPlatform for NativeCopyPlatform {
    fn copy(&mut self, owner: HWND, text: &str) -> std::result::Result<(), ClipboardError> {
        Clipboard::copy_text(owner, text)
    }

    fn arm(&mut self, owner: HWND, timer: usize, delay_ms: u32) -> Result<()> {
        if unsafe { SetTimer(Some(owner), timer, delay_ms, None) } == 0 {
            return Err(Error::from_thread());
        }
        Ok(())
    }

    fn disarm(&mut self, owner: HWND, timer: usize) {
        let _ = unsafe { KillTimer(Some(owner), timer) };
    }
}

pub(crate) struct CopyJob<P: CopyPlatform = NativeCopyPlatform> {
    owner: HWND,
    platform: P,
    request: Option<Request>,
    timer: Option<CopyTimer>,
    _thread: PhantomData<Rc<()>>,
}

impl CopyJob {
    pub fn new(owner: HWND) -> Self {
        Self::with_platform(owner, NativeCopyPlatform)
    }
}

impl<P: CopyPlatform> CopyJob<P> {
    pub(crate) fn with_platform(owner: HWND, platform: P) -> Self {
        Self {
            owner,
            platform,
            request: None,
            timer: None,
            _thread: PhantomData,
        }
    }

    pub fn timer(&self) -> Option<CopyTimer> {
        self.timer
    }

    pub fn is_pending(&self) -> bool {
        self.request.is_some()
    }

    pub fn start(&mut self, picked: PickedColor, format: ColorFormat) -> CopyProgress {
        self.cancel();
        let token = match next_timer_id() {
            Ok(token) => token,
            Err(error) => {
                return CopyProgress::Failed {
                    picked,
                    format,
                    message: error.to_string(),
                };
            }
        };
        self.request = Some(Request {
            token,
            picked,
            format,
            text: format_color(picked.rgb, format),
            retries_left: MAX_RETRIES,
        });
        self.attempt(token)
    }

    pub fn on_timer(&mut self, timer: CopyTimer) -> CopyProgress {
        if self.timer != Some(timer) {
            return CopyProgress::Pending;
        }
        self.stop_timer();
        self.attempt(timer.request)
    }

    pub fn cancel(&mut self) {
        self.request = None;
        self.stop_timer();
    }

    fn stop_timer(&mut self) {
        if let Some(timer) = self.timer.take() {
            self.platform.disarm(self.owner, timer.id);
        }
    }

    fn attempt(&mut self, token: usize) -> CopyProgress {
        let Some(request) = self
            .request
            .as_ref()
            .filter(|request| request.token == token)
        else {
            return CopyProgress::Pending;
        };
        let outcome = self.platform.copy(self.owner, &request.text);
        match outcome {
            Ok(()) => {
                self.request = None;
                CopyProgress::Copied
            }
            Err(ClipboardError::Busy) => {
                let request = self.request.as_mut().unwrap();
                if request.retries_left == 0 {
                    return self.fail(
                        tr(
                            "复制失败：剪贴板仍被占用。请再次点击复制。",
                            "Clipboard still busy. Click Copy to try again.",
                        )
                        .to_owned(),
                    );
                }
                request.retries_left -= 1;
                // A fresh ID for every arming rejects queued notifications from
                // both an older request and an earlier retry of this request.
                match next_timer_id().and_then(|id| {
                    self.platform.arm(self.owner, id, RETRY_MS)?;
                    Ok(id)
                }) {
                    Ok(id) => {
                        self.timer = Some(CopyTimer { id, request: token });
                        CopyProgress::Pending
                    }
                    Err(_) => self.fail(
                        tr(
                            "剪贴板被占用，无法安排重试。请再次点击复制。",
                            "Clipboard busy; retry unavailable. Click Copy to try again.",
                        )
                        .to_owned(),
                    ),
                }
            }
            Err(ClipboardError::Other(error)) => self.fail(crate::tr_format!(
                "复制失败：{error}",
                "Copy failed: {error}"
            )),
        }
    }

    fn fail(&mut self, message: String) -> CopyProgress {
        let request = self.request.take().unwrap();
        self.stop_timer();
        CopyProgress::Failed {
            picked: request.picked,
            format: request.format,
            message,
        }
    }
}

impl<P: CopyPlatform> Drop for CopyJob<P> {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{color::Rgb8, geometry::ScreenPointPx, state::SampleKind};
    use std::{
        cell::Cell,
        collections::{HashSet, VecDeque},
    };
    use windows::Win32::Foundation::E_FAIL;

    #[derive(Default)]
    struct FakePlatform {
        outcomes: VecDeque<std::result::Result<(), ClipboardError>>,
        copies: Vec<(HWND, String)>,
        armed: Vec<usize>,
        active: HashSet<usize>,
        fail_timer: bool,
        disarmed: Rc<Cell<usize>>,
    }

    impl CopyPlatform for FakePlatform {
        fn copy(&mut self, owner: HWND, text: &str) -> std::result::Result<(), ClipboardError> {
            self.copies.push((owner, text.to_owned()));
            self.outcomes
                .pop_front()
                .expect("unexpected clipboard attempt")
        }

        fn arm(&mut self, _: HWND, timer: usize, delay_ms: u32) -> Result<()> {
            assert_eq!(delay_ms, 50);
            assert!(!self.armed.contains(&timer), "timer ID reused");
            if self.fail_timer {
                return Err(Error::new(E_FAIL, "simulated timer failure"));
            }
            self.armed.push(timer);
            self.active.insert(timer);
            Ok(())
        }

        fn disarm(&mut self, _: HWND, timer: usize) {
            assert!(self.active.remove(&timer));
            self.disarmed.set(self.disarmed.get() + 1);
        }
    }

    fn picked() -> PickedColor {
        PickedColor {
            rgb: Rgb8 {
                r: 12,
                g: 34,
                b: 56,
            },
            source: ScreenPointPx { x: -300, y: 450 },
            kind: SampleKind::Frozen,
        }
    }

    fn job(
        outcomes: impl IntoIterator<Item = std::result::Result<(), ClipboardError>>,
    ) -> CopyJob<FakePlatform> {
        CopyJob::with_platform(
            HWND(123_usize as *mut _),
            FakePlatform {
                outcomes: outcomes.into_iter().collect(),
                ..Default::default()
            },
        )
    }

    #[test]
    fn quick_success_only_formats_and_copies_with_the_existing_owner() {
        let mut job = job([Ok(())]);
        assert!(matches!(
            job.start(picked(), ColorFormat::Hex),
            CopyProgress::Copied
        ));
        assert_eq!(job.platform.copies, [(job.owner, "#0C2238".to_owned())]);
        assert!(job.request.is_none());
        assert!(job.timer().is_none());
        assert!(job.platform.armed.is_empty());
    }

    #[test]
    fn busy_then_success_releases_the_retry_timer() {
        let mut job = job([Err(ClipboardError::Busy), Ok(())]);
        assert!(matches!(
            job.start(picked(), ColorFormat::Hex),
            CopyProgress::Pending
        ));
        let timer = job.timer().unwrap();
        assert_ne!(timer.id, timer.request);
        assert!(matches!(job.on_timer(timer), CopyProgress::Copied));
        assert!(job.timer().is_none());
        assert!(job.platform.active.is_empty());
        assert_eq!(job.platform.copies.len(), 2);
        assert!(matches!(job.on_timer(timer), CopyProgress::Pending));
        assert_eq!(job.platform.copies.len(), 2);
    }

    #[test]
    fn exhausted_busy_retries_preserve_color_and_format_for_manual_recovery() {
        let mut job = job((0..4).map(|_| Err(ClipboardError::Busy)));
        let mut progress = job.start(picked(), ColorFormat::Hex);
        let mut previous = None;
        for _ in 0..3 {
            assert!(matches!(progress, CopyProgress::Pending));
            let timer = job.timer().unwrap();
            assert_ne!(Some(timer), previous);
            if let Some(previous) = previous {
                let count = job.platform.copies.len();
                assert!(matches!(job.on_timer(previous), CopyProgress::Pending));
                assert_eq!(job.platform.copies.len(), count);
            }
            progress = job.on_timer(timer);
            previous = Some(timer);
        }
        let CopyProgress::Failed {
            picked: preserved,
            format,
            message,
        } = progress
        else {
            panic!("expected final failure");
        };
        assert_eq!(preserved, picked());
        assert_eq!(format, ColorFormat::Hex);
        assert!(!message.is_empty());
        assert_eq!(job.platform.copies.len(), 4);
        assert_eq!(job.platform.armed.len(), 3);
        assert!(job.platform.active.is_empty());
        assert!(job.request.is_none());
    }

    #[test]
    fn non_busy_failure_does_not_schedule_a_retry() {
        let mut job = job([Err(ClipboardError::Other(Error::new(
            E_FAIL,
            "copy adapter failure",
        )))]);
        let CopyProgress::Failed { message, .. } = job.start(picked(), ColorFormat::Hex) else {
            panic!("expected immediate failure");
        };
        assert!(message.contains("copy adapter failure"));
        assert!(job.platform.armed.is_empty());
        assert!(job.request.is_none());
    }

    #[test]
    fn timer_failure_becomes_recoverable_copy_failure() {
        let mut job = job([Err(ClipboardError::Busy)]);
        job.platform.fail_timer = true;
        assert!(matches!(
            job.start(picked(), ColorFormat::Hex),
            CopyProgress::Failed { .. }
        ));
        assert!(job.request.is_none());
        assert!(job.timer().is_none());
    }

    #[test]
    fn cancellation_and_replacement_reject_old_requests_and_queued_timers() {
        let mut job = job([Err(ClipboardError::Busy), Err(ClipboardError::Busy), Ok(())]);
        job.start(picked(), ColorFormat::Hex);
        let old = job.timer().unwrap();
        job.cancel();
        assert!(job.platform.active.is_empty());
        assert!(matches!(job.on_timer(old), CopyProgress::Pending));
        assert!(matches!(job.attempt(old.request), CopyProgress::Pending));
        job.start(picked(), ColorFormat::Hex);
        let new = job.timer().unwrap();
        assert_ne!(old.id, new.id);
        assert_ne!(old.request, new.request);
        assert!(matches!(job.on_timer(old), CopyProgress::Pending));
        assert!(matches!(
            job.on_timer(CopyTimer {
                id: new.id,
                request: old.request
            }),
            CopyProgress::Pending
        ));
        assert!(matches!(job.attempt(old.request), CopyProgress::Pending));
        assert_eq!(job.platform.copies.len(), 2);
        assert!(matches!(job.on_timer(new), CopyProgress::Copied));
        assert!(job.platform.active.is_empty());
    }

    #[test]
    fn replacement_cancels_active_timer_before_copying_the_new_value() {
        let mut job = job([Err(ClipboardError::Busy), Ok(())]);
        job.start(picked(), ColorFormat::Hex);
        let old = job.timer().unwrap();
        let mut next = picked();
        next.rgb = Rgb8 {
            r: 255,
            g: 255,
            b: 255,
        };
        assert!(matches!(
            job.start(next, ColorFormat::Hex),
            CopyProgress::Copied
        ));
        assert!(job.platform.active.is_empty());
        assert_eq!(job.platform.copies.last().unwrap().1, "#FFFFFF");
        assert!(matches!(job.on_timer(old), CopyProgress::Pending));
        assert_eq!(job.platform.copies.len(), 2);
    }

    #[test]
    fn dropping_a_pending_job_disarms_the_host_timer() {
        let mut job = job([Err(ClipboardError::Busy)]);
        let disarmed = job.platform.disarmed.clone();
        job.start(picked(), ColorFormat::Hex);
        assert!(job.timer().is_some());
        drop(job);
        assert_eq!(disarmed.get(), 1);
    }
}
