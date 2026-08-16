#[cfg(test)]
mod tests {
    use super::*;

    fn query_shape() -> QueryShape {
        QueryShape {
            class: 1,
            csi8: false,
            mode: None,
        }
    }

    fn attached_viewer(next: u64) -> Machine {
        let mut machine = Machine::new(7, [1; 16], [2; 16]);
        machine.query_next = next;
        machine.register_controller(7);
        machine
            .transition(Transition::Peer(
                0,
                7,
                Request::Attach(0, 0, true, false, Some([3; 16])),
            ))
            .unwrap();
        machine
    }

    fn resume_viewer(machine: &mut Machine, conn: ConnId) {
        machine.transition(Transition::Disconnect(7)).unwrap();
        machine.register_controller(conn);
        machine
            .transition(Transition::Peer(
                3,
                conn,
                Request::Lease(
                    LeaseRequest {
                        operation: LeaseOperation::Resume,
                        role: LeaseRole::Viewer,
                        epoch: 1,
                        incarnation: [1; 16],
                        token: [3; 16],
                    },
                    Some([4; 16]),
                ),
            ))
            .unwrap();
        machine
            .transition(Transition::Peer(
                4,
                conn,
                Request::Attach(0, 0, false, false, None),
            ))
            .unwrap();
    }

    fn plain_input(request_id: u64, bytes: &[u8]) -> OwnedInput {
        OwnedInput {
            epoch: 1,
            request_id,
            exact_payload: [
                1_u32.to_le_bytes().as_slice(),
                request_id.to_le_bytes().as_slice(),
                &[0],
                bytes,
            ]
            .concat()
            .into(),
        }
    }

    fn fresh_lease(
        machine: &mut Machine,
        now: u64,
        conn: ConnId,
        token: Option<[u8; 16]>,
    ) -> LeaseResult {
        machine.register_controller(conn);
        machine
            .transition(Transition::Peer(
                now,
                conn,
                Request::Lease(LeaseRequest::fresh(LeaseRole::InputOnly), token),
            ))
            .unwrap()
            .into_iter()
            .find_map(|effect| match effect {
                Effect::LeaseReply(id, result) if id == conn => Some(result),
                _ => None,
            })
            .expect("lease result")
    }

    #[test]
    fn mismatched_completion_kind_does_not_consume_a_pending_write() {
        let mut machine = attached_viewer(1);
        let input = plain_input(1, b"pending");
        let effects = machine
            .transition(Transition::Peer(
                1,
                7,
                Request::Input(input.clone(), None),
            ))
            .unwrap();
        let [Effect::Write(ticket, bytes)] = effects.as_slice() else {
            panic!("expected one write");
        };
        assert_eq!(bytes, b"pending");

        assert!(machine
            .transition(Transition::Complete(
                2,
                *ticket,
                Completion::Sources(true),
            ))
            .unwrap()
            .is_empty());
        let completed = machine
            .transition(Transition::Complete(
                3,
                *ticket,
                Completion::Write(bytes.len() as u64, None),
            ))
            .unwrap();
        assert!(matches!(
            completed.as_slice(),
            [Effect::Send(7, Reply::Input(receipt))]
                if InputReceipt::decode(receipt).is_ok_and(|value| {
                    value.status == 0 && value.request == input.request_id
                })
        ));
    }

    #[test]
    fn correlation_exhaustion_reports_once_then_cancels_in_output_order() {
        let mut machine = attached_viewer(u64::MAX);
        let first = machine
            .transition(Transition::Query(
                1,
                Arc::from(b"\x1b[c".as_slice()),
                query_shape(),
                Some(b"old".to_vec()),
            ))
            .unwrap();
        assert!(matches!(
            first.as_slice(),
            [Effect::QuerySend(7, query)] if query.correlation == u64::MAX
        ));

        let exhausted = machine
            .transition(Transition::Query(
                2,
                Arc::from(b"\x1b[c".as_slice()),
                query_shape(),
                Some(b"new".to_vec()),
            ))
            .unwrap();
        assert!(matches!(
            exhausted.as_slice(),
            [
                Effect::Send(7, Reply::ControllerError(13, _)),
                Effect::Close(7),
                Effect::Write(old, old_bytes),
                Effect::Write(new, new_bytes),
            ] if old.get() == 0 && new.get() == 0 && old_bytes == b"old" && new_bytes == b"new"
        ));
        assert!(!machine.status(7).query_available);

        resume_viewer(&mut machine, 8);
        let later = machine
            .transition(Transition::Query(
                5,
                Arc::from(b"\x1b[c".as_slice()),
                query_shape(),
                Some(b"later".to_vec()),
            ))
            .unwrap();
        assert!(matches!(
            later.as_slice(),
            [Effect::Write(ticket, bytes)] if ticket.get() == 0 && bytes == b"later"
        ));
    }

    #[test]
    fn outstanding_limit_wins_when_the_correlation_space_ends_at_the_same_boundary() {
        let mut machine = attached_viewer(u64::MAX - 63);
        for index in 0_u8..64 {
            let effects = machine
                .transition(Transition::Query(
                    u64::from(index),
                    Arc::from(b"\x1b[c".as_slice()),
                    query_shape(),
                    Some(vec![index]),
                ))
                .unwrap();
            assert!(matches!(effects.as_slice(), [Effect::QuerySend(7, _)]));
        }

        let overloaded = machine
            .transition(Transition::Query(
                64,
                Arc::from(b"\x1b[c".as_slice()),
                query_shape(),
                Some(vec![64]),
            ))
            .unwrap();
        assert!(matches!(overloaded.first(), Some(Effect::Close(7))));
        assert_eq!(overloaded.len(), 66);
        assert!(
            !overloaded
                .iter()
                .any(|effect| matches!(effect, Effect::Send(_, Reply::ControllerError(..))))
        );
        for (index, effect) in overloaded[1..].iter().enumerate() {
            assert!(matches!(effect, Effect::Write(ticket, bytes)
                if ticket.get() == 0 && bytes.as_slice() == [index as u8]));
        }

        resume_viewer(&mut machine, 8);
        let exhausted = machine
            .transition(Transition::Query(
                65,
                Arc::from(b"\x1b[c".as_slice()),
                query_shape(),
                Some(b"final".to_vec()),
            ))
            .unwrap();
        assert!(matches!(
            exhausted.as_slice(),
            [
                Effect::Send(8, Reply::ControllerError(13, _)),
                Effect::Close(8),
                Effect::Write(ticket, bytes),
            ] if ticket.get() == 0 && bytes == b"final"
        ));
    }

    #[test]
    fn maximum_epoch_is_granted_once_and_token_failure_consumes_nothing() {
        let mut machine = Machine::new(7, [9; 16], [8; 16]);
        machine.allocated = u32::MAX - 1;

        let failed = fresh_lease(&mut machine, 0, 1, None);
        assert_eq!(
            (failed.outcome, failed.reason),
            (ResultOutcome::Refused, ResultReason::Exhausted)
        );
        let final_grant = fresh_lease(&mut machine, 1, 1, Some([1; 16]));
        assert_eq!(final_grant.epoch, u32::MAX);
        machine
            .transition(Transition::Peer(
                2,
                1,
                Request::Release(u32::MAX, [1; 16]),
            ))
            .unwrap();
        let exhausted = fresh_lease(&mut machine, 2, 2, Some([2; 16]));
        assert_eq!(
            (exhausted.outcome, exhausted.reason),
            (ResultOutcome::Refused, ResultReason::Exhausted)
        );
    }

    #[test]
    fn termination_timeout_reply_follows_the_forced_native_effect() {
        let mut machine = Machine::new(7, [1; 16], [2; 16]);
        machine.configure(b"session".to_vec(), 1024);
        machine.register_controller(7);

        assert!(matches!(
            machine
                .transition(Transition::Peer(
                    0,
                    7,
                    Request::Terminate(b"session", 7, [1; 16], false),
                ))
                .unwrap()
                .as_slice(),
            [Effect::Terminate(false)]
        ));
        assert!(machine
            .transition(Transition::TerminationApplied(3, false))
            .unwrap()
            .is_empty());
        assert!(matches!(
            machine
                .transition(Transition::Tick(10_000))
                .unwrap()
                .as_slice(),
            [Effect::Terminate(true), Effect::ReportTermination(7)]
        ));
        assert!(machine
            .transition(Transition::TerminationApplied(2, false))
            .unwrap()
            .is_empty());
        assert!(matches!(
            machine
                .transition(Transition::ReportTermination(7))
                .unwrap()
                .as_slice(),
            [Effect::Send(7, Reply::Termination(3, 7, 2, _))]
        ));
    }
}
