use moor::session::{
    InputAdmission, LeaseMachine, LeaseOperation, LeaseRequest, LeaseRole, OwnedInput, Phase,
    ResultOutcome, ResultReason, TokenSource, legal_in_phase,
};

struct Tokens(Vec<Option<[u8; 16]>>);
impl TokenSource for Tokens {
    fn token(&mut self) -> Option<[u8; 16]> {
        self.0.remove(0)
    }
}
fn token(n: u8) -> [u8; 16] {
    [n; 16]
}
fn fresh(role: LeaseRole) -> LeaseRequest {
    LeaseRequest {
        operation: LeaseOperation::Fresh,
        role,
        epoch: 0,
        incarnation: [0; 16],
        token: [0; 16],
    }
}
fn input(epoch: u32, id: u64, bytes: &[u8]) -> OwnedInput {
    OwnedInput {
        epoch,
        request_id: id,
        exact_payload: bytes.to_vec(),
    }
}

#[test]
fn disconnect_resume_rotates_token_and_preserves_input_replay() {
    let incarnation = token(9);
    let mut machine = LeaseMachine::new(incarnation);
    let mut tokens = Tokens(vec![Some(token(1)), Some(token(2))]);
    let granted = machine.request(10, &fresh(LeaseRole::InputOnly), 0, &mut tokens);
    assert_eq!(
        (granted.outcome, granted.epoch, granted.token),
        (ResultOutcome::Granted, 1, token(1))
    );
    let request = input(1, 1, b"complete input payload");
    assert_eq!(
        machine.admit_input_at(10, &request, 1),
        InputAdmission::Execute
    );
    assert_eq!(
        machine.admit_input_at(10, &request, 2),
        InputAdmission::Refuse(ResultReason::BadSequence)
    );
    machine
        .finish_input(10, request.clone(), b"exact receipt".to_vec())
        .unwrap();
    machine.disconnect(10);

    let resumed = machine.request(
        11,
        &LeaseRequest {
            operation: LeaseOperation::Resume,
            role: LeaseRole::InputOnly,
            epoch: 1,
            incarnation,
            token: token(1),
        },
        5_000,
        &mut tokens,
    );
    assert_eq!(
        (resumed.outcome, resumed.token),
        (ResultOutcome::Resumed, token(2))
    );
    assert_eq!(
        machine.admit_input(11, &request),
        InputAdmission::Replay(b"exact receipt".to_vec())
    );

    let old = machine.request(
        12,
        &LeaseRequest {
            operation: LeaseOperation::Resume,
            role: LeaseRole::InputOnly,
            epoch: 1,
            incarnation,
            token: token(1),
        },
        5_001,
        &mut Tokens(vec![]),
    );
    assert_eq!(old.outcome, ResultOutcome::Refused);
}

#[test]
fn input_replay_rejects_changed_or_skipped_requests() {
    let mut machine = LeaseMachine::new(token(9));
    let grant = machine.request(
        1,
        &fresh(LeaseRole::Viewer),
        0,
        &mut Tokens(vec![Some(token(1))]),
    );
    let first = input(grant.epoch, 1, b"one");
    assert_eq!(
        machine.admit_input_at(1, &first, 1),
        InputAdmission::Execute
    );
    machine.finish_input(1, first.clone(), vec![7]).unwrap();
    assert_eq!(
        machine.admit_input(1, &first),
        InputAdmission::Replay(vec![7])
    );
    assert_eq!(
        machine.admit_input(1, &input(1, 1, b"changed")),
        InputAdmission::Refuse(ResultReason::BadSequence)
    );
    assert_eq!(
        machine.admit_input(1, &input(1, 3, b"skip")),
        InputAdmission::Refuse(ResultReason::BadSequence)
    );
}

#[test]
fn owner_activity_refreshes_deadline() {
    let mut machine = LeaseMachine::new(token(9));
    let grant = machine.request(
        1,
        &fresh(LeaseRole::Viewer),
        0,
        &mut Tokens(vec![Some(token(1))]),
    );
    machine.touch_owner(1, 9_000).unwrap();
    machine.expire(10_001);
    assert_eq!(
        machine.admit_input_at(1, &input(grant.epoch, 1, b"x"), 10_001),
        InputAdmission::Execute
    );
}

#[test]
fn maximum_epoch_is_granted_once_and_token_failure_consumes_nothing() {
    let mut machine = LeaseMachine::with_allocated(token(9), u32::MAX - 1);
    let failed = machine.request(1, &fresh(LeaseRole::Viewer), 0, &mut Tokens(vec![None]));
    assert_eq!(
        (failed.outcome, failed.reason),
        (ResultOutcome::Refused, ResultReason::Exhausted)
    );
    let final_grant = machine.request(
        1,
        &fresh(LeaseRole::Viewer),
        1,
        &mut Tokens(vec![Some(token(1))]),
    );
    assert_eq!(final_grant.epoch, u32::MAX);
    machine.release(1, u32::MAX, token(1));
    let exhausted = machine.request(
        2,
        &fresh(LeaseRole::Viewer),
        2,
        &mut Tokens(vec![Some(token(2))]),
    );
    assert_eq!(
        (exhausted.outcome, exhausted.reason),
        (ResultOutcome::Refused, ResultReason::Exhausted)
    );
}

#[test]
fn phase_table_fences_state_changing_frames() {
    assert!(legal_in_phase(Phase::Unattached, 0x03));
    assert!(legal_in_phase(Phase::InputOnly, 0x09));
    assert!(!legal_in_phase(Phase::InputOnly, 0x03));
    assert!(legal_in_phase(Phase::Observer, 0x15));
    assert!(legal_in_phase(Phase::Viewer, 0x0c));
    assert!(!legal_in_phase(Phase::Closing, 0x0f));
}
