#![cfg(unix)]

use moor::events::{Cursor, canonical_header};
use moor::runtime::private::{companion, lifecycle_running, now};
use moor::store::{Kind, Store};
use std::fs::{self, DirBuilder, File};
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

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

fn invoked_command(alias: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_moor"));
    command.arg0(alias);
    command
}

fn invoked(alias: &str, args: &[&str]) -> Output {
    invoked_command(alias).args(args).output().unwrap()
}

fn isolated_root(alias: &str) -> PathBuf {
    std::env::temp_dir().join(format!(".{alias}-{}", unsafe { libc::geteuid() }))
}

fn stale_socket(path: &Path) {
    drop(UnixListener::bind(path).unwrap());
}

fn set_age(path: &Path, seconds: u64) {
    let running = lifecycle_running(
        b"\x01/test-session",
        (None, 1),
        [9; 16],
        (now().saturating_sub(seconds * 1000), 1, [0xa5; 16]),
        ("posix-bytes", None, None),
    );
    drop(
        Store::create(
            &companion(path, ".exit"),
            Kind::Exit,
            1,
            running.as_bytes(),
            0,
            0,
        )
        .unwrap(),
    );
}

fn terminal_pair(rows: u16, columns: u16) -> (File, File) {
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut::<libc::termios>(),
                &mut size,
            )
        },
        0
    );
    let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
    let mut modes: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut modes) }, 0);
    modes.c_lflag |= libc::ECHO | libc::ICANON;
    assert_eq!(
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &modes) },
        0
    );
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(
        flags >= 0
            && unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                == 0
    );
    (master, slave)
}

fn terminal_command(slave: &File) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_moor"));
    command.stdin(Stdio::from(slave.try_clone().unwrap()));
    command.stdout(Stdio::from(slave.try_clone().unwrap()));
    command.stderr(Stdio::from(slave.try_clone().unwrap()));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}

fn terminal_output(master: &mut File, needle: &[u8], timeout: Duration) -> Vec<u8> {
    let until = Instant::now() + timeout;
    let mut output = Vec::new();
    let mut bytes = [0; 4096];
    while Instant::now() < until {
        match master.read(&mut bytes) {
            Ok(0) => break,
            Ok(count) => output.extend_from_slice(&bytes[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(error) => panic!("terminal read: {error}"),
        }
        if output.windows(needle.len()).any(|part| part == needle) {
            break;
        }
    }
    output
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

fn launch_channel(generation: u32) -> RawFd {
    let mut descriptors = [0; 2];
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    let mut record = [0u8; 32];
    record[..8].copy_from_slice(b"MOORLCH3");
    record[8] = 1;
    record[12..16].copy_from_slice(&generation.to_le_bytes());
    record[16..].fill(7);
    assert_eq!(
        unsafe { libc::write(descriptors[1], record.as_ptr().cast(), record.len()) },
        record.len() as isize
    );
    assert_eq!(unsafe { libc::close(descriptors[1]) }, 0);
    descriptors[0]
}

fn instrumentation(dir: &Path, exit: Option<u8>) -> PathBuf {
    let source = dir.join(format!("ack-{}.c", exit.unwrap_or_default()));
    let library = dir.join(format!("ack-{}.so", exit.unwrap_or_default()));
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
  int fd=atoi(f); unsetenv("DESK_MOOR_INSTRUMENT_CHANNEL"); unsetenv("DESK_MOOR_INSTRUMENT_NONCE"); write(fd,b,36);
#ifdef EXIT_STATUS
  _exit(EXIT_STATUS);
#else
  close(fd);
#endif
}"#,
    )
    .unwrap();
    let mut compiler = Command::new("cc");
    compiler
        .args(["-shared", "-fPIC", "-o"])
        .arg(&library)
        .arg(&source);
    if let Some(status) = exit {
        compiler.arg(format!("-DEXIT_STATUS={status}"));
    }
    assert!(compiler.status().unwrap().success());
    if exit.is_some() {
        File::options()
            .write(true)
            .open(&library)
            .unwrap()
            .set_len(16 << 20)
            .unwrap();
    }
    fs::set_permissions(&library, fs::Permissions::from_mode(0o500)).unwrap();
    library
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
fn child_start_failure_is_crlf_diagnostic_status_127_without_residue() {
    let dir = temp();
    let missing = dir.join("missing-child");
    let system = Command::new(&missing).spawn().unwrap_err().to_string();
    for mode in ["start", "run"] {
        let socket = dir.join(mode);
        let output = moor(&[mode, socket.to_str().unwrap(), missing.to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(127), "{output:?}");
        assert_eq!(
            output.stderr,
            format!(
                "moor: could not execute {}: {system}\r\n",
                missing.display()
            )
            .as_bytes()
        );
        assert!(
            output.stdout.is_empty() && fs::symlink_metadata(&socket).is_err(),
            "{output:?}"
        );
        for suffix in [".log", ".events", ".exit", ".instrument"] {
            assert!(fs::symlink_metadata(companion(&socket, suffix)).is_err());
        }
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn requested_child_resets_signals_and_closes_unintended_descriptors() {
    let dir = temp();
    let source = dir.join("process-state.c");
    let probe = dir.join("process-state");
    fs::write(
        &source,
        r#"#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdlib.h>
int main(int argc, char **argv) {
  struct sigaction action; sigset_t mask;
  if (argc != 2 || sigaction(SIGUSR1, 0, &action) || sigprocmask(SIG_SETMASK, 0, &mask)) return 8;
  errno = 0; int descriptor_closed = fcntl(atoi(argv[1]), F_GETFD) < 0 && errno == EBADF;
  return action.sa_handler == SIG_DFL && !sigismember(&mask, SIGUSR2) && descriptor_closed ? 0 : 9;
}"#,
    )
    .unwrap();
    assert!(
        Command::new("cc")
            .args(["-o"])
            .arg(&probe)
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    let leaked = File::open(&source).unwrap();
    assert_eq!(
        unsafe { libc::fcntl(leaked.as_raw_fd(), libc::F_SETFD, 0) },
        0
    );
    let socket = dir.join("session");
    let mut command = Command::new(env!("CARGO_BIN_EXE_moor"));
    command.args([
        "run",
        socket.to_str().unwrap(),
        probe.to_str().unwrap(),
        &leaked.as_raw_fd().to_string(),
    ]);
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGUSR1, libc::SIG_IGN);
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGUSR2);
            if libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stderr_sink_is_opened_once_without_following_or_blocking() {
    let dir = temp();
    let sink = dir.join("stderr");
    fs::write(&sink, b"before\n").unwrap();
    fs::set_permissions(&sink, fs::Permissions::from_mode(0o600)).unwrap();
    let socket = dir.join("session");
    let started = moor(&[
        "start",
        socket.to_str().unwrap(),
        "-2",
        sink.to_str().unwrap(),
        "/bin/sh",
        "-c",
        "printf 'after\\n' >&2; sleep 30",
    ]);
    assert!(started.status.success(), "{started:?}");
    let until = Instant::now() + Duration::from_secs(2);
    while !fs::read(&sink).unwrap().ends_with(b"after\n") && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fs::read(&sink).unwrap(), b"before\nafter\n");
    assert!(
        moor(&["kill", "-f", "-q", socket.to_str().unwrap()])
            .status
            .success()
    );

    let link = dir.join("stderr-link");
    symlink(&sink, &link).unwrap();
    let refused = moor(&[
        "start",
        socket.to_str().unwrap(),
        "-2",
        link.to_str().unwrap(),
        "/bin/true",
    ]);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(!socket.exists());

    let fifo = dir.join("stderr-fifo");
    assert_eq!(
        unsafe {
            libc::mkfifo(
                std::ffi::CString::new(fifo.as_os_str().as_bytes())
                    .unwrap()
                    .as_ptr(),
                0o600,
            )
        },
        0
    );
    let before = Instant::now();
    let refused = moor(&[
        "start",
        socket.to_str().unwrap(),
        "-2",
        fifo.to_str().unwrap(),
        "/bin/true",
    ]);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(
        before.elapsed() < Duration::from_secs(1),
        "FIFO open blocked for {:?}",
        before.elapsed()
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cleanup_refuses_a_dangling_rendezvous_symlink() {
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    DirBuilder::new().mode(0o700).create(&root).unwrap();
    let path = root.join("dangling");
    symlink(root.join("missing"), &path).unwrap();
    fs::create_dir(companion(&path, ".exit")).unwrap();
    let removed = invoked(alias, &["rm", "dangling"]);
    assert_eq!(removed.status.code(), Some(1), "{removed:?}");
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(companion(&path, ".exit").exists());
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(marker).unwrap();
}

#[test]
fn restrictive_umask_still_creates_an_exact_owner_only_root() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let _ = fs::remove_dir_all(&root);
    let mut command = invoked_command(alias);
    command.arg("list");
    unsafe {
        command.pre_exec(|| {
            libc::umask(0o777);
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let meta = fs::symlink_metadata(&root).unwrap();
    assert!(meta.file_type().is_dir());
    assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    fs::remove_dir(&root).unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn copied_lifecycle_cannot_authorize_external_store_deletion() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    DirBuilder::new().mode(0o700).create(&root).unwrap();
    let target = root.join("copied");
    let source = root.join("source");
    let external = dir.join("event-store");
    let header = canonical_header(1, "AS9z", None, Cursor(0, 0, 0, 1));
    drop(Store::create(&external, Kind::Event, 1, header.as_bytes(), 0, 0).unwrap());
    let mut identity = vec![1];
    identity.extend_from_slice(source.as_os_str().as_bytes());
    let running = lifecycle_running(
        &identity,
        (None, 1),
        [9; 16],
        (1, 1, [7; 16]),
        ("posix-bytes", Some(external.as_os_str().as_bytes()), None),
    );
    drop(
        Store::create(
            &companion(&target, ".exit"),
            Kind::Exit,
            1,
            running.as_bytes(),
            0,
            0,
        )
        .unwrap(),
    );
    let removed = invoked(alias, &["rm", "copied"]);
    assert!(removed.status.success(), "{removed:?}");
    assert!(Store::read_only(&external, Kind::Event, 1).is_ok());
    assert!(fs::symlink_metadata(companion(&target, ".exit")).is_err());
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn operational_errors_use_stdout_and_launch_validation_uses_stderr() {
    let dir = temp();
    let missing = dir.join("missing");
    let killed = moor(&["kill", missing.to_str().unwrap()]);
    assert_eq!(killed.status.code(), Some(1));
    assert_eq!(
        killed.stdout,
        format!("moor: session '{}' does not exist\n", missing.display()).as_bytes()
    );
    assert!(killed.stderr.is_empty());

    let tailed = moor(&["tail", missing.to_str().unwrap()]);
    assert_eq!(tailed.status.code(), Some(1));
    assert_eq!(
        tailed.stdout,
        format!("moor: no log for session '{}'\n", missing.display()).as_bytes()
    );
    assert!(tailed.stderr.is_empty());

    let sink = dir.join("absent-stderr");
    let launched = moor(&[
        "start",
        missing.to_str().unwrap(),
        "-2",
        sink.to_str().unwrap(),
        "/bin/true",
    ]);
    assert_eq!(launched.status.code(), Some(1));
    assert!(launched.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&launched.stderr).starts_with("moor: No such file or directory"),
        "{launched:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn graceful_kill_escalates_and_reports_the_requested_outcome() {
    let dir = temp();
    let socket = dir.join("term-resistant");
    let name = socket.to_str().unwrap();
    let started = moor(&[
        "start",
        name,
        "/bin/sh",
        "-c",
        "trap '' TERM; printf 'ready\\n'; while :; do sleep 1; done",
    ]);
    assert!(started.status.success(), "{started:?}");
    wait_for(&socket, b"ready\r\n");
    let before = Instant::now();
    let stopped = moor(&["kill", name]);
    let elapsed = before.elapsed();
    assert!(stopped.status.success(), "{stopped:?}");
    assert_eq!(
        stopped.stdout,
        format!("session '{name}' stopped\n").as_bytes()
    );
    assert!(!socket.exists());
    assert!(
        elapsed >= Duration::from_secs(4) && elapsed < Duration::from_secs(10),
        "graceful escalation took {elapsed:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn graceful_escalation_ignores_a_realtime_step_after_sigterm() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp();
    let source = dir.join("jump.c");
    let library = dir.join("jump.so");
    fs::write(
        &source,
        r#"#define _GNU_SOURCE
#include <signal.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
static volatile sig_atomic_t jumped;
int kill(pid_t pid, int signal) {
  int result=(int)syscall(SYS_kill,pid,signal);
  if(signal==SIGTERM) jumped=1;
  return result;
}
int clock_gettime(clockid_t clock, struct timespec *value) {
  int result=(int)syscall(SYS_clock_gettime,clock,value);
  if(result==0 && clock==CLOCK_REALTIME && jumped) value->tv_sec+=30;
  return result;
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
    let socket = dir.join("realtime-step");
    let name = socket.to_str().unwrap();
    let started = Command::new(env!("CARGO_BIN_EXE_moor"))
        .args([
            "start",
            name,
            "/bin/sh",
            "-c",
            "trap '' TERM; printf 'ready\\n'; while :; do sleep 1; done",
        ])
        .env("LD_PRELOAD", &library)
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
    wait_for(&socket, b"ready\r\n");
    let before = Instant::now();
    let stopped = moor(&["kill", name]);
    assert!(stopped.status.success(), "{stopped:?}");
    assert!(
        before.elapsed() >= Duration::from_secs(4),
        "realtime step caused early escalation"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ordinary_kill_targets_the_current_foreground_process_group() {
    let dir = temp();
    let source = dir.join("foreground-group.c");
    let probe = dir.join("foreground-group");
    fs::write(
        &source,
        r#"#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>
int main(void) {
  pid_t worker=fork(); if(worker<0) return 2;
  if(worker==0) { signal(SIGTTOU,SIG_IGN);
    if(setpgid(0,0)<0 || tcsetpgrp(0,getpgrp())<0) return 3;
    printf("worker:%d\n",(int)getpid()); fflush(stdout); for(;;) pause(); }
  signal(SIGTERM,SIG_IGN); int status; while(waitpid(worker,&status,0)<0) {}
  return WIFSIGNALED(status) && WTERMSIG(status)==SIGTERM ? 0 : 4;
}"#,
    )
    .unwrap();
    assert!(
        Command::new("cc")
            .args(["-o"])
            .arg(&probe)
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    let socket = dir.join("foreground-group-session");
    let name = socket.to_str().unwrap();
    assert!(
        moor(&["start", name, probe.to_str().unwrap()])
            .status
            .success()
    );
    wait_for(&socket, b"worker:");
    let output = tail(&socket).stdout;
    let pid = std::str::from_utf8(&output)
        .unwrap()
        .split("worker:")
        .nth(1)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .trim_end_matches('\r')
        .parse::<i32>()
        .unwrap();
    let before = Instant::now();
    let stopped = moor(&["kill", name]);
    assert!(stopped.status.success(), "{stopped:?}");
    assert!(
        before.elapsed() < Duration::from_secs(2),
        "foreground termination fell back: {:?}",
        before.elapsed()
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "foreground worker {pid} survived"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn holder_signal_uses_bounded_normal_path_shutdown_and_cleanup() {
    let dir = temp();
    let socket = dir.join("signalled-holder");
    let name = socket.to_str().unwrap();
    let mut holder = Command::new(env!("CARGO_BIN_EXE_moor"))
        .args([
            "run",
            name,
            "/bin/sh",
            "-c",
            "trap '' TERM; printf 'ready\\n'; while :; do sleep 1; done",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(&socket, b"ready\r\n");
    let before = Instant::now();
    assert_eq!(unsafe { libc::kill(holder.id() as i32, libc::SIGTERM) }, 0);
    let deadline = before + Duration::from_secs(10);
    while holder.try_wait().unwrap().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    if holder.try_wait().unwrap().is_none() {
        holder.kill().unwrap();
    }
    let output = holder.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        before.elapsed() >= Duration::from_secs(4) && before.elapsed() < Duration::from_secs(10)
    );
    assert!(!socket.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_second_holder_signal_escalates_without_resetting_the_deadline() {
    let dir = temp();
    let socket = dir.join("twice-signalled-holder");
    let name = socket.to_str().unwrap();
    let holder = Command::new(env!("CARGO_BIN_EXE_moor"))
        .args([
            "run",
            name,
            "/bin/sh",
            "-c",
            "trap '' TERM; printf 'ready\\n'; while :; do sleep 1; done",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(&socket, b"ready\r\n");
    let before = Instant::now();
    assert_eq!(unsafe { libc::kill(holder.id() as i32, libc::SIGTERM) }, 0);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(unsafe { libc::kill(holder.id() as i32, libc::SIGINT) }, 0);
    let output = holder.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        before.elapsed() < Duration::from_secs(2),
        "second signal did not escalate: {:?}",
        before.elapsed()
    );
    assert!(!socket.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn attach_replays_and_detaches_without_stopping_the_session() {
    let dir = temp();
    let socket = dir.join("attachable");
    let name = socket.to_str().unwrap();
    let start = moor(&[
        "start",
        name,
        "/bin/sh",
        "-c",
        "printf 'attached\\n'; sleep 30",
    ]);
    assert!(start.status.success(), "{start:?}");
    wait_for(&socket, b"attached\r\n");
    // attach requires a controlling terminal (§13.1), so the viewer runs under
    // a real pseudo-terminal and the detach byte is typed into its master.
    let (mut master, slave) = terminal_pair(24, 80);
    let mut attach = terminal_command(&slave)
        .args(["attach", name])
        .spawn()
        .unwrap();
    terminal_output(&mut master, b"attached", Duration::from_secs(5));
    master.write_all(&[0x1c]).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = attach.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < until, "attach did not detach");
        thread::sleep(Duration::from_millis(20));
    };
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
fn headless_child_gets_sane_default_geometry_and_termios() {
    let dir = temp();
    let socket = dir.join("terminal-defaults");
    let output = moor(&[
        "start",
        socket.to_str().unwrap(),
        "/bin/sh",
        "-c",
        "stty size; stty -a",
    ]);
    assert!(output.status.success(), "{output:?}");
    let until = Instant::now() + Duration::from_secs(5);
    while socket.exists() && Instant::now() < until {
        thread::sleep(Duration::from_millis(20));
    }
    let logged = tail(&socket);
    assert!(logged.status.success(), "{logged:?}");
    assert!(
        logged.stdout.windows(7).any(|bytes| bytes == b"24 80\r\n"),
        "{logged:?}"
    );
    assert!(
        logged.stdout.windows(6).any(|bytes| bytes == b"icanon"),
        "{logged:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn creating_viewer_copies_terminal_geometry_and_restores_local_modes() {
    let dir = temp();
    let socket = dir.join("viewer-terminal");
    let name = socket.to_str().unwrap();
    let (mut master, slave) = terminal_pair(33, 101);
    let mut before: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut before) },
        0
    );
    let mut create = terminal_command(&slave)
        .args(["new", name, "/bin/sh", "-c", "stty size; sleep 30"])
        .spawn()
        .unwrap();
    let output = terminal_output(&mut master, b"33 101\r\n", Duration::from_secs(3));
    master.write_all(&[0x1c]).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while create.try_wait().unwrap().is_none() && Instant::now() < until {
        thread::sleep(Duration::from_millis(20));
    }
    if create.try_wait().unwrap().is_none() {
        create.kill().unwrap();
    }
    let _ = create.wait();
    let mut after: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut after) }, 0);
    let _ = moor(&["kill", "-f", "-q", name]);
    fs::remove_dir_all(dir).unwrap();
    assert!(
        output
            .windows(b"33 101\r\n".len())
            .any(|part| part == b"33 101\r\n"),
        "{output:?}"
    );
    assert_eq!(before.c_lflag, after.c_lflag);
}

#[test]
fn bulk_remove_discovers_rendezvous_and_exit_union_with_exact_reporting() {
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    DirBuilder::new().mode(0o700).create(&root).unwrap();
    let started = invoked(alias, &["start", "live", "/bin/sh", "-c", "sleep 30"]);
    assert!(started.status.success(), "{started:?}");
    fs::write(root.join("indeterminate"), b"not a socket").unwrap();
    fs::create_dir(root.join("exited.exit")).unwrap();
    stale_socket(&root.join("stale"));
    fs::create_dir(root.join("stale.exit")).unwrap();

    let quiet = invoked(alias, &["rm", "-a", "-q"]);
    assert!(quiet.status.success(), "{quiet:?}");
    assert_eq!(
        quiet.stdout,
        b"skipped indeterminate (indeterminate)\nskipped live (running)\n"
    );
    assert!(!root.join("exited.exit").exists());
    assert!(!root.join("stale").exists());
    fs::create_dir(root.join("exited.exit")).unwrap();
    stale_socket(&root.join("stale"));
    fs::create_dir(root.join("stale.exit")).unwrap();

    let removed = invoked(alias, &["rm", "-a"]);
    assert!(removed.status.success(), "{removed:?}");
    assert_eq!(
        removed.stdout,
        b"removed exited\nskipped indeterminate (indeterminate)\nskipped live (running)\nremoved stale\n2 session(s) removed\n"
    );
    assert!(
        invoked(alias, &["kill", "-f", "-q", "live"])
            .status
            .success()
    );
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(marker).unwrap();
}

#[test]
fn list_discovers_the_same_union_and_rejects_incomparable_ages() {
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    DirBuilder::new().mode(0o700).create(&root).unwrap();
    for (name, age) in [
        ("seconds", 5),
        ("minutes", 120),
        ("hours", 10_800),
        ("days", 345_600),
    ] {
        let path = root.join(name);
        stale_socket(&path);
        set_age(&path, age);
    }
    set_age(&root.join("exited"), 120);
    stale_socket(&root.join("paired"));
    fs::create_dir(root.join("paired.exit")).unwrap();

    let listed = invoked(alias, &["list", "-a"]);
    assert!(listed.status.success(), "{listed:?}");
    let text = String::from_utf8(listed.stdout).unwrap();
    for expected in [
        "seconds",
        "minutes",
        "hours",
        "days",
        "exited",
        "[exited]",
        "since unknown",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in {text:?}");
    }
    assert!(
        text.lines().all(|line| line.contains("since unknown")),
        "{text:?}"
    );
    assert_eq!(text.matches("paired").count(), 1, "{text:?}");
    let ordinary = invoked(alias, &["list"]);
    assert!(
        !String::from_utf8(ordinary.stdout)
            .unwrap()
            .contains("exited")
    );
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(marker).unwrap();
}

#[test]
fn list_status_probe_is_bounded_and_marks_fully_attached_viewers() {
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let started = invoked(alias, &["start", "live", "/bin/sh", "-c", "sleep 30"]);
    assert!(started.status.success(), "{started:?}");
    // attach requires a controlling terminal (§13.1).
    let (mut input, slave) = terminal_pair(24, 80);
    let mut attach = invoked_command(alias)
        .args(["attach", "live"])
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    let mut last = invoked(alias, &["list"]);
    let attached = loop {
        let found = last
            .stdout
            .windows(b"[attached]".len())
            .any(|part| part == b"[attached]");
        if found || Instant::now() >= until {
            break found;
        }
        thread::sleep(Duration::from_millis(20));
        last = invoked(alias, &["list"]);
    };
    input.write_all(&[0x1c]).unwrap();
    drop(input);
    let until = Instant::now() + Duration::from_secs(2);
    while attach.try_wait().unwrap().is_none() && Instant::now() < until {
        thread::sleep(Duration::from_millis(20));
    }
    if attach.try_wait().unwrap().is_none() {
        attach.kill().unwrap();
    }
    let _ = attach.wait();
    assert!(
        invoked(alias, &["kill", "-f", "-q", "live"])
            .status
            .success()
    );
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(marker).unwrap();
    assert!(attached, "last list result: {last:?}");

    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    DirBuilder::new().mode(0o700).create(&root).unwrap();
    let listener = UnixListener::bind(root.join("stalled")).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = [0; 256];
        while stream.read(&mut bytes).unwrap_or(0) != 0 {}
    });
    let before = Instant::now();
    let listed = invoked(alias, &["list"]);
    let elapsed = before.elapsed();
    server.join().unwrap();
    assert!(listed.status.success(), "{listed:?}");
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(marker).unwrap();
    assert!(
        elapsed < Duration::from_secs(1),
        "status probe took {elapsed:?}"
    );
}

#[test]
fn deeply_nested_socket_path_uses_a_directory_relative_address() {
    let dir = temp();
    let nested = dir.join("a".repeat(70)).join("b".repeat(70));
    fs::create_dir_all(&nested).unwrap();
    let socket = nested.join("session");
    assert!(socket.as_os_str().len() > 108);
    let name = socket.to_str().unwrap();
    let started = moor(&["start", name, "/bin/sh", "-c", "sleep 30"]);
    assert!(started.status.success(), "{started:?}");
    assert!(moor(&["kill", "-f", name]).status.success());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn legal_stage_like_session_names_remain_visible_and_untouched() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let stage_like = format!("foo.stage-{}", std::process::id());
    for session in [&stage_like, "foo"] {
        let started = invoked(alias, &["start", session, "/bin/sh", "-c", "sleep 30"]);
        assert!(started.status.success(), "{started:?}");
    }
    let listed = String::from_utf8(invoked(alias, &["list"]).stdout).unwrap();
    assert!(
        listed.lines().any(|line| line.starts_with(&stage_like)),
        "{listed:?}"
    );
    assert!(
        listed.lines().any(|line| line.starts_with("foo ")),
        "{listed:?}"
    );
    for session in [&stage_like, "foo"] {
        assert!(invoked(alias, &["kill", "-f", session]).status.success());
    }
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn supervised_launch_validates_private_record_and_propagates_generation() {
    let dir = temp();
    let socket = dir.join("supervised");
    let events = dir.join("events");
    let descriptor = launch_channel(42);
    let output = Command::new(env!("CARGO_BIN_EXE_moor"))
        .args([
            "start",
            socket.to_str().unwrap(),
            "-T",
            events.to_str().unwrap(),
            "/bin/sh",
            "-c",
            "printf '<%s><%s>\\n' \"$MOOR_GENERATION\" \"$DESK_SESSION_GENERATION\"; sleep 30",
        ])
        .env("DESK_MOOR_LAUNCH_CHANNEL", descriptor.to_string())
        .env("MOOR_GENERATION", "42")
        .env("DESK_SESSION_GENERATION", "42")
        .output()
        .unwrap();
    unsafe { libc::close(descriptor) };
    assert!(output.status.success(), "{output:?}");
    wait_for(&socket, b"<42><42>\r\n");
    let (_, body) = Store::read_only(&events, Kind::Event, 42).unwrap();
    assert!(
        String::from_utf8(body)
            .unwrap()
            .contains("\"generation\":42")
    );
    assert!(
        moor(&["kill", "-f", socket.to_str().unwrap()])
            .status
            .success()
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn inherited_generation_without_launch_channel_is_stripped() {
    let dir = temp();
    let socket = dir.join("unsupervised");
    let output = Command::new(env!("CARGO_BIN_EXE_moor"))
        .args([
            "start",
            socket.to_str().unwrap(),
            "/bin/sh",
            "-c",
            "printf '<%s><%s>\\n' \"$MOOR_GENERATION\" \"$DESK_SESSION_GENERATION\"; sleep 30",
        ])
        .env("MOOR_GENERATION", "91")
        .env("DESK_SESSION_GENERATION", "91")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    wait_for(&socket, b"<><>\r\n");
    assert!(
        moor(&["kill", "-f", socket.to_str().unwrap()])
            .status
            .success()
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn instrumentation_must_ack_from_inside_the_requested_child() {
    let dir = temp();
    let library = instrumentation(&dir, None);
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
    let stages = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension() == Some(std::ffi::OsStr::new("instrument")))
        .collect::<Vec<_>>();
    assert_eq!(stages.len(), 1, "{stages:?}");
    assert_eq!(stages[0].file_stem().unwrap().as_encoded_bytes().len(), 64);
    assert_ne!(stages[0], companion(&socket, ".instrument"));
    assert!(
        moor(&["kill", "-f", socket.to_str().unwrap()])
            .status
            .success()
    );
    assert!(stages[0].exists());
    assert!(moor(&["rm", socket.to_str().unwrap()]).status.success());
    assert!(!stages[0].exists());
    assert!(library.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn child_exit_after_instrument_ack_is_finalized_before_publication() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let library = instrumentation(&dir, Some(23));
    for (mode, expected) in [("start", 1), ("run", 23)] {
        let session = format!("early-{mode}");
        let socket = root.join(&session);
        let events = dir.join(format!("{mode}-events"));
        let output = invoked(
            alias,
            &[
                mode,
                &session,
                "-T",
                events.to_str().unwrap(),
                "-S",
                library.to_str().unwrap(),
                "/bin/true",
            ],
        );
        assert_eq!(output.status.code(), Some(expected), "{output:?}");
        let diagnostic = (mode == "start")
            .then(|| format!("{alias}: child exited before session publication\n"));
        assert_eq!(
            output.stderr,
            diagnostic.as_deref().unwrap_or_default().as_bytes(),
            "{output:?}"
        );
        assert!(
            output.stdout.is_empty() && fs::symlink_metadata(&socket).is_err(),
            "{output:?}"
        );
        let (commit, lifecycle) =
            Store::read_only(&companion(&socket, ".exit"), Kind::Exit, 1).unwrap();
        assert_eq!(commit.index, 2);
        assert!(
            String::from_utf8(lifecycle)
                .unwrap()
                .contains("\"phase\":\"exited\"")
        );
        let (_, events) = Store::read_only(&events, Kind::Event, 1).unwrap();
        assert!(
            String::from_utf8(events)
                .unwrap()
                .contains("\"type\":\"exit\"")
        );
        assert!(Store::read_only(&companion(&socket, ".log"), Kind::Log, 1).is_ok());
        let listed = invoked(alias, &["list", "-a"]);
        let listed = String::from_utf8(listed.stdout).unwrap();
        assert!(
            listed
                .lines()
                .any(|line| line.contains(&session) && line.contains("[exited]")),
            "{listed:?}"
        );
        assert!(invoked(alias, &["rm", &session]).status.success());
    }
    assert!(!root.exists() || fs::read_dir(&root).unwrap().next().is_none());
    let _ = fs::remove_dir(&root);
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
            .env(
                "MOOR_SESSION",
                std::ffi::OsString::from_vec(legacy.to_vec()),
            )
            .env("MOOR_SESSION_V2", v2)
            .output()
            .unwrap()
    };
    let output = run(&legacy, &v2);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"has\\x3Acolon > inner\n");
    let malformed = run(&legacy, "v2:not-base64");
    assert_eq!(malformed.status.code(), Some(1));
    assert_eq!(
        malformed.stderr,
        b"moor: session ancestry v2 is malformed\n"
    );
    let mismatch = run(b"/tmp/different", &v2);
    assert_eq!(mismatch.status.code(), Some(1));
    assert_eq!(
        mismatch.stderr,
        b"moor: session ancestry carriers disagree\n"
    );
}

#[test]
fn observed_exit_is_durable_listed_and_emitted_to_events() {
    let dir = temp();
    let session = dir.file_name().unwrap().to_str().unwrap();
    let socket = std::env::temp_dir()
        .join(format!(".moor-{}", unsafe { libc::geteuid() }))
        .join(session);
    let events = dir.join("event-store");
    let output = moor(&[
        "start",
        session,
        "-T",
        events.to_str().unwrap(),
        "/bin/sh",
        "-c",
        "printf '\x1b[>0q'; sleep .2; exit 7",
    ]);
    assert!(output.status.success(), "{output:?}");
    let until = Instant::now() + Duration::from_secs(5);
    while socket.exists() && Instant::now() < until {
        thread::sleep(Duration::from_millis(20));
    }
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
    assert!(
        listed
            .stdout
            .windows(session.len())
            .any(|w| w == session.as_bytes()),
        "{listed:?}"
    );
    assert!(
        listed
            .stdout
            .windows(b"[exited]".len())
            .any(|w| w == b"[exited]"),
        "{listed:?}"
    );
    assert!(moor(&["rm", session]).status.success());
    assert!(!events.exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn attach_pins_every_liveness_cell_of_the_frozen_state_matrix() {
    // §13.3 freezes three states and three messages. Nothing covered attach on
    // either stale residue shape, which is how it kept emitting the absent
    // diagnostic. Both shapes are one liveness state (§2.3, §4.5), so both must
    // report "is not running".
    let dir = temp();
    let cell = |name: &str, expected: &str| {
        let path = dir.join(name);
        let out = moor(&["attach", path.to_str().unwrap()]);
        assert_eq!(out.status.code(), Some(1), "{name}: {out:?}");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            text.contains(expected),
            "{name}: wanted {expected:?}, got {text:?}"
        );
        assert!(out.stderr.is_empty(), "{name}: {out:?}");
    };

    cell("absent", "does not exist");

    // stale, orphaned rendezvous: a bound socket with no listener behind it.
    stale_socket(&dir.join("stale"));
    cell("stale", "is not running");

    // stale, exit-record-only residue: what `list -a` renders as [exited].
    set_age(&dir.join("exited"), 1);
    assert!(!dir.join("exited").exists());
    cell("exited", "is not running");

    // indeterminate: something is there that we could not identify as ours.
    fs::write(dir.join("indeterminate"), b"not a socket").unwrap();
    cell("indeterminate", "could not be identified");

    fs::remove_dir_all(dir).unwrap();
}
