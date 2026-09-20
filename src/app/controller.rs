//! Main-thread session orchestration. Window callbacks never borrow this owner.
#![forbid(unsafe_code)]

use crate::{
    app::diagnostics,
    core::{
        geometry::ScreenPointPx,
        state::{AppState, Event, PickedColor, SampleKind, StateMachine},
    },
    platform::windows::{
        capture::GdiSampler,
        input::{InputEvent, InputSession},
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
    machine: StateMachine,
    timer: Option<SessionTimer>,
    next_timer: usize,
    session: Option<SessionResources>,
    sample_attempts: u64,
    completed: Option<PickedColor>,
}

struct SessionResources {
    input: InputSession,
    preview: PreviewWindow,
    sampler: GdiSampler,
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
        }
    }

    pub fn start(&mut self) -> Result<bool> {
        if self.active() {
            return Ok(false);
        }
        self.completed = None;
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
                sampler,
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
        // Starting/Live/Finishing use this timer. It also detects worker failure
        // when a queue-full PostMessage could not deliver a wake.
        if let Err(error) = self.replace_timer() {
            self.stop("startup_timer_failure");
            return Err(error);
        }
        diagnostics::event(format_args!("session.starting session={}", session.0));
        Ok(true)
    }

    pub fn state(&self) -> AppState {
        self.machine.state()
    }
    pub fn active(&self) -> bool {
        self.machine.session_id().is_some()
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
        if let Some(resources) = self.session.as_ref() {
            resources.preview.hide();
            if let Err(error) = resources.input.request_finish() {
                diagnostics::event(format_args!("input.finish_request_failed error={error}"));
            }
        }
        if let Err(error) = self.replace_timer() {
            diagnostics::event(format_args!("session.cleanup_timer_failed error={error}"));
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
        if matches!(self.state(), AppState::Live { .. }) {
            let outcome = cursor_position().and_then(|point| self.sample_at(point).map(|_| ()));
            if let Err(error) = outcome {
                self.stop("sample_failure");
                return Err(error);
            }
        }
        self.process_input()
    }

    /// Runs outside callbacks. Only collect a worker after is_finished is true.
    pub fn process_input(&mut self) -> Result<()> {
        let Some(resources) = self.session.as_mut() else {
            return Ok(());
        };
        let events = resources.input.drain_events();
        // Live uses its timer; movement still must be drained for coalescing.
        let _ = resources.input.take_movement();
        let mut error = None;
        for event in events {
            if let Err(failure) = self.handle_input(event) {
                self.stop("input_event_failure");
                error.get_or_insert(failure);
            }
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
                if matches!(self.state(), AppState::Live { .. }) {
                    if let Some(picked) = self.sample_at(point)? {
                        self.transition(Event::Confirm { session, picked })?;
                        self.begin_finish("confirmed");
                    } else if let Some(resources) = self.session.as_ref() {
                        resources.input.reject_candidate()?;
                    }
                } else if let Some(resources) = self.session.as_ref() {
                    resources.input.reject_candidate()?;
                }
            }
            InputEvent::Cancel { session } if self.machine.session_id() == Some(session) => {
                self.stop("cancelled")
            }
            // M3 consumes wheel safely; M4 adds snapshot/zoom handling here.
            InputEvent::Wheel { .. } | InputEvent::Failed { .. } | InputEvent::Stopped { .. } => {}
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
        match resources.sampler.sample_pixel(point) {
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

    fn replace_timer(&mut self) -> Result<()> {
        self.timer.take();
        self.next_timer = self
            .next_timer
            .checked_add(1)
            .ok_or_else(|| Error::new(E_FAIL, "Timer generation exhausted"))?;
        self.timer = Some(SessionTimer::start(self.host, self.next_timer)?);
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
