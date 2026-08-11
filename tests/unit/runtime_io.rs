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
