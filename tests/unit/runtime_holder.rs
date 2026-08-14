#[cfg(test)]
mod descriptor_deadline_tests {
    use super::*;
    use crate::runtime::io::Duplex;
    use crate::runtime::private::lifecycle_running;
    use crate::store::{Commit, Kind, Store, StoreError};
    use std::io::Cursor;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    struct SlowNative(bool, Arc<AtomicBool>);

    impl Native for SlowNative {
        fn resize(&mut self, _: u16, _: u16) -> Result<()> {
            self.0 = true;
            Ok(())
        }
        fn terminate(&mut self, _: bool) -> (u8, bool) {
            (0, false)
        }
        fn exited(&mut self) -> Result<Option<NativeExit>> {
            Ok(None)
        }
        fn abandon(&mut self) {
            self.1.store(true, Ordering::Release);
        }
    }

    #[test]
    fn native_redraw_defaults_to_the_platform_resize_contract() {
        let mut native = SlowNative(false, Arc::new(AtomicBool::new(false)));
        native.redraw(24, 80).unwrap();
        assert!(native.0);
    }

    fn duplex() -> Duplex {
        Duplex::closing(Cursor::new(Vec::new()), std::io::sink(), 1024, || {})
    }

    fn fixture(name: &str) -> (Runtime<SlowNative>, [std::path::PathBuf; 2]) {
        let root = std::env::temp_dir().join(format!(
            "moor-{name}-{}-{}",
            std::process::id(),
            monotonic()
        ));
        let log_path = root.with_extension("log");
        let running = lifecycle_running(
            b"\x01/session",
            (Some(7), 7),
            [1; 16],
            (1, 1, [2; 16]),
            ("posix-bytes", None, None),
        );
        let lifecycle = Store::create(&root, Kind::Exit, 7, running.as_bytes(), 0, 0).unwrap();
        let log = Store::create(&log_path, Kind::Log, 7, b"old", 0, 3).unwrap();
        let runtime = Runtime::new(HolderConfig {
            core: CoreConfig {
                generation: 7,
                identity: b"session".to_vec(),
                incarnation: [1; 16],
                semantic_token: [0; 16],
                replay_limit: 1024,
            },
            pty: duplex(),
            storage: SessionStorage::new(Some((log, 64)), None, lifecycle, 1, 1024),
            status: Vec::new(),
            commit_at: 0,
            synthetic: 0,
            native: SlowNative(false, Arc::new(AtomicBool::new(false))),
        });
        (runtime, [root, log_path])
    }

    fn add_peer(runtime: &mut Runtime<SlowNative>, id: ConnId) {
        runtime.peers.insert(
            id,
            Peer {
                pipe: duplex(),
                codec: Some(Codec::new(Profile::Controller)),
                preface: Vec::new(),
                scope: 7,
                handshaking: false,
                deadline: 0,
                pid: None,
                refusal: None,
            },
        );
        runtime.machine.register_controller(id);
    }

    fn cleanup(paths: [std::path::PathBuf; 2]) {
        for path in paths {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match std::fs::remove_dir_all(&path) {
                    Ok(()) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                    Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                    Err(error) => panic!("remove {}: {error}", path.display()),
                }
            }
        }
    }

    #[test]
    fn expired_termination_abandons_native_resources_before_drive_returns() {
        let (mut runtime, paths) = fixture("termination-abandon");
        let abandoned = runtime.native.1.clone();
        runtime.shutdown_requested(0, true);
        runtime.transition(Transition::Tick(10_000)).unwrap();

        assert_eq!(runtime.drive(|_, _| None, || None).unwrap(), None);
        assert!(
            abandoned.load(Ordering::Acquire),
            "uncertain native resources were dropped synchronously after the deadline"
        );
        drop(runtime);
        cleanup(paths);
    }

    #[test]
    fn each_deferred_descriptor_rechecks_its_absolute_deadline() {
        let (mut runtime, paths) = fixture("descriptor-deadline");
        for id in [1, 2] {
            add_peer(&mut runtime, id);
        }
        let now = monotonic();
        runtime.peers.get_mut(&1).unwrap().deadline = now + 50;
        runtime.peers.get_mut(&2).unwrap().deadline = now + 50;
        runtime.descriptors.extend([
            (1, Descriptor::Attach(81, 24, true, false, Some([3; 16]))),
            (2, Descriptor::Status),
        ]);

        let mut calls = 0;
        runtime.poll_descriptors_with(&mut || {
            calls += 1;
            // v4's status-first attach dropped one deadline recheck from the
            // prefix, so the whole exchange makes exactly three clock reads:
            // the loop admission, the Attached precheck, and send_status's
            // post-payload check. The lapse lands on that LAST read — the one
            // after the slow native operation — which is precisely the
            // recheck this test exists to pin.
            if calls < 3 { now } else { now + 51 }
        });

        assert!(runtime.native.0, "the first descriptor did not run slowly");
        assert!(
            !runtime.peers.contains_key(&1),
            "the first descriptor acknowledged after its whole-exchange deadline"
        );
        assert!(
            !runtime.peers.contains_key(&2),
            "the later descriptor was admitted after its absolute deadline"
        );
        drop(runtime);
        cleanup(paths);
    }

    #[test]
    fn status_rechecks_its_deadline_at_output_issuance() {
        let (mut runtime, paths) = fixture("status-output-deadline");
        add_peer(&mut runtime, 3);
        let now = monotonic();
        runtime.peers.get_mut(&3).unwrap().deadline = now + 50;
        runtime.descriptors.push_back((3, Descriptor::Status));
        let mut calls = 0;
        runtime.poll_descriptors_with(&mut || {
            calls += 1;
            if calls == 1 { now } else { now + 51 }
        });
        assert!(
            !runtime.peers.contains_key(&3),
            "STATUS was issued after its whole-exchange deadline"
        );
        drop(runtime);
        cleanup(paths);
    }

    #[test]
    fn attach_rechecks_its_deadline_before_terminal_output() {
        let (mut runtime, paths) = fixture("attach-output-deadline");
        add_peer(&mut runtime, 4);
        let now = monotonic();
        runtime.peers.get_mut(&4).unwrap().deadline = now + 50;
        runtime.descriptors.push_back((
            4,
            Descriptor::Attach(80, 24, true, false, Some([4; 16])),
        ));
        let mut calls = 0;
        runtime.poll_descriptors_with(&mut || {
            calls += 1;
            if calls < 3 { now } else { now + 51 }
        });
        assert!(!runtime.peers.contains_key(&4));
        assert!(
            !runtime.native.0,
            "terminal output was issued before the final deadline check"
        );
        drop(runtime);
        cleanup(paths);
    }

    #[test]
    fn queued_descriptor_is_authoritative_for_ingress_gating() {
        let (mut runtime, paths) = fixture("descriptor-authority");
        add_peer(&mut runtime, 5);
        assert!(matches!(
            runtime.storage.try_status_snapshot(),
            SnapshotState::Ready(_)
        ));
        runtime.descriptors.push_back((5, Descriptor::Status));

        let status = Message {
            scope: 7,
            kind: 13,
            payload: (&[][..]).into(),
        };
        assert_eq!(
            runtime.controller_message_at(5, &status, monotonic()),
            Err(wire::WireError::Malformed),
            "queue membership must prevent a second request from the same peer"
        );

        runtime.storage.release_status_snapshot();
        drop(runtime);
        cleanup(paths);
    }

    #[test]
    fn expired_identity_ingress_cannot_ack_or_cancel_the_deadline() {
        let (mut runtime, paths) = fixture("controller-ingress-deadline");
        runtime.peers.insert(
            5,
            Peer {
                pipe: duplex(),
                codec: Some(Codec::new(Profile::Controller)),
                preface: Vec::new(),
                scope: 0,
                handshaking: true,
                deadline: 100,
                pid: None,
                refusal: None,
            },
        );
        let hello = Message {
            scope: 0,
            kind: 1,
            payload: wire::controller_hello(b"session")
                .unwrap()
                .as_slice()
                .into(),
        };
        runtime.message_at(5, &hello, 100).unwrap();
        assert!(
            !runtime.peers.contains_key(&5),
            "expired HELLO was acknowledged"
        );

        add_peer(&mut runtime, 6);
        runtime.peers.get_mut(&6).unwrap().deadline = 100;
        let clear = Message {
            scope: 7,
            kind: 0x19,
            payload: wire::log_clear_payload([1; 16], 1)
                .unwrap()
                .as_slice()
                .into(),
        };
        runtime.message_at(6, &clear, 100).unwrap();
        assert!(
            !runtime.peers.contains_key(&6),
            "expired first request canceled the whole-exchange deadline"
        );
        assert_eq!(runtime.storage.pending(), 0);

        runtime.peers.insert(
            7,
            Peer {
                pipe: duplex(),
                codec: Some(Codec::new(Profile::Semantic)),
                preface: Vec::new(),
                scope: 0,
                handshaking: true,
                deadline: 100,
                pid: None,
                refusal: None,
            },
        );
        let semantic = Message {
            scope: 0,
            kind: 1,
            payload: (&[][..]).into(),
        };
        runtime.message_at(7, &semantic, 100).unwrap();
        assert!(
            !runtime.peers.contains_key(&7),
            "expired semantic HELLO reached decoding"
        );
        drop(runtime);
        cleanup(paths);
    }

    #[test]
    fn output_exhaustion_does_not_authenticate_silent_controller_dialects() {
        let (mut runtime, paths) = fixture("output-exhaustion-handshakes");
        for _ in 0..16 {
            let id = runtime.next_peer;
            runtime.accept(duplex(), true, None, false);
            runtime.peer_bytes(id, b"MOOR".to_vec());
        }
        runtime.apply_with([PolicyEffect::OutputExhausted], &mut monotonic, None);
        let overflow = runtime.next_peer;
        runtime.accept(duplex(), true, None, false);
        runtime.peer_bytes(overflow, b"MOOR".to_vec());

        assert_eq!(runtime.peers.len(), 16);
        drop(runtime);
        cleanup(paths);
    }

    #[test]
    fn maximum_peer_id_is_never_inserted_or_wrapped() {
        let (mut runtime, paths) = fixture("peer-id-exhaustion");
        runtime.next_peer = u64::MAX - 1;
        runtime.accept(duplex(), true, Some(1), false);
        runtime.accept(duplex(), true, Some(1), false);

        assert_eq!(runtime.next_peer, u64::MAX);
        assert!(runtime.peers.contains_key(&(u64::MAX - 1)));
        assert!(!runtime.peers.contains_key(&u64::MAX));
        assert_eq!(runtime.peers.len(), 1);
        drop(runtime);
        cleanup(paths);
    }

    #[test]
    fn an_asynchronous_clear_io_failure_closes_without_a_result() {
        let (mut runtime, paths) = fixture("clear-failure");
        add_peer(&mut runtime, 9);

        runtime.storage_done(Done {
            lane: 0,
            purpose: Purpose::Clear(9, 3),
            result: Err(StoreError::Io(std::io::ErrorKind::TimedOut.into())),
        });

        assert!(
            !runtime.peers.contains_key(&9),
            "an indeterminate submitted clear sent a result instead of closing"
        );
        drop(runtime);
        cleanup(paths);
    }

    #[test]
    fn a_pre_mutation_exhausted_clear_returns_unavailable() {
        let (mut runtime, paths) = fixture("clear-exhausted");
        add_peer(&mut runtime, 10);

        runtime.storage_done(Done {
            lane: 0,
            purpose: Purpose::Clear(10, 3),
            result: Err(StoreError::Exhausted),
        });

        assert!(
            runtime.peers.contains_key(&10),
            "a definite pre-mutation refusal was treated as indeterminate"
        );
        drop(runtime);
        cleanup(paths);
    }

    #[test]
    fn failed_clear_result_delivery_quarantines_the_log_lane() {
        let (mut runtime, paths) = fixture("clear-result-loss");
        add_peer(&mut runtime, 12);
        runtime.peers.get_mut(&12).unwrap().pipe.shutdown();

        runtime.storage_done(Done {
            lane: 0,
            purpose: Purpose::Clear(12, 1),
            result: Ok((
                Commit {
                    slot: 0,
                    body: 0,
                    kind: Kind::Log,
                    generation: 7,
                    epoch: 2,
                    index: 2,
                    length: 0,
                    start: 3,
                    end: 3,
                    hash: [0; 32],
                },
                true,
            )),
        });

        assert_eq!(
            runtime.storage.health() & 1,
            0,
            "result loss did not quarantine the submitted clear"
        );
        drop(runtime);
        cleanup(paths);
    }

    #[test]
    fn disconnecting_a_submitted_clear_quarantines_its_log_lane() {
        let (mut runtime, paths) = fixture("clear-disconnect");
        add_peer(&mut runtime, 11);
        assert!(matches!(
            runtime.storage.try_status_snapshot(),
            SnapshotState::Ready(_)
        ));
        runtime.storage.clear(11, 1, 3).unwrap();

        runtime.disconnect(11);

        assert_eq!(runtime.storage.health() & 1, 0);
        assert_eq!(runtime.storage.clear(12, 1, 3), Err(StorageError::Disabled));
        runtime.storage.release_status_snapshot();
        drop(runtime);
        cleanup(paths);
    }
}
