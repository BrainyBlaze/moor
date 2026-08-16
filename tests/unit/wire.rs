mod tests {
    use super::*;

    #[test]
    fn encode_preflights_sequence_space_and_never_leaves_a_more_prefix() {
        let mut codec = Codec::new(Profile::Semantic);
        codec.next_out = u32::MAX - 1;
        let mut out = b"unchanged".to_vec();
        assert_eq!(
            codec.encode(1, 3, &vec![0; (1 << 16) + 1], &mut out),
            Err(WireError::ResourceExhausted)
        );
        assert_eq!(out, b"unchanged");
    }
}
