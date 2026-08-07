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
#[cfg(not(target_os = "macos"))]
use interprocess::os::unix::local_socket::ListenerOptionsExt;
use nix::dir::Dir;
use nix::fcntl::{AtFlags, FcntlArg, OFlag, fcntl, open, openat};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::stat::{Mode, SFlag, fchmod, fstatat, mkdirat};
use nix::unistd::{UnlinkatFlags, unlinkat};
use path_absolutize::Absolutize;
use signal_hook::iterator::Signals;
use std::cell::Cell;
use std::ffi::{CString, OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, String>;
type StoreResult<T> = std::result::Result<T, StoreError>;
const SAFE_OPEN_FLAGS: i32 = libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
const DIRECTORY_FLAGS: OFlag =
    OFlag::from_bits_retain(libc::O_RDONLY | libc::O_DIRECTORY | SAFE_OPEN_FLAGS);
const INSTRUMENT_FLAGS: OFlag =
    OFlag::from_bits_retain(libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | SAFE_OPEN_FLAGS);
static UMASK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Umask(libc::mode_t);

impl Drop for Umask {
    fn drop(&mut self) {
        unsafe {
            libc::umask(self.0);
        }
    }
}

pub(crate) fn with_umask<T>(mask: libc::mode_t, operation: impl FnOnce() -> T) -> T {
    let _lock = UMASK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _restore = Umask(unsafe { libc::umask(mask) });
    operation()
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pthread_fchdir_np(fd: libc::c_int) -> libc::c_int;
}

#[cfg(target_os = "macos")]
struct ThreadDirectory(bool);

#[cfg(target_os = "macos")]
impl ThreadDirectory {
    fn enter(fd: libc::c_int) -> io::Result<Self> {
        pthread_directory(unsafe { pthread_fchdir_np(fd) })?;
        Ok(Self(true))
    }

    fn restore(mut self) -> io::Result<()> {
        pthread_directory(unsafe { pthread_fchdir_np(-1) })?;
        self.0 = false;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl Drop for ThreadDirectory {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                pthread_fchdir_np(-1);
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn pthread_directory(status: libc::c_int) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else if status == -1 {
        Err(io::Error::last_os_error())
    } else {
        Err(io::Error::from_raw_os_error(status))
    }
}

trait Text<T> {
    fn text(self) -> Result<T>;
}

impl<T, E: std::fmt::Display> Text<T> for std::result::Result<T, E> {
    fn text(self) -> Result<T> {
        self.map_err(|error| error.to_string())
    }
}

fn os<T>(result: nix::Result<T>) -> io::Result<T> {
    result.map_err(io::Error::from)
}

fn file(result: nix::Result<OwnedFd>) -> io::Result<File> {
    os(result).map(File::from)
}

fn dir(descriptor: OwnedFd) -> io::Result<Dir> {
    let raw = descriptor.as_raw_fd();
    // nix 0.30 transfers ownership before fdopendir and otherwise leaks its error path.
    Dir::from_fd(descriptor)
        .inspect_err(|_| drop(unsafe { File::from_raw_fd(raw) }))
        .map_err(Into::into)
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
crate::schema!(struct Instrument fields; read: File, write: File, stage: File, parent: File, leaf: OsString, identity: (u64, u64), hash: [u8; 32], nonce: [u8; 16]);
crate::schema!(struct PreparedInstrument fields; source: File, stage: File, parent: File, leaf: OsString, path: PathBuf, identity: (u64, u64), armed: bool);
crate::schema!(struct EventTarget fields; operand: PathBuf, target: StoreTarget);
crate::schema!(struct StoreTarget fields; parent: File, leaf: OsString, directory: File, identity: (u64, u64), prepared: PreparedStore, validator: Option<Store>, exact_selection: bool, owned: bool, armed: bool);
struct RawTerminal(ViewerTerminal);
struct ChildGuard(Option<Child>);
struct PendingEvent(PathBuf, File, OsString, Option<File>);
struct Stage(File, OsString, (u64, u64), bool);
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

    fn revalidate_stores(&self) -> Result<()> {
        self.lifecycle.revalidate()?;
        if let Some(event) = &self.event {
            event.revalidate(&self.root)?;
        }
        if let Some(log) = &self.log {
            log.revalidate()?;
        }
        Ok(())
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
        validate_event_aliases(event, &marker)?;
    }
    Ok(marker)
}

pub(crate) fn create(
    mode: CreateMode,
    path: &Path,
    mut command: Vec<OsString>,
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
    if command.is_empty() {
        command.push(
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
        );
    }
    let parent = socket_parent(path)?;
    let leaf = OsString::from(format!(".moor-{}.stage", hex(shared::random_array()?)));
    // A restrictive temporary umask gives the unpredictable staged name its
    // exact mode at bind time, even when the caller supplied a stricter mask.
    let listener = with_umask(0o177, || {
        socket_name(&parent, &leaf, |stage_name| {
            let options = ListenerOptions::new()
                .name(stage_name)
                .reclaim_name(false)
                .nonblocking(ListenerNonblockingMode::Accept);
            #[cfg(not(target_os = "macos"))]
            let options = options.mode(0o600);
            options.create_sync()
        })
    });
    let listener = listener?;
    let identity = entry_identity(&parent, &leaf, SFlag::S_IFSOCK, Some(0o600))
        .text()?
        .ok_or_else(|| "staged rendezvous identity changed".to_string())?;
    let stage = Stage(parent, leaf, identity, true);
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

fn accept_blocking(listener: &LocalListener) -> Option<LocalStream> {
    let stream = listener.accept().ok()?;
    // Darwin propagates O_NONBLOCK from an accepting listener to the new
    // socket. Runtime I/O owns its own threads and requires blocking streams.
    stream.set_nonblocking(false).ok()?;
    Some(stream)
}

impl Drop for Stage {
    fn drop(&mut self) {
        if self.3 {
            self.rollback();
        }
    }
}

impl Stage {
    fn matches(&self) -> bool {
        entry_identity(&self.0, &self.1, SFlag::S_IFSOCK, None)
            .is_ok_and(|identity| identity == Some(self.2))
    }

    fn published_identity(&self, destination: &OsStr) -> Result<(u64, u64)> {
        entry_identity(&self.0, destination, SFlag::S_IFSOCK, None)
            .text()?
            .filter(|identity| *identity == self.2)
            .ok_or_else(|| "published rendezvous identity changed".into())
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

    fn holder_ancestor(&self, pid: u32) -> bool {
        live_holder_ancestor(pid)
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
    if let Err(error) = config.revalidate_stores() {
        return abort_unpublished(state, &running, &mut config, &mut ready, error, true);
    }
    if let Some(observed) = early.or(state.observe_exit()?) {
        return finalize_unpublished_exit(&mut state, &running, observed, &mut config, &mut ready);
    }
    let destination = path.file_name().ok_or("rendezvous has no name")?;
    let publication = crate::require(config.stage.matches(), "staged rendezvous identity changed")
        .and_then(|()| {
            publish_exclusive(&config.stage.0, &config.stage.1, destination)
                .and_then(|()| config.stage.published_identity(destination))
        });
    let marker = match publication {
        Ok(marker) => marker,
        Err(error) => {
            config.stage.rollback_published(destination);
            return abort_unpublished(state, &running, &mut config, &mut ready, error, false);
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
        |_, _| {
            let stream = accept_blocking(&listener)?;
            let (trusted, pid) = peer_identity(&stream);
            stream
                .set_send_timeout(Some(Duration::from_millis(250)))
                .ok()?;
            Some((
                Duplex::socket(stream, [], cancel).ok()?,
                trusted,
                pid,
                false,
            ))
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
) -> Result<i32> {
    let status = state.drive(|_, _| None, || None)?.unwrap_or(observed);
    let (exit, durable) = state.finish_exit(running, status, None);
    if durable {
        config.retain_stores();
    }
    crate::return_if!(ready.output.is_none(), Ok(exit));
    eprintln!(
        "{}: child exited before session publication",
        name::program(config.invoked)
    );
    ready.notice(3, 1);
    Ok(1)
}

fn abort_unpublished(
    mut state: Runtime<UnixNative>,
    running: &str,
    config: &mut Config<'_>,
    ready: &mut shared::LaunchReporter<UnixStream>,
    error: String,
    diagnose: bool,
) -> Result<i32> {
    let deadline = Instant::now() + Duration::from_millis(25);
    let observed = loop {
        if let Some(observed) = state.observe_exit()? {
            break Some(observed);
        }
        if Instant::now() >= deadline {
            break None;
        }
        thread::sleep(Duration::from_millis(1));
    };
    if let Some(observed) = observed {
        return finalize_unpublished_exit(&mut state, running, observed, config, ready);
    }
    let mut signal = Some(true);
    let _ = state.drive(|_, _| None, || signal.take())?;
    drop(state);
    if diagnose && ready.output.is_some() {
        eprintln!("{}: {error}", name::program(config.invoked));
    }
    ready.notice(3, 1);
    Err(error)
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
            unsafe { libc::_exit(store.0.initialize(store.1, store.2).is_err() as i32) }
        }
        workers.push(pid);
    }
    while !workers.is_empty() && !failed && Instant::now() < deadline {
        workers.retain(|pid| {
            let mut observed = 0;
            match unsafe { libc::waitpid(*pid, &mut observed, libc::WNOHANG) } {
                result if result == *pid => {
                    let success = libc::WIFEXITED(observed) && libc::WEXITSTATUS(observed) == 0;
                    failed |= !success;
                    false
                }
                -1 if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted => {
                    failed = true;
                    false
                }
                _ => true,
            }
        });
        if !failed {
            thread::sleep(Duration::from_millis(2));
        }
    }
    if !failed && workers.is_empty() {
        return Ok(());
    }
    for pid in &workers {
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
    let reap_deadline = Instant::now() + Duration::from_millis(250);
    while !workers.is_empty() && Instant::now() < reap_deadline {
        workers.retain(|pid| {
            let mut observed = 0;
            (unsafe { libc::waitpid(*pid, &mut observed, libc::WNOHANG) }) != *pid
        });
        thread::sleep(Duration::from_millis(2));
    }
    Err((
        if failed {
            "store initialization failed".into()
        } else {
            "store initialization timed out".into()
        },
        workers.is_empty(),
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
    let identity = std::mem::take(&mut config.launch.identity);
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
    let event_manifest = event_path.map(|path| path.as_os_str().as_bytes());
    let instrument_path = config
        .instrument
        .as_ref()
        .map(|instrument| instrument.path.as_path());
    if let (Some(event), Some(instrument)) = (event_path, instrument_path) {
        validate_event_alias(event, instrument)?;
    }
    let instrument_manifest = instrument_path.map(|path| path.as_os_str().as_bytes());
    let running = shared::lifecycle_running(
        &identity,
        (supervised.then_some(generation), generation),
        incarnation,
        (start_wall, start_mono, boot),
        ("posix-bytes", event_manifest, instrument_manifest),
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
            event_identity: event_manifest,
            instrument_identity: instrument_manifest,
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
    let instrument = config
        .instrument
        .as_mut()
        .map(|source| source.configure(&mut process))
        .transpose()?;
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
    let pty = Duplex::tracked(reader, master.try_clone().text()?, 1 << 20);
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
    let running = std::mem::take(&mut artifacts.running);
    let mut holder = artifacts.runtime(
        pty,
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

fn connected(path: &Path, timeout: Duration, stream: LocalStream) -> Result<WireClient> {
    crate::ensure!(peer_owned(&stream), "holder peer identity mismatch");
    let deadline = Instant::now() + timeout;
    stream
        .set_send_timeout(Some(Duration::from_millis(250)))
        .text()?;
    WireClient::from_stream(stream, identity(path)?, deadline, cancel)
}

fn bounded(path: &Path, timeout: Duration) -> Result<WireClient> {
    let (_parent, stream) = socket_stream(path).map_err(|(error, _)| error)?;
    connected(path, timeout, stream)
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
        |sender| {
            thread::spawn(move || {
                run_viewer_input(
                    io::stdin(),
                    sender,
                    InputConfig {
                        detach,
                        pass_suspend,
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
        || {
            let (_parent, stream) = socket_stream(path).map_err(|(_, refused)| refused)?;
            connected(path, timeout, stream).map_err(|_| false)
        },
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
    crate::ensure!(
        crate::store::private_directory(&root, true).text()?,
        format!("session root '{}' is not owner-only", root.display())
    );
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

fn validate_event_aliases(event: &Path, marker: &Path) -> Result<()> {
    validate_event_alias(event, marker)?;
    for suffix in [".log", ".exit"] {
        validate_event_alias(event, &shared::companion(marker, suffix))?;
    }
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
    validate_event_aliases(path, marker)?;
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
        let name = component.as_os_str();
        directory = open_directory_at(&directory, name)
            .map_err(|error| reject(component_cause(&directory, name, false, &error)))?;
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
    directory_entries::<_, io::Error>(directory, |entry| {
        let slots = ["body.0", "body.1", "commit.0", "commit.1"];
        Ok(Some(if slots.iter().any(|name| entry == name.as_bytes()) {
            "pre-existing-slot"
        } else {
            "extra-entry"
        }))
    })
    .map_err(|_| reject("io-error"))?
    .map_or(Ok(()), |cause| Err(reject(cause)))
}

pub(crate) fn directory_entries<T, E>(
    directory: &File,
    mut visit: impl FnMut(&[u8]) -> std::result::Result<Option<T>, E>,
) -> std::result::Result<Option<T>, E>
where
    E: From<io::Error>,
{
    let duplicate = directory.as_fd().try_clone_to_owned()?;
    let flags = OFlag::from_bits_retain(os(fcntl(&duplicate, FcntlArg::F_GETFL))?);
    os(fcntl(
        &duplicate,
        FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK),
    ))?;
    let mut stream = dir(duplicate)?;
    drop(stream.iter());
    for entry in stream.iter() {
        let entry = os(entry)?;
        let name = entry.file_name().to_bytes();
        if !matches!(name, b"." | b"..")
            && let Some(value) = visit(name)?
        {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn event_store_error(operand: &Path, error: StoreError) -> String {
    let cause = if matches!(error, StoreError::Corrupt) {
        "identity-changed"
    } else {
        "io-error"
    };
    event_rejection(operand, cause)
}

crate::schema!(enum DirectoryFailure; Io(io::Error), Identity(io::Error), Changed);

impl DirectoryFailure {
    fn artifact(self) -> String {
        match self {
            Self::Io(error) | Self::Identity(error) => error.to_string(),
            Self::Changed => "artifact identity changed".into(),
        }
    }

    fn event(self, operand: &Path) -> String {
        let cause = match self {
            Self::Identity(_) | Self::Changed => "identity-changed",
            Self::Io(_) => "io-error",
        };
        event_rejection(operand, cause)
    }
}

fn create_directory_at(
    parent: &File,
    leaf: &OsStr,
    require_owner: bool,
) -> std::result::Result<(File, (u64, u64)), DirectoryFailure> {
    // Creation dispatch is pre-thread/fork; bound the umask window to this syscall.
    let mask = unsafe { libc::umask(0o077) };
    let created = os(mkdirat(parent, leaf, Mode::from_bits_retain(0o700)));
    unsafe { libc::umask(mask) };
    if let Err(error) = created {
        return Err(if error.kind() == io::ErrorKind::AlreadyExists {
            DirectoryFailure::Identity(error)
        } else {
            DirectoryFailure::Io(error)
        });
    }
    let inspect = DirectoryFailure::Identity;
    let identity = entry_identity(parent, leaf, SFlag::S_IFDIR, None)
        .map_err(inspect)?
        .ok_or(DirectoryFailure::Changed)?;
    let opened = (|| {
        let directory = open_directory_at(parent, leaf).map_err(inspect)?;
        let meta = directory.metadata().map_err(DirectoryFailure::Io)?;
        crate::return_if!(file_id(&meta) != identity, Err(DirectoryFailure::Changed));
        os(fchmod(&directory, Mode::from_bits_retain(0o700))).map_err(DirectoryFailure::Io)?;
        let meta = directory.metadata().map_err(DirectoryFailure::Io)?;
        let valid = file_id(&meta) == identity
            && meta.mode() & 0o777 == 0o700
            && (!require_owner || owned(&meta))
            && entry_identity(parent, leaf, SFlag::S_IFDIR, Some(0o700)).map_err(inspect)?
                == Some(identity);
        crate::return_if!(!valid, Err(DirectoryFailure::Changed));
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
        let (directory, identity) =
            create_directory_at(&parent, &leaf, true).map_err(DirectoryFailure::artifact)?;
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
        let prepared = validate(&directory)
            .and_then(|()| Store::prepare_at(&directory).map_err(store_error))
            .inspect_err(|_| {
                if owned && directory_entry_matches(&parent, &leaf, identity) {
                    let _ = remove_directory_at(&parent, &leaf);
                }
            })?;
        Ok(Self {
            parent,
            leaf,
            directory,
            identity,
            prepared,
            validator: None,
            exact_selection,
            owned,
            armed: true,
        })
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
        crate::require(
            !self.exact_selection
                || store
                    .selected_result()
                    .is_ok_and(|selected| selected == *store.selected()),
            "store initialization failed: selected commit changed",
        )
    }

    fn revalidate_store(&self) -> StoreResult<bool> {
        self.prepared.revalidate_at(&self.directory)?;
        let validator = self.validator.as_ref().ok_or(StoreError::Corrupt)?;
        Ok(validator.selected_result()? == *validator.selected())
    }

    fn revalidate(&self) -> Result<()> {
        crate::require(
            directory_entry_matches(&self.parent, &self.leaf, self.identity),
            "artifact identity changed",
        )?;
        let selected = self
            .revalidate_store()
            .map_err(|error| format!("store identity changed: {error:?}"))?;
        crate::require(selected, "store selected commit changed")
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
                let (directory, identity) = create_directory_at(&parent, &leaf, false)
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
            .map_err(|error| event_store_error(&self.operand, error))
            .map(|_| ())
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
        let stage = open_file_at(
            &parent,
            leaf.as_os_str(),
            INSTRUMENT_FLAGS,
            Mode::from_bits_retain(0o500),
        )
        .text()?;
        let identity = (|| -> Result<_> {
            os(fchmod(&stage, Mode::from_bits_retain(0o500))).text()?;
            let meta = stage.metadata().text()?;
            crate::ensure!(
                meta.is_file() && protected(&meta, 0o500),
                "instrumentation stage identity changed"
            );
            Ok(file_id(&meta))
        })()
        .inspect_err(|_| {
            let _ = unlink_at(&parent, &leaf);
        })?;
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

pub(crate) fn open_directory(path: &Path) -> io::Result<File> {
    file(open(path, DIRECTORY_FLAGS, Mode::empty()))
}

fn open_directory_at(parent: &File, name: &OsStr) -> io::Result<File> {
    open_file_at(parent, name, DIRECTORY_FLAGS, Mode::empty())
}

pub(crate) fn open_file_at(
    parent: &File,
    name: &OsStr,
    flags: OFlag,
    mode: Mode,
) -> io::Result<File> {
    file(openat(parent, name, flags, mode))
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
        Err(error) if error.kind() == io::ErrorKind::NotFound => return "missing",
        Err(_) => return "io-error",
    };
    match SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT {
        SFlag::S_IFLNK => "link",
        SFlag::S_IFDIR => "identity-changed",
        _ if final_component => "wrong-type",
        _ => "not-directory",
    }
}

pub(crate) fn stat_at(parent: &File, name: &OsStr) -> io::Result<libc::stat> {
    os(fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW))
}

pub(crate) fn stat_identity(stat: &libc::stat) -> (u64, u64) {
    #[cfg(target_os = "macos")]
    let device = u64::try_from(stat.st_dev).unwrap_or(u64::MAX);
    #[cfg(not(target_os = "macos"))]
    let device = stat.st_dev;
    (device, stat.st_ino)
}

fn entry_identity(
    parent: &File,
    name: &OsStr,
    kind: SFlag,
    mode: Option<libc::mode_t>,
) -> io::Result<Option<(u64, u64)>> {
    let stat = stat_at(parent, name)?;
    let matches = SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT == kind
        && mode.is_none_or(|mode| stat.st_mode & 0o777 == mode);
    Ok(matches.then(|| stat_identity(&stat)))
}

fn directory_entry_matches(parent: &File, name: &OsStr, identity: (u64, u64)) -> bool {
    entry_identity(parent, name, SFlag::S_IFDIR, None).ok() == Some(Some(identity))
}

fn file_entry_matches(
    parent: &File,
    name: &OsStr,
    identity: (u64, u64),
    mode: libc::mode_t,
) -> bool {
    entry_identity(parent, name, SFlag::S_IFREG, Some(mode)).ok() == Some(Some(identity))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    path.absolutize().map(|path| path.into_owned()).text()
}

fn identity(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = absolute(path)?.into_os_string().into_vec();
    bytes.insert(0, 1);
    Ok(bytes)
}

fn socket_parent(path: &Path) -> Result<File> {
    let parent = path.parent().ok_or("rendezvous has no parent")?;
    open_directory(parent).text()
}

fn socket_name<T, E: std::fmt::Display>(
    parent: &File,
    leaf: &OsStr,
    open: impl FnOnce(Name<'_>) -> std::result::Result<T, E>,
) -> Result<T> {
    #[cfg(target_os = "macos")]
    {
        // /dev/fd entries are not traversable directories on macOS. Resolve
        // the short leaf using Darwin's per-thread working directory, never
        // mutating the process-wide directory forbidden by schema section 2.2.
        let directory = ThreadDirectory::enter(parent.as_raw_fd()).text()?;
        let result = Path::new(leaf)
            .to_fs_name::<GenericFilePath>()
            .text()
            .and_then(|name| open(name).text());
        directory.restore().text()?;
        result
    }
    #[cfg(not(target_os = "macos"))]
    {
        let alias = descriptor_path(parent).join(leaf);
        let name = alias.as_os_str().to_fs_name::<GenericFilePath>().text()?;
        open(name).text()
    }
}

#[cfg(not(target_os = "macos"))]
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

pub(crate) fn unlink_at(parent: &File, name: &OsStr) -> Result<()> {
    os(unlinkat(parent, name, UnlinkatFlags::NoRemoveDir)).text()
}

fn remove_directory_at(parent: &File, name: &OsStr) -> Result<()> {
    os(unlinkat(parent, name, UnlinkatFlags::RemoveDir)).text()
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
        // musl does not export the renameat2 wrapper; the Linux kernel ABI is
        // the same operation and preserves atomic RENAME_NOREPLACE semantics.
        libc::syscall(
            libc::SYS_renameat2,
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

fn socket_stream(path: &Path) -> std::result::Result<(File, LocalStream), (String, bool)> {
    let parent = socket_parent(path).map_err(|error| (error, false))?;
    let leaf = path
        .file_name()
        .ok_or_else(|| ("rendezvous has no name".into(), false))?;
    let mut refused = false;
    let stream = socket_name(&parent, leaf, |name| {
        LocalStream::connect(name).inspect_err(|error| {
            refused = error.kind() == io::ErrorKind::ConnectionRefused;
        })
    })
    .map_err(|error| (error, refused))?;
    Ok((parent, stream))
}

fn peer_identity(stream: &LocalStream) -> (bool, Option<u32>) {
    let Ok(credentials) = stream.peer_creds() else {
        return (false, None);
    };
    let same_user = credentials.euid() == Some(uid());
    #[cfg(target_os = "macos")]
    let pid = socket_peer_pid(stream);
    #[cfg(not(target_os = "macos"))]
    let pid = credentials
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0);
    let pid = pid.filter(|_| same_user);
    (pid.is_some(), pid)
}

fn peer_owned(stream: &LocalStream) -> bool {
    peer_identity(stream).0
}

#[cfg(target_os = "macos")]
fn socket_peer_pid(stream: &LocalStream) -> Option<u32> {
    let LocalStream::UdSocket(socket) = stream;
    let mut pid = 0i32;
    let mut length = std::mem::size_of_val(&pid) as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            socket.inner().as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            std::ptr::from_mut(&mut pid).cast(),
            &mut length,
        )
    };
    (result == 0 && length as usize == std::mem::size_of_val(&pid) && pid > 0).then_some(pid as u32)
}

fn live_holder_ancestor(mut pid: u32) -> bool {
    let holder = std::process::id();
    for _ in 0..4096 {
        if pid == holder {
            return true;
        }
        let Some(parent) = process_parent(pid) else {
            return false;
        };
        if parent == 0 || parent == pid {
            return false;
        }
        pid = parent;
    }
    false
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn process_parent(pid: u32) -> Option<u32> {
    let stat = fs::read(format!("/proc/{pid}/stat")).ok()?;
    let end = stat.iter().rposition(|byte| *byte == b')')?;
    let mut fields = stat[end + 1..]
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty());
    (fields.next()?.len() == 1).then_some(())?;
    std::str::from_utf8(fields.next()?).ok()?.parse().ok()
}

#[cfg(target_os = "macos")]
fn process_parent(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info);
    let read = unsafe {
        libc::proc_pidinfo(
            i32::try_from(pid).ok()?,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            size as i32,
        )
    };
    (read == size as i32).then_some(info.pbi_ppid)
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn process_parent(_: u32) -> Option<u32> {
    None
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
    let (external, expected) = shared::cleanup_artifacts(path, Some(&expected_identity), |bytes| {
        Some(PathBuf::from(OsString::from_vec(bytes)))
    });
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
            0o500,
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
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    let mut descriptor = [PollFd::new(fd, PollFlags::POLLIN | PollFlags::POLLHUP)];
    match os(poll(
        &mut descriptor,
        PollTimeout::try_from(timeout).unwrap_or(PollTimeout::MAX),
    )) {
        Ok(ready) => Ok(ready > 0),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(false),
        Err(error) => Err(error),
    }
}

fn uid() -> u32 {
    unsafe { libc::geteuid() }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    #[test]
    fn descriptor_relative_socket_name_never_changes_process_cwd() {
        let before = std::env::current_dir().unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("moor-thread-cwd-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        let parent = open_directory(&path).unwrap();
        let (entered, wait) = sync_channel(0);
        let (observed, receive) = sync_channel(0);
        let observer = thread::spawn(move || {
            wait.recv().unwrap();
            observed.send(std::env::current_dir().unwrap()).unwrap();
        });
        let during = socket_name(&parent, OsStr::new("probe"), move |_| {
            entered.send(()).unwrap();
            Ok::<_, io::Error>(receive.recv().unwrap())
        })
        .unwrap();
        observer.join().unwrap();
        assert_eq!(during, before);
        assert_eq!(std::env::current_dir().unwrap(), before);
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn accepted_socket_is_blocking_before_runtime_io_starts() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "moor-blocking-accept-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        let parent = open_directory(&path).unwrap();
        let leaf = OsStr::new("probe");
        let listener = socket_name(&parent, leaf, |name| {
            ListenerOptions::new()
                .name(name)
                .reclaim_name(false)
                .nonblocking(ListenerNonblockingMode::Accept)
                .create_sync()
        })
        .unwrap();
        let client = socket_name(&parent, leaf, LocalStream::connect).unwrap();
        let accepted = accept_blocking(&listener).expect("pending local connection");
        let LocalStream::UdSocket(accepted) = accepted;
        let flags = fcntl(accepted.as_fd(), FcntlArg::F_GETFL).unwrap();
        assert_eq!(flags & libc::O_NONBLOCK, 0);
        drop(client);
        drop(listener);
        fs::remove_file(path.join(leaf)).unwrap();
        fs::remove_dir(path).unwrap();
    }
}
