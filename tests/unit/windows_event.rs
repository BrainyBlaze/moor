#[test]
fn absent_event_is_pinned_before_creation_becomes_observable() {
    let nonce = format!("{}-{}", std::process::id(), now());
    let base = std::env::temp_dir().join(format!("moor-event-root-{nonce}"));
    let outside = std::env::temp_dir().join(format!("moor-event-outside-{nonce}"));
    create_store_path(&base).unwrap();
    fs::create_dir(&outside).unwrap();
    let event = base.join("events");
    let mut target = event_target(&event, &base).unwrap();

    materialize_event(&mut target, sid().unwrap(), |created| {
        assert!(fs::remove_dir(created).is_err());
        let linked = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(created)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(!linked.status.success(), "{linked:?}");
    })
    .unwrap();
    validate_event(&target, sid().unwrap()).unwrap();
    drop(target);
    fs::remove_dir(&event).unwrap();
    assert!(!outside.join("body.0").exists());
    fs::remove_dir(base).unwrap();
    fs::remove_dir(outside).unwrap();
}

#[test]
fn event_slots_are_created_against_the_validated_directory_handle() {
    let nonce = format!("{}-{}", std::process::id(), now());
    let base = std::env::temp_dir().join(format!("moor-event-binding-{nonce}"));
    create_store_path(&base).unwrap();
    let first = base.join("first");
    let second = base.join("second");
    let mut first_target = event_target(&first, &base).unwrap();
    let mut second_target = event_target(&second, &base).unwrap();
    materialize_event(&mut first_target, sid().unwrap(), |_| {}).unwrap();
    materialize_event(&mut second_target, sid().unwrap(), |_| {}).unwrap();
    let header = crate::events::canonical_header(
        1,
        "c2Vzc2lvbg==",
        Some(7),
        crate::events::Cursor(0, 0, 0, 1),
    );

    let result = crate::store::Store::create_event_at(
        &second,
        first_target.guards.last().unwrap(),
        7,
        header.as_bytes(),
    );
    assert!(result.is_err(), "accepted a mismatched path/handle binding");
    for directory in [&first, &second] {
        for slot in ["body.0", "body.1", "commit.0", "commit.1"] {
            assert!(
                !directory.join(slot).exists(),
                "slot escaped rollback: {directory:?}/{slot}"
            );
        }
    }

    drop((first_target, second_target));
    fs::remove_dir(first).unwrap();
    fs::remove_dir(second).unwrap();
    fs::remove_dir(base).unwrap();
}

#[test]
fn stderr_validation_and_inheritance_use_one_pinned_handle() {
    let nonce = format!("{}-{}", std::process::id(), now());
    let base = std::env::temp_dir().join(format!("moor-stderr-binding-{nonce}"));
    create_store_path(&base).unwrap();
    let sink = base.join("stderr");
    let displaced = base.join("displaced");
    drop(
        stage_file(&sink, sid().unwrap(), "FA", "create stderr fixture", |file| {
            file.write_all(b"before").map_err(string)
        })
        .unwrap(),
    );

    let handle = open_stderr_operand(&sink, sid().unwrap(), |_| {
        assert!(
            fs::rename(&sink, &displaced).is_err(),
            "stderr pathname was replaceable while its inherited handle was live"
        );
    })
    .unwrap();
    handle.write(b"-after", "append stderr fixture").unwrap();
    drop(handle);
    assert_eq!(fs::read(&sink).unwrap(), b"before-after");

    fs::remove_file(sink).unwrap();
    fs::remove_dir(base).unwrap();
}

#[test]
fn instrumentation_copy_source_remains_the_validated_open_object_after_rename() {
    let nonce = format!("{}-{}", std::process::id(), now());
    let base = std::env::temp_dir().join(format!("moor-instrument-binding-{nonce}"));
    create_store_path(&base).unwrap();
    let source = base.join("source.dll");
    let replacement = base.join("replacement.dll");
    let displaced = base.join("displaced.dll");
    for (path, bytes) in [(&source, &b"validated"[..]), (&replacement, &b"replacement"[..])] {
        drop(
            stage_file(path, sid().unwrap(), "FRFX", "create instrument fixture", |file| {
                file.write_all(bytes).map_err(string)
            })
            .unwrap(),
        );
    }

    let mut file = open_instrument_operand(&source, sid().unwrap(), |_| {
        fs::rename(&source, &displaced).unwrap();
        fs::rename(&replacement, &source).unwrap();
    })
    .unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"validated");
    assert_eq!(fs::read(&source).unwrap(), b"replacement");

    drop(file);
    fs::remove_file(source).unwrap();
    fs::remove_file(displaced).unwrap();
    fs::remove_dir(base).unwrap();
}

#[test]
fn post_rename_marker_validation_removes_only_the_staged_identity() {
    let nonce = format!("{}-{}", std::process::id(), now());
    let base = std::env::temp_dir().join(format!("moor-marker-binding-{nonce}"));
    create_store_path(&base).unwrap();
    let marker = base.join("marker");
    let stage_path = base.join("marker.stage");
    let displaced = base.join("displaced");
    let (file, identity, ()) = stage_file(
        &stage_path,
        sid().unwrap(),
        "FR",
        "create marker fixture",
        |file| file.write_all(b"original").map_err(string),
    )
    .unwrap();
    let stage = Staged {
        path: stage_path,
        file,
        identity,
    };

    let result = publish_marker_stage(&stage, &marker, sid().unwrap(), |_| {
        fs::rename(&marker, &displaced).unwrap();
        drop(
            stage_file(
                &marker,
                sid().unwrap(),
                "FR",
                "create successor marker",
                |file| file.write_all(b"successor").map_err(string),
            )
            .unwrap(),
        );
    });
    assert_eq!(result.unwrap_err(), "marker identity changed");
    assert!(delete_file(&stage.file));
    drop(stage);
    assert_eq!(fs::read(&marker).unwrap(), b"successor");
    assert!(
        fs::symlink_metadata(&displaced).is_err(),
        "staged marker identity survived exact-handle rollback"
    );

    fs::remove_file(marker).unwrap();
    fs::remove_dir(base).unwrap();
}
