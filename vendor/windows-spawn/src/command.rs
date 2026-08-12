//! Reusable process-launch intent.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::io::{AsHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output};

use crate::child::{Child, SuspendedChild};
use crate::handles::Stdio;
use crate::options::SpawnOptions;
use crate::plan::{IoMode, SpawnPlan};
use crate::sys;
use crate::transaction::SpawnTransaction;

#[derive(Debug)]
pub(crate) enum Arg {
    Text(OsString),
    Raw(OsString),
    Handle(OwnedHandle),
}

#[derive(Debug)]
pub(crate) enum EnvOp {
    Set(OsString, EnvValue),
    Remove(OsString),
}

#[derive(Debug)]
pub(crate) enum EnvValue {
    Text(OsString),
    Handle(OwnedHandle, HandleFormat),
}

#[derive(Debug)]
pub(crate) enum HandleFormat {
    Decimal,
    LowerHex,
}

/// A reusable description of a Windows process launch.
///
/// Handles embedded by [`Self::arg_handle`] and [`Self::env_handle`] are
/// privately duplicated when configured. Each spawn duplicates those handles
/// again into the actual parent process and only then lowers their numeric
/// values to decimal text.
///
/// # Examples
///
/// Run a command to completion and capture what it wrote, terminating any
/// descendants it leaves behind:
///
/// ```
/// use windows_spawn::{Command, DropPolicy, SpawnOptions};
///
/// // `.bat` and `.cmd` are rejected, so a shell boundary is always explicit.
/// let shell = std::env::var_os("COMSPEC").expect("COMSPEC is set on Windows");
/// let mut command = Command::new(shell);
/// command.args(["/D", "/S", "/C"]).raw_arg("echo hello");
///
/// let output = command.output_with(SpawnOptions::new().drop_policy(DropPolicy::KillTree))?;
///
/// assert!(output.status.success());
/// assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Debug)]
pub struct Command {
    pub(crate) program: OsString,
    pub(crate) args: Vec<Arg>,
    pub(crate) env_clear: bool,
    pub(crate) env_ops: Vec<EnvOp>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) stdin: Option<Stdio>,
    pub(crate) stdout: Option<Stdio>,
    pub(crate) stderr: Option<Stdio>,
}

impl Command {
    /// Creates a command which will execute `program`.
    #[must_use]
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            env_clear: false,
            env_ops: Vec::new(),
            cwd: None,
            stdin: None,
            stdout: None,
            stderr: None,
        }
    }

    /// Appends a normally quoted argument.
    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        self.args.push(Arg::Text(arg.as_ref().to_os_string()));
        self
    }

    /// Appends multiple normally quoted arguments.
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self.arg(arg);
        }
        self
    }

    /// Appends text verbatim to the Windows command line.
    ///
    /// The text is separated from the preceding element by one space but is
    /// otherwise neither quoted nor escaped.
    pub fn raw_arg<S: AsRef<OsStr>>(&mut self, text: S) -> &mut Self {
        self.args.push(Arg::Raw(text.as_ref().to_os_string()));
        self
    }

    /// Appends a handle argument whose child-table value is lowered at spawn.
    ///
    /// # Errors
    ///
    /// Returns an error if the source handle cannot be duplicated.
    pub fn arg_handle<T: AsHandle>(&mut self, handle: &T) -> io::Result<&mut Self> {
        self.args.push(Arg::Handle(sys::duplicate_local(
            handle.as_handle(),
            false,
        )?));
        Ok(self)
    }

    /// Sets one environment variable.
    pub fn env<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.env_ops.push(EnvOp::Set(
            key.as_ref().to_os_string(),
            EnvValue::Text(value.as_ref().to_os_string()),
        ));
        self
    }

    /// Sets multiple environment variables.
    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (key, value) in vars {
            self.env(key, value);
        }
        self
    }

    /// Sets an environment variable to a handle's child-table numeric value.
    ///
    /// # Errors
    ///
    /// Returns an error if the source handle cannot be duplicated.
    pub fn env_handle<K: AsRef<OsStr>, T: AsHandle>(
        &mut self,
        key: K,
        handle: &T,
    ) -> io::Result<&mut Self> {
        self.env_ops.push(EnvOp::Set(
            key.as_ref().to_os_string(),
            EnvValue::Handle(
                sys::duplicate_local(handle.as_handle(), false)?,
                HandleFormat::Decimal,
            ),
        ));
        Ok(self)
    }

    /// Sets an environment variable to a handle's child-table numeric value
    /// as lowercase hexadecimal without a prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the source handle cannot be duplicated.
    pub fn env_handle_lower_hex<K: AsRef<OsStr>, T: AsHandle>(
        &mut self,
        key: K,
        handle: &T,
    ) -> io::Result<&mut Self> {
        self.env_ops.push(EnvOp::Set(
            key.as_ref().to_os_string(),
            EnvValue::Handle(
                sys::duplicate_local(handle.as_handle(), false)?,
                HandleFormat::LowerHex,
            ),
        ));
        Ok(self)
    }

    /// Removes an environment variable case-insensitively.
    pub fn env_remove<K: AsRef<OsStr>>(&mut self, key: K) -> &mut Self {
        self.env_ops
            .push(EnvOp::Remove(key.as_ref().to_os_string()));
        self
    }

    /// Clears the inherited environment and prior recorded modifications.
    pub fn env_clear(&mut self) -> &mut Self {
        self.env_clear = true;
        self.env_ops.clear();
        self
    }

    /// Sets the child working directory.
    pub fn current_dir<P: AsRef<Path>>(&mut self, dir: P) -> &mut Self {
        self.cwd = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Configures standard input.
    pub fn stdin<T: Into<Stdio>>(&mut self, stdio: T) -> &mut Self {
        self.stdin = Some(stdio.into());
        self
    }

    /// Configures standard output.
    pub fn stdout<T: Into<Stdio>>(&mut self, stdio: T) -> &mut Self {
        self.stdout = Some(stdio.into());
        self
    }

    /// Configures standard error.
    pub fn stderr<T: Into<Stdio>>(&mut self, stdio: T) -> &mut Self {
        self.stderr = Some(stdio.into());
        self
    }

    /// Returns the originally configured program.
    #[must_use]
    pub fn get_program(&self) -> &OsStr {
        &self.program
    }

    /// Returns the configured working directory.
    #[must_use]
    pub fn get_current_dir(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Spawns with default options.
    ///
    /// # Errors
    ///
    /// Returns validation, resource-acquisition, or process-creation errors.
    pub fn spawn(&mut self) -> io::Result<Child> {
        self.spawn_with(SpawnOptions::new())
    }

    /// Spawns using one operation's borrowed capabilities and policy.
    ///
    /// # Errors
    ///
    /// Returns validation, resource-acquisition, or process-creation errors.
    pub fn spawn_with(&mut self, options: SpawnOptions<'_>) -> io::Result<Child> {
        let plan = SpawnPlan::new_running(self, options, IoMode::Spawn)?;
        Ok(SpawnTransaction::new(&plan)?.commit_child())
    }

    /// Spawns in the suspended type state with default options.
    ///
    /// # Errors
    ///
    /// Returns validation, resource-acquisition, or process-creation errors.
    pub fn spawn_suspended(&mut self) -> io::Result<SuspendedChild> {
        self.spawn_suspended_with(SpawnOptions::new())
    }

    /// Spawns in the suspended type state using explicit options.
    ///
    /// # Errors
    ///
    /// Returns validation, resource-acquisition, or process-creation errors.
    pub fn spawn_suspended_with(
        &mut self,
        options: SpawnOptions<'_>,
    ) -> io::Result<SuspendedChild> {
        let plan = SpawnPlan::new_suspended(self, options, IoMode::Spawn)?;
        Ok(SpawnTransaction::new(&plan)?.commit_suspended())
    }

    /// Runs the process and waits for its status using default options.
    ///
    /// # Errors
    ///
    /// Returns an error from spawning, waiting, or retrieving the exit code.
    pub fn status(&mut self) -> io::Result<ExitStatus> {
        self.status_with(SpawnOptions::new())
    }

    /// Runs the process and waits for its status using explicit options.
    ///
    /// # Errors
    ///
    /// Returns an error from spawning, waiting, or retrieving the exit code.
    pub fn status_with(&mut self, options: SpawnOptions<'_>) -> io::Result<ExitStatus> {
        self.spawn_with(options)?.wait()
    }

    /// Runs the process and captures output using default options.
    ///
    /// # Errors
    ///
    /// Returns an error from spawning, waiting, reading, or Job termination.
    pub fn output(&mut self) -> io::Result<Output> {
        self.output_with(SpawnOptions::new())
    }

    /// Runs the process and captures output using explicit options.
    ///
    /// # Errors
    ///
    /// Returns an error from spawning, waiting, reading, or Job termination.
    pub fn output_with(&mut self, options: SpawnOptions<'_>) -> io::Result<Output> {
        let plan = SpawnPlan::new_running(self, options, IoMode::Output)?;
        SpawnTransaction::new(&plan)?
            .commit_child()
            .wait_with_output()
    }
}
