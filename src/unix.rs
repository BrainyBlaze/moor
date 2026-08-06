use crate::cli::{CreateMode, Options};
use crate::events::Cursor;
use crate::name;
use crate::runtime::client::{Client as WireClient, CommandResult, missing, probe_session};
use crate::runtime::holder::{Native, NativeExit, Runtime};
use crate::runtime::io::{Duplex, InputConfig, InputState, attach_viewer_to, run_viewer_input};
use crate::runtime::private as shared;
use crate::store::{Kind, PreparedStore, Store, StoreError};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{
    GenericFilePath, Listener as LocalListener, ListenerNonblockingMode, ListenerOptions, Name,
    Stream as LocalStream,
};
use interprocess::os::unix::local_socket::ListenerOptionsExt;
use path_absolutize::Absolutize;
use signal_hook::iterator::Signals;
use std::cell::Cell;
use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, String>;
type StoreResult<T> = std::result::Result<T, StoreError>;

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

crate::schema!(struct Config<'a> fields; path: &'a Path, root: PathBuf, launch: LaunchSeed, event: Option<EventTarget>, lifecycle: StoreTarget, log: Option<StoreTarget>, stage: Stage,
    command: Vec<OsString>, options: &'a Options, invoked: &'a OsStr, terminal: (Option<libc::termios>, libc::winsize), stderr: Option<File>, instrument: Option<PreparedInstrument>);
crate::schema!(struct UnixNative fields; control: File, group: i32, child: Child);
crate::schema!(struct ViewerTerminal derive [Clone, Copy] fields; fd: i32, saved: libc::termios);
crate::schema!(struct LaunchSeed fields; generation: u32, supervised: bool, incarnation: [u8; 16], semantic_token: [u8; 16], identity: Vec<u8>, start: (u64, u64, [u8; 16]));
crate::schema!(struct Instrument fields; read: File, write: File, stage: File, parent: File, leaf: OsString, identity: (u64, u64), hash: [u8; 32],
    nonce: [u8; 16]);
struct PreparedInstrument {
    source: File,
    stage: File,
    parent: File,
    leaf: OsString,
    path: PathBuf,
    identity: (u64, u64),
    armed: bool,
}

struct RawTerminal(ViewerTerminal);
struct ChildGuard(Option<Child>);
struct PendingEvent(PathBuf, File, OsString, Option<File>);
struct EventTarget {
    operand: PathBuf,
    target: StoreTarget,
}
struct StoreTarget {
    parent: File,
    leaf: OsString,
    directory: File,
    identity: (u64, u64),
    prepared: PreparedStore,
    validator: Option<Store>,
    exact_selection: bool,
    owned: bool,
    armed: bool,
}
struct Stage(File, OsString, Option<(u64, u64)>, bool);
struct SetupError(String, bool);

impl ChildGuard {
    fn child(&mut self) -> &mut Child {
        self.0.as_mut().unwrap()
    }

    fn release(mut self) -> Child {
        self.0.take().unwrap()
    }
}

impl Config<'_> {
    fn store_targets(&mut self) -> impl DoubleEndedIterator<Item = &mut StoreTarget> {
        [
            self.event.as_mut().map(|event| &mut event.target),
            Some(&mut self.lifecycle),
            self.log.as_mut(),
        ]
        .into_iter()
        .flatten()
    }

    fn retain_store_targets(&mut self) {
        self.store_targets().for_each(StoreTarget::retain);
    }

    fn retain_stores(&mut self) {
        self.retain_store_targets();
        if let Some(instrument) = self.instrument.as_mut() {
            instrument.retain();
        }
    }

    fn retain_artifacts(&mut self) {
        self.retain_stores();
        self.stage.retain();
    }

    fn rollback_artifacts(&mut self) {
        if let Some(instrument) = self.instrument.as_mut() {
            instrument.rollback();
        }
        self.store_targets().rev().for_each(StoreTarget::rollback);
        self.stage.rollback();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(child) = self.0.as_mut() else {
            return;
        };
        let group = i32::try_from(child.id()).unwrap_or_default();
        if group > 0 {
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl From<String> for SetupError {
    fn from(error: String) -> Self {
        Self(error, false)
    }
}

pub(crate) fn clock() -> Result<(u64, [u8; 16])> {
    Ok((monotonic()?, boot_id()?))
}

pub(crate) fn preflight_create(
    options: &Options,
    session: &OsStr,
    invoked: &OsStr,
) -> Result<PathBuf> {
    if let Some(event) = options.events.as_deref() {
        crate::ensure!(event.is_absolute(), event_rejection(event, "not-absolute"));
    }
    let marker = resolve(session, invoked)?;
    if let Some(event) = options.events.as_deref() {
        let resolved = absolute(event).map_err(|_| event_rejection(event, "io-error"))?;
        let root = root(invoked)?;
        crate::ensure!(
            resolved != root && resolved.starts_with(&root),
            event_rejection(event, "outside-root")
        );
        for other in [
            marker.clone(),
            shared::companion(&marker, ".log"),
            shared::companion(&marker, ".exit"),
        ] {
            validate_event_alias(event, &other)?;
        }
    }
    Ok(marker)
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
    let root = root(invoked)?;
    let event = validate_event_target(options.events.as_deref(), &root, path)?;
    let stderr = options.stderr.as_deref().map(open_stderr).transpose()?;
    let instrument_source = options
        .instrument
        .as_deref()
        .map(open_instrument)
        .transpose()?;
    let (generation, supervised) = launch_generation(invoked)?;
    let incarnation = shared::random_array::<16>()?;
    let identity = identity(path)?;
    let semantic_token = options
        .events
        .is_some()
        .then(shared::random_array::<16>)
        .transpose()?
        .unwrap_or([0; 16]);
    let start_wall = shared::now();
    let (start_mono, boot) = clock()?;
    let instrument_path = instrument_source
        .as_ref()
        .map(|_| shared::instrument_stage(&root, &identity, generation, incarnation))
        .transpose()?;
    if let (Some(event), Some(instrument)) = (options.events.as_deref(), instrument_path.as_deref())
    {
        validate_event_alias(event, instrument)?;
    }
    let launch = LaunchSeed {
        generation,
        supervised,
        incarnation,
        semantic_token,
        identity,
        start: (start_wall, start_mono, boot),
    };
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
    let (parent, publish) = socket_alias(path)?;
    let stage = Stage(
        parent,
        OsString::from(format!(".moor-{}.stage", hex(shared::random_array()?))),
        None,
        true,
    );
    let stage_alias = publish.with_file_name(&stage.1);
    let stage_name = stage_alias
        .as_os_str()
        .to_fs_name::<GenericFilePath>()
        .text()?;
    let listener = ListenerOptions::new()
        .name(stage_name)
        .mode(0o600)
        .reclaim_name(false)
        .nonblocking(ListenerNonblockingMode::Accept)
        .create_sync()
        .text()?;
    let mut stage = stage;
    stage.capture()?;
    let event = event.map(PendingEvent::prepare).transpose()?;
    let log = (options.log_cap != 0)
        .then(|| StoreTarget::prepare(&stage.0, &shared::companion(path, ".log")))
        .transpose()?;
    let lifecycle = StoreTarget::prepare(&stage.0, &shared::companion(path, ".exit"))?;
    let instrument = instrument_source
        .zip(instrument_path.as_deref())
        .map(|(source, path)| PreparedInstrument::prepare(source, &root, path))
        .transpose()?;
    let mut config = Config {
        path,
        root,
        launch,
        event,
        lifecycle,
        log,
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
    parent
        .set_read_timeout(Some(Duration::from_secs(10)))
        .text()?;
    config.retain_artifacts();
    let adopted = Cell::new(false);
    let launched = shared::await_launch_probe(
        parent,
        |generation| {
            bounded(path, Duration::from_millis(250)).is_ok_and(|client| {
                let published = client.generation == generation;
                client.cancel();
                published
            })
        },
        |_| adopted.set(true),
    );
    let mut stopped = false;
    if launched.is_err() {
        stopped = holder_exited(pid, Duration::ZERO);
        if !stopped {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
            stopped = holder_exited(pid, Duration::from_millis(250));
        }
    } else if !adopted.get() {
        stopped = holder_exited(pid, Duration::from_millis(250));
    }
    if !adopted.get() && stopped {
        config.rollback_artifacts();
    }
    let (result, _) = launched?;
    Ok(i32::from(result))
}

fn holder_exited(pid: libc::pid_t, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let mut status = 0;
        match unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } {
            observed if observed == pid => return true,
            0 if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => continue,
            _ => return false,
        }
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if self.3 {
            self.rollback();
        }
    }
}

impl Stage {
    fn capture(&mut self) -> Result<()> {
        let stat = stat_at(&self.0, &self.1).text()?;
        crate::ensure!(
            stat.st_mode & libc::S_IFMT == libc::S_IFSOCK,
            "staged rendezvous identity changed"
        );
        self.2 = Some(stat_identity(&stat));
        Ok(())
    }

    fn matches(&self) -> bool {
        self.2.is_some_and(|identity| {
            stat_at(&self.0, &self.1).ok().is_some_and(|stat| {
                stat.st_mode & libc::S_IFMT == libc::S_IFSOCK && stat_identity(&stat) == identity
            })
        })
    }

    fn revalidate(&self) -> Result<()> {
        crate::ensure!(self.matches(), "staged rendezvous identity changed");
        Ok(())
    }

    fn published_identity(&self, destination: &OsStr) -> Result<(u64, u64)> {
        let stat = stat_at(&self.0, destination).text()?;
        let identity = stat_identity(&stat);
        crate::ensure!(
            stat.st_mode & libc::S_IFMT == libc::S_IFSOCK && self.2 == Some(identity),
            "published rendezvous identity changed"
        );
        Ok(identity)
    }

    fn rollback_published(&self, destination: &OsStr) {
        if self.published_identity(destination).is_ok() {
            let _ = unlink_at(&self.0, destination);
            let _ = self.0.sync_all();
        }
    }

    fn retain(&mut self) {
        self.3 = false;
    }

    fn rollback(&mut self) {
        self.3 = false;
        if self.matches() {
            let _ = unlink_at(&self.0, &self.1);
        }
    }
}

impl Native for UnixNative {
    fn resize(&mut self, rows: u16, columns: u16) -> Result<()> {
        let size = window(rows, columns);
        syscall!(unsafe { libc::ioctl(self.control.as_raw_fd(), libc::TIOCSWINSZ, &size) } >= 0);
        Ok(())
    }

    fn redraw(&mut self, rows: u16, columns: u16) -> Result<()> {
        let mut prior = window(0, 0);
        syscall!(
            unsafe { libc::ioctl(self.control.as_raw_fd(), libc::TIOCGWINSZ, &mut prior) } >= 0
        );
        let unchanged = (prior.ws_row, prior.ws_col) == (rows, columns);
        self.resize(rows, columns)?;
        if unchanged {
            let foreground = unsafe { libc::tcgetpgrp(self.control.as_raw_fd()) };
            syscall!(
                [foreground, self.group]
                    .into_iter()
                    .any(|group| group > 0 && unsafe { libc::kill(-group, libc::SIGWINCH) } == 0)
            );
        }
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
    let (path, invoked) = (config.path, config.invoked);
    let mut signals = Signals::new([libc::SIGINT, libc::SIGTERM, libc::SIGHUP]).text()?;
    let mut handled = 0;
    let (mut state, running, early) = match holder_setup(&mut config, |generation| {
        ready.generation = generation;
        ready.notice(1, 0);
    }) {
        Ok(setup) => setup,
        Err(SetupError(error, child)) => {
            let status = if child { 127 } else { 1 };
            if child {
                let _ = write!(io::stderr(), "{}: {error}\r\n", name::program(invoked));
            } else if ready.output.is_some() {
                eprintln!("{}: {error}", name::program(invoked));
            }
            ready.notice(3, status);
            return if child { Ok(127) } else { Err(error) };
        }
    };
    let artifacts_valid = config
        .lifecycle
        .revalidate()
        .and_then(|()| {
            config
                .event
                .as_ref()
                .map(|event| event.revalidate(&config.root))
                .transpose()
                .map(|_| ())
        })
        .and_then(|()| {
            config
                .log
                .as_ref()
                .map(StoreTarget::revalidate)
                .transpose()
                .map(|_| ())
        });
    if let Err(error) = artifacts_valid {
        if let Some(observed) = wait_natural_exit(&mut state, Duration::from_millis(25))? {
            return finalize_unpublished_exit(
                &mut state,
                &running,
                observed,
                &mut config,
                &mut ready,
                invoked,
            );
        }
        let mut signal = Some(true);
        let stopped = state.drive(|| None, || signal.take())?.is_some();
        drop(state);
        let _ = stopped;
        if ready.output.is_some() {
            eprintln!("{}: {error}", name::program(invoked));
        }
        ready.notice(3, 1);
        return Err(error);
    }
    if let Some(observed) = early.or(state.observe_exit()?) {
        return finalize_unpublished_exit(
            &mut state,
            &running,
            observed,
            &mut config,
            &mut ready,
            invoked,
        );
    }
    let destination = path.file_name().ok_or("rendezvous has no name")?;
    let publication = config.stage.revalidate().and_then(|()| {
        publish_exclusive(&config.stage.0, &config.stage.1, destination)
            .and_then(|()| config.stage.published_identity(destination))
    });
    let marker = match publication {
        Ok(marker) => marker,
        Err(error) => {
            config.stage.rollback_published(destination);
            if let Some(observed) = wait_natural_exit(&mut state, Duration::from_millis(25))? {
                return finalize_unpublished_exit(
                    &mut state,
                    &running,
                    observed,
                    &mut config,
                    &mut ready,
                    invoked,
                );
            }
            let mut signal = Some(true);
            let stopped = state.drive(|| None, || signal.take())?.is_some();
            drop(state);
            let _ = stopped;
            ready.notice(3, 1);
            return Err(error);
        }
    };
    config.retain_artifacts();
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

fn finalize_unpublished_exit(
    state: &mut Runtime<UnixNative>,
    running: &str,
    observed: NativeExit,
    config: &mut Config<'_>,
    ready: &mut shared::LaunchReporter<UnixStream>,
    invoked: &OsStr,
) -> Result<i32> {
    let status = state.drive(|| None, || None)?.unwrap_or(observed);
    let (exit, durable) = state.finish_exit(running, status, None);
    if durable {
        config.retain_stores();
    }
    if ready.output.is_none() {
        return Ok(exit);
    }
    eprintln!(
        "{}: child exited before session publication",
        name::program(invoked)
    );
    ready.notice(3, 1);
    Ok(1)
}

fn wait_natural_exit(
    state: &mut Runtime<UnixNative>,
    timeout: Duration,
) -> Result<Option<NativeExit>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(observed) = state.observe_exit()? {
            return Ok(Some(observed));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(1));
    }
}

struct InitialStore<'a>(&'a StoreTarget, &'a Store, &'a [u8]);

fn initialize_stores(stores: &[InitialStore<'_>]) -> std::result::Result<(), (String, bool)> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut workers = Vec::with_capacity(stores.len());
    let mut failed = false;
    for store in stores {
        let mut descriptors = Vec::from(store.0.prepared.raw_descriptors());
        descriptors.extend(store.1.raw_descriptors());
        descriptors.extend([store.0.parent.as_raw_fd(), store.0.directory.as_raw_fd()]);
        descriptors.sort_unstable();
        descriptors.dedup();
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            failed = true;
            break;
        }
        if pid == 0 {
            unsafe { close_fds::close_open_fds(3, &descriptors) };
            let status = u8::from(store.0.initialize(store.1, store.2).is_err());
            unsafe { libc::_exit(i32::from(status)) }
        }
        workers.push((pid, None));
    }
    while workers.iter().any(|worker| worker.1.is_none()) && Instant::now() < deadline {
        for (pid, status) in &mut workers {
            if status.is_some() {
                continue;
            }
            let mut observed = 0;
            match unsafe { libc::waitpid(*pid, &mut observed, libc::WNOHANG) } {
                result if result == *pid => {
                    let success = libc::WIFEXITED(observed) && libc::WEXITSTATUS(observed) == 0;
                    *status = Some(success);
                    if !success {
                        failed = true;
                    }
                }
                -1 if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted => {
                    *status = Some(false);
                    failed = true;
                }
                _ => {}
            }
        }
        if failed {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    if !failed && workers.iter().all(|worker| worker.1 == Some(true)) {
        return Ok(());
    }
    for (pid, status) in &workers {
        if status.is_none() {
            unsafe { libc::kill(*pid, libc::SIGKILL) };
        }
    }
    let reap_deadline = Instant::now() + Duration::from_millis(250);
    while workers.iter().any(|worker| worker.1.is_none()) && Instant::now() < reap_deadline {
        for (pid, status) in &mut workers {
            if status.is_none() {
                let mut observed = 0;
                if unsafe { libc::waitpid(*pid, &mut observed, libc::WNOHANG) } == *pid {
                    *status = Some(false);
                }
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
    let confirmed = workers.iter().all(|worker| worker.1.is_some());
    Err((
        if failed {
            "store initialization failed".into()
        } else {
            "store initialization timed out".into()
        },
        confirmed,
    ))
}

fn holder_setup(
    config: &mut Config<'_>,
    mut adopt: impl FnMut(u32),
) -> std::result::Result<(Runtime<UnixNative>, String, Option<NativeExit>), SetupError> {
    let (path, options, invoked) = (config.path, config.options, config.invoked);
    let generation = config.launch.generation;
    let supervised = config.launch.supervised;
    let incarnation = config.launch.incarnation;
    let identity = config.launch.identity.clone();
    let semantic_token = config.launch.semantic_token;
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
    let (start_wall, start_mono, boot) = config.launch.start;
    let event_path = options.events.as_deref();
    let event_manifest = event_path.map(|path| path.as_os_str().as_bytes().to_vec());
    let instrument_path = config
        .instrument
        .as_ref()
        .map(|instrument| instrument.path.clone());
    if let (Some(event), Some(instrument)) = (event_path, instrument_path.as_deref()) {
        validate_event_alias(event, instrument)?;
    }
    let instrument_manifest = instrument_path
        .as_deref()
        .map(|path| path.as_os_str().as_bytes().to_vec());
    let running = shared::lifecycle_running(
        &identity,
        (supervised.then_some(generation), generation),
        incarnation,
        (start_wall, start_mono, boot),
        (
            "posix-bytes",
            event_manifest.as_deref(),
            instrument_manifest.as_deref(),
        ),
    );
    let event_initial = event_path.map(|_| {
        crate::events::canonical_header(
            start_wall,
            &STANDARD.encode(&identity),
            supervised.then_some(generation),
            Cursor(0, 0, 0, 1),
        )
    });
    let running_body = running.as_bytes();
    let lifecycle_store = config
        .lifecycle
        .lease(Kind::Exit, generation, running_body)
        .map_err(|error| format!("store lease failed: {error:?}"))?;
    let event_store = config
        .event
        .as_mut()
        .zip(event_initial.as_deref())
        .map(|(event, body)| event.lease(generation, body.as_bytes()))
        .transpose()?;
    let log_store = config
        .log
        .as_mut()
        .map(|log| {
            log.lease(Kind::Log, generation, &[])
                .map_err(|error| format!("store lease failed: {error:?}"))
        })
        .transpose()?;
    adopt(generation);
    let mut initial = vec![InitialStore(
        &config.lifecycle,
        &lifecycle_store,
        running_body,
    )];
    if let (Some(target), Some(store), Some(body)) = (
        config.event.as_ref(),
        event_store.as_ref(),
        event_initial.as_deref(),
    ) {
        initial.push(InitialStore(&target.target, store, body.as_bytes()));
    }
    if let (Some(target), Some(store)) = (config.log.as_ref(), log_store.as_ref()) {
        initial.push(InitialStore(target, store, &[]));
    }
    if let Err((error, rollback)) = initialize_stores(&initial) {
        if !rollback {
            config.retain_store_targets();
        }
        return Err(error.into());
    }
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
            event_store: None,
            stores: Some(shared::ArtifactStores {
                lifecycle: lifecycle_store,
                event: event_store,
                log: log_store,
            }),
            event_layout: 2,
            log_cap: options.log_cap,
        },
    )?;
    let instrument = instrument_setup(config.instrument.as_mut(), &mut process)?;
    if let Some(event) = config.event.as_ref() {
        event.revalidate(&config.root)?;
    }
    let inherited = instrument
        .as_ref()
        .map_or(-1, |instrument| instrument.write.as_raw_fd());
    unsafe {
        use std::os::unix::process::CommandExt;
        process.pre_exec(move || child_process(inherited));
    }
    let child = process.spawn().map_err(|error| {
        SetupError(
            format!("could not execute {}: {error}", name::render(&executable)),
            true,
        )
    })?;
    let mut child = ChildGuard(Some(child));
    instrument_ack(instrument, child.child().id(), generation)?;
    let reader = master.try_clone().text()?;
    let (pty, done_rx) = Duplex::tracked(reader, master.try_clone().text()?, 1 << 20);
    let cwd = absolute(options.directory.as_deref().unwrap_or(Path::new(".")))?;
    let pid = child.child().id();
    crate::wire::put_wide(&mut artifacts.status, cwd.as_os_str().as_bytes())
        .map_err(crate::protocol)?;
    artifacts.status.extend_from_slice(&pid.to_le_bytes());
    artifacts.status.extend_from_slice(&pid.to_le_bytes());
    artifacts
        .status
        .extend_from_slice(&shared::random_array::<16>()?);
    let exited = child.child().try_wait().text()?.map(native_exit);
    let running = artifacts.running.clone();
    let mut holder = artifacts.runtime(
        (pty, done_rx),
        (
            synthetic,
            UnixNative {
                control: master,
                group: pid as i32,
                child: child.release(),
            },
        ),
    );
    holder.set_geometry(config.terminal.1.ws_row, config.terminal.1.ws_col);
    Ok((holder, running, exited))
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

fn event_rejection(path: &Path, cause: &str) -> String {
    format!(
        "event store rejected: {} ({cause})",
        name::render(path.as_os_str())
    )
}

fn validate_event_alias(event: &Path, other: &Path) -> Result<()> {
    let resolved = absolute(event).map_err(|_| event_rejection(event, "io-error"))?;
    crate::ensure!(
        resolved != absolute(other).map_err(|_| event_rejection(event, "io-error"))?,
        event_rejection(event, "identity-changed")
    );
    Ok(())
}

fn validate_event_target(
    path: Option<&Path>,
    root: &Path,
    marker: &Path,
) -> Result<Option<PendingEvent>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let reject = |cause| event_rejection(path, cause);
    let resolved = absolute(path).map_err(|_| reject("io-error"))?;
    crate::ensure!(
        resolved != root && resolved.starts_with(root),
        reject("outside-root")
    );
    for other in [
        marker.to_owned(),
        shared::companion(marker, ".log"),
        shared::companion(marker, ".exit"),
    ] {
        validate_event_alias(path, &other)?;
    }
    let (parent, leaf, opened) = open_event_target(root, &resolved, path)?;
    if let Some(opened) = &opened {
        validate_event_directory(opened, path)?;
    }
    Ok(Some(PendingEvent(path.to_owned(), parent, leaf, opened)))
}

fn open_event_target(
    root: &Path,
    resolved: &Path,
    operand: &Path,
) -> Result<(File, OsString, Option<File>)> {
    let reject = |cause| event_rejection(operand, cause);
    let relative = resolved
        .strip_prefix(root)
        .map_err(|_| reject("outside-root"))?;
    let mut directory = open_directory(root).map_err(|_| reject("io-error"))?;
    let leaf = relative
        .file_name()
        .ok_or_else(|| reject("outside-root"))?
        .to_owned();
    for component in relative.parent().unwrap_or(Path::new("")).components() {
        match open_directory_at(&directory, component.as_os_str()) {
            Ok(opened) => directory = opened,
            Err(error) => {
                return Err(reject(component_cause(
                    &directory,
                    component.as_os_str(),
                    false,
                    &error,
                )));
            }
        }
    }
    let opened = match open_directory_at(&directory, &leaf) {
        Ok(opened) => Some(opened),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(reject(component_cause(&directory, &leaf, true, &error)));
        }
    };
    Ok((directory, leaf, opened))
}

fn validate_event_directory(directory: &File, operand: &Path) -> Result<()> {
    let reject = |cause| event_rejection(operand, cause);
    let meta = directory.metadata().map_err(|_| reject("io-error"))?;
    crate::ensure!(owned(&meta), reject("wrong-owner"));
    crate::ensure!(meta.mode() & 0o777 == 0o700, reject("wrong-mode"));
    if let Some(entry) = first_directory_entry(directory).map_err(|_| reject("io-error"))? {
        let slots = ["body.0", "body.1", "commit.0", "commit.1"];
        return Err(reject(if slots.iter().any(|name| entry == *name) {
            "pre-existing-slot"
        } else {
            "extra-entry"
        }));
    }
    Ok(())
}

fn first_directory_entry(directory: &File) -> io::Result<Option<OsString>> {
    directory_entries(directory, |name| {
        Ok(Some(OsString::from_vec(name.to_vec())))
    })
}

pub(crate) fn directory_entries<T, E>(
    directory: &File,
    mut visit: impl FnMut(&[u8]) -> std::result::Result<Option<T>, E>,
) -> std::result::Result<Option<T>, E>
where
    E: From<io::Error>,
{
    use std::ffi::CStr;
    let descriptor = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let duplicate = unsafe { File::from_raw_fd(descriptor) };
    let flags = unsafe { libc::fcntl(duplicate.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || flags & libc::O_NONBLOCK == 0
            && unsafe {
                libc::fcntl(
                    duplicate.as_raw_fd(),
                    libc::F_SETFL,
                    flags | libc::O_NONBLOCK,
                )
            } < 0
    {
        return Err(io::Error::last_os_error().into());
    }
    let descriptor = duplicate.into_raw_fd();
    let stream = unsafe { libc::fdopendir(descriptor) };
    if stream.is_null() {
        drop(unsafe { File::from_raw_fd(descriptor) });
        return Err(io::Error::last_os_error().into());
    }
    struct Stream(*mut libc::DIR);
    impl Drop for Stream {
        fn drop(&mut self) {
            unsafe { libc::closedir(self.0) };
        }
    }
    let _stream = Stream(stream);
    unsafe { libc::rewinddir(stream) };
    loop {
        nix::errno::Errno::clear();
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let errno = nix::errno::Errno::last_raw();
            return if errno == 0 {
                Ok(None)
            } else {
                Err(io::Error::from_raw_os_error(errno).into())
            };
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if !matches!(name, b"." | b"..")
            && let Some(value) = visit(name)?
        {
            return Ok(Some(value));
        }
    }
}

fn event_store_error(operand: &Path, error: StoreError) -> String {
    let cause = if matches!(error, StoreError::Corrupt) {
        "identity-changed"
    } else {
        "io-error"
    };
    event_rejection(operand, cause)
}

enum DirectoryFailure {
    Io(io::Error),
    Identity(Option<io::Error>),
}

impl DirectoryFailure {
    fn artifact(self) -> String {
        match self {
            Self::Io(error) | Self::Identity(Some(error)) => error.to_string(),
            Self::Identity(None) => "artifact identity changed".into(),
        }
    }

    fn event(self, operand: &Path) -> String {
        let cause = match self {
            Self::Identity(_) => "identity-changed",
            Self::Io(_) => "io-error",
        };
        event_rejection(operand, cause)
    }
}

fn create_directory_at(
    parent: &File,
    leaf: &OsStr,
    name: &std::ffi::CStr,
    require_owner: bool,
) -> std::result::Result<(File, (u64, u64)), DirectoryFailure> {
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
        let error = io::Error::last_os_error();
        return Err(if error.kind() == io::ErrorKind::AlreadyExists {
            DirectoryFailure::Identity(Some(error))
        } else {
            DirectoryFailure::Io(error)
        });
    }
    let inspect = |error| DirectoryFailure::Identity(Some(error));
    let stat = stat_at(parent, leaf).map_err(inspect)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(DirectoryFailure::Identity(None));
    }
    let identity = stat_identity(&stat);
    let opened = (|| {
        chmod_at(parent, leaf, 0o700).map_err(DirectoryFailure::Io)?;
        let stat = stat_at(parent, leaf).map_err(inspect)?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
            || stat_identity(&stat) != identity
            || stat.st_mode & 0o777 != 0o700
        {
            return Err(DirectoryFailure::Identity(None));
        }
        let directory = open_directory_at(parent, leaf).map_err(inspect)?;
        let meta = directory.metadata().map_err(DirectoryFailure::Io)?;
        if !meta.is_dir() || file_id(&meta) != identity || require_owner && !protected(&meta, 0o700)
        {
            return Err(DirectoryFailure::Identity(None));
        }
        Ok(directory)
    })();
    if opened.is_err() && directory_entry_matches(parent, leaf, identity) {
        let _ = remove_directory_at(parent, leaf);
    }
    opened.map(|directory| (directory, identity))
}

impl StoreTarget {
    fn prepare(parent: &File, path: &Path) -> Result<Self> {
        let parent = parent.try_clone().text()?;
        let leaf = path
            .file_name()
            .ok_or("artifact path has no name")?
            .to_owned();
        let name = native_name(&leaf)?;
        let (directory, identity) =
            create_directory_at(&parent, &leaf, &name, true).map_err(DirectoryFailure::artifact)?;
        Self::from_directory(
            (parent, leaf, directory, identity),
            true,
            true,
            |_| Ok(()),
            |error| format!("store preparation failed: {error:?}"),
        )
    }

    fn from_directory(
        binding: (File, OsString, File, (u64, u64)),
        owned: bool,
        exact_selection: bool,
        validate: impl FnOnce(&File) -> Result<()>,
        store_error: impl FnOnce(StoreError) -> String,
    ) -> Result<Self> {
        let (parent, leaf, directory, identity) = binding;
        let prepared =
            validate(&directory).and_then(|()| Store::prepare_at(&directory).map_err(store_error));
        match prepared {
            Ok(prepared) => Ok(Self {
                parent,
                leaf,
                directory,
                identity,
                prepared,
                validator: None,
                exact_selection,
                owned,
                armed: true,
            }),
            Err(error) => {
                if owned && directory_entry_matches(&parent, &leaf, identity) {
                    let _ = remove_directory_at(&parent, &leaf);
                }
                Err(error)
            }
        }
    }

    fn lease(&mut self, kind: Kind, generation: u32, initial: &[u8]) -> StoreResult<Store> {
        let store = self
            .prepared
            .lease_at(&self.directory, kind, generation, initial, 0, 0)?;
        self.validator = Some(store.duplicate()?);
        Ok(store)
    }

    fn initialize(&self, store: &Store, initial: &[u8]) -> Result<()> {
        self.parent.sync_all().text()?;
        self.prepared
            .initialize_leased_at(&self.directory, store, initial)
            .map_err(|error| format!("store initialization failed: {error:?}"))?;
        crate::ensure!(
            !self.exact_selection
                || store
                    .selected_result()
                    .is_ok_and(|selected| selected == *store.selected()),
            "store initialization failed: selected commit changed"
        );
        Ok(())
    }

    fn revalidate_store(&self) -> StoreResult<bool> {
        self.prepared.revalidate_at(&self.directory)?;
        let validator = self.validator.as_ref().ok_or(StoreError::Corrupt)?;
        Ok(validator.selected_result()? == *validator.selected())
    }

    fn revalidate(&self) -> Result<()> {
        crate::ensure!(
            directory_entry_matches(&self.parent, &self.leaf, self.identity),
            "artifact identity changed"
        );
        let selected = self
            .revalidate_store()
            .map_err(|error| format!("store identity changed: {error:?}"))?;
        crate::ensure!(selected, "store selected commit changed");
        Ok(())
    }

    fn retain(&mut self) {
        self.armed = false;
    }

    fn rollback(&mut self) {
        self.armed = false;
        self.prepared.rollback_at(&self.directory);
        if self.owned && directory_entry_matches(&self.parent, &self.leaf, self.identity) {
            let _ = remove_directory_at(&self.parent, &self.leaf);
        }
    }
}

impl Drop for StoreTarget {
    fn drop(&mut self) {
        if self.armed {
            self.rollback();
        }
    }
}

impl PendingEvent {
    fn prepare(self) -> Result<EventTarget> {
        let PendingEvent(operand, parent, leaf, opened) = self;
        let (directory, identity, owned) = match opened {
            Some(directory) => {
                let identity = directory
                    .metadata()
                    .map(|meta| file_id(&meta))
                    .map_err(|_| event_rejection(&operand, "io-error"))?;
                (directory, identity, false)
            }
            None => {
                let name = native_name(&leaf).map_err(|_| event_rejection(&operand, "io-error"))?;
                let (directory, identity) = create_directory_at(&parent, &leaf, &name, false)
                    .map_err(|error| error.event(&operand))?;
                (directory, identity, true)
            }
        };
        let target = StoreTarget::from_directory(
            (parent, leaf, directory, identity),
            owned,
            false,
            |directory| {
                for _ in 0..=u8::from(owned) {
                    validate_event_directory(directory, &operand)?;
                }
                Ok(())
            },
            |error| event_store_error(&operand, error),
        )?;
        Ok(EventTarget { operand, target })
    }
}

impl EventTarget {
    fn lease(&mut self, generation: u32, initial: &[u8]) -> Result<Store> {
        self.target
            .lease(Kind::Event, generation, initial)
            .map_err(|error| event_store_error(&self.operand, error))
    }

    fn revalidate(&self, root: &Path) -> Result<()> {
        let resolved =
            absolute(&self.operand).map_err(|_| event_rejection(&self.operand, "io-error"))?;
        let (_, _, current) = open_event_target(root, &resolved, &self.operand)?;
        let current = current.ok_or_else(|| event_rejection(&self.operand, "identity-changed"))?;
        crate::ensure!(
            current
                .metadata()
                .map(|meta| file_id(&meta))
                .map_err(|_| event_rejection(&self.operand, "io-error"))?
                == self.target.identity,
            event_rejection(&self.operand, "identity-changed")
        );
        self.target
            .revalidate_store()
            .map_err(|error| event_store_error(&self.operand, error))?;
        Ok(())
    }
}

impl PreparedInstrument {
    fn prepare(source: File, root: &Path, path: &Path) -> Result<Self> {
        crate::ensure!(
            path.parent() == Some(root),
            "instrumentation stage escaped root"
        );
        let leaf = path
            .file_name()
            .ok_or("instrumentation stage has no name")?
            .to_owned();
        let parent = open_directory(root).text()?;
        let name = native_name(&leaf)?;
        let descriptor = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
                0o500,
            )
        };
        syscall!(descriptor >= 0);
        let stage = unsafe { File::from_raw_fd(descriptor) };
        if unsafe { libc::fchmod(stage.as_raw_fd(), 0o500) } != 0 {
            let error = io::Error::last_os_error();
            let _ = unlink_at(&parent, &leaf);
            return Err(error.to_string());
        }
        let meta = match stage.metadata() {
            Ok(meta) => meta,
            Err(error) => {
                let _ = unlink_at(&parent, &leaf);
                return Err(error.to_string());
            }
        };
        let identity = file_id(&meta);
        if !meta.is_file() || !protected(&meta, 0o500) {
            let _ = unlink_at(&parent, &leaf);
            return Err("instrumentation stage identity changed".into());
        }
        Ok(Self {
            source,
            stage,
            parent,
            leaf,
            path: path.to_owned(),
            identity,
            armed: true,
        })
    }

    fn configure(&mut self, process: &mut Command) -> Result<Instrument> {
        let hash = shared::copy_digest(&mut self.source, Some(&mut self.stage))?;
        self.stage.sync_all().text()?;
        crate::ensure!(
            file_entry_matches(&self.parent, &self.leaf, self.identity, 0o500),
            "instrumentation stage identity changed"
        );
        let mut staged = self.stage.try_clone().text()?;
        crate::ensure!(
            shared::copy_digest(&mut staged, None)? == hash,
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
        let mut preload = self.path.as_os_str().to_owned();
        if let Some(prior) = std::env::var_os(loader).filter(|value| !value.is_empty()) {
            preload.push(separator);
            preload.push(prior);
        }
        process.env(loader, preload);
        Ok(Instrument {
            read: read.into(),
            write: write.into(),
            stage: staged,
            parent: self.parent.try_clone().text()?,
            leaf: self.leaf.clone(),
            identity: self.identity,
            hash,
            nonce,
        })
    }

    fn retain(&mut self) {
        self.armed = false;
    }

    fn rollback(&mut self) {
        self.armed = false;
        if file_entry_matches(&self.parent, &self.leaf, self.identity, 0o500) {
            let _ = unlink_at(&self.parent, &self.leaf);
        }
    }
}

impl Drop for PreparedInstrument {
    fn drop(&mut self) {
        if self.armed {
            self.rollback();
        }
    }
}

fn chmod_at(parent: &File, name: &OsStr, mode: libc::mode_t) -> io::Result<()> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    if unsafe { libc::fchmodat(parent.as_raw_fd(), name.as_ptr(), mode, 0) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(crate) fn open_directory(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

fn open_directory_at(parent: &File, name: &OsStr) -> io::Result<File> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn component_cause(
    parent: &File,
    name: &OsStr,
    final_component: bool,
    error: &io::Error,
) -> &'static str {
    if error.kind() == io::ErrorKind::PermissionDenied {
        return "not-searchable";
    }
    let stat = match stat_at(parent, name) {
        Ok(stat) => stat,
        Err(error) => {
            return if error.kind() == io::ErrorKind::NotFound {
                "missing"
            } else {
                "io-error"
            };
        }
    };
    match stat.st_mode & libc::S_IFMT {
        libc::S_IFLNK => "link",
        libc::S_IFDIR => "identity-changed",
        _ if final_component => "wrong-type",
        _ => "not-directory",
    }
}

fn stat_at(parent: &File, name: &OsStr) -> io::Result<libc::stat> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    stat_cstr_at(parent, &name)
}

pub(crate) fn stat_cstr_at(parent: &File, name: &std::ffi::CStr) -> io::Result<libc::stat> {
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        Ok(stat)
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(crate) fn stat_identity(stat: &libc::stat) -> (u64, u64) {
    #[cfg(target_os = "macos")]
    let device = u64::try_from(stat.st_dev).unwrap_or(u64::MAX);
    #[cfg(not(target_os = "macos"))]
    let device = stat.st_dev;
    (device, stat.st_ino)
}

fn directory_entry_matches(parent: &File, name: &OsStr, identity: (u64, u64)) -> bool {
    stat_at(parent, name).ok().is_some_and(|entry| {
        entry.st_mode & libc::S_IFMT == libc::S_IFDIR && stat_identity(&entry) == identity
    })
}

fn file_entry_matches(
    parent: &File,
    name: &OsStr,
    identity: (u64, u64),
    mode: libc::mode_t,
) -> bool {
    stat_at(parent, name).ok().is_some_and(|entry| {
        entry.st_mode & libc::S_IFMT == libc::S_IFREG
            && entry.st_mode & 0o777 == mode
            && stat_identity(&entry) == identity
    })
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
    let alias = descriptor_path(&file).join(path.file_name().ok_or("rendezvous has no name")?);
    Ok((file, alias))
}

fn descriptor_path(file: &File) -> PathBuf {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let root = "/proc/self/fd";
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let root = "/dev/fd";
    Path::new(root).join(file.as_raw_fd().to_string())
}

fn native_name(name: &OsStr) -> Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| "native path contains NUL".into())
}

fn unlink_at(parent: &File, name: &OsStr) -> Result<()> {
    let name = native_name(name)?;
    syscall!(unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == 0);
    Ok(())
}

fn remove_directory_at(parent: &File, name: &OsStr) -> Result<()> {
    let name = native_name(name)?;
    syscall!(unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } == 0);
    Ok(())
}

fn publish_exclusive(parent: &File, stage: &OsStr, destination: &OsStr) -> Result<()> {
    let (stage, destination) = (native_name(stage)?, native_name(destination)?);
    #[cfg(target_os = "macos")]
    let status = unsafe {
        libc::renameatx_np(
            parent.as_raw_fd(),
            stage.as_ptr(),
            parent.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let status = unsafe {
        libc::renameat2(
            parent.as_raw_fd(),
            stage.as_ptr(),
            parent.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    syscall!(status == 0);
    parent.sync_all().text()
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
    cleanup_excluding(path, None)
}

fn cleanup_excluding(path: &Path, event: Option<&Path>) -> Result<()> {
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
    rollback_companions(path, &expected_identity, event)
}

fn rollback_companions(path: &Path, expected_identity: &[u8], event: Option<&Path>) -> Result<()> {
    let (mut external, expected) =
        shared::cleanup_artifacts(path, Some(expected_identity), |bytes| {
            Some(PathBuf::from(OsString::from_vec(bytes)))
        });
    if event.is_some() {
        external[0] = None;
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
    })
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
    source: Option<&mut PreparedInstrument>,
    process: &mut Command,
) -> Result<Option<Instrument>> {
    source.map(|source| source.configure(process)).transpose()
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
    crate::ensure!(
        file_entry_matches(
            &instrument.parent,
            &instrument.leaf,
            instrument.identity,
            0o500
        ) && shared::copy_digest(&mut instrument.stage, None)? == instrument.hash,
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
