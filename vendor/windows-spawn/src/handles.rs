//! Owned capability wrappers used by process creation.

use std::fmt;
use std::fs::File;
use std::io;
use std::os::windows::io::{AsHandle, BorrowedHandle, OwnedHandle};

use crate::child::Child;
use crate::sys;

/// Describes a standard stream source while keeping any supplied handle owned.
pub struct Stdio {
    pub(crate) inner: StdioInner,
}

pub(crate) enum StdioInner {
    Inherit,
    Null,
    Piped,
    Owned(OwnedHandle),
}

impl fmt::Debug for Stdio {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self.inner {
            StdioInner::Inherit => "Inherit",
            StdioInner::Null => "Null",
            StdioInner::Piped => "Piped",
            StdioInner::Owned(_) => "Owned",
        };
        formatter.debug_tuple("Stdio").field(&name).finish()
    }
}

impl Stdio {
    /// Inherits the corresponding standard stream from the caller.
    #[must_use]
    pub const fn inherit() -> Self {
        Self {
            inner: StdioInner::Inherit,
        }
    }

    /// Connects the stream to the Windows null device.
    #[must_use]
    pub const fn null() -> Self {
        Self {
            inner: StdioInner::Null,
        }
    }

    /// Creates an anonymous pipe and returns the caller's end on [`Child`].
    #[must_use]
    pub const fn piped() -> Self {
        Self {
            inner: StdioInner::Piped,
        }
    }

    /// Duplicates a borrowed handle into private, non-inheritable ownership.
    ///
    /// The original handle may be closed immediately after this call.
    ///
    /// # Errors
    ///
    /// Returns the operating-system error if duplication fails.
    pub fn from_borrowed<T: AsHandle>(source: &T) -> io::Result<Self> {
        Ok(Self::from(sys::duplicate_local(source.as_handle(), false)?))
    }
}

impl From<OwnedHandle> for Stdio {
    fn from(handle: OwnedHandle) -> Self {
        Self {
            inner: StdioInner::Owned(handle),
        }
    }
}

impl From<File> for Stdio {
    fn from(file: File) -> Self {
        Self::from(OwnedHandle::from(file))
    }
}

/// A process handle validated for use as `PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`.
#[derive(Debug)]
pub struct ParentProcess {
    handle: OwnedHandle,
}

impl ParentProcess {
    /// Opens a process with process-creation and handle-duplication rights.
    ///
    /// # Errors
    ///
    /// Returns an error if the PID cannot be opened with the required rights.
    pub fn open(pid: u32) -> io::Result<Self> {
        Ok(Self {
            handle: sys::open_parent_process(pid)?,
        })
    }

    /// Adopts and validates an existing process handle.
    ///
    /// # Errors
    ///
    /// Returns an error if the handle does not identify a process.
    pub fn from_handle(handle: OwnedHandle) -> io::Result<Self> {
        sys::validate_process_handle(handle.as_handle())?;
        Ok(Self { handle })
    }
}

impl AsHandle for ParentProcess {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }
}

/// An owned Windows Job object.
#[derive(Debug)]
pub struct Job {
    handle: OwnedHandle,
}

impl Job {
    /// Creates an unnamed Job object.
    ///
    /// # Errors
    ///
    /// Returns the operating-system error if Job creation fails.
    pub fn create() -> io::Result<Self> {
        Ok(Self {
            handle: sys::create_job()?,
        })
    }

    /// Adopts an existing handle after verifying it is a Job handle.
    ///
    /// # Errors
    ///
    /// Returns an error if Job limit information cannot be queried.
    pub fn from_handle(handle: OwnedHandle) -> io::Result<Self> {
        sys::validate_job_handle(handle.as_handle())?;
        Ok(Self { handle })
    }

    /// Creates an independent duplicate of this Job handle.
    ///
    /// # Errors
    ///
    /// Returns the operating-system error if duplication fails.
    pub fn duplicate(&self) -> io::Result<Self> {
        Ok(Self {
            handle: sys::duplicate_local(self.handle.as_handle(), false)?,
        })
    }

    /// Assigns an existing child to the Job.
    ///
    /// # Errors
    ///
    /// Returns an error when Windows rejects the Job assignment.
    pub fn assign(&self, child: &Child) -> io::Result<()> {
        sys::assign_job(self.handle.as_handle(), child.process_handle())
    }

    /// Terminates every process in the Job with `exit_code`.
    ///
    /// # Errors
    ///
    /// Returns the operating-system error if Job termination fails.
    pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
        sys::terminate_job(self.handle.as_handle(), exit_code)
    }

    /// Enables or disables kill-on-close without overwriting other Job limits.
    ///
    /// # Errors
    ///
    /// Returns an error if querying or updating Job limits fails.
    pub fn set_kill_on_close(&self, enable: bool) -> io::Result<()> {
        sys::set_job_kill_on_close(self.handle.as_handle(), enable)
    }
}

impl AsHandle for Job {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }
}

/// A borrowed pseudoconsole capability.
///
/// # Safety
///
/// Implementations must return a valid, nonzero `HPCON` and keep it open and
/// unchanged for the full lifetime of every borrow passed to
/// [`crate::SpawnOptions::pseudoconsole`]. The implementation retains
/// ownership: windows-spawn borrows the value for process creation and never closes
/// or releases it.
#[allow(unsafe_code)]
pub unsafe trait AsPseudoConsole {
    /// Returns the borrowed raw `HPCON` value.
    ///
    /// Library implementations use this method to bridge their owned
    /// pseudoconsole type to windows-spawn. Applications should normally pass the
    /// implementing object to [`crate::SpawnOptions::pseudoconsole`] instead
    /// of reading the numeric value.
    ///
    /// The returned value must satisfy the trait's safety contract. Calling
    /// this method does not transfer ownership.
    fn raw_pseudoconsole(&self) -> isize;
}

#[cfg(test)]
mod tests {
    use std::os::windows::io::AsRawHandle;

    use super::*;

    #[test]
    fn owned_handle_adoption_validates_resource_kind() {
        let mut host = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "ping -n 5 127.0.0.1 >nul"])
            .spawn()
            .unwrap();
        let parent = ParentProcess::open(host.id()).unwrap();
        assert!(format!("{parent:?}").contains("ParentProcess"));
        let adopted_parent =
            ParentProcess::from_handle(sys::duplicate_local(host.as_handle(), false).unwrap())
                .unwrap();
        assert_ne!(
            adopted_parent.as_handle().as_raw_handle(),
            std::ptr::null_mut()
        );

        let job = Job::create().unwrap();
        let duplicate = job.duplicate().unwrap();
        let adopted_job = Job::from_handle(duplicate.handle).unwrap();
        adopted_job.set_kill_on_close(true).unwrap();
        adopted_job.set_kill_on_close(false).unwrap();

        let file = File::open("NUL").unwrap();
        let not_process = sys::duplicate_local(file.as_handle(), false).unwrap();
        assert!(ParentProcess::from_handle(not_process).is_err());
        let not_job = sys::duplicate_local(file.as_handle(), false).unwrap();
        assert!(Job::from_handle(not_job).is_err());
        let _ = host.kill();
        let _ = host.wait();
    }
}
