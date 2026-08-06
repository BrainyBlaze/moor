#[test]
fn accounting_preserves_the_full_u32_overhead_domain() {
    let limit = usage(u32::MAX as usize, u32::MAX as usize).unwrap();
    let high_bit = usage(1usize << 31, 1).unwrap();
    let reserved = reserve(high_bit, usage(0, 1).unwrap(), limit).unwrap();

    assert_eq!(reserved >> 32, 1 << 31);
    assert_eq!(reserved as u32, 2);
    assert_eq!(reserve(limit, usage(1, 0).unwrap(), limit), None);
}
