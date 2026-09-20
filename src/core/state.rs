use super::{color::Rgb8, geometry::ScreenPointPx};

/// A generation token carried by every asynchronous session event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    Live,
    Frozen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickedColor {
    pub rgb: Rgb8,
    pub source: ScreenPointPx,
    pub kind: SampleKind,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    #[default]
    Idle,
    Starting {
        session: SessionId,
        resources_ready: bool,
        input_ready: bool,
    },
    Live {
        session: SessionId,
    },
    Frozen {
        session: SessionId,
    },
    Finishing {
        session: SessionId,
        picked: Option<PickedColor>,
    },
    Result(PickedColor),
    Settings,
}

impl AppState {
    pub const fn session_id(self) -> Option<SessionId> {
        match self {
            Self::Starting { session, .. }
            | Self::Live { session }
            | Self::Frozen { session }
            | Self::Finishing { session, .. } => Some(session),
            Self::Idle | Self::Result(_) | Self::Settings => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Activate,
    ResourcesReady(SessionId),
    InputReady(SessionId),
    /// Sent only after the platform has rolled back all startup resources,
    /// including any input thread that was partially created.
    StartFailed(SessionId),
    Freeze(SessionId),
    ResumeLive(SessionId),
    Confirm {
        session: SessionId,
        picked: PickedColor,
    },
    Cancel(SessionId),
    SessionFailed(SessionId),
    /// The input thread has exited and hooks/session resources are released.
    InputStopped(SessionId),
    CloseResult,
    OpenSettings,
    CloseSettings,
}

impl Event {
    pub const fn session_id(self) -> Option<SessionId> {
        match self {
            Self::ResourcesReady(session)
            | Self::InputReady(session)
            | Self::StartFailed(session)
            | Self::Freeze(session)
            | Self::ResumeLive(session)
            | Self::Confirm { session, .. }
            | Self::Cancel(session)
            | Self::SessionFailed(session)
            | Self::InputStopped(session) => Some(session),
            Self::Activate | Self::CloseResult | Self::OpenSettings | Self::CloseSettings => None,
        }
    }
}

/// Work to execute after releasing the state machine's mutable borrow. This
/// keeps Win32 calls that may reenter window procedures out of state updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    StartSession(SessionId),
    BeginLive(SessionId),
    ShowFrozen(SessionId),
    ResumeLive(SessionId),
    FinishSession(SessionId),
    ShowResult(PickedColor),
    ShowSettings,
    ShowIdle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub changed: bool,
    pub effect: Option<Effect>,
}

impl Transition {
    const IGNORED: Self = Self {
        changed: false,
        effect: None,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionIdExhausted;

impl std::fmt::Display for SessionIdExhausted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("session identifier space exhausted")
    }
}

impl std::error::Error for SessionIdExhausted {}

#[derive(Debug, Default)]
pub struct StateMachine {
    state: AppState,
    last_session: u64,
}

impl StateMachine {
    pub const fn new() -> Self {
        Self {
            state: AppState::Idle,
            last_session: 0,
        }
    }

    pub const fn state(&self) -> AppState {
        self.state
    }

    pub const fn session_id(&self) -> Option<SessionId> {
        self.state.session_id()
    }

    pub fn dispatch(&mut self, event: Event) -> Result<Transition, SessionIdExhausted> {
        if let Some(session) = event.session_id()
            && Some(session) != self.session_id()
        {
            return Ok(Transition::IGNORED);
        }

        let previous = self.state;
        let effect = match (self.state, event) {
            (AppState::Idle | AppState::Result(_), Event::Activate) => {
                let next = self.last_session.checked_add(1).ok_or(SessionIdExhausted)?;
                self.last_session = next;
                let session = SessionId(next);
                self.state = AppState::Starting {
                    session,
                    resources_ready: false,
                    input_ready: false,
                };
                Some(Effect::StartSession(session))
            }
            (
                AppState::Starting {
                    session,
                    resources_ready,
                    input_ready,
                },
                Event::ResourcesReady(_) | Event::InputReady(_),
            ) => {
                let resources_ready = resources_ready || matches!(event, Event::ResourcesReady(_));
                let input_ready = input_ready || matches!(event, Event::InputReady(_));
                if resources_ready && input_ready {
                    self.state = AppState::Live { session };
                    Some(Effect::BeginLive(session))
                } else {
                    self.state = AppState::Starting {
                        session,
                        resources_ready,
                        input_ready,
                    };
                    None
                }
            }
            (AppState::Starting { .. }, Event::StartFailed(_)) => {
                self.state = AppState::Idle;
                Some(Effect::ShowIdle)
            }
            (AppState::Live { session }, Event::Freeze(_)) => {
                self.state = AppState::Frozen { session };
                Some(Effect::ShowFrozen(session))
            }
            (AppState::Frozen { session }, Event::ResumeLive(_)) => {
                self.state = AppState::Live { session };
                Some(Effect::ResumeLive(session))
            }
            (
                AppState::Live { session } | AppState::Frozen { session },
                Event::Confirm { picked, .. },
            ) => {
                self.state = AppState::Finishing {
                    session,
                    picked: Some(picked),
                };
                Some(Effect::FinishSession(session))
            }
            (
                AppState::Starting { session, .. }
                | AppState::Live { session }
                | AppState::Frozen { session },
                Event::Cancel(_) | Event::SessionFailed(_),
            ) => {
                self.state = AppState::Finishing {
                    session,
                    picked: None,
                };
                Some(Effect::FinishSession(session))
            }
            (AppState::Finishing { session, .. }, Event::SessionFailed(_)) => {
                // Draining is already underway. Invalidate any pending result
                // without issuing another stop request or skipping cleanup.
                self.state = AppState::Finishing {
                    session,
                    picked: None,
                };
                None
            }
            (AppState::Finishing { picked, .. }, Event::InputStopped(_)) => {
                if let Some(picked) = picked {
                    self.state = AppState::Result(picked);
                    Some(Effect::ShowResult(picked))
                } else {
                    self.state = AppState::Idle;
                    Some(Effect::ShowIdle)
                }
            }
            (AppState::Result(_), Event::CloseResult)
            | (AppState::Settings, Event::CloseSettings) => {
                self.state = AppState::Idle;
                Some(Effect::ShowIdle)
            }
            (AppState::Idle, Event::OpenSettings) => {
                self.state = AppState::Settings;
                Some(Effect::ShowSettings)
            }
            _ => None,
        };
        Ok(Transition {
            changed: previous != self.state,
            effect,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_generation_never_wraps() {
        let mut machine = StateMachine {
            last_session: u64::MAX,
            ..StateMachine::new()
        };
        assert_eq!(machine.dispatch(Event::Activate), Err(SessionIdExhausted));
        assert_eq!(machine.state(), AppState::Idle);
    }
}
