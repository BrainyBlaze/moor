const USER: &str = "S-1-5-21-1-2-3-42";

fn parsed(value: impl AsRef<str>) -> LocalBox<SecurityDescriptor> {
    value.as_ref().parse().unwrap()
}

fn first_ace(descriptor: &SecurityDescriptor) -> *mut ACE_HEADER {
    let acl = descriptor.dacl().unwrap() as *const windows_permissions::Acl;
    let mut ace = ptr::null_mut();
    assert_ne!(unsafe { GetAce(acl.cast(), 0, &mut ace) }, 0);
    ace.cast()
}

#[test]
fn structural_validation_accepts_only_the_exact_protected_owner_and_aces() {
    let (expected, _) = descriptor(USER, "FA").unwrap();
    let reordered = parsed(format!("O:{USER}D:PAI(A;;FA;;;{USER})(A;;FA;;;SY)"));
    assert!(descriptor_matches(&expected, &expected).unwrap());
    assert!(descriptor_matches(&reordered, &expected).unwrap());

    for invalid in [
        format!("O:S-1-5-21-1-2-3-43D:P(A;;FA;;;SY)(A;;FA;;;{USER})"),
        format!("O:{USER}D:AI(A;;FA;;;SY)(A;;FA;;;{USER})"),
        format!("O:{USER}D:P(A;;FA;;;{USER})"),
        format!("O:{USER}D:P(A;;FA;;;{USER})(A;;FA;;;{USER})"),
        format!("O:{USER}D:P(A;;FA;;;SY)(A;;FA;;;{USER})(A;;FA;;;WD)"),
        format!("O:{USER}D:P(A;;FA;;;SY)(A;;FR;;;{USER})"),
        format!("O:{USER}D:P(D;;FA;;;SY)(A;;FA;;;{USER})"),
        format!("O:{USER}D:P(A;CI;FA;;;SY)(A;;FA;;;{USER})"),
    ] {
        assert!(
            !descriptor_matches(&parsed(&invalid), &expected).unwrap(),
            "accepted {invalid}"
        );
    }

    let invalid_flags = parsed(format!("O:{USER}D:P(A;;FA;;;SY)(A;;FA;;;{USER})"));
    unsafe { (*first_ace(&invalid_flags)).AceFlags = 0x20 };
    assert!(!descriptor_matches(&invalid_flags, &expected).unwrap());

    let invalid_type = parsed(format!("O:{USER}D:P(A;;FA;;;SY)(A;;FA;;;{USER})"));
    unsafe { (*first_ace(&invalid_type)).AceType = u8::MAX };
    assert!(!descriptor_matches(&invalid_type, &expected).unwrap());
}

#[test]
fn instrumentation_dacl_allows_outside_read_execute_but_never_outside_write() {
    let (owner, _) = descriptor(USER, "FA").unwrap();
    for valid in [
        format!("O:{USER}D:P(A;;FA;;;SY)(A;;FA;;;{USER})"),
        format!("O:{USER}D:P(A;;FA;;;SY)(A;;FA;;;{USER})(A;;FRFX;;;WD)"),
    ] {
        assert!(
            instrument_descriptor_matches(&parsed(valid), &owner).unwrap(),
            "rejected valid instrumentation DACL"
        );
    }
    for invalid in [
        format!("O:S-1-5-21-1-2-3-43D:P(A;;FA;;;SY)(A;;FA;;;{USER})"),
        format!("O:{USER}D:(A;;FA;;;SY)(A;;FA;;;{USER})"),
        format!("O:{USER}D:P(A;;FA;;;SY)(A;;FA;;;{USER})(A;;FW;;;WD)"),
        format!("O:{USER}D:P(A;;FA;;;SY)(A;;FA;;;{USER})(A;;GA;;;WD)"),
        format!("O:{USER}D:P(A;;FA;;;SY)(A;;FA;;;{USER})(A;;WD;;;WD)"),
    ] {
        assert!(
            !instrument_descriptor_matches(&parsed(&invalid), &owner).unwrap(),
            "accepted {invalid}"
        );
    }
}

#[test]
fn file_descriptor_query_validates_a_created_store_directory() {
    let path = std::env::temp_dir().join(format!(
        "moor-windows-descriptor-{}-{}",
        std::process::id(),
        now()
    ));
    create_store_path(&path).unwrap();
    validate(&path, sid().unwrap(), "FA", true).unwrap();
    fs::remove_dir(path).unwrap();
}

#[test]
fn viewer_modes_are_raw_input_and_vt_output() {
    let [input, output] = viewer_modes(
        ENABLE_PROCESSED_INPUT | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_QUICK_EDIT_MODE,
        0,
    );
    assert_eq!(
        input
            & (ENABLE_PROCESSED_INPUT
                | ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_QUICK_EDIT_MODE),
        0
    );
    assert_ne!(input & ENABLE_VIRTUAL_TERMINAL_INPUT, 0);
    assert_eq!(
        input & (ENABLE_WINDOW_INPUT | ENABLE_MOUSE_INPUT | ENABLE_EXTENDED_FLAGS),
        ENABLE_WINDOW_INPUT | ENABLE_MOUSE_INPUT | ENABLE_EXTENDED_FLAGS
    );
    assert_ne!(output & ENABLE_PROCESSED_OUTPUT, 0);
    assert_ne!(output & ENABLE_VIRTUAL_TERMINAL_PROCESSING, 0);
    assert_ne!(output & DISABLE_NEWLINE_AUTO_RETURN, 0);
}

#[test]
fn viewer_input_mode_controls_are_flushed_through_the_attach_writer() {
    #[derive(Default)]
    struct Output {
        bytes: Vec<u8>,
        flushes: usize,
    }
    impl std::io::Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    let mut output = Output::default();
    ViewerInputMode::write(&mut output, WIN32_INPUT_ENABLE).unwrap();
    ViewerInputMode::write(&mut output, WIN32_INPUT_DISABLE).unwrap();
    assert_eq!(
        output.bytes,
        [WIN32_INPUT_ENABLE, WIN32_INPUT_DISABLE].concat()
    );
    assert_eq!(output.flushes, 2);
}

#[test]
fn pseudoconsole_retirement_never_joins_the_close_operation() {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let started = Instant::now();
    retire_pseudo_with(1, move |_| {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "pseudoconsole retirement joined the close operation"
    );
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    release_tx.send(()).unwrap();
}

#[test]
fn prepublication_exit_is_finalizable_only_after_requested_child_release() {
    let mut host = Native {
        early_exit: Some(23),
        ..Native::default()
    };
    assert!(finalizable_unpublished_exit(&mut host).unwrap().is_none());
    host.child_released = true;
    assert!(matches!(
        finalizable_unpublished_exit(&mut host).unwrap(),
        Some(NativeExit::Code(23))
    ));
}

#[test]
fn console_records_preserve_text_repeats_and_modern_or_legacy_nul() {
    let mut key = KEY_EVENT_RECORD {
        bKeyDown: 1,
        wRepeatCount: 1,
        ..KEY_EVENT_RECORD::default()
    };
    let zero = unsafe { VkKeyScanW(0) } as u16;
    key.uChar.UnicodeChar = u16::from(b'A');
    assert_eq!(console_wide(key), Some((u16::from(b'A'), 1)));
    key.wRepeatCount = 3;
    assert_eq!(console_wide(key), Some((u16::from(b'A'), 3)));
    key.wRepeatCount = 0;
    assert_eq!(console_wide(key), None);
    key.bKeyDown = 0;
    assert_eq!(console_wide(key), None);

    key.bKeyDown = 1;
    key.wRepeatCount = 1;
    key.uChar.UnicodeChar = 0;
    key.wVirtualKeyCode = 0x10;
    key.wVirtualScanCode = 0x2a;
    key.dwControlKeyState = SHIFT_PRESSED;
    assert_eq!(console_wide(key), None);
    key.wVirtualKeyCode = 0x26;
    key.wVirtualScanCode = 0x48;
    key.dwControlKeyState = 0;
    assert_eq!(console_wide(key), None);

    key.wVirtualKeyCode = 0;
    key.wVirtualScanCode = 0;
    assert_eq!(console_wide(key), Some((0, 1)));
    key.wVirtualScanCode = 1;
    assert_eq!(console_wide(key), None);

    key.wVirtualKeyCode = zero & 0xff;
    key.wVirtualScanCode = 0x2a;
    key.dwControlKeyState = (u32::from(zero & 0x100 != 0) * SHIFT_PRESSED)
        | (u32::from(zero & 0x200 != 0) * LEFT_CTRL_PRESSED)
        | (u32::from(zero & 0x400 != 0) * LEFT_ALT_PRESSED)
        | CAPSLOCK_ON
        | NUMLOCK_ON
        | SCROLLLOCK_ON;
    assert_eq!(console_wide(key), Some((0, 1)));
    key.wRepeatCount = 3;
    key.dwControlKeyState = (key.dwControlKeyState & !(LEFT_CTRL_PRESSED | LEFT_ALT_PRESSED))
        | (u32::from(zero & 0x200 != 0) * RIGHT_CTRL_PRESSED)
        | (u32::from(zero & 0x400 != 0) * RIGHT_ALT_PRESSED);
    assert_eq!(console_wide(key), Some((0, 3)));

    key.wVirtualKeyCode = 0xff;
    key.dwControlKeyState = SHIFT_PRESSED | RIGHT_CTRL_PRESSED | RIGHT_ALT_PRESSED | CAPSLOCK_ON;
    assert_eq!(console_wide_with_nul(key, -1), Some((0, 3)));
}

fn key_record(key: KEY_EVENT_RECORD) -> INPUT_RECORD {
    INPUT_RECORD {
        EventType: KEY_EVENT as u16,
        Event: INPUT_RECORD_0 { KeyEvent: key },
    }
}

fn text_record(unit: u16, repeat: u16) -> INPUT_RECORD {
    let mut key = KEY_EVENT_RECORD {
        bKeyDown: 1,
        wRepeatCount: repeat,
        ..KEY_EVENT_RECORD::default()
    };
    key.uChar.UnicodeChar = unit;
    key_record(key)
}

fn nul_record(repeat: u16) -> INPUT_RECORD {
    let mapping = unsafe { VkKeyScanW(0) } as u16;
    let mut key = KEY_EVENT_RECORD {
        bKeyDown: 1,
        wRepeatCount: repeat,
        wVirtualKeyCode: mapping & 0xff,
        dwControlKeyState: (u32::from(mapping & 0x100 != 0) * SHIFT_PRESSED)
            | (u32::from(mapping & 0x200 != 0) * LEFT_CTRL_PRESSED)
            | (u32::from(mapping & 0x400 != 0) * LEFT_ALT_PRESSED),
        ..KEY_EVENT_RECORD::default()
    };
    key.uChar.UnicodeChar = 0;
    key_record(key)
}

fn legacy_nul_record(repeat: u16) -> INPUT_RECORD {
    let mut key = KEY_EVENT_RECORD {
        bKeyDown: 1,
        wRepeatCount: repeat,
        ..KEY_EVENT_RECORD::default()
    };
    key.uChar.UnicodeChar = 0;
    key_record(key)
}

fn resize_record(rows: i16, columns: i16) -> INPUT_RECORD {
    INPUT_RECORD {
        EventType: WINDOW_BUFFER_SIZE_EVENT as u16,
        Event: INPUT_RECORD_0 {
            WindowBufferSizeEvent: WINDOW_BUFFER_SIZE_RECORD {
                dwSize: COORD {
                    X: columns,
                    Y: rows,
                },
            },
        },
    }
}

#[test]
fn console_record_translation_encodes_the_exact_cp65001_input_vector() {
    let mut input = ConsoleInput::new(ptr::null_mut());
    let mut wide = Vec::new();
    for record in [
        text_record(b'A'.into(), 1),
        text_record(0xd83d, 1),
        text_record(0xde42, 1),
        text_record(0x00e9, 1),
        legacy_nul_record(1),
        text_record(b'Z'.into(), 1),
    ] {
        assert_eq!(input.record(record, CP_UTF8, &mut wide), Ok(None));
    }
    let mut output = Vec::new();
    ConsoleInput::encode(CP_UTF8, &wide, &mut output).unwrap();
    assert_eq!(output, b"A\xf0\x9f\x99\x82\xc3\xa9\0Z");
}

#[test]
fn console_record_translation_preserves_scalars_repeats_nul_and_event_boundaries() {
    let mut input = ConsoleInput::new(ptr::null_mut());
    let mut wide = Vec::new();
    let mut output = Vec::new();

    assert_eq!(
        input.record(text_record(b'A'.into(), 3), CP_UTF8, &mut wide),
        Ok(None)
    );
    assert_eq!(input.record(nul_record(2), CP_UTF8, &mut wide), Ok(None));
    assert_eq!(
        input.record(text_record(0xd83d, 1), CP_UTF8, &mut wide),
        Ok(None)
    );
    assert_eq!(
        input.record(text_record(0xde42, 1), CP_UTF8, &mut wide),
        Ok(None)
    );
    ConsoleInput::encode(CP_UTF8, &wide, &mut output).unwrap();
    assert_eq!(output, b"AAA\0\0\xf0\x9f\x99\x82");

    wide.clear();
    output.clear();
    assert_eq!(
        input.record(text_record(0xd83d, 1), CP_UTF8, &mut wide),
        Ok(None)
    );
    let resize = resize_record(41, 101);
    assert_eq!(
        input.record(resize, CP_UTF8, &mut wide),
        Ok(Some((41, 101)))
    );
    assert_eq!(
        input.record(text_record(0xde42, 1), CP_UTF8, &mut wide),
        Ok(None)
    );
    assert!(wide.is_empty(), "surrogate pair crossed a resize event");

    assert_eq!(
        input.record(text_record(0xd83d, 2), CP_UTF8, &mut wide),
        Ok(None)
    );
    assert_eq!(
        input.record(text_record(0xde42, 2), CP_UTF8, &mut wide),
        Ok(None)
    );
    assert!(wide.is_empty(), "repeated surrogate halves were paired");

    wide.clear();
    output.clear();
    assert_eq!(
        input.record(text_record(0xd83d, 2), CP_UTF8, &mut wide),
        Ok(None)
    );
    assert_eq!(
        input.record(text_record(0xde42, 1), CP_UTF8, &mut wide),
        Ok(None)
    );
    assert!(wide.is_empty(), "mismatched surrogate repeats were paired");

    assert_eq!(
        input.record(text_record(0xd83d, 1), CP_UTF8, &mut wide),
        Ok(None)
    );
    let ignored = INPUT_RECORD {
        EventType: FOCUS_EVENT as u16,
        ..INPUT_RECORD::default()
    };
    assert_eq!(input.record(ignored, CP_UTF8, &mut wide), Ok(None));
    let mut release = KEY_EVENT_RECORD {
        bKeyDown: 0,
        wRepeatCount: 1,
        ..KEY_EVENT_RECORD::default()
    };
    release.uChar.UnicodeChar = 0xde42;
    assert_eq!(
        input.record(key_record(release), CP_UTF8, &mut wide),
        Ok(None)
    );
    assert_eq!(
        input.record(text_record(0xde42, 1), CP_UTF8, &mut wide),
        Ok(None)
    );
    ConsoleInput::encode(CP_UTF8, &wide, &mut output).unwrap();
    assert_eq!(output, b"\xf0\x9f\x99\x82");

    wide.clear();
    output.clear();
    assert_eq!(
        input.record(text_record(0xd83d, 1), CP_UTF8, &mut wide),
        Ok(None)
    );
    assert_eq!(
        input.record(text_record(0xde42, 1), 437, &mut wide),
        Ok(None)
    );
    assert!(wide.is_empty(), "surrogate pair crossed a code-page change");
    assert_eq!(
        input.record(text_record(b'B'.into(), 1), 0, &mut wide),
        Err(())
    );
    assert!(wide.is_empty());
}

#[test]
fn console_batch_preserves_prefetched_text_resize_text_order() {
    let mut input = ConsoleInput::new(ptr::null_mut());
    input.records[0] = text_record(b'A'.into(), 1);
    input.records[1] = resize_record(41, 101);
    input.records[2] = text_record(b'B'.into(), 1);
    input.count = 3;

    assert_eq!(
        input.state_with(|_, _| Ok(false)),
        InputState::Bytes(b"A".to_vec())
    );
    assert_eq!(
        input.state_with(|_, _| Ok(false)),
        InputState::Resize(41, 101)
    );
    assert_eq!(
        input.state_with(|_, timeout| {
            assert_eq!(timeout, 0, "buffered text incurred an input delay");
            Ok(false)
        }),
        InputState::Bytes(b"B".to_vec())
    );
    assert_eq!(
        input.state_with(|_, timeout| {
            assert_eq!(timeout, 50, "idle input did not use the bounded wait");
            Ok(false)
        }),
        InputState::Pending
    );
}

#[test]
fn console_batch_serializes_unicode_as_utf8_for_a_legacy_outer_codepage() {
    let mut input = ConsoleInput::new(ptr::null_mut());
    input.records[0] = text_record(0xd83d, 1);
    input.records[1] = text_record(0xde42, 1);
    input.records[2] = text_record(0x00e9, 1);
    input.count = 3;

    assert_eq!(
        input.state_with(|_, _| Ok(false)),
        InputState::Bytes(b"\xf0\x9f\x99\x82\xc3\xa9".to_vec())
    );
}

fn carrier_records(bytes: &[u8]) -> Vec<INPUT_RECORD> {
    let (prefix, rest): (Vec<_>, _) = if let Some(rest) = bytes.strip_prefix(b"\xc2\x9b") {
        (vec![text_record(0x009b, 1)], rest)
    } else {
        (Vec::new(), bytes)
    };
    prefix
        .into_iter()
        .chain(rest.iter().copied().map(|byte| text_record(byte.into(), 1)))
        .collect()
}

#[test]
fn win32_carrier_decoder_accepts_only_canonical_complete_key_records() {
    let decoded = win32_input_carrier(b"\x1b[220;43;28;1;8;3_").unwrap();
    assert_eq!(
        (
            decoded.virtual_key,
            decoded.scan_code,
            decoded.unicode,
            decoded.key_down,
            decoded.control_state,
            decoded.repeat,
            decoded.c1,
        ),
        (220, 43, 28, true, 8, 3, false)
    );
    let decoded = win32_input_carrier(b"\xc2\x9b220;43;28;0;8;3_").unwrap();
    assert_eq!(
        (
            decoded.unicode,
            decoded.key_down,
            decoded.repeat,
            decoded.c1
        ),
        (28, false, 3, true)
    );
    for malformed in [
        b"\x1b[220;43;28;1;8;3".as_slice(),
        b"\x1b[0220;43;28;1;8;3_",
        b"\x1b[220;43;65536;1;8;3_",
        b"\x1b[220;43;28;2;8;3_",
        b"\x1b[220;43;28;1;8;0_",
        b"\x1b[220;43;28;1;8;3;0_",
        b"\x1b[220;43;28;1;8;x_",
    ] {
        assert_eq!(win32_input_carrier(malformed), None, "{malformed:?}");
    }
}

#[test]
fn console_batch_exposes_carrier_detach_and_one_following_record_atomically() {
    const A: &[u8] = b"\x1b[65;30;65;1;0;1_";
    const DETACH: &[u8] = b"\x1b[220;43;28;1;8;1_";
    const Z: &[u8] = b"\x1b[90;44;90;1;0;1_";
    let records = [A, DETACH, Z]
        .into_iter()
        .flat_map(carrier_records)
        .collect::<Vec<_>>();
    let mut input = ConsoleInput::with_detach(ptr::null_mut(), Some(0x1c));
    input.records[..records.len()].copy_from_slice(&records);
    input.count = records.len();

    assert_eq!(
        input.state_with(|_, _| Ok(false)),
        InputState::Framed(vec![
            InputFrame::Key(A.to_vec(), A.to_vec(), Some(b'A'), 1),
            InputFrame::Key(DETACH.to_vec(), DETACH.to_vec(), Some(0x1c), 1),
            InputFrame::Key(Z.to_vec(), Z.to_vec(), Some(b'Z'), 1),
        ])
    );
    assert_eq!(
        input.state_with(|_, timeout| {
            assert_eq!(timeout, 50);
            Ok(false)
        }),
        InputState::Pending
    );
}

#[test]
fn carrier_syntax_never_matches_a_printable_detach_byte() {
    const A: &[u8] = b"\x1b[65;30;65;1;0;1_";
    let records = carrier_records(A);
    let mut input = ConsoleInput::with_detach(ptr::null_mut(), Some(b';'));
    input.records[..records.len()].copy_from_slice(&records);
    input.count = records.len();
    assert_eq!(
        input.state_with(|_, _| Ok(false)),
        InputState::Framed(vec![
            InputFrame::Key(A.to_vec(), A.to_vec(), Some(b'A'), 1,)
        ])
    );
}

#[test]
fn carrier_recognizer_is_fragment_safe_at_every_native_read_boundary() {
    for carrier in [
        b"\x1b[65;30;65;1;0;1_".as_slice(),
        b"\xc2\x9b65;30;65;1;0;1_",
    ] {
        let records = carrier_records(carrier);
        for split in 0..=records.len() {
            let mut input = ConsoleInput::with_detach(ptr::null_mut(), Some(0x1c));
            input.records[..split].copy_from_slice(&records[..split]);
            input.count = split;
            let mut refilled = false;
            let state = input.state_with(|input, timeout| {
                if refilled || split == records.len() {
                    assert_eq!(timeout, 0, "completed carrier incurred an input delay");
                    return Ok(false);
                }
                assert_eq!(timeout, 50, "partial carrier was not bounded");
                refilled = true;
                input.records[..records.len() - split].copy_from_slice(&records[split..]);
                input.next = 0;
                input.count = records.len() - split;
                Ok(true)
            });
            let decoded = win32_input_carrier(carrier).unwrap();
            assert_eq!(
                state,
                InputState::Framed(vec![decoded.frame(carrier.to_vec())]),
                "split {split} of {carrier:?}"
            );
        }
    }
}

#[test]
fn carrier_candidate_deadline_does_not_reset_on_slow_fragments() {
    let initial = carrier_records(b"\x1b[65;");
    let continuation = carrier_records(b"30");
    let mut input = ConsoleInput::with_detach(ptr::null_mut(), Some(0x1c));
    input.records[..initial.len()].copy_from_slice(&initial);
    input.count = initial.len();
    let mut refilled = false;

    assert_eq!(
        input.state_with(|input, timeout| {
            assert!(!refilled, "expired candidate requested another native read");
            assert_eq!(timeout, 50);
            refilled = true;
            input.carrier_started = Some(Instant::now() - Duration::from_millis(51));
            input.records[..continuation.len()].copy_from_slice(&continuation);
            input.next = 0;
            input.count = continuation.len();
            Ok(true)
        }),
        InputState::Bytes(b"\x1b[65;30".to_vec())
    );
}

#[test]
fn malformed_incomplete_and_overlong_carriers_replay_exactly() {
    let mut overlong = b"\x1b[".to_vec();
    overlong.extend(std::iter::repeat_n(b'1', WIN32_CARRIER_LIMIT + 4));
    overlong.push(b'_');
    for bytes in [
        b"\x1b[1;2;x".as_slice(),
        b"\x1b[65;30;65;1;0".as_slice(),
        overlong.as_slice(),
    ] {
        let records = carrier_records(bytes);
        let mut input = ConsoleInput::with_detach(ptr::null_mut(), Some(0x1c));
        input.records[..records.len()].copy_from_slice(&records);
        input.count = records.len();
        assert_eq!(
            input.state_with(|_, timeout| {
                assert!(matches!(timeout, 0 | 50));
                Ok(false)
            }),
            InputState::Bytes(bytes.to_vec()),
            "{bytes:?}"
        );
    }
}

#[test]
fn incomplete_carrier_replays_before_a_native_resize() {
    let partial = b"\x1b[65;30";
    let mut records = carrier_records(partial);
    records.push(resize_record(41, 101));
    let mut input = ConsoleInput::with_detach(ptr::null_mut(), Some(0x1c));
    input.records[..records.len()].copy_from_slice(&records);
    input.count = records.len();
    assert_eq!(
        input.state_with(|_, _| Ok(false)),
        InputState::Bytes(partial.to_vec())
    );
    assert_eq!(
        input.state_with(|_, _| Ok(false)),
        InputState::Resize(41, 101)
    );
}

#[test]
fn incomplete_carrier_replays_before_a_native_read_failure() {
    let partial = b"\xc2\x9b65;30";
    let records = carrier_records(partial);
    let mut input = ConsoleInput::with_detach(ptr::null_mut(), Some(0x1c));
    input.records[..records.len()].copy_from_slice(&records);
    input.count = records.len();
    assert_eq!(
        input.state_with(|_, timeout| {
            assert_eq!(timeout, 50);
            Err(())
        }),
        InputState::Bytes(partial.to_vec())
    );
    assert_eq!(
        input.state_with(|_, _| panic!("closed input attempted another native read")),
        InputState::Closed
    );
}

#[test]
fn disabled_detach_bypasses_carrier_recognition() {
    const CARRIER: &[u8] = b"\x1b[220;43;28;1;8;3_";
    let records = carrier_records(CARRIER);
    let mut input = ConsoleInput::with_detach(ptr::null_mut(), None);
    input.records[..records.len()].copy_from_slice(&records);
    input.count = records.len();
    assert_eq!(
        input.state_with(|_, _| Ok(false)),
        InputState::Bytes(CARRIER.to_vec())
    );
}

#[test]
fn carrier_semantics_distinguish_nul_navigation_release_and_modifiers() {
    let up = win32_input_carrier(b"\x1b[38;72;0;1;0;1_").unwrap();
    assert_eq!(
        up.frame(b"\x1b[38;72;0;1;0;1_".to_vec()),
        InputFrame::Key(
            b"\x1b[38;72;0;1;0;1_".to_vec(),
            b"\x1b[38;72;0;1;0;1_".to_vec(),
            None,
            1,
        )
    );

    let mapping = unsafe { VkKeyScanW(0) } as u16;
    let control = (u32::from(mapping & 0x100 != 0) * SHIFT_PRESSED)
        | (u32::from(mapping & 0x200 != 0) * LEFT_CTRL_PRESSED)
        | (u32::from(mapping & 0x400 != 0) * LEFT_ALT_PRESSED);
    let modern = format!("\x1b[{};0;0;1;{};1_", mapping & 0xff, control).into_bytes();
    let modern_frame = win32_input_carrier(&modern).unwrap().frame(modern.clone());
    assert!(matches!(modern_frame, InputFrame::Key(_, _, Some(0), 1)));
    let legacy = b"\x1b[0;0;0;1;0;1_".to_vec();
    assert!(matches!(
        win32_input_carrier(&legacy).unwrap().frame(legacy),
        InputFrame::Key(_, _, Some(0), 1)
    ));

    let release = b"\x1b[220;43;28;0;8;1_".to_vec();
    assert!(matches!(
        win32_input_carrier(&release).unwrap().frame(release),
        InputFrame::Meta(_)
    ));
    let modifier = b"\x1b[17;29;0;1;0;1_".to_vec();
    assert!(matches!(
        win32_input_carrier(&modifier).unwrap().frame(modifier),
        InputFrame::Meta(_)
    ));
}

#[test]
fn carrier_repeat_rewrite_preserves_all_fields_and_introducer() {
    for bytes in [
        b"\x1b[220;43;28;1;8;3_".as_slice(),
        b"\xc2\x9b220;43;28;1;8;3_",
    ] {
        let carrier = win32_input_carrier(bytes).unwrap();
        let InputFrame::Key(original, once, Some(0x1c), 3) = carrier.frame(bytes.to_vec()) else {
            panic!("detach carrier was not classified as a key")
        };
        assert_eq!(original, bytes);
        let expected = if carrier.c1 {
            b"\xc2\x9b220;43;28;1;8;1_".as_slice()
        } else {
            b"\x1b[220;43;28;1;8;1_"
        };
        assert_eq!(once, expected);
    }
}

#[test]
fn generated_character_repeat_counts_are_expanded_inside_a_carrier() {
    const CARRIER: &[u8] = b"\x1b[11;30;65;1;0;1_";
    let mut records = vec![text_record(0x1b, 1), text_record(b'['.into(), 1)];
    records.push(text_record(b'1'.into(), 2));
    records.extend(carrier_records(b";30;65;1;0;1_"));
    let mut input = ConsoleInput::with_detach(ptr::null_mut(), Some(0x1c));
    input.records[..records.len()].copy_from_slice(&records);
    input.count = records.len();
    assert_eq!(
        input.state_with(|_, _| Ok(false)),
        InputState::Framed(vec![
            win32_input_carrier(CARRIER)
                .unwrap()
                .frame(CARRIER.to_vec()),
        ])
    );
}

#[test]
fn creation_size_requires_attaching_viewers_but_defaults_headless_callers() {
    assert_eq!(creation_size(false, None).unwrap(), (24, 80));
    assert_eq!(creation_size(true, Some((33, 101))).unwrap(), (33, 101));
    assert_eq!(creation_size(false, Some((41, 132))).unwrap(), (41, 132));
    assert_eq!(
        creation_size(true, None).unwrap_err(),
        "no controlling terminal"
    );
}

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/unit/windows_event.rs"
));
