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

    #[test]
    fn concurrent_private_directory_creation_converges_on_one_valid_directory() {
        use std::sync::{Arc, Barrier};

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "moor-private-directory-race-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&base).unwrap();
        for round in 0..16 {
            let path = Arc::new(base.join(round.to_string()));
            let barrier = Arc::new(Barrier::new(16));
            let workers = (0..16)
                .map(|_| {
                    let (path, barrier) = (path.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        private_directory(&path, true)
                    })
                })
                .collect::<Vec<_>>();
            for worker in workers {
                assert!(worker.join().unwrap().unwrap());
            }
            fs::remove_dir(path.as_ref()).unwrap();
        }
        fs::remove_dir(base).unwrap();
    }
}
