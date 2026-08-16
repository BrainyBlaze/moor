#![cfg(unix)]

use moor::events::{Cursor, canonical_header};
use moor::runtime::private::{companion, environment_key, lifecycle_running, now};
use moor::store::{Kind, Store};
use std::fs::{self, DirBuilder, File};
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
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

fn true_program() -> &'static str {
    #[cfg(target_os = "macos")]
    return "/usr/bin/true";
    #[cfg(not(target_os = "macos"))]
    "/bin/true"
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
    let size = libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let size = std::ptr::from_ref(&size).cast_mut();
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut::<libc::termios>(),
                size,
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

fn terminal_command(slave: File) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_moor"));
    command.stdin(Stdio::from(slave.try_clone().unwrap()));
    command.stdout(Stdio::from(slave.try_clone().unwrap()));
    command.stderr(Stdio::from(slave));
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

fn wait_child(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let until = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= until {
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_terminal_child(
    child: &mut Child,
    master: &mut File,
    timeout: Duration,
) -> Option<ExitStatus> {
    let until = Instant::now() + timeout;
    let mut bytes = [0; 4096];
    loop {
        loop {
            match master.read(&mut bytes) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= until {
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
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
#include <fcntl.h>
#include <stdio.h>
#ifdef __linux__
#include <sys/inotify.h>
#endif
#include <sys/stat.h>
#include <unistd.h>
__attribute__((constructor)) static void ack(void) {
  char *f=getenv("MOOR_INSTRUMENT_CHANNEL"), *n=getenv("MOOR_INSTRUMENT_NONCE");
  if(!f || !n) return; unsigned char b[36]={'M','O','O','R','I','N','S','3',1};
  int watch=-1;
#ifdef __linux__
  char *observe=getenv("MOOR_EXIT_ON_EVENT_OPEN"); if(observe) {
    watch=inotify_init1(IN_CLOEXEC); if(watch<0 || inotify_add_watch(watch,observe,IN_OPEN)<0) _exit(123); }
#endif
  char *occupy=getenv("MOOR_OCCUPY_RENDEZVOUS"); if(occupy && symlink("missing",occupy)) _exit(124);
  char *s=getenv("MOOR_SUBSTITUTE_EVENT"); if(s) { char from[4096], moved[4096];
    snprintf(from,sizeof(from),"%s/body.1",s); snprintf(moved,sizeof(moved),"%s/displaced",s);
    if(rename(from,moved)) _exit(121); int out=open(from,O_WRONLY|O_CREAT|O_EXCL,0600);
    if(out<0) _exit(122); write(out,"replacement",11); fchmod(out,0600); close(out); }
  b[12]=1; uint32_t p=(uint32_t)getpid(); for(int i=0;i<4;i++) b[16+i]=(p>>(8*i))&255;
  for(int i=0;i<16;i++) { char x[3]={n[i*2],n[i*2+1],0}; b[20+i]=(unsigned char)strtoul(x,0,16); }
  int fd=atoi(f); unsetenv("MOOR_INSTRUMENT_CHANNEL"); unsetenv("MOOR_INSTRUMENT_NONCE"); write(fd,b,36);
#ifdef __linux__
  if(watch>=0) { close(fd); char event[4096]; read(watch,event,sizeof(event)); _exit(23); }
#endif
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
    #[cfg(not(target_os = "macos"))]
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

fn inert_instrumentation(dir: &Path) -> PathBuf {
    // A valid, loadable shared object with no constructor: it loads cleanly and
    // never writes an ACK, so §4.7 fails closed on the acknowledgement deadline.
    // An invalid-byte file is NOT portable — on Darwin the loader/child hangs
    // rather than terminating — so a real empty dylib is used instead.
    let source = dir.join("inert-instrument.c");
    let library = dir.join("inert-instrument.so");
    fs::write(&source, "int moor_inert_marker = 0;\n").unwrap();
    assert!(
        Command::new("cc")
            .args(["-shared", "-fPIC", "-o"])
            .arg(&library)
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    fs::set_permissions(&library, fs::Permissions::from_mode(0o500)).unwrap();
    library
}

fn chatty_exiting_program(dir: &Path) -> PathBuf {
    // Writes to its terminal and exits at once: the shape whose teardown used
    // to deadlock on macOS when the launch failed after the child had already
    // queued output (a session leader exiting with an undrained pty blocks in
    // the kernel until the master hangs up).
    let source = dir.join("chatty-child.c");
    let program = dir.join("chatty-child");
    fs::write(
        &source,
        r#"#include <stdio.h>
int main(void) {
  printf("chatty\n");
  fflush(stdout);
  return 7;
}"#,
    )
    .unwrap();
    assert!(
        Command::new("cc")
            .args(["-o"])
            .arg(&program)
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    program
}

fn instrumentable_program(dir: &Path) -> PathBuf {
    let source = dir.join("instrument-child.c");
    let program = dir.join("instrument-child");
    fs::write(
        &source,
        r#"#include <stdlib.h>
#include <unistd.h>
int main(int argc, char **argv) {
  if(argc > 1) sleep((unsigned int)strtoul(argv[1], 0, 10));
  return 0;
}"#,
    )
    .unwrap();
    assert!(
        Command::new("cc")
            .args(["-o"])
            .arg(&program)
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    program
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn storage_delay_shim(dir: &Path) -> PathBuf {
    let source = dir.join("storage-delay.c");
    let library = dir.join("storage-delay.so");
    fs::write(
        &source,
        r#"#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <fcntl.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
#ifndef SYS_newfstatat
#define SYS_newfstatat SYS_fstatat64
#endif
static void delay(int fd,const char *variable,const char *needle) {
  char link[64], path[PATH_MAX]; snprintf(link,sizeof(link),"/proc/self/fd/%d",fd);
  ssize_t n=readlink(link,path,sizeof(path)-1); if(n<0) return; path[n]=0;
  char *value=getenv(variable); if(value && strstr(path,needle)) {
    char *entered=getenv("MOOR_TEST_FLOCK_ENTERED");
    if(entered && strstr(variable,"FLOCK")) { int out=open(entered,O_WRONLY|O_CREAT|O_EXCL,0600); if(out>=0) close(out); }
    usleep(strtoul(value,0,10)*1000);
  }
}
static void trace_lock(int fd) {
  char *trace=getenv("MOOR_TEST_FLOCK_TRACE"); if(!trace) return;
  char link[64], path[PATH_MAX]; snprintf(link,sizeof(link),"/proc/self/fd/%d",fd);
  ssize_t n=readlink(link,path,sizeof(path)-1); if(n<0) return; path[n]=0;
  int out=open(trace,O_WRONLY|O_CREAT|O_APPEND,0600); if(out<0) return;
  write(out,path,n); write(out,"\n",1); close(out);
}
static int fail_fsync(int fd) {
  char *needle=getenv("MOOR_TEST_FSYNC_FAIL_SUBSTR"); if(!needle) return 0;
  char link[64], path[PATH_MAX]; snprintf(link,sizeof(link),"/proc/self/fd/%d",fd);
  ssize_t n=readlink(link,path,sizeof(path)-1); if(n<0) return 0; path[n]=0;
  return strstr(path,needle)!=0;
}
int fsync(int fd) { if(fail_fsync(fd)) { errno=EIO; return -1; } delay(fd,"MOOR_TEST_FSYNC_DELAY_MS","/body.0"); return syscall(SYS_fsync,fd); }
int flock(int fd,int operation) { trace_lock(fd); delay(fd,"MOOR_TEST_FLOCK_DELAY_MS",".exit/commit.0"); return syscall(SYS_flock,fd,operation); }
int fstatat(int fd,const char *path,struct stat *meta,int flags) {
  int result=syscall(SYS_newfstatat,fd,path,meta,flags); char *name=getenv("MOOR_TEST_DIRECTORY_SWAP_NAME");
  if(!result && name && !strcmp(path,name)) {
    unsetenv("MOOR_TEST_DIRECTORY_SWAP_NAME");
    char *victim=getenv("MOOR_TEST_DIRECTORY_SWAP_VICTIM"), *moved=getenv("MOOR_TEST_DIRECTORY_SWAP_MOVED");
    char *entered=getenv("MOOR_TEST_DIRECTORY_SWAP_ENTERED");
    if(!victim || !moved || !entered || renameat(fd,path,AT_FDCWD,moved) || symlinkat(victim,fd,path)) _exit(120);
    int marker=syscall(SYS_openat,AT_FDCWD,entered,O_WRONLY|O_CREAT|O_EXCL,0600);
    if(marker<0) _exit(121); syscall(SYS_close,marker);
  }
  return result;
}
"#,
    )
    .unwrap();
    let built = Command::new("cc")
        .args(["-shared", "-fPIC", "-o"])
        .arg(&library)
        .arg(&source)
        .status()
        .unwrap();
    assert!(built.success());
    library
}

#[cfg(target_os = "linux")]
fn resource_exhausting_instrumentation(dir: &Path) -> PathBuf {
    let source = dir.join("exhaust-holder.c");
    let library = dir.join("exhaust-holder.so");
    fs::write(
        &source,
        r#"#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <signal.h>
#include <sys/resource.h>
#include <unistd.h>
__attribute__((constructor)) static void ack(void) {
  char *p=getenv("MOOR_GUARD_PID_FILE"), *f=getenv("MOOR_INSTRUMENT_CHANNEL"), *n=getenv("MOOR_INSTRUMENT_NONCE");
  signal(SIGHUP,SIG_IGN); signal(SIGTERM,SIG_IGN);
  if(!p || !f || !n) return; int fd=atoi(f), ready[2]; pipe(ready); pid_t survivor=fork();
  if(survivor==0) { close(fd); close(ready[0]); int out=open(p,O_WRONLY|O_CREAT|O_TRUNC,0600); char text[32];
    int size=snprintf(text,sizeof(text),"%d\n",getpid()); write(out,text,size); close(out); write(ready[1],"x",1); close(ready[1]); for(;;) pause(); }
  close(ready[1]); char confirmed; read(ready[0],&confirmed,1); close(ready[0]);
  unsigned char b[36]={'M','O','O','R','I','N','S','3',1}; b[12]=1; uint32_t pid=(uint32_t)getpid();
  for(int i=0;i<4;i++) b[16+i]=(pid>>(8*i))&255;
  for(int i=0;i<16;i++) { char x[3]={n[i*2],n[i*2+1],0}; b[20+i]=(unsigned char)strtoul(x,0,16); }
  unsetenv("MOOR_INSTRUMENT_CHANNEL"); unsetenv("MOOR_INSTRUMENT_NONCE"); write(fd,b,36); close(fd);
  struct rlimit limit; prlimit(getppid(),RLIMIT_NOFILE,0,&limit); limit.rlim_cur=0; prlimit(getppid(),RLIMIT_NOFILE,&limit,0);
  for(;;) pause();
}"#,
    )
    .unwrap();
    assert!(
        Command::new("cc")
            .args(["-shared", "-fPIC", "-o"])
            .arg(&library)
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    File::options()
        .write(true)
        .open(&library)
        .unwrap()
        .set_len(16 << 20)
        .unwrap();
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
        true_program(),
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
        true_program(),
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
fn creation_never_replaces_a_dangling_rendezvous_symlink() {
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    DirBuilder::new().mode(0o700).create(&root).unwrap();
    let path = root.join("dangling-create");
    symlink(root.join("missing"), &path).unwrap();

    let created = invoked(alias, &["start", "dangling-create", true_program()]);
    let preserved = fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink());
    if created.status.success() {
        let _ = invoked(alias, &["kill", "-f", "dangling-create"]);
    }
    assert_eq!(created.status.code(), Some(1), "{created:?}");
    assert_eq!(
        created.stdout,
        format!("{alias}: session 'dangling-create' could not be identified\n").as_bytes()
    );
    assert!(created.stderr.is_empty(), "{created:?}");
    assert!(preserved, "publication replaced the dangling symlink");

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(marker).unwrap();
}

#[test]
fn publication_failure_confirms_child_death_and_rolls_back_owned_companions() {
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    DirBuilder::new().mode(0o700).create(&root).unwrap();
    let path = root.join("rollback");
    let pidfile = marker.join("pid");
    symlink(root.join("missing"), &path).unwrap();
    let script = format!(
        "trap '' HUP TERM; echo $$ > '{}'; sleep 30",
        pidfile.display()
    );

    let created = invoked(alias, &["start", "rollback", "/bin/sh", "-c", &script]);
    let until = Instant::now() + Duration::from_secs(2);
    while !pidfile.exists() && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    let pid = fs::read_to_string(&pidfile)
        .ok()
        .and_then(|pid| pid.trim().parse::<i32>().ok());
    thread::sleep(Duration::from_millis(100));
    let alive = pid.is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0);
    if let Some(pid) = pid.filter(|_| alive) {
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }

    assert_eq!(created.status.code(), Some(1), "{created:?}");
    assert!(
        !alive,
        "unpublished child {pid:?} survived publication failure"
    );
    assert!(!companion(&path, ".log").exists());
    assert!(!companion(&path, ".exit").exists());
    assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(marker).unwrap();
}

#[test]
fn event_substitution_failure_preserves_the_replacement_during_rollback() {
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let event = companion(&root.join("substituted"), ".events");
    let library = instrumentation(&marker, None);
    let program = instrumentable_program(&marker);
    let output = invoked_command(alias)
        .args([
            "start",
            "substituted",
            "-T",
            event.to_str().unwrap(),
            "-S",
            library.to_str().unwrap(),
            program.to_str().unwrap(),
            "30",
        ])
        .env("MOOR_SUBSTITUTE_EVENT", &event)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(fs::read(event.join("body.1")).unwrap(), b"replacement");
    assert!(event.join("displaced").exists());
    for name in ["body.0", "commit.0", "commit.1"] {
        assert!(!event.join(name).exists(), "unexpected survivor {name}");
    }
    let session = root.join("substituted");
    assert!(!companion(&session, ".log").exists());
    assert!(!companion(&session, ".exit").exists());
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(marker).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn natural_exit_during_publication_failure_is_finalized_and_retained() {
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let session = root.join("exit-race");
    let event = root.join("exit-race-events");
    let library = instrumentation(&marker, None);
    let output = invoked_command(alias)
        .args([
            "start",
            "exit-race",
            "-T",
            event.to_str().unwrap(),
            "-S",
            library.to_str().unwrap(),
            true_program(),
        ])
        .env("MOOR_EXIT_ON_EVENT_OPEN", &event)
        .env("MOOR_OCCUPY_RENDEZVOUS", &session)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        output.stderr,
        format!("{alias}: child exited before session publication\n").as_bytes()
    );
    assert!(
        fs::symlink_metadata(&session).is_ok_and(|meta| meta.file_type().is_symlink()),
        "{output:?}"
    );
    let (_, lifecycle) = Store::read_only(&companion(&session, ".exit"), Kind::Exit, 1).unwrap();
    assert!(
        String::from_utf8(lifecycle)
            .unwrap()
            .contains("\"phase\":\"exited\"")
    );
    let (_, events) = Store::read_only(&event, Kind::Event, 1).unwrap();
    assert!(
        String::from_utf8(events)
            .unwrap()
            .contains("\"type\":\"exit\"")
    );
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(marker).unwrap();
}

#[test]
fn literal_dot_moor_stage_remains_a_legal_bare_session() {
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let _ = fs::remove_dir_all(&root);

    let started = invoked(
        alias,
        &["start", ".moor-stage", "/bin/sh", "-c", "sleep 30"],
    );
    if started.status.success() {
        let _ = invoked(alias, &["kill", "-f", ".moor-stage"]);
        let _ = invoked(alias, &["rm", ".moor-stage"]);
    }
    assert!(started.status.success(), "{started:?}");

    let _ = fs::remove_dir_all(root);
    fs::remove_dir_all(marker).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn path_form_publication_is_staged_on_the_destination_filesystem() {
    use std::os::unix::fs::MetadataExt;

    let shared = Path::new("/dev/shm");
    if !shared.is_dir()
        || fs::metadata(shared).unwrap().dev() == fs::metadata(std::env::temp_dir()).unwrap().dev()
    {
        return;
    }
    let dir = shared.join(format!("moor-e2e-{}-cross-device", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let socket = dir.join("session");
    let name = socket.to_str().unwrap();

    let started = moor(&["start", name, "/bin/sh", "-c", "sleep 30"]);
    if started.status.success() {
        let _ = moor(&["kill", "-f", name]);
        let _ = moor(&["rm", name]);
    }
    assert!(started.status.success(), "{started:?}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failure_before_holder_entry_unlinks_the_staged_rendezvous() {
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let mut command = invoked_command(alias);
    command.args(["start", "no-stage-leak", "/bin/sh", "-c", "sleep 30"]);
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: 6,
                rlim_max: 6,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let residue = fs::read_dir(&root)
        .map(|entries| entries.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(residue.is_empty(), "{residue:?}");
    let _ = fs::remove_dir(&root);
    fs::remove_dir_all(marker).unwrap();
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn preadoption_holder_death_rolls_back_the_prepared_event_store() {
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let shim = storage_delay_shim(&marker);
    for precreated in [false, true] {
        DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .or_else(|error| {
                (error.kind() == std::io::ErrorKind::AlreadyExists)
                    .then_some(())
                    .ok_or(error)
            })
            .unwrap();
        let session = format!("holder-death-{precreated}");
        let event = root.join(format!("events-{precreated}"));
        if precreated {
            DirBuilder::new().mode(0o700).create(&event).unwrap();
        }
        let entered = marker.join(format!("lease-entered-{precreated}"));
        let creator = invoked_command(alias)
            .args([
                "start",
                &session,
                "-T",
                event.to_str().unwrap(),
                true_program(),
            ])
            .env("LD_PRELOAD", &shim)
            .env("MOOR_TEST_FLOCK_DELAY_MS", "10000")
            .env("MOOR_TEST_FLOCK_ENTERED", &entered)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let creator_pid = creator.id();
        let children = format!("/proc/{creator_pid}/task/{creator_pid}/children");
        let until = Instant::now() + Duration::from_secs(2);
        let holder = loop {
            let found = fs::read_to_string(&children)
                .ok()
                .and_then(|pids| pids.split_whitespace().next()?.parse::<i32>().ok());
            if (found.is_some() && entered.exists()) || Instant::now() >= until {
                break found;
            }
            thread::sleep(Duration::from_millis(1));
        }
        .expect("background holder was not forked");
        assert!(event.exists(), "event preparation did not precede fork");
        assert_eq!(unsafe { libc::kill(holder, libc::SIGKILL) }, 0);
        let output = creator.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        if precreated {
            assert!(event.is_dir());
            assert!(fs::read_dir(&event).unwrap().next().is_none());
            fs::remove_dir(&event).unwrap();
        } else {
            assert!(!event.exists());
        }
        assert!(!companion(&root.join(&session), ".exit").exists());
    }
    let _ = fs::remove_dir(&root);
    fs::remove_dir_all(marker).unwrap();
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn preadoption_rollback_preserves_substituted_companions_and_instrument() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let session = root.join("identity-ledger");
    let event = root.join("identity-events");
    let instrument = instrumentation(&dir, None);
    let shim = storage_delay_shim(&dir);
    let entered = dir.join("lease-entered");
    let creator = invoked_command(alias)
        .args([
            "start",
            "identity-ledger",
            "-T",
            event.to_str().unwrap(),
            "-S",
            instrument.to_str().unwrap(),
            true_program(),
        ])
        .env("LD_PRELOAD", &shim)
        .env("MOOR_TEST_FLOCK_DELAY_MS", "10000")
        .env("MOOR_TEST_FLOCK_ENTERED", &entered)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let creator_pid = creator.id();
    let children = format!("/proc/{creator_pid}/task/{creator_pid}/children");
    let deadline = Instant::now() + Duration::from_secs(2);
    let (holder, staged) = loop {
        let holder = fs::read_to_string(&children)
            .ok()
            .and_then(|pids| pids.split_whitespace().next()?.parse::<i32>().ok());
        let staged = fs::read_dir(&root).ok().and_then(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .find(|path| path.extension() == Some(std::ffi::OsStr::new("instrument")))
        });
        if let (Some(holder), Some(staged)) = (holder, staged)
            && entered.exists()
        {
            break (holder, staged);
        }
        assert!(
            Instant::now() < deadline,
            "artifacts were not prepared before fork"
        );
        thread::sleep(Duration::from_millis(1));
    };
    let log = companion(&session, ".log");
    let lifecycle = companion(&session, ".exit");
    let original_log = root.join("original-log");
    let original_lifecycle = root.join("original-lifecycle");
    let original_instrument = root.join("original-instrument");
    fs::rename(&log, &original_log).unwrap();
    fs::rename(&lifecycle, &original_lifecycle).unwrap();
    fs::rename(&staged, &original_instrument).unwrap();
    DirBuilder::new().mode(0o700).create(&log).unwrap();
    DirBuilder::new().mode(0o700).create(&lifecycle).unwrap();
    fs::write(&staged, b"replacement").unwrap();
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o500)).unwrap();

    assert_eq!(unsafe { libc::kill(holder, libc::SIGKILL) }, 0);
    let output = creator.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(fs::read_dir(&log).unwrap().next().is_none());
    assert!(fs::read_dir(&lifecycle).unwrap().next().is_none());
    assert_eq!(fs::read(&staged).unwrap(), b"replacement");
    assert!(fs::read_dir(&original_log).unwrap().next().is_none());
    assert!(fs::read_dir(&original_lifecycle).unwrap().next().is_none());
    assert!(original_instrument.is_file());
    assert!(!event.exists());

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn stalled_initial_fsync_is_cancelled_with_exact_rollback() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let event = root.join("stalled-events");
    let shim = storage_delay_shim(&dir);
    let started = Instant::now();
    let output = invoked_command(alias)
        .args([
            "start",
            "stalled",
            "-T",
            event.to_str().unwrap(),
            true_program(),
        ])
        .env("LD_PRELOAD", &shim)
        .env("MOOR_TEST_FSYNC_DELAY_MS", "5000")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(started.elapsed() < Duration::from_secs(3), "{output:?}");
    assert!(!root.exists() || fs::read_dir(&root).unwrap().next().is_none());
    let _ = fs::remove_dir(&root);
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn three_initial_store_flushes_run_in_parallel_under_one_deadline() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let event = root.join("parallel-events");
    let shim = storage_delay_shim(&dir);
    let started = Instant::now();
    let output = invoked_command(alias)
        .args([
            "start",
            "parallel",
            "-T",
            event.to_str().unwrap(),
            "/bin/sh",
            "-c",
            "sleep 30",
        ])
        .env("LD_PRELOAD", &shim)
        .env("MOOR_TEST_FSYNC_DELAY_MS", "800")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(started.elapsed() < Duration::from_secs(2), "{output:?}");
    assert!(invoked(alias, &["kill", "-f", "parallel"]).status.success());
    assert!(invoked(alias, &["rm", "parallel"]).status.success());
    assert!(!root.exists() || fs::read_dir(&root).unwrap().next().is_none());
    let _ = fs::remove_dir(&root);
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn writer_leases_are_taken_in_lifecycle_event_log_order() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let session = root.join("lease-order");
    let event = root.join("lease-order-events");
    let shim = storage_delay_shim(&dir);
    let trace = dir.join("locks");
    let output = invoked_command(alias)
        .args([
            "start",
            "lease-order",
            "-T",
            event.to_str().unwrap(),
            "/bin/sh",
            "-c",
            "sleep 30",
        ])
        .env("LD_PRELOAD", &shim)
        .env("MOOR_TEST_FLOCK_TRACE", &trace)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let locks = fs::read_to_string(&trace).unwrap();
    let locks = locks.lines().take(3).collect::<Vec<_>>();
    assert_eq!(locks.len(), 3, "{locks:?}");
    assert!(locks[0].ends_with("lease-order.exit/commit.0"), "{locks:?}");
    assert!(
        locks[1].ends_with("lease-order-events/commit.0"),
        "{locks:?}"
    );
    assert!(locks[2].ends_with("lease-order.log/commit.0"), "{locks:?}");
    assert!(
        invoked(alias, &["kill", "-f", "lease-order"])
            .status
            .success()
    );
    assert!(invoked(alias, &["rm", "lease-order"]).status.success());
    assert!(!session.exists());
    assert!(!root.exists() || fs::read_dir(&root).unwrap().next().is_none());
    let _ = fs::remove_dir(&root);
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn post_spawn_setup_failure_reaps_the_requested_process_group() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let pid_file = dir.join("requested.pid");
    let library = resource_exhausting_instrumentation(&dir);
    let output = invoked_command(alias)
        .args([
            "run",
            "guarded",
            "-S",
            library.to_str().unwrap(),
            "/bin/sh",
            "-c",
            "sleep 30",
        ])
        .env("MOOR_GUARD_PID_FILE", &pid_file)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let pid = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    if alive {
        let group = unsafe { libc::getpgid(pid) };
        unsafe {
            libc::kill(if group > 0 { -group } else { pid }, libc::SIGKILL);
        }
    }
    let _ = fs::remove_dir_all(&root);
    fs::remove_dir_all(dir).unwrap();
    assert!(
        !alive,
        "requested process {pid} survived holder setup failure"
    );
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
fn concurrent_restrictive_umask_root_creation_is_atomic() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let executable = dir.join(alias);
    let gate = dir.join("go");
    let _ = fs::remove_dir_all(&root);
    symlink(env!("CARGO_BIN_EXE_moor"), &executable).unwrap();
    let children = (0..32)
        .map(|_| {
            let mut command = Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    "while [ ! -e \"$1\" ]; do :; done; exec \"$2\" list",
                    "sh",
                ])
                .arg(&gate)
                .arg(&executable)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            unsafe {
                command.pre_exec(|| {
                    libc::umask(0o777);
                    Ok(())
                });
            }
            command.spawn().unwrap()
        })
        .collect::<Vec<_>>();
    File::create(gate).unwrap();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    let meta = fs::symlink_metadata(&root).unwrap();
    assert!(meta.file_type().is_dir());
    assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    fs::remove_dir(&root).unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn restrictive_umask_still_creates_an_exact_event_store() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let event = root.join("events");
    let inherited = dir.join("inherited-umask");
    let script = format!("umask > '{}'; sleep 30", inherited.display());
    let mut command = invoked_command(alias);
    command
        .args([
            "start",
            "restrictive-event",
            "-T",
            event.to_str().unwrap(),
            "/bin/sh",
            "-c",
        ])
        .arg(script);
    unsafe {
        command.pre_exec(|| {
            libc::umask(0o777);
            Ok(())
        });
    }
    let output = command.output().unwrap();
    let rendezvous_mode = fs::symlink_metadata(root.join("restrictive-event"))
        .ok()
        .map(|meta| meta.permissions().mode() & 0o777);
    let event_mode = fs::symlink_metadata(&event)
        .ok()
        .map(|meta| meta.permissions().mode() & 0o777);
    let slot_modes = ["body.0", "body.1", "commit.0", "commit.1"].map(|name| {
        fs::symlink_metadata(event.join(name))
            .ok()
            .map(|meta| meta.permissions().mode() & 0o777)
    });
    let until = Instant::now() + Duration::from_secs(2);
    while output.status.success()
        && !fs::symlink_metadata(&inherited).is_ok_and(|meta| meta.len() != 0)
        && Instant::now() < until
    {
        thread::sleep(Duration::from_millis(10));
    }
    let _ = fs::set_permissions(&inherited, fs::Permissions::from_mode(0o600));
    let inherited = fs::read_to_string(inherited).ok();
    if output.status.success() {
        let _ = invoked(alias, &["kill", "-f", "restrictive-event"]);
        let _ = invoked(alias, &["rm", "restrictive-event"]);
    }
    assert!(output.status.success(), "{output:?}");
    assert_eq!(rendezvous_mode, Some(0o600));
    assert_eq!(event_mode, Some(0o700));
    assert_eq!(slot_modes, [Some(0o600); 4]);
    assert_eq!(inherited.as_deref().map(str::trim), Some("0777"));
    let _ = fs::remove_dir_all(root);
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn directory_mode_setting_never_follows_a_substituted_symlink() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    DirBuilder::new().mode(0o700).create(&root).unwrap();
    let event = root.join(format!("{alias}-mode-race"));
    let victim = dir.join("victim");
    let moved = dir.join("created-directory");
    let entered = dir.join("swap-entered");
    fs::write(&victim, b"caller-owned").unwrap();
    fs::set_permissions(&victim, fs::Permissions::from_mode(0o600)).unwrap();
    let shim = storage_delay_shim(&dir);

    let output = invoked_command(alias)
        .args([
            "start",
            "mode-race",
            "-T",
            event.to_str().unwrap(),
            true_program(),
        ])
        .env("LD_PRELOAD", shim)
        .env("MOOR_TEST_DIRECTORY_SWAP_NAME", event.file_name().unwrap())
        .env("MOOR_TEST_DIRECTORY_SWAP_VICTIM", &victim)
        .env("MOOR_TEST_DIRECTORY_SWAP_MOVED", &moved)
        .env("MOOR_TEST_DIRECTORY_SWAP_ENTERED", &entered)
        .output()
        .unwrap();
    let mode = fs::symlink_metadata(&victim).unwrap().permissions().mode() & 0o777;
    let swapped = fs::symlink_metadata(&event).is_ok_and(|meta| meta.file_type().is_symlink());
    let created = moved.is_dir();
    let triggered = entered.is_file();
    let _ = fs::remove_file(&event);
    let _ = fs::remove_dir_all(&root);
    fs::remove_dir_all(&dir).unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        swapped && created && triggered,
        "fault injection did not run"
    );
    assert_eq!(mode, 0o600, "directory mode setting followed the symlink");
}

#[test]
fn event_target_requires_an_absolute_path_inside_the_invoked_root() {
    use std::os::unix::fs::MetadataExt;

    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let outside = marker.join("outside-events");
    DirBuilder::new().mode(0o700).create(&root).unwrap();
    let stale = root.join("relative");
    stale_socket(&stale);
    let stale_id = fs::symlink_metadata(&stale).unwrap().ino();

    let relative = invoked(
        alias,
        &["start", "relative", "-T", "events", true_program()],
    );
    assert_eq!(relative.status.code(), Some(1), "{relative:?}");
    assert_eq!(
        relative.stderr,
        format!("{alias}: event store rejected: events (not-absolute)\n").as_bytes()
    );
    assert_eq!(fs::symlink_metadata(&stale).unwrap().ino(), stale_id);

    let outside_text = outside.to_str().unwrap();
    let escaped = moor::name::render(outside.as_os_str());
    let rejected = invoked(
        alias,
        &["start", "outside", "-T", outside_text, true_program()],
    );
    assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
    assert_eq!(
        rejected.stderr,
        format!("{alias}: event store rejected: {escaped} (outside-root)\n").as_bytes()
    );
    assert!(!outside.exists());
    assert!(!root.join("outside").exists());

    fs::remove_file(stale).unwrap();
    let _ = fs::remove_dir_all(root);
    fs::remove_dir_all(marker).unwrap();
}

#[test]
fn event_target_cannot_escape_the_root_through_a_linked_ancestor() {
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    DirBuilder::new().mode(0o700).create(&root).unwrap();
    let outside = marker.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, root.join("linked-parent")).unwrap();
    let event = root.join("linked-parent/events");
    let output = invoked(
        alias,
        &[
            "start",
            "escaped",
            "-T",
            event.to_str().unwrap(),
            "/bin/sh",
            "-c",
            "sleep 30",
        ],
    );
    if output.status.success() {
        let _ = invoked(alias, &["kill", "-f", "escaped"]);
        let _ = invoked(alias, &["rm", "escaped"]);
    }
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        output.stderr,
        format!(
            "{alias}: event store rejected: {} (link)\n",
            moor::name::render(event.as_os_str())
        )
        .as_bytes()
    );
    assert!(!outside.join("events").exists());
    assert!(!root.join("escaped").exists());
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(marker).unwrap();
}

#[test]
fn event_target_cannot_alias_the_rendezvous_or_derived_companions() {
    use std::os::unix::fs::MetadataExt;

    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    DirBuilder::new().mode(0o700).create(&root).unwrap();
    for (session, suffix) in [
        ("same-rendezvous", ""),
        ("same-log", ".log"),
        ("same-exit", ".exit"),
    ] {
        let rendezvous = root.join(session);
        let event = PathBuf::from(format!("{}{suffix}", rendezvous.display()));
        stale_socket(&rendezvous);
        let stale = fs::symlink_metadata(&rendezvous).unwrap().ino();
        let output = invoked(
            alias,
            &[
                "start",
                session,
                "-T",
                event.to_str().unwrap(),
                "/bin/sh",
                "-c",
                "sleep 30",
            ],
        );
        if output.status.success() {
            let _ = invoked(alias, &["kill", "-f", session]);
            let _ = invoked(alias, &["rm", session]);
        }
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(
            output.stderr,
            format!(
                "{alias}: event store rejected: {} (identity-changed)\n",
                moor::name::render(event.as_os_str())
            )
            .as_bytes()
        );
        assert_eq!(fs::symlink_metadata(&rendezvous).unwrap().ino(), stale);
        assert!(!companion(&rendezvous, ".log").exists());
        assert!(!companion(&rendezvous, ".exit").exists());
        fs::remove_file(&rendezvous).unwrap();
    }
    let _ = fs::remove_dir(&root);
    fs::remove_dir_all(marker).unwrap();
}

#[test]
fn event_manifest_retains_the_exact_absolute_operand_spelling() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    DirBuilder::new().mode(0o700).create(&root).unwrap();
    fs::create_dir(root.join("parent")).unwrap();
    let event = root.join("parent").join("..").join("events");
    let event_text = event.to_str().unwrap();
    let started = invoked(
        alias,
        &[
            "start",
            "exact-event",
            "-T",
            event_text,
            "/bin/sh",
            "-c",
            "sleep 30",
        ],
    );
    assert!(started.status.success(), "{started:?}");

    let session = root.join("exact-event");
    let (_, lifecycle) = Store::read_only(&companion(&session, ".exit"), Kind::Exit, 1).unwrap();
    let expected = STANDARD.encode(event.as_os_str().as_bytes());
    assert!(
        String::from_utf8(lifecycle)
            .unwrap()
            .contains(&format!("\"event_path\":\"{expected}\""))
    );
    assert!(
        invoked(alias, &["kill", "-f", "exact-event"])
            .status
            .success()
    );
    assert!(invoked(alias, &["rm", "exact-event"]).status.success());

    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(marker).unwrap();
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
        true_program(),
    ]);
    assert_eq!(launched.status.code(), Some(1));
    assert!(launched.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&launched.stderr)
            .starts_with("moor: standard-error sink rejected: "),
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

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
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
    use base64::{Engine as _, engine::general_purpose::STANDARD};

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
    let mut attach = terminal_command(slave)
        .args(["attach", name])
        .env("MOOR_SESSION", name)
        .env("MOOR_SESSION_V2", format!("v2:{}", STANDARD.encode(name)))
        .spawn()
        .unwrap();
    terminal_output(&mut master, b"attached", Duration::from_secs(5));
    master.write_all(&[0x1c]).unwrap();
    let status = wait_terminal_child(&mut attach, &mut master, Duration::from_secs(5))
        .expect("attach did not detach");
    assert!(status.success(), "attach: {status:?}");
    assert!(socket.exists());
    assert!(moor(&["kill", "-f", name]).status.success());
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn shipped_binary_refuses_attach_when_the_live_holder_is_an_ancestor() {
    let dir = temp();
    let socket = dir.join("self-attach");
    let alias = dir.join("peer) name");
    symlink(env!("CARGO_BIN_EXE_moor"), &alias).unwrap();
    let name = socket.to_str().unwrap();
    let mut holder = Command::new(env!("CARGO_BIN_EXE_moor"))
        .args([
            "run",
            name,
            "/bin/sh",
            "-c",
            "while [ ! -S \"$1\" ]; do sleep .01; done; exec \"$2\" attach \"$1\"",
            "self-attach",
            name,
            alias.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while holder.try_wait().unwrap().is_none() && Instant::now() < until {
        thread::sleep(Duration::from_millis(20));
    }
    if holder.try_wait().unwrap().is_none() {
        holder.kill().unwrap();
    }
    let output = holder.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let logged = tail(&socket);
    assert!(logged.status.success(), "{logged:?}");
    assert!(
        logged
            .stdout
            .windows(
                b"holder refused request (11): holder is an ancestor of attaching process\r\n"
                    .len()
            )
            .any(|part| part
                == b"holder refused request (11): holder is an ancestor of attaching process\r\n"),
        "{logged:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn same_size_winch_redraw_notifies_the_child_exactly_once() {
    let dir = temp();
    let socket = dir.join("same-size-winch");
    let name = socket.to_str().unwrap();
    let start = moor(&[
        "start",
        name,
        "/bin/sh",
        "-c",
        "trap 'printf \"WINCH\\n\"' WINCH; printf 'READY\\n'; while :; do read line || :; done",
    ]);
    assert!(start.status.success(), "{start:?}");
    wait_for(&socket, b"READY\r\n");

    let (mut master, slave) = terminal_pair(24, 80);
    let mut attach = terminal_command(slave)
        .args(["attach", "-r", "winch", name])
        .spawn()
        .unwrap();
    let _ = terminal_output(&mut master, b"READY\r\n", Duration::from_secs(1));
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let logged = tail(&socket);
        if logged
            .stdout
            .windows(b"WINCH\r\n".len())
            .any(|part| part == b"WINCH\r\n")
            || Instant::now() >= until
        {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    thread::sleep(Duration::from_millis(100));
    let _ = master.write_all(&[0x1c]);
    let mut status = wait_terminal_child(&mut attach, &mut master, Duration::from_secs(2));
    let detached = status.is_some();
    if status.is_none() {
        // A session leader can remain in Darwin terminal teardown while an
        // external slave is open. The command builder consumes that slave;
        // close the remaining master as well before bounded forced cleanup.
        drop(master);
        attach.kill().unwrap();
        status = wait_child(&mut attach, Duration::from_secs(2));
    }
    let logged = tail(&socket);
    let _ = moor(&["kill", "-f", "-q", name]);
    fs::remove_dir_all(dir).unwrap();

    assert!(
        detached && status.is_some_and(|status| status.success()),
        "attach did not detach cleanly: {status:?}"
    );
    assert_eq!(
        logged
            .stdout
            .windows(b"WINCH\r\n".len())
            .filter(|part| *part == b"WINCH\r\n")
            .count(),
        1,
        "{logged:?}"
    );
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
    // Closure §6.3 freezes the headless configuration rather than adopting the
    // kernel default. HUPCL is the observable discriminator on Linux: the
    // kernel's tty_std_termios sets it, the frozen set does not.
    assert!(
        logged.stdout.windows(6).any(|bytes| bytes == b"-hupcl"),
        "kernel-default termios adopted where closure §6.3 freezes the set: {logged:?}"
    );
    assert!(
        logged
            .stdout
            .windows(10)
            .any(|bytes| bytes == b"38400 baud"),
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
        unsafe { libc::tcgetattr(master.as_raw_fd(), &mut before) },
        0
    );
    let mut create = terminal_command(slave)
        .args(["new", name, "/bin/sh", "-c", "stty size; sleep 30"])
        .spawn()
        .unwrap();
    let output = terminal_output(&mut master, b"33 101\r\n", Duration::from_secs(3));
    master.write_all(&[0x1c]).unwrap();
    let mut status = wait_terminal_child(&mut create, &mut master, Duration::from_secs(2));
    if status.is_none() {
        create.kill().unwrap();
        status = wait_terminal_child(&mut create, &mut master, Duration::from_secs(2));
    }
    let mut after: libc::termios = unsafe { std::mem::zeroed() };
    let restored = unsafe { libc::tcgetattr(master.as_raw_fd(), &mut after) };
    let _ = moor(&["kill", "-f", "-q", name]);
    fs::remove_dir_all(dir).unwrap();
    assert_eq!(restored, 0);
    assert!(
        status.is_some_and(|status| status.success()),
        "viewer exited with {status:?}"
    );
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
    drop(slave);
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
    let mut attach_status = wait_child(&mut attach, Duration::from_secs(2));
    if attach_status.is_none() {
        attach.kill().unwrap();
        attach_status = wait_child(&mut attach, Duration::from_secs(2));
    }
    assert!(
        invoked(alias, &["kill", "-f", "-q", "live"])
            .status
            .success()
    );
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(marker).unwrap();
    assert!(
        attach_status.is_some_and(|status| status.success()),
        "attach did not detach cleanly: {attach_status:?}"
    );
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
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let socket = root.join("supervised");
    let events = root.join("events");
    let generation_key = environment_key(std::ffi::OsStr::new(alias), "_GENERATION");
    let script = format!(
        "printf '<%s><%s>\\n' \"${}\" \"$MOOR_SESSION_GENERATION\"; sleep 30",
        generation_key.to_str().unwrap()
    );
    let descriptor = launch_channel(42);
    let output = invoked_command(alias)
        .args([
            "start",
            "supervised",
            "-T",
            events.to_str().unwrap(),
            "/bin/sh",
            "-c",
            &script,
        ])
        .env(
            environment_key(std::ffi::OsStr::new(alias), "_LAUNCH_CHANNEL"),
            descriptor.to_string(),
        )
        .env(&generation_key, "42")
        .env("MOOR_SESSION_GENERATION", "42")
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
        invoked(alias, &["kill", "-f", "supervised"])
            .status
            .success()
    );
    assert!(invoked(alias, &["rm", "supervised"]).status.success());
    fs::remove_dir_all(root).unwrap();
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
            "printf '<%s><%s>\\n' \"$MOOR_GENERATION\" \"$MOOR_SESSION_GENERATION\"; sleep 30",
        ])
        .env("MOOR_GENERATION", "91")
        .env("MOOR_SESSION_GENERATION", "91")
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
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let library = instrumentation(&dir, None);
    let program = instrumentable_program(&dir);
    let socket = dir.join("instrumented");
    let output = invoked(
        alias,
        &[
            "start",
            socket.to_str().unwrap(),
            "-S",
            library.to_str().unwrap(),
            program.to_str().unwrap(),
            "30",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let stages = fs::read_dir(&root)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension() == Some(std::ffi::OsStr::new("instrument")))
        .collect::<Vec<_>>();
    assert_eq!(stages.len(), 1, "{stages:?}");
    assert_eq!(stages[0].file_stem().unwrap().as_encoded_bytes().len(), 64);
    assert_ne!(stages[0], companion(&socket, ".instrument"));
    assert!(
        invoked(alias, &["kill", "-f", socket.to_str().unwrap()])
            .status
            .success()
    );
    assert!(stages[0].exists());
    assert!(
        invoked(alias, &["rm", socket.to_str().unwrap()])
            .status
            .success()
    );
    assert!(!stages[0].exists());
    assert!(library.exists());
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn child_exit_after_instrument_ack_is_finalized_before_publication() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let library = instrumentation(&dir, Some(23));
    let program = instrumentable_program(&dir);
    for (mode, expected) in [("start", 1), ("run", 23)] {
        let session = format!("early-{mode}");
        let socket = root.join(&session);
        let events = root.join(format!("{mode}-events"));
        let output = invoked(
            alias,
            &[
                mode,
                &session,
                "-T",
                events.to_str().unwrap(),
                "-S",
                library.to_str().unwrap(),
                program.to_str().unwrap(),
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
        let removed = invoked(alias, &["rm", &session]);
        assert!(removed.status.success(), "{removed:?}");
    }
    assert!(!root.exists() || fs::read_dir(&root).unwrap().next().is_none());
    let _ = fs::remove_dir(&root);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn current_reads_only_the_v2_ancestry_carrier() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let first = b"/tmp/has:colon";
    let second = b"/tmp/inner";
    let v2 = format!("v2:{}:{}", STANDARD.encode(first), STANDARD.encode(second));
    let run = |legacy: Option<&[u8]>, v2: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_moor"));
        command
            .arg("current")
            .env_remove("MOOR_SESSION")
            .env_remove("MOOR_SESSION_V2");
        if let Some(value) = legacy {
            command.env("MOOR_SESSION", std::ffi::OsString::from_vec(value.to_vec()));
        }
        if let Some(value) = v2 {
            command.env("MOOR_SESSION_V2", value);
        }
        command.output().unwrap()
    };

    // V2 alone is the whole contract: no second carrier is consulted or needed.
    let output = run(None, Some(&v2));
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"has\\x3Acolon > inner\n");

    // A malformed V2 is an error, never a downgrade to guessing.
    let malformed = run(None, Some("v2:not-base64"));
    assert_eq!(malformed.status.code(), Some(1));
    assert_eq!(
        malformed.stderr,
        b"moor: session ancestry v2 is malformed\n"
    );

    // The retired carrier is DEAD, not merely optional: a contradictory
    // MOOR_SESSION changes nothing, because nothing reads it. Under the old
    // dual-carrier contract this exact input was the frozen "carriers
    // disagree" failure; v2 removed the second carrier, so there is nothing
    // left to disagree with.
    let ignored = run(Some(b"/tmp/different"), Some(&v2));
    assert!(ignored.status.success(), "{ignored:?}");
    assert_eq!(ignored.stdout, b"has\\x3Acolon > inner\n");

    // And the retired carrier alone yields NO ancestry — the ambiguous
    // colon-joined value must never be parsed again. Outside any session
    // `current` is frozen as exit 1 with no output, and that is exactly what
    // a process holding only the dead variable now is: outside any session.
    let alone = run(Some(b"/tmp/a:/tmp/b"), None);
    assert_eq!(alone.status.code(), Some(1), "{alone:?}");
    assert!(
        alone.stdout.is_empty() && alone.stderr.is_empty(),
        "{alone:?}"
    );
}

#[test]
fn observed_exit_is_durable_listed_and_emitted_to_events() {
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let session = "observed-exit";
    let socket = root.join(session);
    let events = root.join("event-store");
    let output = invoked(
        alias,
        &[
            "start",
            session,
            "-T",
            events.to_str().unwrap(),
            "/bin/sh",
            "-c",
            "printf '\x1b[>0q'; sleep .2; exit 7",
        ],
    );
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
    let listed = invoked(alias, &["list", "-a"]);
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
    assert!(invoked(alias, &["rm", session]).status.success());
    assert!(!events.exists());
    fs::remove_dir_all(root).unwrap();
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

#[test]
fn rejected_working_directory_names_the_directory_not_the_executable() {
    // Closure §6.2 / OB-32: a failed directory change must be distinguishable
    // from a failed command. Frozen template: `could not enter <path> (<cause>)`,
    // stderr, LF, status 1 — never 127 and never the executable's name.
    let dir = temp();
    let session = dir.join("wd");

    // <cause> = missing
    let gone = dir.join("nonexistent");
    let out = moor(&[
        "run",
        session.to_str().unwrap(),
        "-d",
        gone.to_str().unwrap(),
        true_program(),
    ]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("moor: could not enter {} (missing)\n", gone.display()),
        "{out:?}"
    );

    // <cause> = not-directory
    let file = dir.join("plain");
    fs::write(&file, b"x").unwrap();
    let out = moor(&[
        "run",
        session.to_str().unwrap(),
        "-d",
        file.to_str().unwrap(),
        true_program(),
    ]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("moor: could not enter {} (not-directory)\n", file.display()),
        "{out:?}"
    );

    // <cause> = not-searchable (meaningless when running as root, so guard)
    if unsafe { libc::geteuid() } != 0 {
        let sealed = dir.join("sealed");
        fs::create_dir(&sealed).unwrap();
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000)).unwrap();
        let out = moor(&[
            "run",
            session.to_str().unwrap(),
            "-d",
            sealed.to_str().unwrap(),
            true_program(),
        ]);
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!(
                "moor: could not enter {} (not-searchable)\n",
                sealed.display()
            ),
            "{out:?}"
        );
    }

    // Regression: a valid directory still works and the child's status is kept.
    let out = moor(&[
        "run",
        session.to_str().unwrap(),
        "-d",
        dir.to_str().unwrap(),
        true_program(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn launch_rejections_use_the_frozen_templates_with_causes() {
    // Closure §6.2 template matrix: `session root rejected`,
    // `standard-error sink rejected`, `instrumentation rejected` — each as
    // `<template>: <path> (<cause>)` on stderr, LF, status 1. Ad-hoc texts and
    // raw OS error lines are nonconforming.
    let dir = temp();
    let session = dir.join("templates");

    // standard-error sink: missing
    let absent = dir.join("absent-sink");
    let out = moor(&[
        "start",
        session.to_str().unwrap(),
        "-2",
        absent.to_str().unwrap(),
        true_program(),
    ]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "moor: standard-error sink rejected: {} (missing)\n",
            absent.display()
        ),
        "{out:?}"
    );

    // standard-error sink: wrong-mode (0644 is broader than the protected 0600)
    let broad = dir.join("broad-sink");
    fs::write(&broad, b"").unwrap();
    fs::set_permissions(&broad, fs::Permissions::from_mode(0o644)).unwrap();
    let out = moor(&[
        "start",
        session.to_str().unwrap(),
        "-2",
        broad.to_str().unwrap(),
        true_program(),
    ]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "moor: standard-error sink rejected: {} (wrong-mode)\n",
            broad.display()
        ),
        "{out:?}"
    );

    // standard-error sink: link (a symlink must not be followed)
    let target = dir.join("real-sink");
    fs::write(&target, b"").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let linked = dir.join("linked-sink");
    std::os::unix::fs::symlink(&target, &linked).unwrap();
    let out = moor(&[
        "start",
        session.to_str().unwrap(),
        "-2",
        linked.to_str().unwrap(),
        true_program(),
    ]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "moor: standard-error sink rejected: {} (link)\n",
            linked.display()
        ),
        "{out:?}"
    );

    // standard-error sink: wrong-type (a directory cannot be a sink; the
    // append-open fails with EISDIR before fstat, which must not surface as
    // io-error)
    let sink_dir = dir.join("sink-dir");
    fs::create_dir(&sink_dir).unwrap();
    let out = moor(&[
        "start",
        session.to_str().unwrap(),
        "-2",
        sink_dir.to_str().unwrap(),
        true_program(),
    ]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "moor: standard-error sink rejected: {} (wrong-type)\n",
            sink_dir.display()
        ),
        "{out:?}"
    );

    // instrumentation: missing
    let gone = dir.join("absent-object.so");
    let out = moor(&[
        "run",
        session.to_str().unwrap(),
        "-S",
        gone.to_str().unwrap(),
        true_program(),
    ]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "moor: instrumentation rejected: {} (missing)\n",
            gone.display()
        ),
        "{out:?}"
    );

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rejected_session_root_uses_the_frozen_template() {
    // A root path occupied by a plain file is the reproducible wrong-type
    // rejection: the frozen template names the root, not an ad-hoc phrase.
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    fs::write(&root, b"occupied").unwrap();
    let out = invoked(alias, &["start", "root-check", true_program()]);
    fs::remove_file(&root).unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "{alias}: session root rejected: {} (wrong-type)\n",
            root.display()
        ),
        "{out:?}"
    );
    fs::remove_dir_all(marker).unwrap();
}

#[test]
fn loader_encoding_refusal_applies_to_the_staged_path() {
    // Issue #17 item 8 / closure §6.5: the value placed in the loader variable
    // is the staged path, so a per-user root whose spelling is loader-hostile
    // (here: whitespace via TMPDIR) must be refused even though the caller's
    // -S operand is loader-clean.
    let dir = temp();
    let object = dir.join("object.so");
    fs::write(&object, b"not a real library").unwrap();
    fs::set_permissions(&object, fs::Permissions::from_mode(0o644)).unwrap();
    // §4.7: the Darwin delimiter is the colon; elsewhere whitespace/dollar.
    #[cfg(target_os = "macos")]
    let hostile = dir.join("host:ile");
    #[cfg(not(target_os = "macos"))]
    let hostile = dir.join("host ile");
    fs::create_dir(&hostile).unwrap();
    let session = dir.join("stage-check");
    let out = Command::new(env!("CARGO_BIN_EXE_moor"))
        .env("TMPDIR", &hostile)
        .args([
            "run",
            session.to_str().unwrap(),
            "-S",
            object.to_str().unwrap(),
            true_program(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    // Exact equality: the row must display the caller's operand, not leak the
    // generated stage path whose spelling caused the refusal.
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "moor: instrumentation rejected: {} (io-error)\n",
            object.display()
        ),
        "{out:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rejected_session_root_reports_a_symlink_as_link() {
    // §2.2 and the closed cause enum distinguish a symlinked root (`link`)
    // from a non-directory occupant (`wrong-type`).
    let marker = temp();
    let alias = marker.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let target = marker.join("elsewhere");
    fs::create_dir(&target).unwrap();
    std::os::unix::fs::symlink(&target, &root).unwrap();
    let out = invoked(alias, &["start", "root-link", true_program()]);
    fs::remove_file(&root).unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "{alias}: session root rejected: {} (link)\n",
            root.display()
        ),
        "{out:?}"
    );
    fs::remove_dir_all(marker).unwrap();
}

#[test]
fn execute_only_working_directory_is_accepted() {
    // chdir needs only search permission: an execute-only directory is valid
    // even though it cannot be read, so validation must not demand O_RDONLY.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let dir = temp();
    let sealed = dir.join("exec-only");
    fs::create_dir(&sealed).unwrap();
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o111)).unwrap();
    let out = moor(&[
        "run",
        dir.join("xo").to_str().unwrap(),
        "-d",
        sealed.to_str().unwrap(),
        true_program(),
    ]);
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unacknowledged_instrumentation_uses_the_frozen_row_with_the_operand() {
    // Review reproduction: a protected regular file that is not a loadable
    // object passes every static check, the loader skips it, no ACK arrives,
    // and §4.7 fails closed. The frozen row must name the caller's operand
    // with cause load-unacknowledged — not leak a raw record diagnostic.
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let object = inert_instrumentation(&dir);
    let program = instrumentable_program(&dir);
    let out = invoked(
        alias,
        &[
            "run",
            "ack",
            "-S",
            object.to_str().unwrap(),
            program.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "{alias}: instrumentation rejected: {} (load-unacknowledged)\n",
            object.display()
        ),
        "{out:?}"
    );
    let _ = fs::remove_dir_all(&root);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unacknowledged_instrumentation_of_an_exited_chatty_child_fails_closed_promptly() {
    // Regression: the child loads an inert object (no ACK), writes to its
    // terminal, and exits before the acknowledgement deadline. The launch must
    // still fail closed with the frozen row — promptly. Before the fix the
    // holder's teardown waited on the exited child while the child sat in the
    // kernel waiting for its queued output to drain, so `start` only returned
    // after the launcher's own ten-second deadline with a generic diagnostic.
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let object = inert_instrumentation(&dir);
    let program = chatty_exiting_program(&dir);
    let socket = dir.join("chatty");
    let started = Instant::now();
    let out = invoked(
        alias,
        &[
            "start",
            socket.to_str().unwrap(),
            "-S",
            object.to_str().unwrap(),
            program.to_str().unwrap(),
        ],
    );
    let elapsed = started.elapsed();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "{alias}: instrumentation rejected: {} (load-unacknowledged)\n",
            object.display()
        ),
        "{out:?}"
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "failed launch took {elapsed:?}: teardown waited on its own child"
    );
    assert!(!socket.exists(), "a failed launch published a rendezvous");
    let _ = fs::remove_dir_all(&root);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn attach_without_controlling_terminal_refuses_and_leaves_the_session_live() {
    // Closure §6.4: attach validates the controlling terminal before touching
    // anything; the refusal is `no controlling terminal` on stderr, status 1,
    // and the live session must remain undisturbed.
    let dir = temp();
    let session = dir.join("keepalive");
    let started = moor(&[
        "start",
        session.to_str().unwrap(),
        "/bin/sh",
        "-c",
        "sleep 30",
    ]);
    assert!(started.status.success(), "{started:?}");
    let out = unsafe {
        use std::os::unix::process::CommandExt;
        Command::new(env!("CARGO_BIN_EXE_moor"))
            .args(["attach", session.to_str().unwrap()])
            .stdin(Stdio::null())
            .pre_exec(|| {
                // A fresh session guarantees the child has no controlling
                // terminal regardless of how the harness was launched.
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
            .output()
            .unwrap()
    };
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "moor: no controlling terminal\n",
        "{out:?}"
    );
    // The session survived the refusal: rm against a live session is refused
    // with `is running`, which is simultaneously the liveness probe and the
    // proof the refusal did not disturb it.
    let probe = moor(&["rm", session.to_str().unwrap()]);
    assert_eq!(probe.status.code(), Some(1), "{probe:?}");
    assert_eq!(
        String::from_utf8_lossy(&probe.stdout),
        format!("moor: session '{}' is running\n", session.display()),
        "the session must still be live after the refused attach: {probe:?}"
    );
    let _ = moor(&["kill", "-f", "-q", session.to_str().unwrap()]);
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn failed_event_initializer_names_the_caller_operand() {
    // Review blocker: an event-store initializer failure inside the forked
    // worker must surface as the frozen `event store rejected: <operand>
    // (io-error)` row, not the generic store diagnostic — the event target is
    // the caller-supplied one. The shim fails fsync only for paths under the
    // event directory, so lifecycle/log initialize normally.
    let dir = temp();
    let alias = dir.file_name().unwrap().to_str().unwrap();
    let root = isolated_root(alias);
    let event = root.join("failing-events");
    let shim = storage_delay_shim(&dir);
    let output = invoked_command(alias)
        .args([
            "start",
            "event-fail",
            "-T",
            event.to_str().unwrap(),
            true_program(),
        ])
        .env("LD_PRELOAD", &shim)
        .env("MOOR_TEST_FSYNC_FAIL_SUBSTR", "failing-events")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "{alias}: event store rejected: {} (io-error)\n",
            event.display()
        ),
        "{output:?}"
    );
    let _ = fs::remove_dir_all(&root);
    fs::remove_dir_all(dir).unwrap();
}
