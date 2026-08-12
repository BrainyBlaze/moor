#![cfg_attr(not(windows), allow(missing_docs))]
#![cfg(windows)]

//! End-to-end Windows process creation tests.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::mem::size_of_val;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows_spawn::{
    AsPseudoConsole, Command, CreationFlags, DropPolicy, Job, Mitigation, MitigationPolicy,
    ParentProcess, SpawnOptions, Stdio,
};
use windows_sys::Win32::Foundation::{
    DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, GetFileType, ReadFile, WriteFile, BY_HANDLE_FILE_INFORMATION,
    FILE_TYPE_UNKNOWN,
};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, GetConsoleCP, GetStdHandle, COORD, HPCON,
    STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessHandleCount, GetProcessId, GetProcessMitigationPolicy,
    GetThreadId, ProcessExtensionPointDisablePolicy, SuspendThread, TerminateProcess,
    WaitForSingleObject,
};

const PCON_ISOLATION_HELPER: &str = "WINDOWS_SPAWN_PCON_ISOLATION_HELPER";
const PCON_STDIO_PROBE: &str = "WINDOWS_SPAWN_PCON_STDIO_PROBE";
const PCON_STDIN_MARKER: &[u8] = b"windows-spawn-pcon-stdin";
const PCON_STDOUT_MARKER: &[u8] = b"windows-spawn-pcon-stdout";
const PCON_STDERR_MARKER: &[u8] = b"windows-spawn-pcon-stderr";

fn cmd(script: &str) -> Command {
    let mut command = Command::new("cmd.exe");
    command.args(["/D", "/S", "/C"]).raw_arg(script);
    command
}

fn temporary_path(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time must be after the epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "windows-spawn-{label}-{}-{nonce}",
        std::process::id()
    ))
}

fn local_duplicate<T: AsHandle>(source: &T, inheritable: bool) -> io::Result<OwnedHandle> {
    let mut duplicate = std::ptr::null_mut();
    // SAFETY: both process pseudo-handles are valid, the source remains
    // borrowed for the call, and `duplicate` is writable output storage.
    let success = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            source.as_handle().as_raw_handle(),
            GetCurrentProcess(),
            &mut duplicate,
            0,
            i32::from(inheritable),
            DUPLICATE_SAME_ACCESS,
        )
    };
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: DuplicateHandle returned a new, uniquely owned handle.
        Ok(unsafe { OwnedHandle::from_raw_handle(duplicate as RawHandle) })
    }
}

fn inheritable_duplicate<T: AsHandle>(source: &T) -> io::Result<OwnedHandle> {
    local_duplicate(source, true)
}

fn file_identity(handle: HANDLE) -> io::Result<(u32, u64)> {
    // SAFETY: the all-zero value is a valid output buffer initialization.
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    // SAFETY: the caller supplies the handle under test and the output buffer
    // remains writable for the duration of the call.
    if unsafe { GetFileInformationByHandle(handle, &mut information) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        let index =
            (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
        Ok((information.dwVolumeSerialNumber, index))
    }
}

struct ProcessExitGuard {
    process: OwnedHandle,
    armed: bool,
}

impl ProcessExitGuard {
    fn new(process: OwnedHandle) -> Self {
        Self {
            process,
            armed: true,
        }
    }

    fn wait(&mut self, timeout: Duration) -> io::Result<bool> {
        let timeout = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: the duplicated process handle remains valid for the wait.
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), timeout) } {
            WAIT_OBJECT_0 => {
                self.armed = false;
                Ok(true)
            }
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl Drop for ProcessExitGuard {
    fn drop(&mut self) {
        if self.armed {
            // Bypass the crate path so its mutants cannot disable cleanup.
            // SAFETY: the duplicate has the source process handle's access.
            let _ = unsafe { TerminateProcess(self.process.as_raw_handle(), 1) };
            // SAFETY: the same owned process handle remains valid here.
            let _ = unsafe { WaitForSingleObject(self.process.as_raw_handle(), 5_000) };
        }
    }
}

fn wait_bounded(child: &mut windows_spawn::Child) -> io::Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn args_environment_cwd_and_wait_cache_work() -> io::Result<()> {
    let directory = temporary_path("cwd");
    fs::create_dir(&directory)?;

    let mut command = cmd("echo [%WINDOWS_SPAWN_VALUE%]&cd&exit /b 37");
    command
        .env("WINDOWS_SPAWN_VALUE", "hello world")
        .current_dir(&directory)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let first = child.wait()?;
    let second = child.wait()?;
    assert_eq!(first, second);
    assert_eq!(first.code(), Some(37));

    fs::remove_dir(directory)?;
    Ok(())
}

#[test]
fn output_drains_stdout_and_stderr_beyond_pipe_capacity() -> io::Result<()> {
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "$x='x'*131072; [Console]::Out.Write($x); [Console]::Error.Write($x); exit 23",
    ]);
    let output = command.output()?;
    assert_eq!(output.status.code(), Some(23));
    assert_eq!(output.stdout.len(), 131_072);
    assert_eq!(output.stderr.len(), 131_072);
    Ok(())
}

#[test]
fn null_and_owned_file_stdio_work() -> io::Result<()> {
    let path = temporary_path("stdout");
    let file = File::create(&path)?;
    let mut command = cmd("echo file-output");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::null());
    assert!(command.status()?.success());
    let contents = fs::read_to_string(&path)?;
    assert!(contents.contains("file-output"));
    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn suspended_child_resumes_once_into_normal_state() -> io::Result<()> {
    let mut command = cmd("exit /b 19");
    let suspended = command.spawn_suspended()?;
    let pid = suspended.id();
    let process_handle = suspended.as_handle().as_raw_handle();
    let thread_handle = suspended.primary_thread_handle().as_raw_handle();
    // SAFETY: SuspendedChild owns both handles for these non-mutating queries.
    assert_eq!(unsafe { GetProcessId(process_handle) }, pid);
    // SAFETY: the primary thread handle is valid until resume consumes it.
    assert_ne!(unsafe { GetThreadId(thread_handle) }, 0);

    let mut child = suspended.resume()?;
    assert_eq!(child.id(), pid);
    assert_eq!(child.as_handle().as_raw_handle(), process_handle);
    let status = (0..200).find_map(|_| {
        let status = child.try_wait().expect("resumed child must be queryable");
        if status.is_none() {
            thread::sleep(Duration::from_millis(10));
        }
        status
    });
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert_eq!(status.and_then(|status| status.code()), Some(19));
    Ok(())
}

#[test]
fn resume_rejects_an_externally_changed_suspend_count() -> io::Result<()> {
    let mut command = cmd("ping -n 10 127.0.0.1 >nul");
    let suspended = command.spawn_suspended()?;
    let mut process = ProcessExitGuard::new(local_duplicate(&suspended, false)?);
    // SAFETY: the primary thread handle remains owned by SuspendedChild and
    // has THREAD_SUSPEND_RESUME access from CreateProcessW.
    let previous_suspend_count =
        unsafe { SuspendThread(suspended.primary_thread_handle().as_raw_handle()) };
    assert_eq!(previous_suspend_count, 1);

    assert_eq!(
        suspended.resume().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(process.wait(Duration::from_secs(5))?);
    Ok(())
}

#[test]
fn inherited_and_null_standard_handles_are_usable() -> io::Result<()> {
    let test_binary = std::env::current_exe()?;
    let mut inherited = Command::new(&test_binary);
    inherited
        .args(["--exact", "native_standard_handle_probe", "--nocapture"])
        .env("WINDOWS_SPAWN_STDIO_PROBE", "inherit");
    inherited.stdin(Stdio::null());
    assert!(inherited.status()?.success());

    let mut null = Command::new(test_binary);
    null.args(["--exact", "native_standard_handle_probe", "--nocapture"])
        .env("WINDOWS_SPAWN_STDIO_PROBE", "null");
    null.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    assert!(null.status()?.success());
    Ok(())
}

#[test]
fn native_standard_handle_probe() {
    let Ok(mode) = std::env::var("WINDOWS_SPAWN_STDIO_PROBE") else {
        return;
    };
    // SAFETY: this only inspects the process-owned standard input slot.
    let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    // SAFETY: this only inspects the process-owned standard output slot.
    let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    // SAFETY: this only inspects the process-owned standard error slot.
    let error = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    let valid = |handle: HANDLE| {
        !handle.is_null()
            && handle != INVALID_HANDLE_VALUE
            // SAFETY: a non-null standard-handle value is valid for this
            // non-owning query; FILE_TYPE_UNKNOWN detects invalid values.
            && unsafe { GetFileType(handle) } != FILE_TYPE_UNKNOWN
    };
    assert!(valid(output));
    assert!(valid(error));

    if mode == "null" {
        assert!(valid(input));
        let mut byte = [0_u8; 1];
        let mut read = 0_u32;
        // SAFETY: buffers and byte-count outputs are valid for synchronous I/O.
        let read_succeeded =
            unsafe { ReadFile(input, byte.as_mut_ptr(), 1, &mut read, std::ptr::null_mut()) };
        assert_ne!(read_succeeded, 0);
        assert_eq!(read, 0);
        let mut written = 0_u32;
        // SAFETY: the one-byte buffer and byte-count output remain valid.
        let wrote_stdout =
            unsafe { WriteFile(output, byte.as_ptr(), 1, &mut written, std::ptr::null_mut()) };
        assert_ne!(wrote_stdout, 0);
        assert_eq!(written, 1);
        // SAFETY: the one-byte buffer and byte-count output remain valid.
        let wrote_stderr =
            unsafe { WriteFile(error, byte.as_ptr(), 1, &mut written, std::ptr::null_mut()) };
        assert_ne!(wrote_stderr, 0);
        assert_eq!(written, 1);
    }
}

#[test]
fn direct_child_pipe_reads_reach_eof() -> io::Result<()> {
    let mut command = cmd("echo stdout-line&echo stderr-line 1>&2");
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    assert!(child.wait()?.success());

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut buffer = [0_u8; 128];
    let stdout_length = stdout.read(&mut buffer)?;
    assert!(String::from_utf8_lossy(&buffer[..stdout_length]).contains("stdout-line"));
    assert_eq!(stdout.read(&mut buffer)?, 0);

    let stderr_length = stderr.read(&mut buffer)?;
    assert!(String::from_utf8_lossy(&buffer[..stderr_length]).contains("stderr-line"));
    assert_eq!(stderr.read(&mut buffer)?, 0);
    Ok(())
}

#[test]
fn explicit_job_attachment_and_kill_tree_output_complete() -> io::Result<()> {
    let outer_job = Job::create()?;
    let inner_job = Job::create()?;
    let mut ordinary = cmd("exit /b 0");
    let options = SpawnOptions::new().job(&outer_job).job(&inner_job);
    assert!(ordinary.status_with(options)?.success());

    // The background grandchild inherits stdout. Without terminating the
    // private Job after root exit, wait_with_output would never observe EOF.
    // Keep the natural grandchild lifetime well beyond the assertion budget.
    // This preserves the EOF proof without making a three-second wall-clock
    // deadline flaky when the full integration suite creates processes in
    // parallel on a loaded CI host.
    let mut tree = cmd("start \"\" /b cmd.exe /D /C \"ping -n 20 127.0.0.1 >nul\" & echo root");
    let started = Instant::now();
    let output = tree.output_with(SpawnOptions::new().drop_policy(DropPolicy::KillTree))?;
    let elapsed = started.elapsed();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("root"));
    assert!(
        elapsed < Duration::from_secs(10),
        "root-bounded output waited {elapsed:?} for the background grandchild"
    );
    Ok(())
}

#[test]
fn requested_mitigation_is_visible_on_the_real_process() -> io::Result<()> {
    let mut command = cmd("ping -n 4 127.0.0.1 >nul");
    let mitigation = MitigationPolicy::new().disable_extension_points(Mitigation::AlwaysOn);
    let mut child = command.spawn_with(SpawnOptions::new().mitigation(mitigation))?;
    let mut flags = 0_u32;
    // SAFETY: the child owns a valid process handle, and `flags` is writable
    // storage of the exact DWORD size required by this policy query.
    let queried = unsafe {
        GetProcessMitigationPolicy(
            child.as_handle().as_raw_handle(),
            ProcessExtensionPointDisablePolicy,
            std::ptr::addr_of_mut!(flags).cast(),
            size_of_val(&flags),
        )
    };
    child.kill()?;
    assert!(!child.wait()?.success());
    if queried == 0 {
        return Err(io::Error::last_os_error());
    }
    assert_eq!(flags & 1, 1);
    Ok(())
}

#[test]
fn failed_transactions_do_not_leak_handles() -> io::Result<()> {
    const PROBE: &str = "WINDOWS_SPAWN_HANDLE_LEAK_PROBE";
    fn handle_count() -> io::Result<u32> {
        let mut count = 0;
        // SAFETY: GetCurrentProcess returns a valid pseudo-handle and `count`
        // points to writable DWORD storage for the duration of the call.
        let success = unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) };
        if success == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(count)
        }
    }
    fn fail_after_acquisition(missing_directory: &PathBuf) {
        let mut command = cmd("exit /b 0");
        command
            .current_dir(missing_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let error = command
            .spawn_with(SpawnOptions::new().drop_policy(DropPolicy::KillTree))
            .expect_err("a missing current directory must fail after resources are acquired");
        assert_ne!(error.kind(), io::ErrorKind::InvalidInput);
    }

    if std::env::var_os(PROBE).is_none() {
        let status = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "failed_transactions_do_not_leak_handles",
                "--test-threads=1",
            ])
            .env(PROBE, "1")
            .status()?;
        assert!(status.success(), "isolated handle-leak probe failed");
        return Ok(());
    }

    let missing_directory = temporary_path("missing-cwd");
    fail_after_acquisition(&missing_directory);
    let before = handle_count()?;
    for _ in 0..64 {
        fail_after_acquisition(&missing_directory);
    }
    let after = handle_count()?;
    assert!(
        after <= before.saturating_add(8),
        "handle count grew from {before} to {after}"
    );
    Ok(())
}

#[test]
fn validation_happens_before_process_creation() {
    let mut batch = Command::new("script.CMD");
    assert_eq!(
        batch.spawn().unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn argument_and_environment_handles_are_lowered_after_duplication() -> io::Result<()> {
    let argument_path = temporary_path("argument-handle");
    let environment_path = temporary_path("environment-handle");
    let argument_file = File::create(&argument_path)?;
    let environment_file = File::create(&environment_path)?;

    let script = concat!(
        "& { param($argumentHandle)",
        "$a=[IntPtr]::new([Int64]$argumentHandle);",
        "$as=[Microsoft.Win32.SafeHandles.SafeFileHandle]::new($a,$false);",
        "$af=[System.IO.FileStream]::new($as,[System.IO.FileAccess]::Write);",
        "$ab=[Text.Encoding]::UTF8.GetBytes('argument');$af.Write($ab,0,$ab.Length);$af.Flush();",
        "$e=[IntPtr]::new([Int64]$env:WINDOWS_SPAWN_HANDLE);",
        "$es=[Microsoft.Win32.SafeHandles.SafeFileHandle]::new($e,$false);",
        "$ef=[System.IO.FileStream]::new($es,[System.IO.FileAccess]::Write);",
        "$eb=[Text.Encoding]::UTF8.GetBytes('environment');$ef.Write($eb,0,$eb.Length);$ef.Flush()",
        "}"
    );
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]);
    command.arg_handle(&argument_file)?;
    command.env_handle("WINDOWS_SPAWN_HANDLE", &environment_file)?;
    drop(argument_file);
    drop(environment_file);
    assert!(command.status()?.success());
    assert!(command.status()?.success());
    assert_eq!(
        fs::read_to_string(&argument_path)?,
        "argumentargument",
        "the reusable Command must re-transfer its private handle"
    );
    assert_eq!(
        fs::read_to_string(&environment_path)?,
        "environmentenvironment",
        "the reusable Command must re-transfer its private handle"
    );
    fs::remove_file(argument_path)?;
    fs::remove_file(environment_path)?;
    Ok(())
}

#[test]
fn alternate_parent_receives_remote_stdio_duplicates() -> io::Result<()> {
    let mut host = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "ping -n 10 127.0.0.1 >nul"])
        .spawn()?;
    let parent = ParentProcess::open(host.id())?;

    let mut command = cmd("echo alternate-parent");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let result = command.output_with(SpawnOptions::new().parent_process(&parent));
    let _ = host.kill();
    let _ = host.wait();

    let output = result?;
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("alternate-parent"));
    Ok(())
}

#[test]
fn alternate_parent_receives_remote_argument_and_environment_handles() -> io::Result<()> {
    let argument_path = temporary_path("remote-argument-handle");
    let environment_path = temporary_path("remote-environment-handle");
    let argument_file = File::create(&argument_path)?;
    let environment_file = File::create(&environment_path)?;
    let mut host = std::process::Command::new("cmd.exe")
        .args(["/D", "/C", "ping -n 10 127.0.0.1 >nul"])
        .spawn()?;
    let parent = ParentProcess::open(host.id())?;

    let script = concat!(
        "& { param($argumentHandle)",
        "$a=[IntPtr]::new([Int64]$argumentHandle);",
        "$as=[Microsoft.Win32.SafeHandles.SafeFileHandle]::new($a,$false);",
        "$af=[System.IO.FileStream]::new($as,[System.IO.FileAccess]::Write);",
        "$ab=[Text.Encoding]::UTF8.GetBytes('argument');$af.Write($ab,0,$ab.Length);$af.Flush();",
        "$e=[IntPtr]::new([Int64]$env:WINDOWS_SPAWN_HANDLE);",
        "$es=[Microsoft.Win32.SafeHandles.SafeFileHandle]::new($e,$false);",
        "$ef=[System.IO.FileStream]::new($es,[System.IO.FileAccess]::Write);",
        "$eb=[Text.Encoding]::UTF8.GetBytes('environment');$ef.Write($eb,0,$eb.Length);$ef.Flush()",
        "}"
    );
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]);
    command
        .arg_handle(&argument_file)?
        .env_handle("WINDOWS_SPAWN_HANDLE", &environment_file)?
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    drop((argument_file, environment_file));

    let result = command.status_with(SpawnOptions::new().parent_process(&parent));
    let _ = host.kill();
    let _ = host.wait();

    assert!(result?.success());
    assert_eq!(fs::read_to_string(&argument_path)?, "argument");
    assert_eq!(fs::read_to_string(&environment_path)?, "environment");
    fs::remove_file(argument_path)?;
    fs::remove_file(environment_path)?;
    Ok(())
}

#[test]
fn child_pipes_try_wait_and_cached_lifecycle_work() -> io::Result<()> {
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "$line=[Console]::In.ReadLine(); [Console]::Out.Write('out:'+ $line); [Console]::Error.Write('err')",
    ]);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    assert!(child.id() > 1);
    assert!(child.try_wait()?.is_none());
    let _ = child.as_handle();
    let mut stdin = child.stdin.take().expect("piped stdin");
    let _ = stdin.as_handle();
    stdin.write_all(b"hello\n")?;
    stdin.flush()?;
    drop(stdin);
    let _ = child.stdout.as_ref().expect("piped stdout").as_handle();
    let _ = child.stderr.as_ref().expect("piped stderr").as_handle();
    let output = child.wait_with_output()?;
    assert!(output.status.success());
    assert_eq!(output.stdout, b"out:hello");
    assert_eq!(output.stderr, b"err");

    let mut exited = cmd("exit /b 0").spawn()?;
    assert!(exited.wait()?.success());
    assert!(exited.try_wait()?.expect("cached exit").success());
    exited.kill()?;

    let mut polled = cmd("exit /b 42").spawn()?;
    let status = (0..100).find_map(|_| {
        let status = polled.try_wait().expect("try_wait must query the child");
        if status.is_none() {
            thread::sleep(Duration::from_millis(10));
        }
        status
    });
    assert_eq!(status.and_then(|status| status.code()), Some(42));

    let mut silent = cmd("exit /b 0");
    silent.stdout(Stdio::null()).stderr(Stdio::null());
    let output = silent.spawn()?.wait_with_output()?;
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    Ok(())
}

struct InvalidPseudoConsole;

// SAFETY: this implementation is used only in validation tests that reject
// the request before the numeric value reaches the Win32 transaction.
unsafe impl AsPseudoConsole for InvalidPseudoConsole {
    fn raw_pseudoconsole(&self) -> isize {
        1
    }
}

struct TestPseudoConsole {
    value: HPCON,
    input_writer: Option<OwnedHandle>,
    output_reader: Option<OwnedHandle>,
}

impl TestPseudoConsole {
    fn create() -> io::Result<Self> {
        fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
            let mut read = std::ptr::null_mut();
            let mut write = std::ptr::null_mut();
            // SAFETY: both output pointers are writable. On success each raw
            // handle is transferred immediately to one OwnedHandle.
            if unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: CreatePipe succeeded and returned two distinct handles.
            Ok(unsafe {
                (
                    OwnedHandle::from_raw_handle(read as RawHandle),
                    OwnedHandle::from_raw_handle(write as RawHandle),
                )
            })
        }

        let (input_reader, input_writer) = pipe()?;
        let (output_reader, output_writer) = pipe()?;
        let mut value = 0;
        // SAFETY: the pipe handles remain valid for the call and `value` is
        // writable HPCON storage. Ownership of HPCON is retained by this type.
        let result = unsafe {
            CreatePseudoConsole(
                COORD { X: 80, Y: 25 },
                input_reader.as_raw_handle() as HANDLE,
                output_writer.as_raw_handle() as HANDLE,
                0,
                &mut value,
            )
        };
        if result < 0 {
            return Err(io::Error::other(format!(
                "CreatePseudoConsole failed with HRESULT {result:#x}"
            )));
        }
        drop((input_reader, output_writer));
        Ok(Self {
            value,
            input_writer: Some(input_writer),
            output_reader: Some(output_reader),
        })
    }

    fn write_input(&self, input: &[u8]) -> io::Result<()> {
        let writer = self
            .input_writer
            .as_ref()
            .expect("a live pseudoconsole retains its input writer");
        let mut written = 0_u32;
        let length = u32::try_from(input.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "input is too large"))?;
        // SAFETY: the input buffer and byte-count output are valid for the
        // synchronous write, and the writer remains owned by self.
        if unsafe {
            WriteFile(
                writer.as_raw_handle(),
                input.as_ptr(),
                length,
                &mut written,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if written != length {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "the pseudoconsole input write was incomplete",
            ));
        }
        Ok(())
    }

    fn wait_for_output_markers(&self, expected: &[&[u8]]) -> io::Result<bool> {
        let output = self
            .output_reader
            .as_ref()
            .expect("a live pseudoconsole retains its output reader");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut received = Vec::new();
        loop {
            let mut available = 0_u32;
            // SAFETY: the pipe handle remains owned by self, available is
            // writable, and the unused optional output pointers are null.
            if unsafe {
                PeekNamedPipe(
                    output.as_raw_handle(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if available != 0 {
                let mut buffer = vec![0_u8; available as usize];
                let mut read = 0_u32;
                // SAFETY: buffer is writable for its length, read is writable,
                // and the owned synchronous pipe handle remains valid.
                if unsafe {
                    ReadFile(
                        output.as_raw_handle(),
                        buffer.as_mut_ptr(),
                        available,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                received.extend_from_slice(&buffer[..read as usize]);
                if expected.iter().all(|marker| {
                    received
                        .windows(marker.len())
                        .any(|window| window == *marker)
                }) {
                    return Ok(true);
                }
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for TestPseudoConsole {
    fn drop(&mut self) {
        // Closing both client pipe ends first prevents ClosePseudoConsole from
        // waiting on an undrained output reader on affected Windows releases.
        drop(self.input_writer.take());
        drop(self.output_reader.take());

        // Some Windows Server 2022 builds can still block indefinitely in
        // ClosePseudoConsole after the attached child exits. Keep this test's
        // teardown bounded; process teardown is the fallback for a stuck close.
        let value = self.value;
        let _ = thread::spawn(move || {
            // SAFETY: this type uniquely owned the HPCON returned by creation.
            unsafe { ClosePseudoConsole(value) };
        });
    }
}

// SAFETY: TestPseudoConsole owns a live HPCON for its full borrow and does not
// transfer that ownership to windows-spawn.
unsafe impl AsPseudoConsole for TestPseudoConsole {
    fn raw_pseudoconsole(&self) -> isize {
        self.value
    }
}

#[test]
fn pseudoconsole_attribute_connects_the_child_console() -> io::Result<()> {
    let pseudoconsole = TestPseudoConsole::create()?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "pseudoconsole_child_probe", "--nocapture"])
        .env("WINDOWS_SPAWN_PCON_PROBE", "1");
    let mut child = command.spawn_with(SpawnOptions::new().pseudoconsole(&pseudoconsole))?;
    let status = if let Some(status) = wait_bounded(&mut child)? {
        status
    } else {
        let _ = child.kill();
        wait_bounded(&mut child)?.expect("ConPTY child must terminate after kill")
    };
    assert!(status.success());
    assert!(pseudoconsole.wait_for_output_markers(&[b"windows-spawn-pcon-attached"])?);
    Ok(())
}

#[test]
fn pseudoconsole_child_probe() {
    if std::env::var_os("WINDOWS_SPAWN_PCON_PROBE").is_none() {
        return;
    }
    // SAFETY: GetConsoleCP has no pointer preconditions. A nonzero code page
    // proves this process was attached to a console. Opening CONOUT$ directly
    // is only an auxiliary connection check; the isolated stdio regression
    // below exercises the child's ordinary stdin/stdout/stderr slots.
    assert_ne!(unsafe { GetConsoleCP() }, 0);
    let mut output = File::options().write(true).open("CONOUT$").unwrap();
    output.write_all(b"windows-spawn-pcon-attached").unwrap();
}

#[test]
fn pseudoconsole_regular_stdio_stays_off_parent_pipes() -> io::Result<()> {
    let mut helper = Command::new(std::env::current_exe()?);
    helper
        .args([
            "--exact",
            "pseudoconsole_stdio_isolation_helper",
            "--nocapture",
        ])
        .env(PCON_ISOLATION_HELPER, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = helper.output()?;
    assert!(
        output.status.success(),
        "isolated ConPTY stdio helper failed: stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for marker in [PCON_STDOUT_MARKER, PCON_STDERR_MARKER] {
        assert!(
            !output
                .stdout
                .windows(marker.len())
                .any(|window| window == marker),
            "ConPTY child output leaked to its parent's stdout"
        );
        assert!(
            !output
                .stderr
                .windows(marker.len())
                .any(|window| window == marker),
            "ConPTY child output leaked to its parent's stderr"
        );
    }
    Ok(())
}

#[test]
fn pseudoconsole_stdio_isolation_helper() -> io::Result<()> {
    if std::env::var_os(PCON_ISOLATION_HELPER).is_none() {
        return Ok(());
    }

    let pseudoconsole = TestPseudoConsole::create()?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "pseudoconsole_regular_stdio_probe",
            "--nocapture",
        ])
        .env(PCON_STDIO_PROBE, "1");
    let mut child = command.spawn_with(SpawnOptions::new().pseudoconsole(&pseudoconsole))?;
    let mut input = PCON_STDIN_MARKER.to_vec();
    input.extend_from_slice(b"\r\n");
    pseudoconsole.write_input(&input)?;

    let status = if let Some(status) = wait_bounded(&mut child)? {
        status
    } else {
        let _ = child.kill();
        wait_bounded(&mut child)?.expect("ConPTY stdio probe must terminate after kill")
    };
    assert!(status.success());
    assert!(pseudoconsole.wait_for_output_markers(&[PCON_STDOUT_MARKER, PCON_STDERR_MARKER])?);
    Ok(())
}

#[test]
fn pseudoconsole_regular_stdio_probe() -> io::Result<()> {
    if std::env::var_os(PCON_STDIO_PROBE).is_none() {
        return Ok(());
    }

    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    if !line
        .as_bytes()
        .windows(PCON_STDIN_MARKER.len())
        .any(|window| window == PCON_STDIN_MARKER)
    {
        return Err(io::Error::other(
            "standard input did not arrive through ConPTY",
        ));
    }
    io::stdout().write_all(PCON_STDOUT_MARKER)?;
    io::stdout().flush()?;
    io::stderr().write_all(PCON_STDERR_MARKER)?;
    io::stderr().flush()
}

#[test]
fn capability_wrappers_and_all_option_builders_are_exercised() -> io::Result<()> {
    let path = temporary_path("capability");
    let file = File::create(&path)?;
    let borrowed_stdio = Stdio::from_borrowed(&file)?;
    assert!(format!("{borrowed_stdio:?}").contains("Owned"));
    assert!(format!("{:?}", Stdio::inherit()).contains("Inherit"));
    assert!(format!("{:?}", Stdio::null()).contains("Null"));
    assert!(format!("{:?}", Stdio::piped()).contains("Piped"));

    let parent = ParentProcess::open(std::process::id())?;
    let _ = parent.as_handle();
    assert!(format!("{parent:?}").contains("ParentProcess"));
    let job = Job::create()?;
    job.set_kill_on_close(true)?;
    job.set_kill_on_close(false)?;
    let duplicate_job = job.duplicate()?;
    let _ = duplicate_job.as_handle();
    assert!(format!("{duplicate_job:?}").contains("Job"));

    let mut flags = CreationFlags::NEW_PROCESS_GROUP;
    flags |= CreationFlags::DEFAULT_ERROR_MODE;
    let pseudoconsole = InvalidPseudoConsole;
    let options = SpawnOptions::new()
        .job(&job)
        .parent_process(&parent)
        .mitigation(MitigationPolicy::new().dep(true))
        .pseudoconsole(&pseudoconsole)
        .creation_flags(flags)
        .drop_policy(DropPolicy::KillTree);
    let rendered = format!("{options:?}");
    assert!(rendered.contains("borrowed") && rendered.contains("KillTree"));

    let mut invalid = cmd("exit /b 0");
    invalid.stdout(Stdio::null());
    assert_eq!(
        invalid.spawn_with(options).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );

    let assigned_job = Job::create()?;
    let mut child = cmd("ping -n 5 127.0.0.1 >nul").spawn()?;
    assigned_job.assign(&child)?;
    assigned_job.terminate(31)?;
    assert_eq!(child.wait()?.code(), Some(31));
    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn handle_list_excludes_an_unlisted_inheritable_handle() -> io::Result<()> {
    const PROBE: &str = "WINDOWS_SPAWN_EXCLUDED_HANDLE_PROBE";
    if let Some(value) = std::env::var_os(PROBE) {
        let value = value.to_string_lossy();
        let mut fields = value.split(':');
        let handle = fields
            .next()
            .expect("probe handle is present")
            .parse::<isize>()
            .expect("probe handle must be an integer");
        let volume = fields
            .next()
            .expect("probe volume is present")
            .parse::<u32>()
            .expect("probe volume must be an integer");
        let index = fields
            .next()
            .expect("probe file index is present")
            .parse::<u64>()
            .expect("probe file index must be an integer");
        assert!(fields.next().is_none(), "probe has exactly three fields");
        if let Ok(identity) = file_identity(handle as HANDLE) {
            assert_ne!(
                identity,
                (volume, index),
                "unlisted inheritable file handle reached the child"
            );
        }
        return Ok(());
    }

    let excluded_path = temporary_path("excluded-handle");
    let listed_file = File::open("NUL")?;
    let excluded_file = File::create(&excluded_path)?;
    let excluded = inheritable_duplicate(&excluded_file)?;
    let excluded_value = excluded.as_handle().as_raw_handle() as isize;
    let (excluded_volume, excluded_index) = file_identity(excluded.as_handle().as_raw_handle())?;
    let probe = format!("{excluded_value}:{excluded_volume}:{excluded_index}");

    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "handle_list_excludes_an_unlisted_inheritable_handle",
            "--test-threads=1",
        ])
        .env(PROBE, probe);
    command.arg_handle(&listed_file)?;
    drop(listed_file);
    let status = command.status()?;
    assert!(status.success());

    drop((excluded_file, excluded));
    fs::remove_file(excluded_path)?;
    Ok(())
}

#[test]
fn dropping_a_kill_tree_child_terminates_the_root() -> io::Result<()> {
    let marker = temporary_path("kill-tree-drop");
    let script = format!(
        "Start-Sleep -Milliseconds 600; Set-Content -LiteralPath '{}' -Value ran",
        marker.display()
    );
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &script,
    ]);
    let child = command.spawn_with(SpawnOptions::new().drop_policy(DropPolicy::KillTree))?;
    drop(child);
    thread::sleep(Duration::from_millis(900));
    assert!(!marker.exists());
    Ok(())
}

#[test]
fn public_job_kill_on_close_terminates_assigned_process() -> io::Result<()> {
    let job = Job::create()?;
    job.set_kill_on_close(true)?;
    let mut child = cmd("ping -n 20 127.0.0.1 >nul").spawn()?;
    job.assign(&child)?;
    drop(job);

    let status = (0..200).find_map(|_| {
        let status = child.try_wait().expect("assigned child must be queryable");
        if status.is_none() {
            thread::sleep(Duration::from_millis(10));
        }
        status
    });
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert!(status.is_some());
    Ok(())
}

#[test]
fn reusable_command_environment_and_accessors_work() -> io::Result<()> {
    let directory = temporary_path("builder-cwd");
    fs::create_dir(&directory)?;
    let mut command = cmd("echo %WINDOWS_SPAWN_ONE%-%WINDOWS_SPAWN_TWO%");
    command
        .envs([("WINDOWS_SPAWN_ONE", "one"), ("WINDOWS_SPAWN_TWO", "old")])
        .env_remove("windows_spawn_two")
        .env("WINDOWS_SPAWN_TWO", "two")
        .current_dir(&directory);
    assert_eq!(command.get_program(), "cmd.exe");
    assert_eq!(command.get_current_dir(), Some(directory.as_path()));
    let output = command.output()?;
    assert!(String::from_utf8_lossy(&output.stdout).contains("one-two"));

    command.env_clear().env("WINDOWS_SPAWN_ONE", "clear");
    let output = command.output()?;
    assert!(String::from_utf8_lossy(&output.stdout).contains("clear-"));
    fs::remove_dir(directory)?;
    Ok(())
}

#[test]
fn dropping_suspended_child_prevents_execution() -> io::Result<()> {
    let path = temporary_path("suspended-drop");
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "suspended_execution_probe", "--nocapture"])
        .env("WINDOWS_SPAWN_SUSPENDED_PROBE", &path);
    let suspended = command.spawn_suspended_with(SpawnOptions::new())?;
    let mut process = ProcessExitGuard::new(local_duplicate(&suspended, false)?);
    thread::sleep(Duration::from_millis(500));
    assert!(!path.exists());
    drop(suspended);
    assert!(
        process.wait(Duration::from_secs(5))?,
        "dropping SuspendedChild must terminate the process"
    );
    assert!(!path.exists());
    Ok(())
}

#[test]
fn suspended_execution_probe() -> io::Result<()> {
    let Some(path) = std::env::var_os("WINDOWS_SPAWN_SUSPENDED_PROBE") else {
        return Ok(());
    };
    fs::write(path, b"ran")
}
