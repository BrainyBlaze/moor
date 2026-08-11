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
            | ENABLE_WINDOW_INPUT,
        0,
    );
    assert_eq!(
        input
            & (ENABLE_PROCESSED_INPUT
                | ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_QUICK_EDIT_MODE
                | ENABLE_WINDOW_INPUT),
        0
    );
    assert_ne!(input & ENABLE_VIRTUAL_TERMINAL_INPUT, 0);
    assert_ne!(input & ENABLE_EXTENDED_FLAGS, 0);
    assert_ne!(output & ENABLE_PROCESSED_OUTPUT, 0);
    assert_ne!(output & ENABLE_VIRTUAL_TERMINAL_PROCESSING, 0);
    assert_ne!(output & DISABLE_NEWLINE_AUTO_RETURN, 0);
}

#[test]
fn creation_size_requires_attaching_viewers_but_defaults_headless_callers() {
    assert_eq!(creation_size(false, None).unwrap(), (24, 80));
    assert_eq!(creation_size(true, Some((33, 101))).unwrap(), (33, 101));
    assert_eq!(creation_size(false, Some((41, 132))).unwrap(), (41, 132));
    assert_eq!(creation_size(true, None).unwrap_err(), "no controlling terminal");
}

#[test]
fn geometry_selection_observes_a_buffer_resize_racing_viewer_startup() {
    let mut state = ([(37, 93); 2], (37, 93));
    let selected = select_size(&mut state, [(37, 93), (41, 101)]).unwrap();
    assert_eq!(selected, (41, 101));
    assert_eq!(
        select_size(&mut state, [(37, 93), (41, 101)]),
        Some((41, 101))
    );
}

#[test]
fn geometry_selection_prefers_and_remembers_a_legacy_window_resize() {
    let mut state = ([(37, 93); 2], (37, 93));
    let selected = select_size(&mut state, [(31, 80), (37, 93)]).unwrap();
    assert_eq!(selected, (31, 80));
    assert_eq!(
        select_size(&mut state, [(31, 80), (37, 93)]),
        Some((31, 80))
    );
    assert_eq!(
        select_size(&mut state, [(29, 79), (41, 101)]),
        Some((29, 79))
    );
}

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/unit/windows_event.rs"
));
