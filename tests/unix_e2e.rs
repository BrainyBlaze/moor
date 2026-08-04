#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use moor::store::{Kind, Store};

fn temp() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!(
        "moor-e2e-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}

fn moor(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_moor"))
        .args(args)
        .output()
        .unwrap()
}

fn tail(socket: &Path) -> Output {
    moor(&["tail", "-n", "100", socket.to_str().unwrap()])
}

fn wait_for(socket: &Path, needle: &[u8]) {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let output = tail(socket);
        if output.status.success() && output.stdout.windows(needle.len()).any(|w| w == needle) {
            return;
        }
        assert!(Instant::now() < until, "tail: {output:?}");
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn background_session_push_log_clear_and_kill_use_the_shipped_binary() {
    let dir = temp();
    let socket = dir.join("session");
    let name = socket.to_str().unwrap();
    let start = moor(&[
        "start",
        name,
        "/bin/sh",
        "-c",
        "printf 'ready\\n'; IFS= read -r line; printf 'got:%s\\n' \"$line\"; sleep 30",
    ]);
    assert!(start.status.success(), "{start:?}");
    wait_for(&socket, b"ready\r\n");

    let mut push = Command::new(env!("CARGO_BIN_EXE_moor"))
        .args(["push", name])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    push.stdin.take().unwrap().write_all(b"hello\n").unwrap();
    assert!(push.wait_with_output().unwrap().status.success());
    wait_for(&socket, b"got:hello\r\n");

    assert!(moor(&["clear", name]).status.success());
    assert_eq!(tail(&socket).stdout, b"");
    let killed = moor(&["kill", "-f", name]);
    assert!(killed.status.success(), "{killed:?}");
    assert!(!socket.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn attach_replays_and_detaches_without_stopping_the_session() {
    let dir = temp();
    let socket = dir.join("attachable");
    let name = socket.to_str().unwrap();
    let start = moor(&["start", name, "/bin/sh", "-c", "printf 'attached\\n'; sleep 30"]);
    assert!(start.status.success(), "{start:?}");
    wait_for(&socket, b"attached\r\n");
    let mut attach = Command::new(env!("CARGO_BIN_EXE_moor"))
        .args(["attach", name]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    attach.stdin.take().unwrap().write_all(&[0x1c]).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    let status = loop { if let Some(status) = attach.try_wait().unwrap() { break status; } assert!(Instant::now() < until, "attach did not detach"); thread::sleep(Duration::from_millis(20)); };
    assert!(status.success(), "attach: {status:?}");
    assert!(socket.exists());
    assert!(moor(&["kill", "-f", name]).status.success());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn foreground_run_returns_the_child_status() {
    let dir = temp();
    let socket = dir.join("foreground");
    let output = moor(&["run", socket.to_str().unwrap(), "/bin/sh", "-c", "exit 7"]);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert!(!socket.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn instrumentation_must_ack_from_inside_the_requested_child() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp();
    let source = dir.join("ack.c");
    let library = dir.join("ack.so");
    fs::write(
        &source,
        r#"#include <stdlib.h>
#include <stdint.h>
#include <unistd.h>
__attribute__((constructor)) static void ack(void) {
  char *f=getenv("DESK_MOOR_INSTRUMENT_CHANNEL"), *n=getenv("DESK_MOOR_INSTRUMENT_NONCE");
  if(!f || !n) return; unsigned char b[36]={'M','O','O','R','I','N','S','3',1};
  b[12]=1; uint32_t p=(uint32_t)getpid(); for(int i=0;i<4;i++) b[16+i]=(p>>(8*i))&255;
  for(int i=0;i<16;i++) { char x[3]={n[i*2],n[i*2+1],0}; b[20+i]=(unsigned char)strtoul(x,0,16); }
  int fd=atoi(f); unsetenv("DESK_MOOR_INSTRUMENT_CHANNEL"); unsetenv("DESK_MOOR_INSTRUMENT_NONCE"); write(fd,b,36); close(fd);
}"#,
    )
    .unwrap();
    let built = Command::new("cc")
        .args(["-shared", "-fPIC", "-o"])
        .arg(&library)
        .arg(&source)
        .status()
        .unwrap();
    assert!(built.success());
    fs::set_permissions(&library, fs::Permissions::from_mode(0o500)).unwrap();
    let socket = dir.join("instrumented");
    let output = moor(&[
        "start",
        socket.to_str().unwrap(),
        "-S",
        library.to_str().unwrap(),
        "/bin/sh",
        "-c",
        "sleep 30",
    ]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        moor(&["kill", "-f", socket.to_str().unwrap()])
            .status
            .success()
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn current_uses_canonical_v2_ancestry_and_rejects_bad_carriers() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let first = b"/tmp/has:colon";
    let second = b"/tmp/inner";
    let legacy = [first.as_slice(), b":", second.as_slice()].concat();
    let v2 = format!("v2:{}:{}", STANDARD.encode(first), STANDARD.encode(second));
    let run = |legacy: &[u8], v2: &str| {
        Command::new(env!("CARGO_BIN_EXE_moor"))
            .arg("current")
            .env("MOOR_SESSION", std::ffi::OsString::from_vec(legacy.to_vec()))
            .env("MOOR_SESSION_V2", v2)
            .output()
            .unwrap()
    };
    let output = run(&legacy, &v2);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"has\\x3Acolon > inner\n");
    let malformed = run(&legacy, "v2:not-base64");
    assert_eq!(malformed.status.code(), Some(1));
    assert_eq!(malformed.stderr, b"moor: session ancestry v2 is malformed\n");
    let mismatch = run(b"/tmp/different", &v2);
    assert_eq!(mismatch.status.code(), Some(1));
    assert_eq!(mismatch.stderr, b"moor: session ancestry carriers disagree\n");
}

#[test]
fn observed_exit_is_durable_listed_and_emitted_to_events() {
    let dir = temp();
    let session = dir.file_name().unwrap().to_str().unwrap();
    let socket = std::env::temp_dir().join(format!(".moor-{}", unsafe { libc::geteuid() })).join(session);
    let events = dir.join("event-store");
    let output = moor(&[
        "start", session, "-T", events.to_str().unwrap(),
        "/bin/sh", "-c", "printf '\x1b[>0q'; sleep .2; exit 7",
    ]);
    assert!(output.status.success(), "{output:?}");
    let until = Instant::now() + Duration::from_secs(5);
    while socket.exists() && Instant::now() < until { thread::sleep(Duration::from_millis(20)); }
    assert!(!socket.exists());
    let exit_path = PathBuf::from(format!("{}.exit", socket.display()));
    let (commit, body) = Store::read_only(&exit_path, Kind::Exit, 1).unwrap();
    let body = String::from_utf8(body).unwrap();
    assert_eq!(commit.index, 2);
    assert!(body.contains("\"phase\":\"exited\""), "{body}");
    assert!(body.contains("\"start_wall_ms\":\""), "{body}");
    assert!(body.contains("\"ended\":\"exited\",\"code\":7"), "{body}");
    let (_, body) = Store::read_only(&events, Kind::Event, 1).unwrap();
    let body = String::from_utf8(body).unwrap();
    assert!(body.contains("\"type\":\"ready\""), "{body}");
    assert!(body.contains("\"type\":\"exit\""), "{body}");
    let listed = moor(&["list", "-a"]);
    assert!(listed.stdout.windows(session.len()).any(|w| w == session.as_bytes()), "{listed:?}");
    assert!(listed.stdout.windows(b"[exited]".len()).any(|w| w == b"[exited]"), "{listed:?}");
    assert!(moor(&["rm", session]).status.success());
    assert!(!events.exists());
    fs::remove_dir_all(dir).unwrap();
}
