//! Owned child process and suspended type state.

use std::io::{self, Read, Write};
use std::os::windows::io::{AsHandle, BorrowedHandle, OwnedHandle};
use std::process::{ExitStatus, Output};
use std::thread;

use crate::handles::Job;
use crate::sys;

/// The writable parent end of a child's standard-input pipe.
#[derive(Debug)]
pub struct ChildStdin {
    handle: OwnedHandle,
}

impl Write for ChildStdin {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        sys::write_handle(self.handle.as_handle(), buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AsHandle for ChildStdin {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }
}

/// The readable parent end of a child's standard-output pipe.
#[derive(Debug)]
pub struct ChildStdout {
    handle: OwnedHandle,
}

impl Read for ChildStdout {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        sys::read_handle(self.handle.as_handle(), buffer)
    }
}

impl AsHandle for ChildStdout {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }
}

/// The readable parent end of a child's standard-error pipe.
#[derive(Debug)]
pub struct ChildStderr {
    handle: OwnedHandle,
}

impl Read for ChildStderr {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        sys::read_handle(self.handle.as_handle(), buffer)
    }
}

impl AsHandle for ChildStderr {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }
}

/// A running or exited process whose handle is owned exactly once.
#[derive(Debug)]
pub struct Child {
    // Declared first so kill-on-close takes effect before pipe and process
    // handles are released by Rust's field drop order.
    kill_job: Option<Job>,
    /// A pipe connected to the child's standard input, when requested.
    pub stdin: Option<ChildStdin>,
    /// A pipe connected to the child's standard output, when requested.
    pub stdout: Option<ChildStdout>,
    /// A pipe connected to the child's standard error, when requested.
    pub stderr: Option<ChildStderr>,
    process: OwnedHandle,
    pid: u32,
    exit: Option<ExitStatus>,
}

impl Child {
    pub(crate) fn new(
        process: OwnedHandle,
        pid: u32,
        kill_job: Option<Job>,
        stdin: Option<OwnedHandle>,
        stdout: Option<OwnedHandle>,
        stderr: Option<OwnedHandle>,
    ) -> Self {
        Self {
            kill_job,
            stdin: stdin.map(|handle| ChildStdin { handle }),
            stdout: stdout.map(|handle| ChildStdout { handle }),
            stderr: stderr.map(|handle| ChildStderr { handle }),
            process,
            pid,
            exit: None,
        }
    }

    pub(crate) fn process_handle(&self) -> BorrowedHandle<'_> {
        self.process.as_handle()
    }

    /// Returns the process identifier captured at creation.
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.pid
    }

    /// Terminates the root process.
    ///
    /// # Errors
    ///
    /// Returns the operating-system error from `TerminateProcess`.
    pub fn kill(&mut self) -> io::Result<()> {
        if self.exit.is_some() {
            return Ok(());
        }
        sys::terminate_process(self.process.as_handle(), 1)
    }

    /// Waits for exit and caches the status.
    ///
    /// # Errors
    ///
    /// Returns an error if waiting or retrieving the exit code fails.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.exit {
            return Ok(status);
        }
        drop(self.stdin.take());
        sys::wait_process(self.process.as_handle())?;
        let status = sys::exit_status(self.process.as_handle())?;
        self.exit = Some(status);
        Ok(status)
    }

    /// Checks for exit without blocking, returning the cached status thereafter.
    ///
    /// # Errors
    ///
    /// Returns an error if querying the process or its exit code fails.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.exit.is_some() {
            return Ok(self.exit);
        }
        if !sys::try_wait_process(self.process.as_handle())? {
            return Ok(None);
        }
        let status = sys::exit_status(self.process.as_handle())?;
        self.exit = Some(status);
        Ok(self.exit)
    }

    /// Waits while draining both output pipes concurrently.
    ///
    /// Under [`crate::DropPolicy::KillTree`], descendants are terminated after
    /// the root exits and before reader threads are joined. This guarantees EOF
    /// even when a grandchild retained a pipe handle.
    ///
    /// # Errors
    ///
    /// Returns an error from process waiting, pipe reading, or Job termination.
    pub fn wait_with_output(mut self) -> io::Result<Output> {
        drop(self.stdin.take());

        let stdout_reader = self
            .stdout
            .take()
            .map(|stream| thread::spawn(move || drain_output(stream.handle.as_handle())));
        let stderr_reader = self
            .stderr
            .take()
            .map(|stream| thread::spawn(move || drain_output(stream.handle.as_handle())));

        let status = self.wait();
        let termination = self
            .kill_job
            .as_ref()
            .map_or(Ok(()), |job| job.terminate(1));
        let (stdout, stderr) = join_readers(stdout_reader, stderr_reader)?;
        termination?;

        Ok(Output {
            status: status?,
            stdout,
            stderr,
        })
    }
}

impl AsHandle for Child {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.process.as_handle()
    }
}

fn drain_output(handle: BorrowedHandle<'_>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8_192];
    loop {
        let Some(read) = std::num::NonZeroUsize::new(sys::read_handle(handle, &mut buffer)?) else {
            return Ok(bytes);
        };
        bytes.extend_from_slice(&buffer[..read.get()]);
    }
}

fn join_reader(reader: Option<thread::JoinHandle<io::Result<Vec<u8>>>>) -> io::Result<Vec<u8>> {
    match reader {
        Some(reader) => reader
            .join()
            .map_err(|_| io::Error::other("output reader thread panicked"))?,
        None => Ok(Vec::new()),
    }
}

fn join_readers(
    stdout: Option<thread::JoinHandle<io::Result<Vec<u8>>>>,
    stderr: Option<thread::JoinHandle<io::Result<Vec<u8>>>>,
) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let stdout = join_reader(stdout);
    let stderr = join_reader(stderr);
    Ok((stdout?, stderr?))
}

/// A process whose primary thread has not yet been resumed.
///
/// Dropping this value without resuming always terminates the process.
/// The consuming transition makes a second resume unrepresentable:
///
/// ```compile_fail
/// use windows_spawn::Command;
///
/// let mut command = Command::new("cmd.exe");
/// let suspended = command.spawn_suspended().unwrap();
/// let _child = suspended.resume().unwrap();
/// let _second = suspended.resume().unwrap();
/// ```
#[derive(Debug)]
#[must_use = "dropping a suspended child terminates it"]
pub struct SuspendedChild {
    child: Option<Child>,
    main_thread: OwnedHandle,
}

impl SuspendedChild {
    pub(crate) fn new(child: Child, main_thread: OwnedHandle) -> Self {
        Self {
            child: Some(child),
            main_thread,
        }
    }

    /// Returns the process identifier captured at creation.
    ///
    /// # Panics
    ///
    /// Panics only if an internal ownership invariant was violated and the
    /// process was removed before this suspended value was consumed.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child
            .as_ref()
            .expect("a suspended child owns its process until resume")
            .id()
    }

    /// Borrows the suspended process's primary thread handle.
    ///
    /// This handle is available for supported thread configuration and
    /// inspection before [`Self::resume`] consumes the suspended state.
    ///
    #[must_use]
    pub fn primary_thread_handle(&self) -> BorrowedHandle<'_> {
        self.main_thread.as_handle()
    }

    /// Resumes the primary thread and transitions to an ordinary [`Child`].
    ///
    /// # Errors
    ///
    /// Returns the operating-system error when the primary thread cannot be
    /// resumed. It also returns `InvalidData` when external suspension or
    /// resumption changed the expected suspend count of exactly one. The
    /// process is terminated during either rollback.
    pub fn resume(mut self) -> io::Result<Child> {
        let previous = sys::resume_thread(self.main_thread.as_handle())?;
        if previous != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("primary thread suspend count was {previous}, expected 1"),
            ));
        }
        self.child
            .take()
            .ok_or_else(|| io::Error::other("suspended child lost its process"))
    }
}

impl AsHandle for SuspendedChild {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.child
            .as_ref()
            .expect("a suspended child owns its process until resume")
            .as_handle()
    }
}

impl Drop for SuspendedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::*;

    #[test]
    fn absent_and_panicked_output_readers_become_results() {
        assert!(join_reader(None).unwrap().is_empty());
        let panicked = thread::spawn(|| -> io::Result<Vec<u8>> { panic!("reader panic") });
        assert_eq!(
            join_reader(Some(panicked)).unwrap_err().kind(),
            io::ErrorKind::Other
        );
    }

    #[test]
    fn both_output_readers_are_joined_when_the_first_panics() {
        let joined = Arc::new(AtomicBool::new(false));
        let stdout = thread::spawn(|| -> io::Result<Vec<u8>> { panic!("stdout panic") });
        let stderr_joined = Arc::clone(&joined);
        let stderr = thread::spawn(move || {
            stderr_joined.store(true, Ordering::Release);
            Ok(Vec::new())
        });

        assert_eq!(
            join_readers(Some(stdout), Some(stderr)).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        assert!(joined.load(Ordering::Acquire));
    }
}
