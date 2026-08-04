use moor::terminal::{Observation, Scanner};

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

#[test]
fn title_and_hyperlink_are_incremental_bounded_and_classified_once() {
    let observations =
        scan_split(b"\x1b]2;\xe2\xa0\x99 working\x07\x1b]8;;https://example.test\x1b\\");
    assert_eq!(
        observations,
        vec![
            Observation::State {
                state: "busy",
                title: "\u{2819} working".into(),
                truncated: false
            },
            Observation::Link {
                uri: "https://example.test".into(),
                truncated: false
            },
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
    let [
        Observation::State {
            title, truncated, ..
        },
    ] = observations.as_slice()
    else {
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
        [Observation::Ready, Observation::Query { class: 1, .. }]
    ));
    let mut scanner = Scanner::new(24);
    assert_eq!(scanner.feed(0, b"\x1b["), vec![]);
    assert!(matches!(
        &scanner.feed(50, b"c")[..],
        [Observation::Degraded {
            scanner: "query",
            reason: "deadline"
        }]
    ));
    assert!(matches!(
        &scanner.feed(51, b"\x9b>0q")[..],
        [Observation::Ready, Observation::Query { class: 3, .. }]
    ));
    assert!(matches!(
        &scanner.feed(52, b"\x1b[c")[..],
        [Observation::Query { .. }]
    ));
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
        [Observation::State {
            truncated: true,
            ..
        }]
    ));
}

#[test]
fn osc_rejects_the_byte_past_its_bound_and_a_missing_selector() {
    let mut scanner = Scanner::new(24);
    let mut full = b"\x1b]2;".to_vec();
    full.extend(vec![b'x'; 65_532]);
    assert!(scanner.feed(0, &full).is_empty());
    let observations = scanner.feed(1, b"\x07");
    assert_eq!(
        observations,
        vec![Observation::Degraded {
            scanner: "osc",
            reason: "limit"
        }]
    );

    let observations = scanner.feed(2, b"plain\x1b];oops\x07");
    assert_eq!(
        observations,
        vec![Observation::Degraded {
            scanner: "osc",
            reason: "malformed"
        }]
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
            .any(|event| matches!(event, Observation::State { title, .. } if title == "ok"))
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
            .any(|event| matches!(event, Observation::State { title, .. } if title == "ok"))
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
            .any(|event| matches!(event, Observation::State { title, .. } if title == "idle"))
    );
}

#[test]
fn hyperlink_close_is_observed_and_oversized_params_are_rejected() {
    let mut scanner = Scanner::new(24);
    assert_eq!(
        scanner.feed(0, b"\x1b]8;;\x07"),
        vec![Observation::Link {
            uri: String::new(),
            truncated: false
        }]
    );
    let mut invalid = b"\x1b]8;".to_vec();
    invalid.extend(vec![b'p'; 1025]);
    invalid.extend_from_slice(b";https://ignored\x07");
    let observations = scanner.feed(1, &invalid);
    assert!(format!("{observations:?}").contains("Degraded"));
    assert!(
        !observations
            .iter()
            .any(|event| matches!(event, Observation::Link { .. }))
    );
}

#[test]
fn unrepresentable_modes_and_abandoned_scroll_clear_exactness() {
    let mut scanner = Scanner::new(24);
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
