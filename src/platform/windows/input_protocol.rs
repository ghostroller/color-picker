//! Pure input ownership protocol; no platform calls or user input contents.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Arming,
    Capturing,
    AwaitDecision,
    Draining,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
    X1,
    X2,
    Escape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Candidate,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub swallow: bool,
    pub signal: Option<Signal>,
}

pub struct Protocol {
    phase: Phase,
    consumed: u8,
}

impl Protocol {
    pub const fn new() -> Self {
        Self {
            phase: Phase::Arming,
            consumed: 0,
        }
    }
    pub fn arm(&mut self) {
        if self.phase == Phase::Arming {
            self.phase = Phase::Capturing;
        }
    }
    pub const fn phase(&self) -> Phase {
        self.phase
    }
    pub fn reject(&mut self) {
        if self.phase == Phase::AwaitDecision {
            self.phase = Phase::Capturing;
        }
    }
    pub fn finish(&mut self) {
        self.phase = if self.consumed == 0 {
            Phase::Stopped
        } else {
            Phase::Draining
        };
    }
    pub fn consumes_wheel(&self) -> bool {
        matches!(self.phase, Phase::Capturing | Phase::AwaitDecision)
    }
    pub fn produces_wheel(&self) -> bool {
        self.phase == Phase::Capturing
    }
    pub fn button(&mut self, button: Button, down: bool) -> Decision {
        let bit = 1 << button as u8;
        let owned = self.consumed & bit != 0;
        if matches!(self.phase, Phase::Arming | Phase::Stopped | Phase::Draining) {
            if owned && !down {
                self.consumed &= !bit;
                if self.consumed == 0 {
                    self.phase = Phase::Stopped;
                }
            }
            return Decision {
                swallow: owned,
                signal: None,
            };
        }
        if down {
            self.consumed |= bit;
            if button == Button::Escape && !owned {
                self.finish();
                return Decision {
                    swallow: true,
                    signal: Some(Signal::Cancel),
                };
            }
            return Decision {
                swallow: true,
                signal: None,
            };
        }
        if !owned {
            // Never consume half of a gesture that began before installation.
            return Decision {
                swallow: false,
                signal: None,
            };
        }
        self.consumed &= !bit;
        let signal = match button {
            Button::Left if self.phase == Phase::Capturing => {
                self.phase = Phase::AwaitDecision;
                Some(Signal::Candidate)
            }
            Button::Right => {
                self.finish();
                Some(Signal::Cancel)
            }
            _ => None,
        };
        Decision {
            swallow: true,
            signal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capturing() -> Protocol {
        let mut protocol = Protocol::new();
        protocol.arm();
        protocol
    }

    #[test]
    fn partial_hook_installation_never_consumes_half_a_gesture() {
        let mut protocol = Protocol::new();
        assert!(!protocol.button(Button::Left, true).swallow);
        assert!(!protocol.button(Button::Escape, true).swallow);
        assert!(!protocol.consumes_wheel());
        protocol.finish(); // The second hook fails before activation.
        assert_eq!(protocol.phase(), Phase::Stopped);
        assert!(!protocol.button(Button::Left, false).swallow);
        protocol.arm();
        assert_eq!(protocol.phase(), Phase::Stopped);
    }

    #[test]
    fn candidate_requires_owned_down_up_and_rejection_allows_another() {
        let mut protocol = capturing();
        assert!(!protocol.button(Button::Left, false).swallow);
        assert!(protocol.button(Button::Left, true).swallow);
        assert_eq!(
            protocol.button(Button::Left, false).signal,
            Some(Signal::Candidate)
        );
        assert_eq!(protocol.phase(), Phase::AwaitDecision);
        assert!(protocol.button(Button::Left, true).swallow);
        assert_eq!(protocol.button(Button::Left, false).signal, None);
        protocol.reject();
        protocol.button(Button::Left, true);
        assert_eq!(
            protocol.button(Button::Left, false).signal,
            Some(Signal::Candidate)
        );
    }

    #[test]
    fn escape_repeats_cancel_once_and_drain_all_owned_releases() {
        let mut protocol = capturing();
        protocol.button(Button::Middle, true);
        assert_eq!(
            protocol.button(Button::Escape, true).signal,
            Some(Signal::Cancel)
        );
        assert_eq!(protocol.button(Button::Escape, true).signal, None);
        assert_eq!(protocol.phase(), Phase::Draining);
        assert!(!protocol.button(Button::X1, true).swallow);
        assert!(!protocol.button(Button::X1, false).swallow);
        assert!(protocol.button(Button::Escape, false).swallow);
        assert_eq!(protocol.phase(), Phase::Draining);
        assert!(protocol.button(Button::Middle, false).swallow);
        assert_eq!(protocol.phase(), Phase::Stopped);
    }

    #[test]
    fn right_click_cancels_on_release_and_failure_drains_side_buttons() {
        let mut protocol = capturing();
        protocol.button(Button::Right, true);
        protocol.button(Button::X2, true);
        assert_eq!(
            protocol.button(Button::Right, false).signal,
            Some(Signal::Cancel)
        );
        protocol.finish(); // Also used for queue, wake and desktop failures.
        protocol.reject();
        assert_eq!(protocol.phase(), Phase::Draining);
        assert!(!protocol.consumes_wheel());
        assert!(protocol.button(Button::X2, false).swallow);
        assert_eq!(protocol.phase(), Phase::Stopped);
    }
}
