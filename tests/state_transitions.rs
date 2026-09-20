use color_picker::core::{
    color::Rgb8,
    geometry::ScreenPointPx,
    state::{AppState, Effect, Event, PickedColor, SampleKind, SessionId, StateMachine},
};

fn picked(kind: SampleKind) -> PickedColor {
    PickedColor {
        rgb: Rgb8::new(64, 158, 255),
        source: ScreenPointPx { x: -120, y: 846 },
        kind,
    }
}

fn starting(machine: &mut StateMachine) -> SessionId {
    let transition = machine.dispatch(Event::Activate).unwrap();
    let session = machine.session_id().unwrap();
    assert_eq!(transition.effect, Some(Effect::StartSession(session)));
    session
}

fn live(machine: &mut StateMachine) -> SessionId {
    let session = starting(machine);
    machine.dispatch(Event::ResourcesReady(session)).unwrap();
    let transition = machine.dispatch(Event::InputReady(session)).unwrap();
    assert_eq!(transition.effect, Some(Effect::BeginLive(session)));
    assert_eq!(machine.state(), AppState::Live { session });
    session
}

#[test]
fn startup_requires_both_resources_and_input_in_either_order() {
    for input_first in [false, true] {
        let mut machine = StateMachine::new();
        let session = starting(&mut machine);
        let (first, second) = if input_first {
            (Event::InputReady(session), Event::ResourcesReady(session))
        } else {
            (Event::ResourcesReady(session), Event::InputReady(session))
        };
        assert!(machine.dispatch(first).unwrap().effect.is_none());
        assert!(matches!(machine.state(), AppState::Starting { .. }));
        assert!(!machine.dispatch(first).unwrap().changed);
        assert_eq!(
            machine.dispatch(second).unwrap().effect,
            Some(Effect::BeginLive(session))
        );
    }
}

#[test]
fn startup_failure_rolls_back_and_does_not_reuse_its_generation() {
    let mut machine = StateMachine::new();
    let failed = starting(&mut machine);
    machine.dispatch(Event::InputReady(failed)).unwrap();
    machine.dispatch(Event::StartFailed(failed)).unwrap();
    assert_eq!(machine.state(), AppState::Idle);
    assert!(starting(&mut machine) > failed);
}

#[test]
fn confirmation_waits_for_input_cleanup_before_exposing_result() {
    let mut machine = StateMachine::new();
    let session = live(&mut machine);
    let color = picked(SampleKind::Live);
    assert!(
        !machine
            .dispatch(Event::InputStopped(session))
            .unwrap()
            .changed
    );
    assert_eq!(machine.state(), AppState::Live { session });
    assert_eq!(
        machine
            .dispatch(Event::Confirm {
                session,
                picked: color,
            })
            .unwrap()
            .effect,
        Some(Effect::FinishSession(session))
    );
    assert_eq!(
        machine.state(),
        AppState::Finishing {
            session,
            picked: Some(color),
        }
    );
    assert_eq!(
        machine
            .dispatch(Event::InputStopped(session))
            .unwrap()
            .effect,
        Some(Effect::ShowResult(color))
    );
    assert_eq!(machine.state(), AppState::Result(color));
    assert_eq!(machine.session_id(), None);
}

#[test]
fn cancellation_from_every_active_stage_drains_to_idle() {
    for stage in 0..3 {
        let mut machine = StateMachine::new();
        let session = if stage == 0 {
            starting(&mut machine)
        } else {
            live(&mut machine)
        };
        if stage == 2 {
            machine.dispatch(Event::Freeze(session)).unwrap();
        }
        assert_eq!(
            machine.dispatch(Event::Cancel(session)).unwrap().effect,
            Some(Effect::FinishSession(session))
        );
        assert_eq!(
            machine.state(),
            AppState::Finishing {
                session,
                picked: None,
            }
        );
        machine.dispatch(Event::InputStopped(session)).unwrap();
        assert_eq!(machine.state(), AppState::Idle);
    }
}

#[test]
fn runtime_failure_drains_without_a_result() {
    let mut machine = StateMachine::new();
    let session = live(&mut machine);
    machine.dispatch(Event::SessionFailed(session)).unwrap();
    machine.dispatch(Event::InputStopped(session)).unwrap();
    assert_eq!(machine.state(), AppState::Idle);
}

#[test]
fn failure_during_draining_discards_the_pending_result() {
    let mut machine = StateMachine::new();
    let session = live(&mut machine);
    machine
        .dispatch(Event::Confirm {
            session,
            picked: picked(SampleKind::Live),
        })
        .unwrap();

    let transition = machine.dispatch(Event::SessionFailed(session)).unwrap();
    assert!(transition.changed);
    assert_eq!(transition.effect, None);
    assert_eq!(
        machine.state(),
        AppState::Finishing {
            session,
            picked: None,
        }
    );
    let repeated = machine.dispatch(Event::SessionFailed(session)).unwrap();
    assert!(!repeated.changed);
    assert_eq!(repeated.effect, None);
    assert_eq!(
        machine
            .dispatch(Event::InputStopped(session))
            .unwrap()
            .effect,
        Some(Effect::ShowIdle)
    );
    assert_eq!(machine.state(), AppState::Idle);
}

#[test]
fn freeze_can_return_to_live_or_confirm_a_cached_color() {
    let mut machine = StateMachine::new();
    let session = live(&mut machine);
    machine.dispatch(Event::Freeze(session)).unwrap();
    assert_eq!(machine.state(), AppState::Frozen { session });
    machine.dispatch(Event::ResumeLive(session)).unwrap();
    assert_eq!(machine.state(), AppState::Live { session });
    machine.dispatch(Event::Freeze(session)).unwrap();
    let color = picked(SampleKind::Frozen);
    machine
        .dispatch(Event::Confirm {
            session,
            picked: color,
        })
        .unwrap();
    machine.dispatch(Event::InputStopped(session)).unwrap();
    assert_eq!(machine.state(), AppState::Result(color));
}

#[test]
fn repeated_activation_never_nests_an_active_session() {
    let mut machine = StateMachine::new();
    let session = starting(&mut machine);
    for advance in [
        Event::ResourcesReady(session),
        Event::InputReady(session),
        Event::Freeze(session),
        Event::Cancel(session),
    ] {
        let before = machine.state();
        assert!(!machine.dispatch(Event::Activate).unwrap().changed);
        assert_eq!(machine.state(), before);
        assert_eq!(machine.session_id(), Some(session));
        machine.dispatch(advance).unwrap();
    }
    assert!(!machine.dispatch(Event::Activate).unwrap().changed);
}

#[test]
fn finishing_accepts_no_new_selection() {
    let mut machine = StateMachine::new();
    let session = live(&mut machine);
    machine.dispatch(Event::Cancel(session)).unwrap();
    assert!(
        !machine
            .dispatch(Event::Confirm {
                session,
                picked: picked(SampleKind::Live),
            })
            .unwrap()
            .changed
    );
    machine.dispatch(Event::InputStopped(session)).unwrap();
    assert_eq!(machine.state(), AppState::Idle);
}

#[test]
fn result_can_close_or_start_a_new_generation() {
    let mut machine = StateMachine::new();
    let first = live(&mut machine);
    for close_first in [false, true] {
        let session = machine.session_id().unwrap();
        machine
            .dispatch(Event::Confirm {
                session,
                picked: picked(SampleKind::Live),
            })
            .unwrap();
        machine.dispatch(Event::InputStopped(session)).unwrap();
        if close_first {
            machine.dispatch(Event::CloseResult).unwrap();
            assert_eq!(machine.state(), AppState::Idle);
        }
        assert!(live(&mut machine) > first);
    }
}

#[test]
fn settings_only_opens_at_idle_and_blocks_activation() {
    let mut machine = StateMachine::new();
    machine.dispatch(Event::OpenSettings).unwrap();
    assert_eq!(machine.state(), AppState::Settings);
    assert!(!machine.dispatch(Event::Activate).unwrap().changed);
    machine.dispatch(Event::CloseSettings).unwrap();
    assert_eq!(machine.state(), AppState::Idle);
    live(&mut machine);
    assert!(!machine.dispatch(Event::OpenSettings).unwrap().changed);
}

#[test]
fn every_old_asynchronous_event_is_ignored_by_the_new_session() {
    let mut machine = StateMachine::new();
    let old = live(&mut machine);
    machine.dispatch(Event::Cancel(old)).unwrap();
    machine.dispatch(Event::InputStopped(old)).unwrap();
    let current = starting(&mut machine);
    let events = [
        Event::ResourcesReady(old),
        Event::InputReady(old),
        Event::StartFailed(old),
        Event::Freeze(old),
        Event::ResumeLive(old),
        Event::Confirm {
            session: old,
            picked: picked(SampleKind::Live),
        },
        Event::Cancel(old),
        Event::SessionFailed(old),
        Event::InputStopped(old),
    ];
    for advance in [
        Event::ResourcesReady(current),
        Event::InputReady(current),
        Event::Freeze(current),
        Event::Cancel(current),
        Event::InputStopped(current),
    ] {
        for event in events {
            let before = machine.state();
            let transition = machine.dispatch(event).unwrap();
            assert!(!transition.changed);
            assert_eq!(transition.effect, None);
            assert_eq!(machine.state(), before);
        }
        machine.dispatch(advance).unwrap();
    }
}
