//! Resource-acquiring spawn transaction and rollback.

use std::cmp::Ordering;
use std::collections::btree_map::{BTreeMap, Entry};
use std::env;
use std::ffi::{OsStr, OsString};
use std::io;
use std::iter;
use std::marker::PhantomData;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, OwnedHandle};
use std::path::{Path, PathBuf};

use crate::child::{Child, SuspendedChild};
use crate::command::{Arg, Command, EnvOp, EnvValue, HandleFormat};
use crate::handles::{Job, StdioInner};
use crate::options::DropPolicy;
use crate::plan::{
    Running, SpawnPlan, SpawnState, StandardHandles, StandardIo, StdioSpec, Suspended,
};
use crate::sys::{self, NullAccess, StandardStream};

const QUOTE: u16 = 0x22;
const BACKSLASH: u16 = 0x5c;
const SPACE: u16 = 0x20;

/// Owns the process between `CreateProcessW` success and an explicit commit.
/// Dropping before commit terminates the partially-created process.
pub(crate) struct SpawnTransaction<M> {
    created: Option<sys::CreatedProcess>,
    kill_job: Option<Job>,
    stdio: StandardHandles<Option<OwnedHandle>>,
    state: PhantomData<M>,
}

impl<M: SpawnState> SpawnTransaction<M> {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn new<'command, 'options>(
        plan: &SpawnPlan<'command, 'options, M>,
    ) -> io::Result<Self> {
        let parent: Option<BorrowedHandle<'options>> = plan.options.parent.map(AsHandle::as_handle);
        let mut transfer = HandleTransfer::new(parent);
        let (stdio_values, stdio) = match &plan.stdio {
            StandardIo::Ordinary(specs) => {
                let prepared = prepare_standard_handles(specs, &mut transfer)?;
                let values = sys::StartupStdio::Ordinary(sys::StandardHandles {
                    stdin: prepared.stdin.child,
                    stdout: prepared.stdout.child,
                    stderr: prepared.stderr.child,
                });
                let owners = StandardHandles {
                    stdin: prepared.stdin.parent,
                    stdout: prepared.stdout.parent,
                    stderr: prepared.stderr.parent,
                };
                (values, owners)
            }
            StandardIo::PseudoConsole => (
                sys::StartupStdio::PseudoConsole,
                StandardHandles {
                    stdin: None,
                    stdout: None,
                    stderr: None,
                },
            ),
        };
        let kill_job = if plan.options.drop_policy == DropPolicy::KillTree {
            let job = Job::create()?;
            job.set_kill_on_close(true)?;
            Some(job)
        } else {
            None
        };
        let mut job_values: Vec<isize> = plan
            .options
            .jobs
            .iter()
            .map(|job| job.as_handle().as_raw_handle() as isize)
            .collect();
        if let Some(job) = &kill_job {
            job_values.push(job.as_handle().as_raw_handle() as isize);
        }

        let command_line = build_command_line(plan.command, &mut transfer)?;
        let environment = build_environment(plan.command, &mut transfer)?;
        let child_path = environment.path.as_deref();
        let application = resolve_executable(&plan.command.program, child_path)?;
        let current_dir = plan
            .command
            .cwd
            .as_ref()
            .map(|path| wide_nul(path.as_os_str()))
            .transpose()?;

        // Freeze every pointer-valued attribute before adding it to the list.
        let inherited_values = transfer.inherited_values().to_vec().into_boxed_slice();
        let parent_value = transfer
            .parent()
            .map(|handle| Box::new(handle.as_raw_handle() as isize));
        let mitigation_words = plan.options.mitigation.words();
        let mitigation_value = (mitigation_words != [0, 0]).then(|| Box::new(mitigation_words));
        let job_values = job_values.into_boxed_slice();
        let pseudoconsole = plan.options.pseudoconsole_raw();

        let attribute_count = u32::from(!inherited_values.is_empty())
            + u32::from(parent_value.is_some())
            + u32::from(mitigation_value.is_some())
            + u32::from(!job_values.is_empty())
            + u32::from(pseudoconsole.is_some());
        let mut attributes = if attribute_count == 0 {
            None
        } else {
            Some(sys::AttributeList::new(attribute_count)?)
        };
        if let Some(list) = &mut attributes {
            if !inherited_values.is_empty() {
                list.set_handle_list(&inherited_values)?;
            }
            if let Some(value) = &parent_value {
                list.set_parent(value)?;
            }
            if let Some(value) = &mitigation_value {
                list.set_mitigation(value)?;
            }
            if !job_values.is_empty() {
                list.set_jobs(&job_values)?;
            }
            if let Some(value) = pseudoconsole {
                list.set_pseudoconsole(value)?;
            }
        }

        let mut command_line = command_line;
        let mut request = sys::ProcessRequest {
            application: &application,
            command_line: &mut command_line,
            environment: environment.block.as_deref(),
            current_dir: current_dir.as_deref(),
            stdio: stdio_values,
            inherit_handles: !inherited_values.is_empty(),
            creation_flags: plan.options.creation_flags.bits(),
            suspended: M::SUSPENDED,
            attributes: attributes.as_ref(),
        };
        let created = sys::create_process(&mut request)?;

        // Attribute backing, local inheritable duplicates, and alternate-parent
        // remote sources all roll back here. The child now owns inherited copies.
        drop(attributes);
        drop(transfer);

        Ok(Self {
            created: Some(created),
            kill_job,
            stdio,
            state: PhantomData,
        })
    }

    fn commit_parts(mut self) -> (Child, OwnedHandle) {
        let created = self
            .created
            .take()
            .expect("an uncommitted transaction owns its process");
        let child = Child::new(
            created.process,
            created.pid,
            self.kill_job.take(),
            self.stdio.stdin.take(),
            self.stdio.stdout.take(),
            self.stdio.stderr.take(),
        );
        (child, created.thread)
    }
}

impl SpawnTransaction<Running> {
    pub(crate) fn commit_child(self) -> Child {
        let (child, thread) = self.commit_parts();
        drop(thread);
        child
    }
}

impl SpawnTransaction<Suspended> {
    pub(crate) fn commit_suspended(self) -> SuspendedChild {
        let (child, thread) = self.commit_parts();
        SuspendedChild::new(child, thread)
    }
}

impl<M> Drop for SpawnTransaction<M> {
    fn drop(&mut self) {
        if let Some(created) = &self.created {
            let _ = sys::terminate_process(created.process.as_handle(), 1);
        }
        drop(self.kill_job.take());
    }
}

struct PreparedStdio {
    child: isize,
    parent: Option<OwnedHandle>,
}

fn prepare_standard_handles(
    specs: &StandardHandles<StdioSpec<'_>>,
    transfer: &mut HandleTransfer<'_>,
) -> io::Result<StandardHandles<PreparedStdio>> {
    Ok(StandardHandles {
        stdin: prepare_stdio(specs.stdin, StandardStream::Input, transfer)?,
        stdout: prepare_stdio(specs.stdout, StandardStream::Output, transfer)?,
        stderr: prepare_stdio(specs.stderr, StandardStream::Error, transfer)?,
    })
}

fn prepare_stdio(
    spec: StdioSpec<'_>,
    stream: StandardStream,
    transfer: &mut HandleTransfer<'_>,
) -> io::Result<PreparedStdio> {
    match spec {
        StdioSpec::Inherit => prepare_inherit(stream, transfer),
        StdioSpec::Null => prepare_null(stream, transfer),
        StdioSpec::Piped => prepare_pipe(stream, transfer),
        StdioSpec::Configured(stdio) => match &stdio.inner {
            StdioInner::Inherit => prepare_inherit(stream, transfer),
            StdioInner::Null => prepare_null(stream, transfer),
            StdioInner::Piped => prepare_pipe(stream, transfer),
            StdioInner::Owned(handle) => Ok(PreparedStdio {
                child: transfer.lower(handle.as_handle())?,
                parent: None,
            }),
        },
    }
}

fn prepare_inherit(
    stream: StandardStream,
    transfer: &mut HandleTransfer<'_>,
) -> io::Result<PreparedStdio> {
    match sys::standard_handle(stream)? {
        Some(handle) => Ok(PreparedStdio {
            child: transfer.lower(handle.as_handle())?,
            parent: None,
        }),
        None => Ok(PreparedStdio {
            child: sys::INVALID_RAW_HANDLE,
            parent: None,
        }),
    }
}

fn prepare_null(
    stream: StandardStream,
    transfer: &mut HandleTransfer<'_>,
) -> io::Result<PreparedStdio> {
    let access = match stream {
        StandardStream::Input => NullAccess::Read,
        StandardStream::Output | StandardStream::Error => NullAccess::Write,
    };
    let handle = sys::null_handle(access)?;
    Ok(PreparedStdio {
        child: transfer.lower(handle.as_handle())?,
        parent: None,
    })
}

fn prepare_pipe(
    stream: StandardStream,
    transfer: &mut HandleTransfer<'_>,
) -> io::Result<PreparedStdio> {
    let pipe = sys::create_pipe(!matches!(stream, StandardStream::Input))?;
    let child = transfer.lower(pipe.child.as_handle())?;
    Ok(PreparedStdio {
        child,
        parent: Some(pipe.parent),
    })
}

struct HandleTransfer<'a> {
    parent: Option<BorrowedHandle<'a>>,
    local: Vec<OwnedHandle>,
    remote: Vec<sys::RemoteHandle<'a>>,
    inherited: Vec<isize>,
}

impl<'a> HandleTransfer<'a> {
    fn new(parent: Option<BorrowedHandle<'a>>) -> Self {
        Self {
            parent,
            local: Vec::new(),
            remote: Vec::new(),
            inherited: Vec::new(),
        }
    }

    fn lower(&mut self, source: BorrowedHandle<'_>) -> io::Result<isize> {
        let value = if let Some(parent) = self.parent {
            let handle = sys::duplicate_remote(source, parent, true)?;
            let value = handle.value();
            self.remote.push(handle);
            value
        } else {
            let handle = sys::duplicate_local(source, true)?;
            let value = handle.as_raw_handle() as isize;
            self.local.push(handle);
            value
        };
        if !self.inherited.contains(&value) {
            self.inherited.push(value);
        }
        Ok(value)
    }

    fn parent(&self) -> Option<BorrowedHandle<'a>> {
        self.parent
    }

    fn inherited_values(&self) -> &[isize] {
        &self.inherited
    }
}

struct Environment {
    block: Option<Vec<u16>>,
    path: Option<OsString>,
}

#[derive(Debug)]
struct EnvKey {
    text: OsString,
    wide: Vec<u16>,
}

impl EnvKey {
    fn new(text: OsString) -> Self {
        let wide = text.encode_wide().collect();
        Self { text, wide }
    }
}

impl Ord for EnvKey {
    fn cmp(&self, other: &Self) -> Ordering {
        sys::compare_ordinal(&self.wide, &other.wide)
    }
}

impl PartialOrd for EnvKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for EnvKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for EnvKey {}

fn build_environment(
    command: &Command,
    transfer: &mut HandleTransfer<'_>,
) -> io::Result<Environment> {
    if !command.env_clear && command.env_ops.is_empty() {
        return Ok(Environment {
            block: None,
            path: None,
        });
    }

    let mut map: BTreeMap<EnvKey, OsString> = BTreeMap::new();
    if !command.env_clear {
        for (key, value) in sys::environment_strings()? {
            map.insert(EnvKey::new(key), value);
        }
    }
    for operation in &command.env_ops {
        match operation {
            EnvOp::Set(key, value) => {
                let value = match value {
                    EnvValue::Text(value) => value.clone(),
                    EnvValue::Handle(handle, format) => {
                        let handle = transfer.lower(handle.as_handle())?;
                        OsString::from(match format {
                            HandleFormat::Decimal => handle.to_string(),
                            HandleFormat::LowerHex => format!("{handle:x}"),
                        })
                    }
                };
                match map.entry(EnvKey::new(key.clone())) {
                    Entry::Occupied(mut entry) => {
                        entry.insert(value);
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(value);
                    }
                }
            }
            EnvOp::Remove(key) => {
                map.remove(&EnvKey::new(key.clone()));
            }
        }
    }

    let path = map.get(&EnvKey::new(OsString::from("PATH"))).cloned();
    let mut block = Vec::new();
    for (key, value) in map {
        block.extend(key.text.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(Environment {
        block: Some(block),
        path,
    })
}

fn build_command_line(
    command: &Command,
    transfer: &mut HandleTransfer<'_>,
) -> io::Result<Vec<u16>> {
    let mut result = Vec::new();
    result.push(QUOTE);
    result.extend(command.program.encode_wide());
    result.push(QUOTE);
    for argument in &command.args {
        result.push(SPACE);
        match argument {
            Arg::Text(text) => append_regular_arg(&mut result, text),
            Arg::Raw(text) => result.extend(text.encode_wide()),
            Arg::Handle(handle) => {
                let text = OsString::from(transfer.lower(handle.as_handle())?.to_string());
                append_regular_arg(&mut result, &text);
            }
        }
    }
    result.push(0);
    Ok(result)
}

fn append_regular_arg(command: &mut Vec<u16>, argument: &OsStr) {
    let bytes = argument.as_encoded_bytes();
    let quote = bytes.is_empty()
        || bytes
            .iter()
            .any(|byte| matches!(*byte, b' ' | b'\t' | b'"' | b'|'));
    if quote {
        command.push(QUOTE);
    }
    let mut backslashes = 0_usize;
    for unit in argument.encode_wide() {
        if unit == BACKSLASH {
            backslashes += 1;
        } else {
            if unit == QUOTE {
                command.extend(iter::repeat(BACKSLASH).take(backslashes + 1));
            }
            backslashes = 0;
        }
        command.push(unit);
    }
    if quote {
        command.extend(iter::repeat(BACKSLASH).take(backslashes));
        command.push(QUOTE);
    }
}

fn resolve_executable(program: &OsStr, child_path: Option<&OsStr>) -> io::Result<Vec<u16>> {
    let path = Path::new(program);
    let has_exe_suffix = program
        .as_encoded_bytes()
        .get(program.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(b".exe"));
    let is_file_name = path.file_name() == Some(program);

    if !is_file_name {
        if has_exe_suffix {
            return wide_nul(program);
        }
        let mut appended = program.to_os_string();
        appended.push(".exe");
        let appended_wide = wide_nul(&appended)?;
        if sys::program_exists(&appended_wide) {
            return Ok(appended_wide);
        }
        return wide_nul(program);
    }

    let has_extension = program.as_encoded_bytes().contains(&b'.');
    let search = |mut directory: PathBuf| -> Option<Vec<u16>> {
        directory.push(program);
        if !has_extension {
            directory.set_extension("exe");
        }
        let wide = wide_nul(directory.as_os_str()).ok()?;
        sys::program_exists(&wide).then_some(wide)
    };

    if let Some(paths) = child_path {
        for directory in env::split_paths(paths).filter(|path| !path.as_os_str().is_empty()) {
            if let Some(found) = search(directory) {
                return Ok(found);
            }
        }
    }
    if let Ok(mut application) = env::current_exe() {
        application.pop();
        if let Some(found) = search(application) {
            return Ok(found);
        }
    }
    if let Some(found) = search(PathBuf::from(sys::system_directory()?)) {
        return Ok(found);
    }
    if let Some(found) = search(PathBuf::from(sys::windows_directory()?)) {
        return Ok(found);
    }
    if let Some(paths) = env::var_os("PATH") {
        for directory in env::split_paths(&paths).filter(|path| !path.as_os_str().is_empty()) {
            if let Some(found) = search(directory) {
                return Ok(found);
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "program not found"))
}

fn wide_nul(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = value.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "string contains an interior NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::windows::ffi::OsStringExt;

    use super::*;

    fn decode(value: &[u16]) -> String {
        String::from_utf16_lossy(value.strip_suffix(&[0]).unwrap_or(value))
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

        #[track_caller]
        fn assert_exited(&mut self, message: &str) {
            match sys::wait_process_for_test(self.process.as_handle(), 5_000) {
                Ok(true) => self.armed = false,
                Ok(false) => panic!("{message}"),
                Err(error) => panic!("{message}: {error}"),
            }
        }
    }

    impl Drop for ProcessExitGuard {
        fn drop(&mut self) {
            if self.armed {
                sys::cleanup_process_for_test(self.process.as_handle());
            }
        }
    }

    #[test]
    fn quotes_regular_and_preserves_raw_arguments() {
        let mut command = Command::new("program.exe");
        command.arg("a b").arg("a\"b").raw_arg("x&&y");
        let mut transfer = HandleTransfer::new(None);
        let line = build_command_line(&command, &mut transfer).unwrap();
        assert_eq!(decode(&line), r#""program.exe" "a b" "a\"b" x&&y"#);
    }

    #[test]
    fn executable_search_finds_system_command_without_current_directory() {
        let command = resolve_executable(OsStr::new("cmd"), None).unwrap();
        assert!(decode(&command).to_ascii_lowercase().ends_with("cmd.exe"));
    }

    #[test]
    fn cleared_environment_is_double_nul() {
        let mut command = Command::new("cmd.exe");
        command.env_clear();
        let mut transfer = HandleTransfer::new(None);
        let environment = build_environment(&command, &mut transfer).unwrap();
        assert_eq!(environment.block.unwrap(), vec![0, 0]);
    }

    #[test]
    fn environment_merges_case_insensitively_and_removes_entries() {
        let mut command = Command::new("cmd.exe");
        command
            .env("Path", "first")
            .env("PATH", "second")
            .env("REMOVE_ME", "value")
            .env_remove("remove_me");
        let mut transfer = HandleTransfer::new(None);
        let environment = build_environment(&command, &mut transfer).unwrap();
        assert_eq!(environment.path, Some(OsString::from("second")));
        let block = environment.block.unwrap();
        let text = String::from_utf16_lossy(&block);
        assert!(!text.contains("first"));
        assert!(!text.contains("REMOVE_ME"));

        let lower = EnvKey::new(OsString::from("alpha"));
        let upper = EnvKey::new(OsString::from("ALPHA"));
        assert_eq!(lower, upper);
        assert_eq!(lower.partial_cmp(&upper), Some(Ordering::Equal));
        assert_ne!(lower, EnvKey::new(OsString::from("beta")));
    }

    #[test]
    fn environment_preserves_windows_ordinal_distinctions() {
        let mut command = Command::new("cmd.exe");
        command
            .env_clear()
            .env("S", "latin-s")
            .env("ſ", "long-s")
            .env("Μ", "greek-mu")
            .env("µ", "micro-sign");
        let mut transfer = HandleTransfer::new(None);
        let block = build_environment(&command, &mut transfer)
            .unwrap()
            .block
            .unwrap();
        let entries: Vec<String> = block
            .split(|unit| *unit == 0)
            .filter(|entry| !entry.is_empty())
            .map(String::from_utf16_lossy)
            .collect();

        assert_eq!(entries.len(), 4, "Windows-distinct keys were overwritten");
        for expected in ["S=latin-s", "ſ=long-s", "Μ=greek-mu", "µ=micro-sign"] {
            assert!(entries.iter().any(|entry| entry == expected));
        }
        assert_ne!(
            EnvKey::new(OsString::from("S")),
            EnvKey::new(OsString::from("ſ"))
        );
        assert_ne!(
            EnvKey::new(OsString::from("Μ")),
            EnvKey::new(OsString::from("µ"))
        );
    }

    #[test]
    fn quoting_covers_empty_and_trailing_backslashes() {
        let mut command = Command::new("program.exe");
        command.arg("").arg(r"C:\path with spaces\");
        let mut transfer = HandleTransfer::new(None);
        let line = decode(&build_command_line(&command, &mut transfer).unwrap());
        assert_eq!(line, r#""program.exe" "" "C:\path with spaces\\""#);
    }

    #[test]
    fn executable_resolution_covers_explicit_child_path_and_not_found() {
        let system = PathBuf::from(sys::system_directory().unwrap());
        let executable = system.join("cmd.exe");
        assert_eq!(
            decode(&resolve_executable(executable.as_os_str(), None).unwrap()),
            executable.to_string_lossy()
        );
        let without_extension = system.join("cmd");
        assert_eq!(
            decode(&resolve_executable(without_extension.as_os_str(), None).unwrap()),
            executable.to_string_lossy()
        );
        assert!(decode(
            &resolve_executable(OsStr::new("cmd.exe"), Some(system.as_os_str())).unwrap()
        )
        .to_ascii_lowercase()
        .ends_with("cmd.exe"));

        let missing = format!("windows-spawn-missing-{}.exe", std::process::id());
        assert_eq!(
            resolve_executable(OsStr::new(&missing), Some(OsStr::new("")))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        let empty_path_probe = format!("windows-spawn-empty-path-{}.exe", std::process::id());
        let empty_path_probe_path = env::current_dir().unwrap().join(&empty_path_probe);
        let probe = File::create(&empty_path_probe_path).unwrap();
        assert_eq!(
            resolve_executable(OsStr::new(&empty_path_probe), Some(OsStr::new(";")))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        drop(probe);
        std::fs::remove_file(empty_path_probe_path).unwrap();
        let nul = OsString::from_wide(&[u16::from(b'x'), 0]);
        assert_eq!(
            wide_nul(&nul).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn uncommitted_running_and_suspended_transactions_roll_back() {
        let mut running_command = Command::new("cmd.exe");
        running_command.args(["/D", "/C", "ping -n 10 127.0.0.1 >nul"]);
        let running = SpawnPlan::new_running(
            &running_command,
            crate::SpawnOptions::new(),
            crate::plan::IoMode::Spawn,
        )
        .unwrap();
        let running_transaction = SpawnTransaction::new(&running).unwrap();
        let mut running_process = ProcessExitGuard::new(
            sys::duplicate_local(
                running_transaction
                    .created
                    .as_ref()
                    .unwrap()
                    .process
                    .as_handle(),
                false,
            )
            .unwrap(),
        );
        drop(running_transaction);
        running_process.assert_exited("rollback did not terminate its running process");

        let mut suspended_command = Command::new("cmd.exe");
        suspended_command.args(["/D", "/C", "ping -n 10 127.0.0.1 >nul"]);
        let suspended = SpawnPlan::new_suspended(
            &suspended_command,
            crate::SpawnOptions::new(),
            crate::plan::IoMode::Spawn,
        )
        .unwrap();
        let suspended_transaction = SpawnTransaction::new(&suspended).unwrap();
        let mut suspended_process = ProcessExitGuard::new(
            sys::duplicate_local(
                suspended_transaction
                    .created
                    .as_ref()
                    .unwrap()
                    .process
                    .as_handle(),
                false,
            )
            .unwrap(),
        );
        drop(suspended_transaction);
        suspended_process.assert_exited("rollback did not terminate its suspended process");
    }

    #[test]
    fn suspended_child_drop_terminates_the_process() {
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/C", "exit /b 0"]);
        let plan = SpawnPlan::new_suspended(
            &command,
            crate::SpawnOptions::new(),
            crate::plan::IoMode::Spawn,
        )
        .unwrap();
        let transaction = SpawnTransaction::new(&plan).unwrap();
        let mut process = ProcessExitGuard::new(
            sys::duplicate_local(
                transaction.created.as_ref().unwrap().process.as_handle(),
                false,
            )
            .unwrap(),
        );
        drop(transaction.commit_suspended());
        process.assert_exited("dropping SuspendedChild did not terminate the process");
    }
}
