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
        ENABLE_PROCESSED_INPUT
            | ENABLE_LINE_INPUT
            | ENABLE_ECHO_INPUT
            | ENABLE_QUICK_EDIT_MODE
            | ENABLE_MOUSE_INPUT
            | ENABLE_WINDOW_INPUT
            | ENABLE_VIRTUAL_TERMINAL_INPUT,
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
    key.dwControlKeyState = (key.dwControlKeyState
        & !(LEFT_CTRL_PRESSED | LEFT_ALT_PRESSED))
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
    assert_eq!(
        input.record(nul_record(2), CP_UTF8, &mut wide),
        Ok(None)
    );
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

#[test]
fn creation_size_requires_attaching_viewers_but_defaults_headless_callers() {
    assert_eq!(creation_size(false, None).unwrap(), (24, 80));
    assert_eq!(creation_size(true, Some((33, 101))).unwrap(), (33, 101));
    assert_eq!(creation_size(false, Some((41, 132))).unwrap(), (41, 132));
    assert_eq!(creation_size(true, None).unwrap_err(), "no controlling terminal");
}

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/unit/windows_event.rs"
));
