#[test]
fn absent_event_substitution_is_rejected_before_slot_creation() {
    let nonce = format!("{}-{}", std::process::id(), now());
    let base = std::env::temp_dir().join(format!("moor-event-root-{nonce}"));
    let outside = std::env::temp_dir().join(format!("moor-event-outside-{nonce}"));
    create_store_path(&base, true).unwrap();
    fs::create_dir(&outside).unwrap();
    let event = base.join("events");
    let mut target = event_target(&event, &base).unwrap();

    let error = materialize_event(&mut target, sid().unwrap(), |created| {
        fs::remove_dir(created).unwrap();
        let linked = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(created)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(linked.status.success(), "{linked:?}");
    })
    .unwrap_err();
    assert_eq!(error, event_rejection(&event, "identity-changed"));
    drop(target);
    assert!(!event.exists(), "substituted junction survived rollback");
    assert!(!outside.join("body.0").exists());
    fs::remove_dir(base).unwrap();
    fs::remove_dir(outside).unwrap();
}
