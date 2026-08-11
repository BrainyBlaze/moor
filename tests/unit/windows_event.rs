#[test]
fn absent_event_is_pinned_before_creation_becomes_observable() {
    let nonce = format!("{}-{}", std::process::id(), now());
    let base = std::env::temp_dir().join(format!("moor-event-root-{nonce}"));
    let outside = std::env::temp_dir().join(format!("moor-event-outside-{nonce}"));
    create_store_path(&base, true).unwrap();
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
