use moor::session::{
    Completion, Effect, Effects, LeaseOperation, LeaseRequest, LeaseResult, LeaseRole, Machine,
    OwnedInput, Reply, Request, ResultOutcome, ResultReason, Transition, WriteTicket,
};
use moor::wire::InputReceipt;

fn token(n: u8) -> [u8; 16] {
    [n; 16]
}

fn new_machine(incarnation: [u8; 16]) -> Machine {
    Machine::new(7, incarnation, [8; 16])
}

fn request<'a>(machine: &mut Machine, now: u64, conn: u64, request: Request<'a>) -> Effects {
    machine
        .transition(Transition::Peer(now, conn, request))
        .unwrap()
}

fn transition<'a>(machine: &mut Machine, event: Transition<'a>) -> Effects {
    machine.transition(event).unwrap()
}

fn lease(
    machine: &mut Machine,
    now: u64,
    conn: u64,
    request_: LeaseRequest,
    token: Option<[u8; 16]>,
) -> LeaseResult {
    request(machine, now, conn, Request::Lease(request_, token))
        .into_iter()
        .find_map(|effect| match effect {
            Effect::LeaseReply(id, result) if id == conn => Some(result),
            _ => None,
        })
        .expect("lease result")
}

fn fresh(machine: &mut Machine, now: u64, conn: u64, token: Option<[u8; 16]>) -> LeaseResult {
    machine.register_controller(conn);
    lease(
        machine,
        now,
        conn,
        LeaseRequest::fresh(LeaseRole::InputOnly),
        token,
    )
}

fn input(epoch: u32, request: u64, terminal: &[u8]) -> OwnedInput {
    OwnedInput {
        epoch,
        request_id: request,
        exact_payload: [
            epoch.to_le_bytes().as_slice(),
            request.to_le_bytes().as_slice(),
            &[0],
            terminal,
        ]
        .concat()
        .into(),
    }
}

fn write(effects: Effects) -> WriteTicket {
    effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Write(ticket, _) => Some(ticket),
            _ => None,
        })
        .expect("terminal write")
}

fn receipt(effects: Effects) -> Vec<u8> {
    effects
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Send(_, Reply::Input(receipt)) => Some(receipt),
            _ => None,
        })
        .expect("input receipt")
}

fn disconnect(machine: &mut Machine, conn: u64) {
    transition(machine, Transition::Disconnect(conn));
}

#[test]
fn disconnect_resume_rotates_token_and_preserves_input_replay() {
    let incarnation = token(9);
    let mut machine = new_machine(incarnation);
    let granted = fresh(&mut machine, 0, 10, Some(token(1)));
    assert_eq!(
        (granted.outcome, granted.epoch, granted.token),
        (ResultOutcome::Granted, 1, token(1))
    );
    let input = input(1, 1, b"complete input payload");
    let ticket = write(request(
        &mut machine,
        1,
        10,
        Request::Input(input.clone(), None),
    ));
    assert!(request(&mut machine, 2, 10, Request::Input(input.clone(), None)).is_empty());
    let exact = receipt(transition(
        &mut machine,
        Transition::Complete(3, ticket, Completion::Write(22, None)),
    ));
    disconnect(&mut machine, 10);
    machine.register_controller(11);
    let resumed = lease(
        &mut machine,
        5_000,
        11,
        LeaseRequest {
            operation: LeaseOperation::Resume,
            role: LeaseRole::InputOnly,
            epoch: 1,
            incarnation,
            token: token(1),
        },
        Some(token(2)),
    );
    assert_eq!(
        (resumed.outcome, resumed.token),
        (ResultOutcome::Resumed, token(2))
    );
    assert_eq!(
        receipt(request(
            &mut machine,
            5_000,
            11,
            Request::Input(input, None)
        )),
        exact
    );
    machine.register_controller(12);
    let old = lease(
        &mut machine,
        5_001,
        12,
        LeaseRequest {
            operation: LeaseOperation::Resume,
            role: LeaseRole::InputOnly,
            epoch: 1,
            incarnation,
            token: token(1),
        },
        None,
    );
    assert_eq!(old.outcome, ResultOutcome::Refused);
}

#[test]
fn input_replay_rejects_changed_or_skipped_requests() {
    let mut machine = new_machine(token(9));
    let grant = fresh(&mut machine, 0, 1, Some(token(1)));
    let first = input(grant.epoch, 1, b"one");
    let ticket = write(request(
        &mut machine,
        1,
        1,
        Request::Input(first.clone(), None),
    ));
    let exact = receipt(transition(
        &mut machine,
        Transition::Complete(1, ticket, Completion::Write(3, None)),
    ));
    assert_eq!(
        receipt(request(&mut machine, 1, 1, Request::Input(first, None))),
        exact
    );
    for refused in [input(1, 1, b"changed"), input(1, 3, b"skip")] {
        let value = InputReceipt::decode(&receipt(request(
            &mut machine,
            1,
            1,
            Request::Input(refused, None),
        )))
        .unwrap();
        assert_eq!((value.status, value.result), (1, 6));
    }
}

#[test]
fn resumed_exact_input_waits_for_the_original_write_and_receives_its_result() {
    let incarnation = token(9);
    let mut machine = new_machine(incarnation);
    let grant = fresh(&mut machine, 0, 10, Some(token(1)));
    let input = input(grant.epoch, 1, b"slow write");
    let ticket = write(request(
        &mut machine,
        1,
        10,
        Request::Input(input.clone(), None),
    ));
    disconnect(&mut machine, 10);
    machine.register_controller(11);
    let resumed = lease(
        &mut machine,
        2,
        11,
        LeaseRequest {
            operation: LeaseOperation::Resume,
            role: LeaseRole::InputOnly,
            epoch: grant.epoch,
            incarnation,
            token: grant.token,
        },
        Some(token(2)),
    );
    assert_eq!(resumed.outcome, ResultOutcome::Resumed);
    assert!(request(&mut machine, 3, 11, Request::Input(input.clone(), None)).is_empty());
    let exact = receipt(transition(
        &mut machine,
        Transition::Complete(3, ticket, Completion::Write(10, None)),
    ));
    assert_eq!(
        receipt(request(&mut machine, 4, 11, Request::Input(input, None))),
        exact
    );
}

#[test]
fn owner_activity_refreshes_deadline() {
    let mut machine = new_machine(token(9));
    let grant = fresh(&mut machine, 0, 1, Some(token(1)));
    request(&mut machine, 9_000, 1, Request::Resize(grant.epoch, 0, 0));
    transition(&mut machine, Transition::Tick(10_001));
    assert!(machine.status(1).owns_lease);
    assert!(matches!(
        request(
            &mut machine,
            10_001,
            1,
            Request::Input(input(grant.epoch, 1, b"x"), None)
        )
        .first(),
        Some(Effect::Write(..))
    ));
}

#[test]
fn expiry_reports_and_clears_the_current_owner_once() {
    let mut machine = new_machine(token(9));
    fresh(&mut machine, 0, 7, Some(token(1)));
    transition(&mut machine, Transition::Tick(9_999));
    assert!(machine.status(7).owns_lease);
    transition(&mut machine, Transition::Tick(10_000));
    assert!(!machine.status(7).owns_lease);
    let effects = transition(&mut machine, Transition::Tick(10_001));
    assert!(effects.is_empty());
}

#[test]
fn late_keepalive_resize_and_input_cannot_revive_an_expired_lease() {
    let mut machine = new_machine(token(9));
    let grant = fresh(&mut machine, 0, 1, Some(token(1)));
    let effects = request(
        &mut machine,
        10_000,
        1,
        Request::Keepalive(grant.epoch, grant.token),
    );
    assert!(matches!(
        effects.as_slice(),
        [
            Effect::Send(1, Reply::ControllerError(15, _)),
            Effect::Close(1)
        ]
    ));

    let mut machine = new_machine(token(9));
    let grant = fresh(&mut machine, 0, 1, Some(token(1)));
    assert!(
        machine
            .transition(Transition::Peer(
                10_000,
                1,
                Request::Resize(grant.epoch, 0, 0),
            ))
            .is_err()
    );

    let mut machine = new_machine(token(9));
    let grant = fresh(&mut machine, 0, 1, Some(token(1)));
    let refused = InputReceipt::decode(&receipt(request(
        &mut machine,
        10_000,
        1,
        Request::Input(input(grant.epoch, 1, b"x"), None),
    )))
    .unwrap();
    assert_eq!((refused.status, refused.result), (1, 15));
}

#[test]
fn invalid_input_sequence_does_not_refresh_the_owner_deadline() {
    let mut machine = new_machine(token(9));
    let grant = fresh(&mut machine, 0, 1, Some(token(1)));
    let refused = InputReceipt::decode(&receipt(request(
        &mut machine,
        9_999,
        1,
        Request::Input(input(grant.epoch, 2, b"skip"), None),
    )))
    .unwrap();
    assert_eq!((refused.status, refused.result), (1, 6));
    transition(&mut machine, Transition::Tick(10_000));
    assert!(!machine.status(1).owns_lease);
}

#[test]
fn machine_phase_table_fences_state_changing_frames() {
    let mut machine = new_machine(token(9));
    machine.register_controller(1);
    assert!(machine.legal(1, 0x03));
    let grant = lease(
        &mut machine,
        0,
        1,
        LeaseRequest::fresh(LeaseRole::InputOnly),
        Some(token(1)),
    );
    assert!(machine.legal(1, 0x09));
    assert!(!machine.legal(1, 0x03));
    request(
        &mut machine,
        1,
        1,
        Request::Release(grant.epoch, grant.token),
    );
    assert!(machine.legal(1, 0x03));

    machine.register_controller(2);
    request(
        &mut machine,
        0,
        2,
        Request::Attach(0, 0, false, false, None),
    );
    assert!(machine.legal(2, 0x15));
    assert!(!machine.legal(2, 0x09));
}

#[test]
fn typed_lease_payloads_round_trip_exact_wire_bytes() {
    let request = LeaseRequest {
        operation: LeaseOperation::Resume,
        role: LeaseRole::InputOnly,
        epoch: 7,
        incarnation: token(8),
        token: token(9),
    };
    let encoded = request.encode_wire().unwrap();
    assert_eq!(encoded.len(), 40);
    assert_eq!(LeaseRequest::decode_wire(&encoded).unwrap(), request);

    let result = LeaseResult {
        outcome: ResultOutcome::Granted,
        reason: ResultReason::None,
        role: LeaseRole::Viewer,
        epoch: 8,
        token: token(10),
    };
    let encoded = result.encode_wire().unwrap();
    assert_eq!(encoded.len(), 24);
    assert_eq!(LeaseResult::decode_wire(&encoded).unwrap(), result);
}
