#[test]
fn accounting_preserves_the_full_u32_overhead_domain() {
    let limit = (u32::MAX as usize, u32::MAX as usize);
    let high_bit = (1usize << 31, 1);
    let reserved = reserve(high_bit, (0, 1), limit).unwrap();

    assert_eq!(reserved, (1 << 31, 2));
    assert_eq!(reserve(limit, (1, 0), limit), None);
}
