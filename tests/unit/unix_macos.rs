use super::*;
use std::sync::mpsc::sync_channel;

#[test]
fn descriptor_relative_socket_name_never_changes_process_cwd() {
    let before = std::env::current_dir().unwrap();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("moor-thread-cwd-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).unwrap();
    let parent = open_directory(&path).unwrap();
    let (entered, wait) = sync_channel(0);
    let (observed, receive) = sync_channel(0);
    let observer = thread::spawn(move || {
        wait.recv().unwrap();
        observed.send(std::env::current_dir().unwrap()).unwrap();
    });
    let during = socket_name(&parent, OsStr::new("probe"), move |_| {
        entered.send(()).unwrap();
        Ok::<_, io::Error>(receive.recv().unwrap())
    })
    .unwrap();
    observer.join().unwrap();
    assert_eq!(during, before);
    assert_eq!(std::env::current_dir().unwrap(), before);
    fs::remove_dir(path).unwrap();
}

#[test]
fn accepted_socket_is_blocking_before_runtime_io_starts() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "moor-blocking-accept-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&path).unwrap();
    let parent = open_directory(&path).unwrap();
    let leaf = OsStr::new("probe");
    let listener = socket_name(&parent, leaf, |name| {
        ListenerOptions::new()
            .name(name)
            .reclaim_name(false)
            .nonblocking(ListenerNonblockingMode::Accept)
            .create_sync()
    })
    .unwrap();
    let client = socket_name(&parent, leaf, LocalStream::connect).unwrap();
    let accepted = accept_blocking(&listener).expect("pending local connection");
    let LocalStream::UdSocket(accepted) = accepted;
    let flags = fcntl(accepted.as_fd(), FcntlArg::F_GETFL).unwrap();
    assert_eq!(flags & libc::O_NONBLOCK, 0);
    drop(client);
    drop(listener);
    fs::remove_file(path.join(leaf)).unwrap();
    fs::remove_dir(path).unwrap();
}
