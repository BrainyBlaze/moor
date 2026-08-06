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
}
