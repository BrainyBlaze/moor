#[test]
fn accounting_preserves_the_full_u32_overhead_domain() {
    let limit = (u32::MAX as usize, u32::MAX as usize);
    let high_bit = (1usize << 31, 1);
    let reserved = reserve(high_bit, (0, 1), limit).unwrap();

    assert_eq!(reserved, (1 << 31, 2));
    assert_eq!(reserve(limit, (1, 0), limit), None);
}

#[test]
fn viewer_flush_transfers_the_input_buffer() {
    let (send, receive) = bounded(1);
    let sender = ViewerSender(send);
    let mut bytes = Vec::with_capacity(64);
    bytes.extend_from_slice(b"input");
    let allocation = bytes.as_ptr();

    assert!(sender.flush(&mut bytes));
    let Command::Input(sent) = receive.recv().unwrap() else {
        panic!("viewer input command was not queued");
    };
    assert_eq!(sent.as_ptr(), allocation, "the input allocation was cloned");
    assert_eq!(bytes.capacity(), 0, "the producer retained the allocation");
}

#[test]
fn viewer_preserves_native_input_resize_order_and_suppresses_duplicates() {
    let (send, receive) = unbounded();
    let worker = std::thread::spawn(move || {
        let mut ready = [
            InputState::Bytes(b"A".to_vec()),
            InputState::Resize(37, 93),
            InputState::Resize(41, 101),
            InputState::Bytes(b"B".to_vec()),
            InputState::Closed,
        ]
        .into_iter();
        run_viewer_input(
            std::io::empty(),
            ViewerSender(send),
            InputConfig {
                detach: None,
                pass_suspend: true,
                last_size: Some((37, 93)),
            },
            move || ready.next().unwrap(),
            || None,
            || {},
            Instant::now,
        )
    });
    let Command::Input(bytes) = receive.recv().unwrap() else {
        panic!("input preceding the resize was not forwarded first");
    };
    assert_eq!(bytes, b"A");
    let Command::Resize(rows, columns) = receive.recv().unwrap() else {
        panic!("native resize was not forwarded in order");
    };
    assert_eq!((rows, columns), (41, 101));
    let Command::Input(bytes) = receive.recv().unwrap() else {
        panic!("input was not forwarded after the resize");
    };
    assert_eq!(bytes, b"B");
    let Command::Release(done) = receive.recv().unwrap() else {
        panic!("viewer did not release cleanly");
    };
    done.send(true).unwrap();
    worker.join().unwrap();
}

#[test]
fn viewer_forwards_the_complete_native_record_before_detaching() {
    let carrier = b"\x1b[90;44;90;1;0;1_".to_vec();
    let (send, receive) = unbounded();
    let expected = carrier.clone();
    let worker = std::thread::spawn(move || {
        let mut ready = [
            InputState::Bytes(vec![0x1c]),
            InputState::Framed(vec![InputFrame::Key(
                carrier.clone(),
                carrier,
                Some(b'Z'),
                1,
            )]),
        ]
        .into_iter();
        run_viewer_input(
            std::io::empty(),
            ViewerSender(send),
            InputConfig {
                detach: Some(0x1c),
                pass_suspend: true,
                last_size: None,
            },
            move || ready.next().unwrap(),
            || None,
            || {},
            Instant::now,
        )
    });
    let Command::Input(bytes) = receive.recv().unwrap() else {
        panic!("native record following detach was not forwarded");
    };
    assert_eq!(bytes, expected);
    let Command::Release(done) = receive.recv().unwrap() else {
        panic!("viewer did not detach after the native record");
    };
    done.send(true).unwrap();
    worker.join().unwrap();
}

#[test]
fn viewer_keeps_raw_byte_detach_semantics_for_native_batches() {
    let (send, receive) = unbounded();
    let worker = std::thread::spawn(move || {
        let mut ready = [
            InputState::Bytes(vec![0x1c]),
            InputState::Bytes(b"ABC".to_vec()),
        ]
        .into_iter();
        run_viewer_input(
            std::io::empty(),
            ViewerSender(send),
            InputConfig {
                detach: Some(0x1c),
                pass_suspend: true,
                last_size: None,
            },
            move || ready.next().unwrap(),
            || None,
            || {},
            Instant::now,
        )
    });
    let Command::Input(bytes) = receive.recv().unwrap() else {
        panic!("different raw byte was not forwarded before detach");
    };
    assert_eq!(bytes, b"A");
    let Command::Release(done) = receive.recv().unwrap() else {
        panic!("viewer did not detach after the different raw byte");
    };
    done.send(true).unwrap();
    worker.join().unwrap();
}

#[test]
fn viewer_never_matches_detach_against_framed_carrier_syntax() {
    for detach in [0x1b, b'[', b'1', b';', b'_'] {
        let carrier = b"\x1b[65;30;65;1;0;1_".to_vec();
        let (send, receive) = unbounded();
        let expected = carrier.clone();
        let worker = std::thread::spawn(move || {
            let mut ready = [
                InputState::Framed(vec![InputFrame::Key(
                    carrier.clone(),
                    carrier,
                    Some(b'A'),
                    1,
                )]),
                InputState::Closed,
            ]
            .into_iter();
            run_viewer_input(
                std::io::empty(),
                ViewerSender(send),
                InputConfig {
                    detach: Some(detach),
                    pass_suspend: true,
                    last_size: None,
                },
                move || ready.next().unwrap(),
                || None,
                || {},
                Instant::now,
            )
        });
        let Command::Input(bytes) = receive.recv().unwrap() else {
            panic!("framed key was not forwarded for detach {detach:#x}");
        };
        assert_eq!(bytes, expected);
        let Command::Release(done) = receive.recv().unwrap() else {
            panic!("carrier syntax armed detach for {detach:#x}");
        };
        done.send(true).unwrap();
        worker.join().unwrap();
    }
}

#[test]
fn viewer_ignores_framed_key_releases_while_doubling_detach() {
    let down = b"\x1b[220;43;28;1;8;1_".to_vec();
    let up = b"\x1b[220;43;28;0;8;1_".to_vec();
    let modifier = b"\x1b[17;29;0;0;0;1_".to_vec();
    let (send, receive) = unbounded();
    let expected_down = down.clone();
    let expected_up = up.clone();
    let expected_modifier = modifier.clone();
    let worker = std::thread::spawn(move || {
        let mut ready = [
            InputState::Framed(vec![
                InputFrame::Key(down.clone(), down.clone(), Some(0x1c), 1),
                InputFrame::Meta(up),
                InputFrame::Meta(modifier),
                InputFrame::Key(down.clone(), down, Some(0x1c), 1),
            ]),
            InputState::Closed,
        ]
        .into_iter();
        run_viewer_input(
            std::io::empty(),
            ViewerSender(send),
            InputConfig {
                detach: Some(0x1c),
                pass_suspend: true,
                last_size: None,
            },
            move || ready.next().unwrap(),
            || None,
            || {},
            Instant::now,
        )
    });
    let Command::Input(bytes) = receive.recv().unwrap() else {
        panic!("framed detach sequence ended prematurely");
    };
    assert_eq!(
        bytes,
        [expected_up, expected_modifier, expected_down].concat()
    );
    let Command::Release(done) = receive.recv().unwrap() else {
        panic!("doubled framed detach did not keep the viewer attached");
    };
    done.send(true).unwrap();
    worker.join().unwrap();
}

#[test]
fn viewer_applies_framed_repeat_counts_as_semantic_occurrences() {
    let repeated = b"\x1b[220;43;28;1;8;3_".to_vec();
    let once = b"\x1b[220;43;28;1;8;1_".to_vec();
    let different = b"\x1b[65;30;65;1;0;1_".to_vec();
    let (send, receive) = unbounded();
    let expected_once = once.clone();
    let expected_different = different.clone();
    let worker = std::thread::spawn(move || {
        let mut ready = [InputState::Framed(vec![
            InputFrame::Key(repeated, once, Some(0x1c), 3),
            InputFrame::Key(different.clone(), different, Some(b'A'), 1),
        ])]
        .into_iter();
        run_viewer_input(
            std::io::empty(),
            ViewerSender(send),
            InputConfig {
                detach: Some(0x1c),
                pass_suspend: true,
                last_size: None,
            },
            move || ready.next().unwrap(),
            || None,
            || {},
            Instant::now,
        )
    });
    let Command::Input(bytes) = receive.recv().unwrap() else {
        panic!("repeat pair and different framed key were not forwarded");
    };
    assert_eq!(bytes, [expected_once, expected_different].concat());
    let Command::Release(done) = receive.recv().unwrap() else {
        panic!("odd repeat did not leave detach armed");
    };
    done.send(true).unwrap();
    worker.join().unwrap();
}

#[test]
fn viewer_even_framed_repeat_forwards_one_occurrence_without_detaching() {
    let repeated = b"\x1b[220;43;28;1;8;2_".to_vec();
    let once = b"\x1b[220;43;28;1;8;1_".to_vec();
    let (send, receive) = unbounded();
    let expected = once.clone();
    let worker = std::thread::spawn(move || {
        let mut ready = [
            InputState::Framed(vec![InputFrame::Key(repeated, once, Some(0x1c), 2)]),
            InputState::Closed,
        ]
        .into_iter();
        run_viewer_input(
            std::io::empty(),
            ViewerSender(send),
            InputConfig {
                detach: Some(0x1c),
                pass_suspend: true,
                last_size: None,
            },
            move || ready.next().unwrap(),
            || None,
            || {},
            Instant::now,
        )
    });
    let Command::Input(bytes) = receive.recv().unwrap() else {
        panic!("even repeat did not forward one occurrence");
    };
    assert_eq!(bytes, expected);
    let Command::Release(done) = receive.recv().unwrap() else {
        panic!("even repeat incorrectly left detach armed");
    };
    done.send(true).unwrap();
    worker.join().unwrap();
}

#[test]
fn viewer_disable_detach_forwards_the_original_repeated_frame() {
    let repeated = b"\x1b[220;43;28;1;8;3_".to_vec();
    let once = b"\x1b[220;43;28;1;8;1_".to_vec();
    let (send, receive) = unbounded();
    let expected = repeated.clone();
    let worker = std::thread::spawn(move || {
        let mut ready = [
            InputState::Framed(vec![InputFrame::Key(repeated, once, Some(0x1c), 3)]),
            InputState::Closed,
        ]
        .into_iter();
        run_viewer_input(
            std::io::empty(),
            ViewerSender(send),
            InputConfig {
                detach: None,
                pass_suspend: true,
                last_size: None,
            },
            move || ready.next().unwrap(),
            || None,
            || {},
            Instant::now,
        )
    });
    let Command::Input(bytes) = receive.recv().unwrap() else {
        panic!("disabled detach did not forward the frame");
    };
    assert_eq!(bytes, expected);
    let Command::Release(done) = receive.recv().unwrap() else {
        panic!("disabled detach did not close cleanly");
    };
    done.send(true).unwrap();
    worker.join().unwrap();
}
