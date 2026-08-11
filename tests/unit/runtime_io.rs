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

#[derive(Debug, Eq, PartialEq)]
enum ObservedInput {
    Bytes(Vec<u8>),
    Resize(u16, u16),
}

struct InputChunks(std::collections::VecDeque<Vec<u8>>);

impl Read for InputChunks {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let Some(chunk) = self.0.pop_front() else {
            return Ok(0);
        };
        bytes[..chunk.len()].copy_from_slice(&chunk);
        Ok(chunk.len())
    }
}

fn observe_input(chunks: Vec<Vec<u8>>, last_size: Option<(u16, u16)>) -> Vec<ObservedInput> {
    let (send, receive) = unbounded();
    let worker = std::thread::spawn(move || {
        run_viewer_input(
            InputChunks(chunks.into()),
            ViewerSender(send),
            InputConfig {
                detach: None,
                pass_suspend: true,
                last_size,
                vt_resize: true,
            },
            || InputState::Ready,
            || None,
            || {},
            Instant::now,
        )
    });
    let mut observed = Vec::new();
    loop {
        match receive.recv().unwrap() {
            Command::Input(bytes) => match observed.last_mut() {
                Some(ObservedInput::Bytes(existing)) => existing.extend(bytes),
                _ => observed.push(ObservedInput::Bytes(bytes)),
            },
            Command::Resize(rows, columns) => {
                observed.push(ObservedInput::Resize(rows, columns))
            }
            Command::Release(done) => {
                done.send(true).unwrap();
                break;
            }
            Command::Keepalive | Command::Abort => panic!("unexpected viewer command"),
        }
    }
    worker.join().unwrap();
    observed
}

#[test]
fn viewer_translates_fragmented_vt_resize_without_leaking_control_bytes() {
    let sequence = b"\x1b[8;41;101t";
    for split in 1..sequence.len() {
        let mut first = b"a".to_vec();
        first.extend_from_slice(&sequence[..split]);
        let mut second = sequence[split..].to_vec();
        second.push(b'b');
        assert_eq!(
            observe_input(vec![first, second], Some((37, 93))),
            vec![
                ObservedInput::Bytes(b"a".to_vec()),
                ObservedInput::Resize(41, 101),
                ObservedInput::Bytes(b"b".to_vec()),
            ],
            "split at {split}"
        );
    }
}

#[test]
fn viewer_suppresses_same_size_vt_resize_and_preserves_invalid_sequences() {
    let invalid = b"x\x1b[8;0;101ty\x1b[8;30000;100tz\x1b[8;99999;1t";
    assert_eq!(
        observe_input(vec![invalid.to_vec()], Some((37, 93))),
        vec![ObservedInput::Bytes(invalid.to_vec())]
    );
    let partial = b"\x1b[8;41";
    assert_eq!(
        observe_input(vec![partial.to_vec()], Some((37, 93))),
        vec![ObservedInput::Bytes(partial.to_vec())]
    );
    assert_eq!(
        observe_input(
            vec![b"a\x1b[8;37;93tb\x1bX\x1b[8;42;102tc".to_vec()],
            Some((37, 93))
        ),
        vec![
            ObservedInput::Bytes(b"ab\x1bX".to_vec()),
            ObservedInput::Resize(42, 102),
            ObservedInput::Bytes(b"c".to_vec()),
        ]
    );
}

#[test]
fn viewer_flushes_a_lone_escape_after_the_readiness_grace() {
    let (send, receive) = unbounded();
    let worker = std::thread::spawn(move || {
        let mut polls = 0;
        run_viewer_input(
            std::io::Cursor::new(b"\x1b"),
            ViewerSender(send),
            InputConfig {
                detach: None,
                pass_suspend: true,
                last_size: Some((37, 93)),
                vt_resize: true,
            },
            move || {
                polls += 1;
                match polls {
                    1 => InputState::Ready,
                    2 => InputState::Pending,
                    _ => InputState::Closed,
                }
            },
            || None,
            || {},
            Instant::now,
        )
    });
    let Command::Input(bytes) = receive.recv().unwrap() else {
        panic!("timed-out escape was not forwarded");
    };
    assert_eq!(bytes, b"\x1b");
    let Command::Release(done) = receive.recv().unwrap() else {
        panic!("viewer did not release cleanly");
    };
    done.send(true).unwrap();
    worker.join().unwrap();
}
