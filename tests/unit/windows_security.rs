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
    create_store_path(&path, true).unwrap();
    validate(&path, sid().unwrap(), "FA", true).unwrap();
    fs::remove_dir(path).unwrap();
}

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/unit/windows_event.rs"
));
