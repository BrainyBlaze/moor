//! Passes a privately duplicated handle through a child-side protocol.

#[cfg(windows)]
fn main() -> std::io::Result<()> {
    use std::fs::File;

    use windows_spawn::Command;

    let log = File::create("worker.log")?;
    let mut command = Command::new("worker.exe");
    command.arg("--log-handle").arg_handle(&log)?;

    // arg_handle stored a private non-inheritable duplicate. The worker
    // protocol parses the following decimal argument as its borrowed handle.
    drop(log);
    let status = command.status()?;
    assert!(status.success());
    Ok(())
}

#[cfg(not(windows))]
fn main() {}
