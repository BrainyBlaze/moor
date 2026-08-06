#[cfg(test)]
mod recovery_tests {
    use super::*;

    fn candidate(index: u64) -> Candidate {
        (
            Commit {
                slot: 0,
                body: 0,
                kind: Kind::Log,
                generation: 7,
                epoch: 1,
                index,
                length: 0,
                start: 0,
                end: 0,
                hash: [0; 32],
            },
            Sha256::new(),
            Vec::new(),
        )
    }

    #[test]
    fn one_invalid_candidate_cannot_hide_an_independently_valid_alternate() {
        let selected = select_candidates(None, Some(candidate(9))).unwrap();
        assert_eq!(selected.0.index, 9);
    }
}
