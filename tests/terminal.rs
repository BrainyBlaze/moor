use moor::terminal::{Observation, Scan, Scanner};

trait Observe {
    fn feed(&mut self, now: u64, bytes: &[u8]) -> Vec<Observation>;
}

impl Observe for Scanner {
    fn feed(&mut self, now: u64, bytes: &[u8]) -> Vec<Observation> {
        self.scan(now, bytes)
            .into_iter()
            .filter_map(|item| match item {
                Scan::Observation(observation) => Some(observation),
                Scan::Release(_) => None,
            })
            .collect()
    }
}

fn scan_split(bytes: &[u8]) -> Vec<Observation> {
    let mut all = Vec::new();
    for split in 0..=bytes.len() {
        let mut scanner = Scanner::new(24);
        let mut out = scanner.feed(0, &bytes[..split]);
        out.extend(scanner.feed(1, &bytes[split..]));
        if split == 0 {
            all = out.clone();
        } else {
            assert_eq!(out, all, "split {split}");
        }
    }
    all
}

fn raw(effects: &[Scan]) -> Vec<u8> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Scan::Release(bytes) => Some(bytes.as_slice()),
            Scan::Observation(_) => None,
        })
        .flatten()
        .copied()
        .collect()
}

#[test]
fn title_and_hyperlink_are_incremental_bounded_and_classified_once() {
    let observations =
        scan_split(b"\x1b]2;\xe2\xa0\x99 working\x07\x1b]8;;https://example.test\x1b\\");
    assert_eq!(
        observations,
        vec![
            Observation::State("busy", "\u{2819} working".into(), false),
            Observation::Link("https://example.test".into(), false),
        ]
    );
    let mut scanner = Scanner::new(24);
    assert_eq!(
        scanner.feed(0, b"\x1b]2;first\x07\x1b]2;second\x07").len(),
        1
    );
    assert!(scanner.feed(1, b"\x1b]2;still idle\x07").is_empty());
}

#[test]
fn invalid_utf8_nul_and_long_values_are_sanitized_before_bounding() {
    let mut scanner = Scanner::new(24);
    let mut title = b"\x1b]2;".to_vec();
    title.extend([0xff, 0, b'a']);
    title.extend(vec![b'x'; 300]);
    title.push(7);
    let observations = scanner.feed(0, &title);
    let [Observation::State(_, title, truncated)] = observations.as_slice() else {
        unreachable!()
    };
    assert!(*truncated);
    assert!(title.len() <= 255);
    assert!(title.starts_with("\u{fffd}\u{fffd}a"));
}

#[test]
fn query_recognition_is_incremental_ready_is_once_and_deadline_is_bounded() {
    let observations = scan_split(b"\x1b[c");
    assert!(matches!(
        &observations[..],
        [Observation::Ready, Observation::Query(1, _)]
    ));
    let mut scanner = Scanner::new(24);
    assert_eq!(scanner.feed(0, b"\x1b["), vec![]);
    assert!(matches!(
        &scanner.feed(50, b"c")[..],
        [Observation::Degraded("query", "deadline")]
    ));
    assert!(matches!(
        &scanner.feed(51, b"\x9b>0q")[..],
        [Observation::Ready, Observation::Query(3, _)]
    ));
    assert!(matches!(
        &scanner.feed(52, b"\x1b[c")[..],
        [Observation::Query(..)]
    ));
}

#[test]
fn query_bytes_are_released_exactly_once_after_the_query_observation_at_every_split() {
    let query = b"\x1b[?2004$p";
    for split in 0..=query.len() {
        let mut scanner = Scanner::new(24);
        let mut effects = scanner.scan(0, &query[..split]);
        effects.extend(scanner.scan(1, &query[split..]));
        let query_at = effects
            .iter()
            .position(|effect| matches!(effect, Scan::Observation(Observation::Query(..))))
            .unwrap();
        assert!(
            effects[..query_at]
                .iter()
                .all(|effect| !matches!(effect, Scan::Release(_))),
            "split {split}: {effects:?}"
        );
        let released = effects
            .iter()
            .filter_map(|effect| match effect {
                Scan::Release(bytes) => Some(bytes.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(released, query, "split {split}: {effects:?}");
    }
}

#[test]
fn quiet_query_candidates_expire_but_osc_bytes_are_never_buffered() {
    let mut scanner = Scanner::new(24);
    assert!(scanner.scan(0, b"\x1b[").is_empty());
    assert!(scanner.expire(49).is_empty());
    assert_eq!(
        scanner.expire(50),
        vec![
            Scan::Observation(Observation::Degraded("query", "deadline")),
            Scan::Release(b"\x1b[".to_vec())
        ]
    );
    assert!(scanner.expire(51).is_empty());

    let osc = b"\x1b]2;an unfinished title";
    assert_eq!(
        Scanner::new(24).scan(0, osc),
        vec![Scan::Release(osc.to_vec())]
    );

    let mut scanner = Scanner::new(24);
    let mut osc_escape = osc.to_vec();
    osc_escape.push(0x1b);
    assert_eq!(
        scanner.scan(0, &osc_escape),
        vec![Scan::Release(osc.to_vec())]
    );
    assert!(scanner.expire(49).is_empty());
    assert_eq!(
        scanner.expire(50),
        vec![
            Scan::Observation(Observation::Degraded("query", "deadline")),
            Scan::Release(vec![0x1b])
        ]
    );
}

#[test]
fn malformed_osc_resynchronizes_to_a_query_without_leaking_its_escape_prefix() {
    let bytes = b"\x1b]2;bad\x1b[c";
    let effects = Scanner::new(24).scan(0, bytes);
    let query = effects
        .iter()
        .position(|effect| matches!(effect, Scan::Observation(Observation::Query(1, _))))
        .unwrap();
    assert!(matches!(&effects[query + 1], Scan::Release(raw) if raw == b"\x1b[c"));
    let released = effects
        .iter()
        .filter_map(|effect| match effect {
            Scan::Release(raw) => Some(raw.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(released, bytes);
}

#[test]
fn tracked_modes_emit_complete_preamble_and_degrade_until_ris() {
    let mut scanner = Scanner::new(24);
    scanner.feed(0, b"\x1b[?7;2004h\x1b(0\x1b[2;20r");
    let preamble = scanner.modes().preamble().unwrap();
    assert!(preamble.starts_with(b"\x1b[?1049l\x1b(0\x1b)B\x1b[?7h\x1b[2;20r"));
    scanner.feed(1, b"\x1b(X");
    assert!(scanner.modes().preamble().is_none());
    scanner.feed(2, b"\x1bc");
    assert!(scanner.modes().preamble().is_some());
}

#[test]
fn private_mode_query_synthesis_uses_exact_tracked_state_and_csi_form() {
    let mut scanner = Scanner::new(24);
    assert_eq!(
        scanner.modes().query(2004, false).unwrap(),
        b"\x1b[?2004;2$y"
    );
    scanner.feed(0, b"\x1b[?2004h");
    assert_eq!(scanner.modes().query(2004, true).unwrap(), b"\x9b?2004;1$y");
    assert_eq!(scanner.modes().query(999, false).unwrap(), b"\x1b[?999;0$y");
    scanner.feed(1, b"\x1b(X");
    assert_eq!(scanner.modes().query(2004, false), None);
}

#[test]
fn osc_uses_the_full_bound_and_has_no_query_deadline() {
    let mut scanner = Scanner::new(24);
    let mut title = b"\x1b]2;".to_vec();
    title.extend(vec![b'x'; 5000]);
    assert!(scanner.feed(0, &title[..2000]).is_empty());
    let observations = scanner.feed(50, &title[2000..]);
    assert!(observations.is_empty());
    let observations = scanner.feed(100, b"\x07");
    assert!(matches!(
        &observations[..],
        [Observation::State(_, _, true)]
    ));
}

#[test]
fn osc_rejects_the_byte_past_its_bound_and_a_missing_selector() {
    let mut scanner = Scanner::new(24);
    let mut full = b"\x1b]2;".to_vec();
    full.extend(vec![b'x'; 65_532]);
    assert!(scanner.feed(0, &full).is_empty());
    let observations = scanner.feed(1, b"\x07");
    assert_eq!(observations, vec![Observation::Degraded("osc", "limit")]);

    let observations = scanner.feed(2, b"plain\x1b];oops\x07");
    assert_eq!(
        observations,
        vec![Observation::Degraded("osc", "malformed")]
    );
}

#[test]
fn cancelled_osc_reports_degradation_and_resynchronizes() {
    let mut scanner = Scanner::new(24);
    let observations = scanner.feed(0, b"\x1b]2;bad\x18\x1b]2;ok\x07");
    assert!(format!("{observations:?}").contains("Degraded"));
    assert!(
        observations
            .iter()
            .any(|event| matches!(event, Observation::State(_, title, _) if title == "ok"))
    );
}

#[test]
fn malformed_or_limit_byte_that_is_an_introducer_is_reprocessed() {
    let mut scanner = Scanner::new(24);
    let observations = scanner.feed(0, b"\x1b]2;bad\x1b]2;ok\x07");
    assert!(format!("{observations:?}").contains("Degraded"));
    assert!(
        observations
            .iter()
            .any(|event| matches!(event, Observation::State(_, title, _) if title == "ok"))
    );

    let mut scanner = Scanner::new(24);
    let mut prefix = b"\x1b[".to_vec();
    prefix.extend(vec![b'1'; 30]);
    assert!(scanner.feed(0, &prefix).is_empty());
    let observations = scanner.feed(1, b"\x9d2;idle\x07");
    assert!(format!("{observations:?}").contains("Degraded"));
    assert!(
        observations
            .iter()
            .any(|event| matches!(event, Observation::State(_, title, _) if title == "idle"))
    );
}

#[test]
fn hyperlink_close_is_observed_and_oversized_params_are_rejected() {
    let mut scanner = Scanner::new(24);
    assert_eq!(
        scanner.feed(0, b"\x1b]8;;\x07"),
        vec![Observation::Link(String::new(), false)]
    );
    let mut invalid = b"\x1b]8;".to_vec();
    invalid.extend(vec![b'p'; 1025]);
    invalid.extend_from_slice(b";https://ignored\x07");
    let observations = scanner.feed(1, &invalid);
    assert!(format!("{observations:?}").contains("Degraded"));
    assert!(
        !observations
            .iter()
            .any(|event| matches!(event, Observation::Link(..)))
    );
}

#[test]
fn unrepresentable_modes_and_abandoned_scroll_clear_exactness() {
    let mut scanner = Scanner::new(24);
    scanner.feed(0, b"\x1b[?+7h");
    assert!(!scanner.modes().exact());
    scanner.feed(0, b"\x1bc");
    scanner.feed(0, b"\x1b[?47h");
    assert!(!scanner.modes().exact());
    scanner.feed(1, b"\x1bc\x1b[0;0r");
    assert!(scanner.modes().exact());
    assert!(
        scanner
            .modes()
            .preamble()
            .unwrap()
            .windows(3)
            .any(|bytes| bytes == b"\x1b[r")
    );
    scanner.feed(2, b"\x9b12345678901234567890123456789012");
    assert!(!scanner.modes().exact());

    scanner.feed(3, b"\x1bc\x1b[2;2r");
    assert!(!scanner.modes().exact());
}

#[test]
fn c1_controls_and_every_abandonment_path_preserve_raw_bytes() {
    for bytes in [
        b"plain\x9d2;c1\x9c".as_slice(),
        b"\x1b]2;cancel\x18tail",
        b"\x1b]2;sub\x1atail",
        b"\x1b]2;broken\x9b>0q",
        b"\x1b]2;broken\x1b[c",
        b"\x1b];missing\x07",
    ] {
        for split in 0..=bytes.len() {
            let mut scanner = Scanner::new(24);
            let mut effects = scanner.scan(0, &bytes[..split]);
            effects.extend(scanner.scan(1, &bytes[split..]));
            assert_eq!(raw(&effects), bytes, "split {split}: {effects:?}");
        }
    }
}

#[test]
fn degradation_episodes_end_only_on_ground_or_valid_same_scanner_input() {
    let mut scanner = Scanner::new(24);
    assert_eq!(
        scanner.feed(0, b"\x1b]bad\x07\x1b]bad\x07"),
        vec![Observation::Degraded("osc", "malformed")]
    );
    assert!(!scanner.exact());
    assert!(scanner.feed(1, b"x").is_empty());
    assert!(scanner.exact());
    assert_eq!(
        scanner.feed(2, b"\x1b]bad\x07"),
        vec![Observation::Degraded("osc", "malformed")]
    );
    assert!(
        scanner
            .feed(3, b"\x1b]2;valid\x07")
            .iter()
            .any(|event| matches!(event, Observation::State(_, title, false) if title == "valid"))
    );
    assert!(scanner.exact());
}

#[test]
fn private_updates_are_atomic_and_exact_bound_is_accepted() {
    let mut scanner = Scanner::new(24);
    scanner.feed(0, b"\x1b[?2004;+;7l");
    assert!(!scanner.modes().exact());
    scanner.feed(1, b"\x1bc");
    assert_eq!(
        scanner.modes().query(2004, false).unwrap(),
        b"\x1b[?2004;2$y"
    );
    assert_eq!(scanner.modes().query(7, false).unwrap(), b"\x1b[?7;1$y");

    let mut title = b"\x1b]2;".to_vec();
    title.extend(vec![b'x'; 65_531]);
    title.push(7);
    assert_eq!(title.len(), 65_536);
    assert!(matches!(
        Scanner::new(24).feed(0, &title).as_slice(),
        [Observation::State(_, _, true)]
    ));
}

#[test]
fn mouse_groups_clear_every_constituent_before_setting_tracked_bits() {
    // Schema §6 groups 9 and 10 are frozen as "reset all constituents in
    // order, then set each tracked enabled bit in the same order". Emitting one
    // h-or-l per mode instead leaves an arbitrary combination already present
    // in the viewer partly intact, and puts the set before the resets.
    let render = |bytes: &[u8]| {
        let mut scanner = Scanner::new(24);
        scanner.scan(0, bytes);
        String::from_utf8(scanner.modes().preamble().expect("exact")).unwrap()
    };
    let esc = char::from(0x1b);
    let seq =
        |modes: &[&str]| -> String { modes.iter().map(|mode| format!("{esc}[{mode}")).collect() };
    let reporting = render(seq(&["?1000h"]).as_bytes());
    assert!(
        reporting.contains(&seq(&["?1000l", "?1002l", "?1003l", "?1000h"])),
        "{reporting:?}"
    );
    let encoding = render(seq(&["?1006h"]).as_bytes());
    assert!(
        encoding.contains(&seq(&["?1005l", "?1006l", "?1006h"])),
        "{encoding:?}"
    );
    // With both groups off, only the five resets appear and nothing is set.
    let quiet = render(b"");
    assert!(
        quiet.contains(&seq(&["?1000l", "?1002l", "?1003l"]))
            && quiet.contains(&seq(&["?1005l", "?1006l"])),
        "{quiet:?}"
    );
    for set in ["?1000h", "?1002h", "?1003h", "?1005h", "?1006h"] {
        assert!(!quiet.contains(set), "{quiet:?} unexpectedly set {set}");
    }
}

#[test]
fn scroll_region_tracking_follows_the_session_row_count() {
    // The tracked-mode scanner resolves a scroll region against the current row
    // count (schema §6), so a fixed 24 corrupts the preamble in both
    // directions on any other geometry: a valid full-range region reads as
    // out-of-range and falsely degrades, while a real 24-row region on a taller
    // session reads as the default and yields an exact-claimed preamble that
    // omits the region the child actually set.
    let esc = char::from(0x1b);
    let render = |rows: u16, bytes: &str| {
        let mut scanner = Scanner::new(rows);
        scanner.scan(0, bytes.as_bytes());
        let exact = scanner.modes().exact();
        let preamble = scanner
            .modes()
            .preamble()
            .map(|bytes| String::from_utf8(bytes).unwrap());
        (exact, preamble)
    };

    // On a 50-row session the full range is the default region.
    let (exact, preamble) = render(50, &format!("{esc}[1;50r"));
    assert!(exact, "a full-range region must not degrade tracking");
    assert!(preamble.unwrap().contains(&format!("{esc}[r")));

    // A 24-row region on that same session is a real region and is restated.
    let (exact, preamble) = render(50, &format!("{esc}[1;24r"));
    assert!(exact);
    assert!(preamble.unwrap().contains(&format!("{esc}[1;24r")));

    // Past the row count is unrepresentable, so exactness is cleared and the
    // preamble is withheld rather than guessed.
    let (exact, preamble) = render(24, &format!("{esc}[1;50r"));
    assert!(!exact, "an out-of-range region must clear exactness");
    assert!(preamble.is_none());
}
