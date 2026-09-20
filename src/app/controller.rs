//! Main-thread session orchestration. Window callbacks never borrow this owner.
#![forbid(unsafe_code)]

use crate::{
    app::diagnostics,
    core::{
        geometry::{ScreenPointPx, freeze_rect},
        state::{AppState, Event, PickedColor, SampleKind, StateMachine},
    },
    platform::windows::{
        capture::GdiSampler,
        input::{InputEvent, InputSession},
        monitors::Monitors,
        session::{SessionTimer, cursor_position, flush_composition},
    },
    ui::windows::{magnifier::MagnifierWindow, preview::PreviewWindow},
};
use std::time::{Duration, Instant};
use windows::{
    Win32::Foundation::{E_FAIL, HANDLE, HWND},
    core::{Error, Result},
};

const MAX_CONSECUTIVE_FAILURES: u32 = 3;

pub struct PreviewController {
    host: HWND,
    machine: StateMachine,
    timer: Option<SessionTimer>,
    next_timer: usize,
    session: Option<SessionResources>,
    sample_attempts: u64,
    completed: Option<PickedColor>,
    wheel: WheelAccumulator,
    pending_hover: Option<ScreenPointPx>,
    last_frozen_draw: Option<Instant>,
}

struct SessionResources {
    input: InputSession,
    preview: PreviewWindow,
    sampler: Option<GdiSampler>,
    magnifier: Option<MagnifierWindow>,
    monitors: Monitors,
    failures: u32,
    input_failure_reported: bool,
    last_sample: Option<PickedColor>,
}

impl PreviewController {
    pub fn new(host: HWND) -> Self {
        Self {
            host,
            machine: StateMachine::new(),
            timer: None,
            next_timer: 0,
            session: None,
            sample_attempts: 0,
            completed: None,
            wheel: WheelAccumulator::default(),
            pending_hover: None,
            last_frozen_draw: None,
        }
    }

    pub fn start(&mut self) -> Result<bool> {
        if !self.activation_allowed() {
            return Ok(false);
        }
        self.completed = None;
        self.wheel = WheelAccumulator::default();
        self.pending_hover = None;
        self.last_frozen_draw = None;
        self.transition(Event::Activate)?;
        let Some(session) = self.machine.session_id() else {
            return Ok(false);
        };
        let resources = (|| {
            let monitors = Monitors::enumerate()?;
            let sampler = GdiSampler::new().map_err(platform_error)?;
            let preview = PreviewWindow::new()?;
            // Resources precede hooks; no partially built UI ever consumes input.
            let input = InputSession::start(session, self.host)?;
            Ok(SessionResources {
                input,
                preview,
                sampler: Some(sampler),
                magnifier: None,
                monitors,
                failures: 0,
                input_failure_reported: false,
                last_sample: None,
            })
        })();
        match resources {
            Ok(resources) => self.session = Some(resources),
            Err(error) => {
                self.transition(Event::StartFailed(session))?;
                return Err(error);
            }
        }
        self.transition(Event::ResourcesReady(session))?;
        // The host waits for both input notifications and the worker HANDLE.
        // Starting/Finishing need no polling timer, even if a wake is lost.
        diagnostics::event(format_args!("session.starting session={}", session.0));
        Ok(true)
    }

    pub fn state(&self) -> AppState {
        self.machine.state()
    }
    pub fn active(&self) -> bool {
        self.machine.session_id().is_some()
    }
    pub fn activation_allowed(&self) -> bool {
        matches!(self.state(), AppState::Idle | AppState::Result(_))
    }
    pub fn input_wait_handle(&self) -> Option<HANDLE> {
        self.session
            .as_ref()
            .and_then(|resources| resources.input.wait_handle())
    }
    pub fn timer_id(&self) -> usize {
        self.timer.as_ref().map_or(0, SessionTimer::id)
    }
    pub fn session_id(&self) -> u64 {
        self.machine.session_id().map_or(0, |id| id.0)
    }
    pub fn sample_attempts(&self) -> u64 {
        self.sample_attempts
    }
    pub fn last_sample(&self) -> Option<PickedColor> {
        self.session
            .as_ref()
            .and_then(|session| session.last_sample)
    }
    pub fn take_result(&mut self) -> Option<PickedColor> {
        self.completed.take()
    }
    pub fn close_result(&mut self) -> Result<()> {
        self.transition(Event::CloseResult)
    }
    pub fn open_settings(&mut self) -> Result<()> {
        self.transition(Event::OpenSettings)
    }
    pub fn close_settings(&mut self) -> Result<()> {
        self.transition(Event::CloseSettings)
    }

    pub fn state_code(&self) -> isize {
        match self.state() {
            AppState::Idle => 0,
            AppState::Starting { .. } => 1,
            AppState::Live { .. } => 2,
            AppState::Frozen { .. } => 3,
            AppState::Finishing { .. } => 4,
            AppState::Result(_) => 5,
            AppState::Settings => 6,
        }
    }

    pub fn stop(&mut self, reason: &str) {
        let Some(session) = self.machine.session_id() else {
            return;
        };
        // Cancellation can invalidate a candidate while its input is draining.
        let event = if matches!(self.state(), AppState::Finishing { .. }) {
            Event::SessionFailed(session)
        } else {
            Event::Cancel(session)
        };
        let _ = self.transition(event);
        self.begin_finish(reason);
    }

    fn begin_finish(&mut self, reason: &str) {
        // Stop sampling now; retain resources until consumed releases drain.
        self.timer.take();
        self.pending_hover = None;
        if let Some(resources) = self.session.as_ref() {
            resources.preview.hide();
            if let Some(magnifier) = resources.magnifier.as_ref() {
                magnifier.hide();
            }
            if let Err(error) = resources.input.request_finish() {
                diagnostics::event(format_args!("input.finish_request_failed error={error}"));
            }
        }
        diagnostics::event(format_args!(
            "session.finishing session={} reason={reason}",
            self.session_id()
        ));
    }

    pub fn on_timer(&mut self, id: usize) -> Result<()> {
        if id == 0 || id != self.timer_id() {
            return Ok(());
        }
        let outcome = match self.state() {
            AppState::Live { .. } => {
                cursor_position().and_then(|point| self.sample_at(point).map(|_| ()))
            }
            AppState::Frozen { .. } => self.flush_frozen_hover(),
            _ => Ok(()),
        };
        if let Err(error) = outcome {
            self.stop("sample_failure");
            return Err(error);
        }
        self.process_input()
    }

    /// Runs outside callbacks. Only collect a worker after is_finished is true.
    pub fn process_input(&mut self) -> Result<()> {
        let Some(resources) = self.session.as_mut() else {
            return Ok(());
        };
        let events = resources.input.drain_events();
        let movement = resources.input.take_movement();
        let mut error = None;
        for event in events {
            if let Err(failure) = self.handle_input(event) {
                self.stop("input_event_failure");
                error.get_or_insert(failure);
            }
        }
        if let Some(point) = movement
            && matches!(self.state(), AppState::Frozen { .. })
            && let Err(failure) = self.queue_frozen_hover(point)
        {
            self.stop("frozen_preview_failure");
            error.get_or_insert(failure);
        }
        if let Some(resources) = self.session.as_mut()
            && !resources.input_failure_reported
            && let Some(failure) = resources.input.failure()
        {
            resources.input_failure_reported = true;
            error.get_or_insert_with(|| platform_error(failure));
            self.stop("input_failure");
        }
        let finished = self
            .session
            .as_ref()
            .is_some_and(|resources| resources.input.is_finished());
        if finished {
            let outcome = self
                .session
                .as_mut()
                .and_then(|resources| resources.input.try_join());
            // The worker may have queued cancellation between the earlier
            // drain and is_finished. Join first, then drain its final events so
            // a late Cancel cannot accidentally expose a pending result.
            let final_events = self
                .session
                .as_mut()
                .map(|resources| resources.input.drain_events())
                .unwrap_or_default();
            for event in final_events {
                if let Err(failure) = self.handle_input(event) {
                    self.stop("final_input_failure");
                    error.get_or_insert(failure);
                }
            }
            if let Some(Err(failure)) = outcome {
                let already_reported = self
                    .session
                    .as_ref()
                    .is_some_and(|resources| resources.input_failure_reported);
                if !already_reported {
                    error.get_or_insert_with(|| platform_error(failure));
                }
                self.stop("input_thread_failure");
            }
            if !matches!(self.state(), AppState::Finishing { .. }) {
                error.get_or_insert_with(|| Error::new(E_FAIL, "输入线程意外结束，取色已取消"));
                self.stop("input_thread_stopped");
            }
            let session = self
                .machine
                .session_id()
                .expect("resources belong to an active session");
            self.timer.take();
            self.session.take();
            self.transition(Event::InputStopped(session))?;
            if let AppState::Result(picked) = self.state() {
                self.completed = Some(picked);
            }
            diagnostics::event(format_args!(
                "preview.stopped session={} timer=0 hooks=0 resources_released=true",
                session.0
            ));
        }
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn handle_input(&mut self, event: InputEvent) -> Result<()> {
        match event {
            InputEvent::Ready { session } if self.machine.session_id() == Some(session) => {
                self.transition(Event::InputReady(session))?;
                if matches!(self.state(), AppState::Live { .. }) {
                    self.replace_timer()?;
                    let point = cursor_position()?;
                    self.sample_at(point)?;
                    diagnostics::event(format_args!(
                        "preview.started session={} timer={} interval_ms=17 hooks_ready=true",
                        session.0,
                        self.timer_id()
                    ));
                }
            }
            InputEvent::Candidate { session, point }
                if self.machine.session_id() == Some(session) =>
            {
                let picked = match self.state() {
                    AppState::Live { .. } => self.sample_at(point)?,
                    AppState::Frozen { .. } => self
                        .session
                        .as_ref()
                        .and_then(|resources| resources.magnifier.as_ref())
                        .and_then(|magnifier| magnifier.hit_test(point)),
                    _ => None,
                };
                if let Some(picked) = picked {
                    self.transition(Event::Confirm { session, picked })?;
                    self.begin_finish("confirmed");
                } else if let Some(resources) = self.session.as_ref() {
                    resources.input.reject_candidate()?;
                }
            }
            InputEvent::Cancel { session } if self.machine.session_id() == Some(session) => {
                self.stop("cancelled")
            }
            InputEvent::Wheel {
                session,
                point,
                delta,
            } if self.machine.session_id() == Some(session) => self.on_wheel(point, delta)?,
            InputEvent::Failed { .. } | InputEvent::Stopped { .. } => {}
            _ => {}
        }
        Ok(())
    }

    fn sample_at(&mut self, point: ScreenPointPx) -> Result<Option<PickedColor>> {
        let Some(resources) = self.session.as_mut() else {
            return Ok(None);
        };
        let Some(monitor) = resources.monitors.at(point).copied() else {
            resources.last_sample = None;
            resources.preview.hide();
            return unavailable(resources, Error::new(E_FAIL, "该位置没有有效显示器"));
        };
        if resources
            .preview
            .rect()
            .is_some_and(|rect| rect.contains(point))
        {
            resources.preview.hide();
            flush_composition()?;
        }
        self.sample_attempts = self.sample_attempts.saturating_add(1);
        let sampler = resources
            .sampler
            .as_mut()
            .ok_or_else(|| Error::new(E_FAIL, "实时采样资源不可用"))?;
        match sampler.sample_pixel(point) {
            Ok(rgb) => {
                resources.failures = 0;
                let picked = PickedColor {
                    rgb,
                    source: point,
                    kind: SampleKind::Live,
                };
                resources.last_sample = Some(picked);
                resources
                    .preview
                    .update(point, Some(rgb), monitor.work_area)?;
                Ok(Some(picked))
            }
            Err(error) => {
                resources.last_sample = None;
                resources.preview.update(point, None, monitor.work_area)?;
                unavailable(resources, platform_error(error))
            }
        }
    }

    fn on_wheel(&mut self, point: ScreenPointPx, delta: i16) -> Result<()> {
        if !matches!(
            self.state(),
            AppState::Live { .. } | AppState::Frozen { .. }
        ) {
            return Ok(());
        }
        let steps = self.wheel.push(delta);
        for _ in 0..steps.unsigned_abs() {
            match self.state() {
                AppState::Live { .. } if steps > 0 => self.freeze(point)?,
                AppState::Frozen { .. } => {
                    let keep_frozen = self
                        .session
                        .as_ref()
                        .and_then(|resources| resources.magnifier.as_ref())
                        .ok_or_else(|| Error::new(E_FAIL, "冻结窗口不可用"))?
                        .change_scale(steps > 0, point)?;
                    if !keep_frozen {
                        self.resume_live()?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn freeze(&mut self, point: ScreenPointPx) -> Result<()> {
        let Some(resources) = self.session.as_mut() else {
            return Ok(());
        };
        let monitor = resources
            .monitors
            .at(point)
            .copied()
            .ok_or_else(|| Error::new(E_FAIL, "该位置没有有效显示器"))?;
        let rect = freeze_rect(point, monitor.bounds)
            .ok_or_else(|| Error::new(E_FAIL, "无法确定冻结区域"))?;
        self.timer.take();
        resources.preview.hide();
        flush_composition()?;
        let image = resources
            .sampler
            .as_mut()
            .ok_or_else(|| Error::new(E_FAIL, "实时采样资源不可用"))?
            .capture_rect(rect)
            .map_err(platform_error)?;
        resources.sampler.take();
        resources.magnifier = Some(MagnifierWindow::new(image, point, monitor.work_area)?);
        let session = resources.input.session_id();
        self.transition(Event::Freeze(session))?;
        self.pending_hover = None;
        self.last_frozen_draw = Some(Instant::now());
        diagnostics::event(format_args!(
            "session.frozen session={} sampler_released=true timer=0",
            session.0
        ));
        Ok(())
    }

    fn resume_live(&mut self) -> Result<()> {
        self.timer.take();
        self.pending_hover = None;
        let Some(resources) = self.session.as_mut() else {
            return Ok(());
        };
        if let Some(magnifier) = resources.magnifier.take() {
            magnifier.hide();
        }
        flush_composition()?;
        resources.sampler = Some(GdiSampler::new().map_err(platform_error)?);
        resources.last_sample = None;
        resources.failures = 0;
        let session = resources.input.session_id();
        self.transition(Event::ResumeLive(session))?;
        self.replace_timer()?;
        self.sample_at(cursor_position()?)?;
        diagnostics::event(format_args!("session.resumed_live session={}", session.0));
        Ok(())
    }

    fn queue_frozen_hover(&mut self, point: ScreenPointPx) -> Result<()> {
        self.pending_hover = Some(point);
        if self
            .last_frozen_draw
            .is_none_or(|last| last.elapsed() >= Duration::from_millis(17))
        {
            self.flush_frozen_hover()
        } else if self.timer.is_none() {
            // A single delayed redraw, never a periodic timer at rest.
            self.replace_timer()
        } else {
            Ok(())
        }
    }

    fn flush_frozen_hover(&mut self) -> Result<()> {
        self.timer.take();
        if let Some(point) = self.pending_hover.take()
            && let Some(magnifier) = self
                .session
                .as_ref()
                .and_then(|resources| resources.magnifier.as_ref())
        {
            magnifier.update_hover(point)?;
            self.last_frozen_draw = Some(Instant::now());
        }
        Ok(())
    }

    fn replace_timer(&mut self) -> Result<()> {
        self.next_timer = self
            .next_timer
            .checked_add(1)
            .ok_or_else(|| Error::new(E_FAIL, "Timer generation exhausted"))?;
        // Preserve the old timer if creating its replacement fails.
        let replacement = SessionTimer::start(self.host, self.next_timer)?;
        self.timer = Some(replacement);
        Ok(())
    }

    fn transition(&mut self, event: Event) -> Result<()> {
        self.machine.dispatch(event).map_err(platform_error)?;
        Ok(())
    }
}

impl Drop for PreviewController {
    fn drop(&mut self) {
        // Normal shutdown already pumped through InputStopped. A fatal host
        // failure still asks the worker to release its own hooks.
        if self.session.is_some() {
            self.stop("host_failure");
        }
        self.timer.take();
    }
}

fn unavailable(resources: &mut SessionResources, error: Error) -> Result<Option<PickedColor>> {
    resources.failures = resources.failures.saturating_add(1);
    if resources.failures == 1 {
        diagnostics::event(format_args!("preview.sample_unavailable error={error}"));
    }
    if resources.failures >= MAX_CONSECUTIVE_FAILURES {
        Err(error)
    } else {
        Ok(None)
    }
}

fn platform_error(error: impl std::fmt::Display) -> Error {
    Error::new(E_FAIL, error.to_string())
}

#[derive(Default)]
struct WheelAccumulator(i32);
impl WheelAccumulator {
    fn push(&mut self, delta: i16) -> i32 {
        self.0 += i32::from(delta);
        let steps = (self.0 / 120).clamp(-8, 8);
        self.0 %= 120;
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::WheelAccumulator;
    #[test]
    fn partial_wheel_ticks_accumulate_and_extreme_deltas_are_bounded() {
        let mut wheel = WheelAccumulator::default();
        assert_eq!(wheel.push(60), 0);
        assert_eq!(wheel.push(60), 1);
        assert_eq!(wheel.push(-90), 0);
        assert_eq!(wheel.push(-30), -1);
        assert_eq!(wheel.push(i16::MAX), 8);
        assert!(wheel.push(i16::MIN).abs() <= 8);
    }
}
