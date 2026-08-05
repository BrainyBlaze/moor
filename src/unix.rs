use crate::cli::{CreateMode, Options};
use crate::name;
use crate::runtime::client::{Client as WireClient, CommandResult, missing, probe_session};
use crate::runtime::holder::{Native, NativeExit, Runtime};
use crate::runtime::io::{Duplex, InputConfig, InputState, attach_viewer_to, run_viewer_input};
use crate::runtime::private as shared;
use crate::store::{Kind, Store};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{
    GenericFilePath, Listener as LocalListener, ListenerNonblockingMode, ListenerOptions, Name,
    Stream as LocalStream,
};
use interprocess::os::unix::local_socket::ListenerOptionsExt;
use path_absolutize::Absolutize;
use signal_hook::iterator::Signals;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, String>;

trait Text<T> {
    fn text(self) -> Result<T>;
}

impl<T, E: std::fmt::Display> Text<T> for std::result::Result<T, E> {
    fn text(self) -> Result<T> {
        self.map_err(|error| error.to_string())
    }
}

fn owned(meta: &fs::Metadata) -> bool {
    meta.uid() == uid()
}

pub(crate) fn protected(meta: &fs::Metadata, mode: u32) -> bool {
    owned(meta) && meta.mode() & 0o777 == mode
}

pub(crate) fn file_id(meta: &fs::Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

fn window(rows: u16, columns: u16) -> libc::winsize {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    size.ws_row = rows;
    size.ws_col = columns;
    size
}

fn private_dir(path: &Path, invalid: impl FnOnce() -> String) -> Result<()> {
    crate::store::private_directory(path, true)
        .text()?
        .then_some(())
        .ok_or_else(invalid)
}

macro_rules! syscall {
    ($valid:expr) => {
        crate::ensure!($valid, io::Error::last_os_error().to_string())
    };
}

crate::schema!(struct Config<'a> fields; path: &'a Path, publish: PathBuf, _parent: File, stage: PathBuf, command: Vec<OsString>, options: &'a Options,
    invoked: &'a OsStr, terminal: (Option<libc::termios>, libc::winsize), stderr: Option<File>, instrument: Option<File>);
crate::schema!(struct UnixNative fields; control: File, group: i32, child: Child);
crate::schema!(struct ViewerTerminal derive [Clone, Copy] fields; fd: i32, saved: libc::termios);
crate::schema!(struct Instrument fields; read: File, write: File, stage: File, path: PathBuf, identity: (u64, u64), hash: [u8; 32],
    nonce: [u8; 16]);

struct RawTerminal(ViewerTerminal);
struct Stage(PathBuf);
struct SetupError(String, bool);

impl From<String> for SetupError {
    fn from(error: String) -> Self {
        Self(error, false)
    }
}

pub(crate) fn clock() -> Result<(u64, [u8; 16])> {
    Ok((monotonic()?, boot_id()?))
}

pub(crate) fn create(
    mode: CreateMode,
    path: &Path,
    command: Vec<OsString>,
    options: &Options,
    invoked: &OsStr,
) -> CommandResult<i32> {
    let interactive = matches!(
        mode,
        CreateMode::Bare | CreateMode::New | CreateMode::LegacyA | CreateMode::LegacyC
    );
    let terminal = terminal_config(interactive)?;
    let stderr = options.stderr.as_deref().map(open_stderr).transpose()?;
    let instrument = options
        .instrument
        .as_deref()
        .map(open_instrument)
        .transpose()?;
    let command = if command.is_empty() {
        vec![
            std::env::var_os("SHELL")
                .filter(|shell| !shell.is_empty())
                .or_else(|| {
                    nix::unistd::User::from_uid(nix::unistd::Uid::effective())
                        .ok()
                        .flatten()
                        .map(|user| user.shell.into_os_string())
                        .filter(|shell| !shell.is_empty())
                })
                .unwrap_or_else(|| "/bin/sh".into()),
        ]
    } else {
        command
    };
    let stage = stage_path(invoked)?;
    let (_stage_parent, listener) = socket_at(&stage, |name| {
        ListenerOptions::new()
            .name(name)
            .mode(0o600)
            .reclaim_name(false)
            .nonblocking(ListenerNonblockingMode::Accept)
            .create_sync()
    })?;
    let (parent, publish) = socket_alias(path)?;
    let config = Config {
        path,
        publish,
        _parent: parent,
        stage,
        command,
        options,
        invoked,
        terminal,
        stderr,
        instrument,
    };
    crate::return_if!(
        matches!(mode, CreateMode::Run | CreateMode::LegacyRun),
        Ok(holder(config, listener, None)?)
    );
    let (parent, child) = UnixStream::pair().text()?;
    let pid = unsafe { libc::fork() };
    syscall!(pid >= 0);
    if pid == 0 {
        drop(parent);
        unsafe { libc::setsid() };
        let status = holder(config, listener, Some(child)).unwrap_or(1);
        unsafe { libc::_exit(status) }
    }
    drop(child);
    drop(listener);
    let (result, _) = shared::await_launch_probe(parent, |generation| {
        bounded(path, Duration::from_millis(250)).is_ok_and(|client| {
            let published = client.generation == generation;
            client.cancel();
            published
        })
    })?;
    Ok(i32::from(result))
}

fn stage_path(invoked: &OsStr) -> Result<PathBuf> {
    let directory = root(invoked)?.join(".moor-stage");
    private_dir(&directory, || "invalid staging directory".into())?;
    Ok(directory.join(hex(shared::random_array()?)))
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        let Some(parent) = self.0.parent() else {
            return;
        };
        let _ = fs::remove_dir(parent);
    }
}

impl Native for UnixNative {
    fn resize(&mut self, rows: u16, columns: u16) -> Result<()> {
        let size = window(rows, columns);
        syscall!(unsafe { libc::ioctl(self.control.as_raw_fd(), libc::TIOCSWINSZ, &size) } >= 0);
        Ok(())
    }

    fn terminate(&mut self, force: bool) -> (u8, bool) {
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        let foreground = unsafe { libc::tcgetpgrp(self.control.as_raw_fd()) };
        let containment = [foreground, self.group]
            .into_iter()
            .position(|group| group > 0 && unsafe { libc::kill(-group, signal) } == 0)
            .map_or(0, |index| index as u8 + 1);
        (containment, false)
    }

    fn exited(&mut self) -> Result<Option<NativeExit>> {
        self.child
            .try_wait()
            .text()
            .map(|status| status.map(native_exit))
    }
}

fn holder(
    mut config: Config<'_>,
    listener: LocalListener,
    ready: Option<UnixStream>,
) -> Result<i32> {
    let mut ready = shared::LaunchReporter {
        output: ready,
        generation: 1,
    };
    let daemon = ready.output.is_some();
    let stage = Stage(std::mem::take(&mut config.stage));
    let (path, invoked) = (config.path, config.invoked);
    let mut signals = Signals::new([libc::SIGINT, libc::SIGTERM, libc::SIGHUP]).text()?;
    let mut handled = 0;
    let (mut state, running, early, generation) = match holder_setup(&mut config) {
        Ok(setup) => setup,
        Err(SetupError(error, child)) => {
            let status = if child { 127 } else { 1 };
            if child {
                let _ = write!(io::stderr(), "{}: {error}\r\n", name::program(invoked));
            } else if ready.output.is_some() {
                eprintln!("{}: {error}", name::program(invoked));
            }
            ready.notice(3, status);
            let _ = cleanup(path);
            return if child { Ok(127) } else { Err(error) };
        }
    };
    if let Some(observed) = early {
        let status = state.drive(|| None, || None)?.unwrap_or(observed);
        let (exit, _) = state.finish_exit(&running, status, None);
        if ready.output.is_none() {
            return Ok(exit);
        }
        eprintln!(
            "{}: child exited before session publication",
            name::program(invoked)
        );
        ready.generation = generation;
        ready.notice(3, 1);
        return Ok(1);
    }
    ready.generation = generation;
    ready.notice(1, 0);
    fs::rename(&stage.0, &config.publish).text()?;
    let marker = file_id(&fs::symlink_metadata(&config.publish).text()?);
    ready.notice(2, 0);
    if daemon {
        let null = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .unwrap();
        for fd in 0..=2 {
            unsafe { libc::dup2(null.as_raw_fd(), fd) };
        }
    }
    let Some(status) = state.drive(
        || {
            let stream = listener.accept().ok()?;
            let trusted = peer_owned(&stream);
            stream
                .set_send_timeout(Some(Duration::from_millis(250)))
                .ok()?;
            Some((Duplex::socket(stream, [], cancel).ok()?, trusted))
        },
        || {
            let received = signals.pending().count();
            handled += received;
            (received != 0).then_some(handled > 1)
        },
    )?
    else {
        return Ok(125);
    };
    let (exit, durable) = state.finish_exit(&running, status, None);
    let owned = fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.file_type().is_socket() && owned(&meta) && file_id(&meta) == marker);
    let unlinked = durable && owned && fs::remove_file(path).is_ok();
    state.retired(unlinked, false);
    Ok(exit)
}

fn holder_setup(
    config: &mut Config<'_>,
) -> std::result::Result<(Runtime<UnixNative>, String, Option<NativeExit>, u32), SetupError> {
    let (path, options, invoked) = (config.path, config.options, config.invoked);
    let (generation, supervised) = launch_generation(invoked)?;
    let incarnation = shared::random_array::<16>()?;
    let identity = identity(path)?;
    let semantic_token = options
        .events
        .is_some()
        .then(shared::random_array::<16>)
        .transpose()?
        .unwrap_or([0; 16]);
    let modes = config.terminal.0.map(nix::sys::termios::Termios::from);
    let pair = nix::pty::openpty(Some(&config.terminal.1), modes.as_ref()).text()?;
    let (master, slave): (File, File) = (pair.master.into(), pair.slave.into());
    shared::extend_ancestry(
        invoked,
        absolute(path)?,
        |bytes| Ok(OsString::from_vec(bytes.to_vec())),
        |value| value.as_bytes().to_vec(),
    )?;
    let synthetic = shared::terminal_environment(invoked);
    let mut command = std::mem::take(&mut config.command).into_iter();
    let executable = command.next().expect("child command");
    let mut process = Command::new(&executable);
    let stdin = slave.try_clone().text()?;
    let stderr = match config.stderr.take() {
        Some(stderr) => stderr,
        None => slave.try_clone().text()?,
    };
    process
        .args(command)
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(slave))
        .stderr(Stdio::from(stderr));
    if let Some(directory) = &options.directory {
        process.current_dir(directory);
    }
    let generation_key = shared::environment_key(invoked, "_GENERATION");
    process
        .env_remove(&generation_key)
        .env_remove("DESK_SESSION_GENERATION")
        .env_remove("DESK_SESSION_SEMANTIC_TOKEN")
        .env_remove("DESK_MOOR_LAUNCH_CHANNEL");
    if supervised {
        let value = generation.to_string();
        process
            .env(&generation_key, &value)
            .env("DESK_SESSION_GENERATION", value);
    }
    if semantic_token != [0; 16] {
        process.env("DESK_SESSION_SEMANTIC_TOKEN", hex(semantic_token));
    }
    let start_wall = shared::now();
    let (start_mono, boot) = clock()?;
    let event_path = options.events.as_deref();
    let encode_path = |path: &Path| absolute(path).map(|path| path.into_os_string().into_vec());
    let event_manifest = event_path.map(encode_path).transpose()?;
    let instrument_path = config
        .instrument
        .as_ref()
        .map(|_| {
            shared::instrument_stage(
                path.parent().ok_or("session path has no parent")?,
                &identity,
                generation,
                incarnation,
            )
        })
        .transpose()?;
    let instrument_manifest = instrument_path.as_deref().map(encode_path).transpose()?;
    let mut artifacts = shared::holder_artifacts(
        &identity,
        (supervised.then_some(generation), generation),
        incarnation,
        semantic_token,
        (start_wall, start_mono, boot),
        shared::ArtifactConfig {
            marker: path,
            event_path,
            encoding: "posix-bytes",
            event_identity: event_manifest.as_deref(),
            instrument_identity: instrument_manifest.as_deref(),
            event_layout: 2,
            log_cap: options.log_cap,
        },
    )?;
    let instrument = instrument_setup(
        config.instrument.take(),
        instrument_path.as_deref(),
        &mut process,
    )?;
    let inherited = instrument
        .as_ref()
        .map_or(-1, |instrument| instrument.write.as_raw_fd());
    unsafe {
        use std::os::unix::process::CommandExt;
        process.pre_exec(move || child_process(inherited));
    }
    let mut child = process.spawn().map_err(|error| {
        SetupError(
            format!("could not execute {}: {error}", name::render(&executable)),
            true,
        )
    })?;
    instrument_ack(instrument, child.id(), generation).inspect_err(|_| {
        let _ = child.kill();
        let _ = child.wait();
    })?;
    let reader = master.try_clone().text()?;
    let (pty, done_rx) = Duplex::tracked(reader, master.try_clone().text()?, 1 << 20);
    let cwd = absolute(options.directory.as_deref().unwrap_or(Path::new(".")))?;
    let pid = child.id();
    crate::wire::put_wide(&mut artifacts.status, cwd.as_os_str().as_bytes())
        .map_err(crate::protocol)?;
    artifacts.status.extend_from_slice(&pid.to_le_bytes());
    artifacts.status.extend_from_slice(&pid.to_le_bytes());
    artifacts
        .status
        .extend_from_slice(&shared::random_array::<16>()?);
    let exited = child.try_wait().text()?.map(native_exit);
    let running = artifacts.running.clone();
    let mut holder = artifacts.runtime(
        (pty, done_rx),
        (
            synthetic,
            UnixNative {
                control: master,
                group: pid as i32,
                child,
            },
        ),
    );
    holder.set_rows(config.terminal.1.ws_row);
    Ok((holder, running, exited, generation))
}

fn native_exit(status: ExitStatus) -> NativeExit {
    use std::os::unix::process::ExitStatusExt;
    status.code().map_or_else(
        || NativeExit::Signal(status.signal().unwrap_or_default() as u32),
        |code| NativeExit::Code(code as u32),
    )
}

impl ViewerTerminal {
    fn detect(fd: i32) -> Option<Self> {
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        crate::return_if!(unsafe { libc::tcgetattr(fd, &mut saved) } != 0, None);
        Some(Self { fd, saved })
    }

    fn apply(&self, modes: &libc::termios) -> Result<()> {
        syscall!(unsafe { libc::tcsetattr(self.fd, libc::TCSAFLUSH, modes) } == 0);
        Ok(())
    }

    fn size(&self) -> Option<(u16, u16)> {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        (unsafe { libc::ioctl(self.fd, libc::TIOCGWINSZ, &mut size) } == 0
            && (size.ws_row == 0) == (size.ws_col == 0))
            .then_some((size.ws_row, size.ws_col))
    }

    fn suspend(&self) {
        let _ = self.apply(&self.saved);
        unsafe { libc::kill(libc::getpid(), libc::SIGTSTP) };
        let _ = self.raw();
    }

    fn raw(&self) -> Result<()> {
        let mut modes = self.saved;
        unsafe { libc::cfmakeraw(&mut modes) };
        self.apply(&modes)
    }

    fn guard(self) -> Result<RawTerminal> {
        self.raw()?;
        Ok(RawTerminal(self))
    }
}
impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = self.0.apply(&self.0.saved);
    }
}

fn bounded(path: &Path, timeout: Duration) -> Result<WireClient> {
    let (_parent, stream) = socket_at(path, LocalStream::connect)?;
    crate::ensure!(peer_owned(&stream), "holder peer identity mismatch");
    let deadline = Instant::now() + timeout;
    stream
        .set_send_timeout(Some(Duration::from_millis(250)))
        .text()?;
    WireClient::from_stream(stream, identity(path)?, deadline, cancel)
}

pub(crate) fn connect(path: &Path) -> Result<WireClient> {
    bounded(path, Duration::from_secs(2))
}

pub(crate) fn attach(path: &Path, options: Options) -> CommandResult<i32> {
    // §13.1 gives attach without a controlling terminal status 1; the Windows
    // path already refused it. Proceeding produced a viewer that wrote the
    // preamble to a pipe and detached at EOF.
    let terminal = ViewerTerminal::detect(libc::STDIN_FILENO)
        .ok_or_else(|| String::from("no controlling terminal"))?;
    let geometry = terminal.size().unwrap_or((0, 0));
    let terminal = Some(terminal);
    let _raw = terminal.map(ViewerTerminal::guard).transpose()?;
    let mut client = connect(path).map_err(|_| missing(path))?;
    let (detach, pass_suspend) = (options.detach, options.pass_suspend);
    let mut output = io::stdout();
    Ok(attach_viewer_to(
        &mut client,
        &options,
        geometry,
        &mut output,
        Duration::from_secs(15),
        |_| connect(path),
        |sender, detached| {
            thread::spawn(move || {
                run_viewer_input(
                    io::stdin(),
                    sender,
                    InputConfig {
                        detach,
                        pass_suspend,
                        state: detached,
                        last_size: terminal.and_then(|terminal| terminal.size()),
                    },
                    || match readable(libc::STDIN_FILENO, Duration::from_millis(50)) {
                        Ok(true) => InputState::Ready,
                        Ok(false) => InputState::Pending,
                        Err(_) => InputState::Closed,
                    },
                    || terminal.and_then(|terminal| terminal.size()),
                    || {
                        if let Some(terminal) = terminal {
                            terminal.suspend();
                        }
                    },
                    Instant::now,
                );
            });
        },
    )?)
}

fn inspect(path: &Path, status: bool, timeout: Duration) -> shared::SessionState {
    probe_session(
        path,
        status,
        || {
            fs::symlink_metadata(path)
                .is_ok_and(|meta| meta.file_type().is_socket() && owned(&meta))
        },
        || bounded(path, timeout).map_err(|error| error.contains("Connection refused")),
    )
}

pub(crate) fn classify(path: &Path) -> shared::SessionState {
    // Schema §9.3 freezes the identity exchange at 2 s. Truncating it made a
    // holder that answers between 250 ms and 2 s classify as indeterminate,
    // which every command then refuses.
    inspect(path, false, Duration::from_secs(2))
}

pub(crate) fn sessions(invoked: &OsStr, status: bool) -> Result<Vec<shared::SessionEntry>> {
    let root = root(invoked)?;
    shared::discover_sessions(
        &root,
        |name| shared::session_name(name, false),
        // OB-8 bounds the whole listing at 2 s, so each entry keeps its own
        // short slice of that budget rather than the full exchange deadline.
        |path, remaining| inspect(path, status, remaining.min(Duration::from_millis(250))),
    )
}

pub(crate) fn current_paths(invoked: &OsStr) -> Result<Vec<PathBuf>> {
    shared::ancestry_paths(invoked, |bytes| Ok(OsString::from_vec(bytes.to_vec())))
}

pub(crate) fn resolve(session: &OsStr, invoked: &OsStr) -> Result<PathBuf> {
    let path = PathBuf::from(session);
    if session.as_bytes().contains(&b'/') {
        absolute(&path)
    } else {
        Ok(root(invoked)?.join(path))
    }
}

fn root(invoked: &OsStr) -> Result<PathBuf> {
    let base = Path::new(invoked)
        .file_name()
        .filter(|name| !name.is_empty())
        .unwrap_or(OsStr::new("moor"));
    let mut directory = OsString::from(".");
    directory.push(base);
    directory.push(format!("-{}", uid()));
    let root = std::env::temp_dir().join(directory);
    private_dir(&root, || {
        format!("session root '{}' is not owner-only", root.display())
    })?;
    Ok(root)
}

fn absolute(path: &Path) -> Result<PathBuf> {
    path.absolutize().map(|path| path.into_owned()).text()
}

fn identity(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = absolute(path)?.into_os_string().into_vec();
    bytes.insert(0, 1);
    Ok(bytes)
}

fn socket_alias(path: &Path) -> Result<(File, PathBuf)> {
    let parent = path.parent().ok_or("rendezvous has no parent")?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(parent)
        .text()?;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let base = format!("/proc/self/fd/{}", file.as_raw_fd());
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let base = format!("/dev/fd/{}", file.as_raw_fd());
    Ok((
        file,
        PathBuf::from(base).join(path.file_name().ok_or("rendezvous has no name")?),
    ))
}

fn socket_at<T, E: std::fmt::Display>(
    path: &Path,
    open: impl FnOnce(Name<'_>) -> std::result::Result<T, E>,
) -> Result<(File, T)> {
    let (parent, alias) = socket_alias(path)?;
    let name = alias.as_os_str().to_fs_name::<GenericFilePath>().text()?;
    Ok((parent, open(name).text()?))
}

fn peer_owned(stream: &LocalStream) -> bool {
    stream
        .peer_creds()
        .ok()
        .and_then(|credentials| credentials.euid())
        == Some(uid())
}

pub(crate) fn cleanup(path: &Path) -> Result<()> {
    let observed = match fs::symlink_metadata(path) {
        Ok(meta) => Some(file_id(&meta)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    crate::ensure!(
        matches!(
            inspect(path, false, Duration::from_millis(250)),
            shared::SessionState::Missing
                | shared::SessionState::Stale
                | shared::SessionState::Exited
        ),
        "rendezvous changed before cleanup"
    );
    let expected_identity = identity(path)?;
    let (external, expected) = shared::cleanup_artifacts(path, Some(&expected_identity), |bytes| {
        Some(PathBuf::from(OsString::from_vec(bytes)))
    });
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            crate::ensure!(
                meta.file_type().is_socket() && owned(&meta) && observed == Some(file_id(&meta)),
                "unowned rendezvous cannot be removed"
            );
            fs::remove_file(path).text()?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound && observed.is_none() => {}
        Err(error) => return Err(error.to_string()),
    }
    shared::cleanup_companions(path, external, true, |target| {
        Store::read_only(target, Kind::Event, None).is_ok()
            || fs::symlink_metadata(target).is_ok_and(|meta| {
                target.extension() == Some(OsStr::new("instrument"))
                    && target
                        .file_stem()
                        .and_then(OsStr::to_str)
                        .is_some_and(|name| {
                            name.len() == 64 && shared::lowercase_hex(name.as_bytes())
                        })
                    && expected.as_deref() == Some(target)
                    && meta.file_type().is_file()
                    && protected(&meta, 0o500)
            })
    })?;
    Ok(())
}

fn launch_generation(invoked: &OsStr) -> Result<(u32, bool)> {
    shared::supervised_generation(
        invoked,
        true,
        "supervised launch record is invalid",
        |selector| {
            let fd = selector
                .to_str()
                .and_then(crate::canonical_u64)
                .and_then(|fd| i32::try_from(fd).ok())
                .ok_or("supervised launch selector is malformed")?;
            shared::decode_launch_record(&fixed_record::<32>(unsafe { File::from_raw_fd(fd) })?)
                .ok_or_else(|| "supervised launch record is invalid".into())
        },
    )
}

fn fixed_record<const N: usize>(mut file: File) -> Result<[u8; N]> {
    let fd = file.as_raw_fd();
    syscall!(unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } >= 0);
    shared::fixed_record(
        &mut file,
        "private launch channel",
        "private launch record has wrong length",
        true,
        |remaining| readable(fd, remaining).map(|ready| Some(usize::from(ready) * usize::MAX)),
    )
}

fn terminal_config(interactive: bool) -> Result<(Option<libc::termios>, libc::winsize)> {
    crate::return_if!(!interactive, Ok((None, window(24, 80))));
    let terminal = ViewerTerminal::detect(libc::STDIN_FILENO).ok_or("no controlling terminal")?;
    let (rows, columns) = terminal
        .size()
        .filter(|(rows, columns)| *rows != 0 && *columns != 0)
        .ok_or("no controlling terminal")?;
    Ok((Some(terminal.saved), window(rows, columns)))
}

fn child_process(inherited: i32) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let signal_max = libc::SIGRTMAX();
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let signal_max = 31;
    unsafe {
        let mut empty: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut empty);
        if libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
        for signal in 1..=signal_max {
            if signal != libc::SIGKILL
                && signal != libc::SIGSTOP
                && libc::signal(signal, libc::SIG_DFL) == libc::SIG_ERR
                && io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL)
            {
                return Err(io::Error::last_os_error());
            }
        }
        close_fds::set_fds_cloexec(
            3,
            if inherited >= 3 {
                std::slice::from_ref(&inherited)
            } else {
                &[]
            },
        );
        if inherited >= 3 && libc::fcntl(inherited, libc::F_SETFD, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn open_stderr(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .append(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .text()?;
    let meta = file.metadata().text()?;
    crate::ensure!(
        meta.file_type().is_file() && protected(&meta, 0o600),
        format!(
            "stderr sink '{}' is not a protected regular file",
            path.display()
        )
    );
    Ok(file)
}

fn open_instrument(path: &Path) -> Result<File> {
    let raw = path.as_os_str().as_bytes();
    let bad_path = raw.iter().any(|byte| {
        *byte == b':'
            || cfg!(not(target_os = "macos")) && (*byte == b'$' || byte.is_ascii_whitespace())
    });
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .text()?;
    let meta = file.metadata().text()?;
    crate::ensure!(
        path.is_absolute()
            && !bad_path
            && meta.file_type().is_file()
            && owned(&meta)
            && meta.mode() & 0o022 == 0,
        "instrumentation object is not a protected loadable path"
    );
    Ok(file)
}

fn instrument_setup(
    source: Option<File>,
    stage: Option<&Path>,
    process: &mut Command,
) -> Result<Option<Instrument>> {
    let Some(mut source) = source else {
        return Ok(None);
    };
    let stage = stage.ok_or("instrumentation stage is unavailable")?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o500)
        .open(stage)
        .text()?;
    let hash = shared::copy_digest(&mut source, Some(&mut output))?;
    output
        .set_permissions(fs::Permissions::from_mode(0o500))
        .text()?;
    output.sync_all().text()?;
    let identity = file_id(&output.metadata().text()?);
    drop(output);
    let mut staged = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(stage)
        .text()?;
    let reopened = staged.metadata().text()?;
    crate::ensure!(
        reopened.file_type().is_file()
            && protected(&reopened, 0o500)
            && file_id(&reopened) == identity
            && shared::copy_digest(&mut staged, None)? == hash,
        "instrumentation stage identity changed"
    );
    let (read, write) = nix::unistd::pipe().text()?;
    let nonce = shared::random_array::<16>()?;
    process.env(
        "DESK_MOOR_INSTRUMENT_CHANNEL",
        write.as_raw_fd().to_string(),
    );
    process.env("DESK_MOOR_INSTRUMENT_NONCE", hex(nonce));
    #[cfg(target_os = "macos")]
    let (loader, separator) = ("DYLD_INSERT_LIBRARIES", ":");
    #[cfg(not(target_os = "macos"))]
    let (loader, separator) = ("LD_PRELOAD", " ");
    let mut preload = stage.as_os_str().to_owned();
    if let Some(prior) = std::env::var_os(loader).filter(|value| !value.is_empty()) {
        preload.push(separator);
        preload.push(prior);
    }
    process.env(loader, preload);
    Ok(Some(Instrument {
        read: read.into(),
        write: write.into(),
        stage: staged,
        path: stage.into(),
        identity,
        hash,
        nonce,
    }))
}

fn instrument_ack(instrument: Option<Instrument>, pid: u32, generation: u32) -> Result<()> {
    let Some(mut instrument) = instrument else {
        return Ok(());
    };
    drop(instrument.write);
    shared::validate_instrument_ack(
        &fixed_record::<36>(instrument.read)?,
        true,
        generation,
        pid,
        instrument.nonce,
    )?;
    let meta = fs::symlink_metadata(instrument.path).text()?;
    crate::ensure!(
        meta.file_type().is_file()
            && file_id(&meta) == instrument.identity
            && shared::copy_digest(&mut instrument.stage, None)? == instrument.hash,
        "instrumentation stage identity changed"
    );
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn monotonic() -> Result<u64> {
    let mut clock: libc::timespec = unsafe { std::mem::zeroed() };
    syscall!(unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut clock) } == 0);
    Ok(clock.tv_sec as u64 * 1000 + clock.tv_nsec as u64 / 1_000_000)
}

#[cfg(target_os = "macos")]
fn monotonic() -> Result<u64> {
    #[repr(C)]
    #[derive(Default)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_continuous_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
    }
    let mut scale = Timebase::default();
    syscall!(unsafe { mach_timebase_info(&mut scale) } == 0 && scale.denom != 0);
    let nanos = u128::from(unsafe { mach_continuous_time() }) * u128::from(scale.numer)
        / u128::from(scale.denom);
    u64::try_from(nanos / 1_000_000).map_err(|_| "monotonic clock overflow".into())
}

#[cfg(not(target_os = "macos"))]
fn boot_id() -> Result<[u8; 16]> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .and_then(|text| crate::runtime::private::parse_boot_uuid(&text))
        .unwrap_or([0; 16]))
}

#[cfg(target_os = "macos")]
fn boot_id() -> Result<[u8; 16]> {
    let mut value: libc::timeval = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of_val(&value);
    syscall!(
        unsafe {
            libc::sysctlbyname(
                c"kern.boottime".as_ptr(),
                (&mut value as *mut libc::timeval).cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        } == 0
    );
    crate::ensure!(
        length == std::mem::size_of_val(&value)
            && value.tv_sec >= 0
            && (0..1_000_000).contains(&value.tv_usec),
        "boot identity is unavailable"
    );
    let mut out = [0; 16];
    out[..8].copy_from_slice(&(value.tv_sec as u64).to_le_bytes());
    out[8..12].copy_from_slice(&(value.tv_usec as u32).to_le_bytes());
    out[12..].copy_from_slice(b"MAC1");
    Ok(out)
}

fn hex(bytes: [u8; 16]) -> String {
    format!("{:032x}", u128::from_be_bytes(bytes))
}

fn cancel(stream: &LocalStream) {
    use std::os::fd::AsFd;
    let LocalStream::UdSocket(stream) = stream;
    unsafe { libc::shutdown(stream.as_fd().as_raw_fd(), libc::SHUT_RDWR) };
}

fn readable(fd: i32, timeout: Duration) -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN | libc::POLLHUP,
        revents: 0,
    };
    let result = unsafe {
        libc::poll(
            &mut descriptor,
            1,
            timeout.as_millis().min(i32::MAX as u128) as i32,
        )
    };
    if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
        Err(io::Error::last_os_error())
    } else {
        Ok(result > 0)
    }
}

fn uid() -> u32 {
    unsafe { libc::geteuid() }
}
