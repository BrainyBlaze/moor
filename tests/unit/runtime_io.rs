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
