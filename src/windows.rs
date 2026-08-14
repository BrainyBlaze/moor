use crate::{require, wire::crc32c};
use rustpython_wtf8::{Wtf8, Wtf8Buf};
use zerocopy::byteorder::{LE, U32, U64};

pub type Result<T> = std::result::Result<T, String>;

pub fn wtf8_encode(wide: &[u16]) -> Vec<u8> {
    Wtf8Buf::from_wide(wide).into_bytes()
}

pub fn wtf8_decode(bytes: &[u8]) -> Result<Vec<u16>> {
    let value = Wtf8::from_bytes(bytes).ok_or("malformed WTF-8")?;
    let wide = value.encode_wide().collect::<Vec<_>>();
    let canonical = Wtf8Buf::from_wide(&wide).as_bytes() == bytes;
    crate::ensure!(canonical, "noncanonical WTF-8");
    Ok(wide)
}

pub fn cim_boot_identity(value: &str) -> Option<[u8; 16]> {
    use time::{PrimitiveDateTime, UtcOffset, macros::format_description};
    crate::return_if!(value.len() != 25, None);
    let local = PrimitiveDateTime::parse(
        &value[..21],
        format_description!("[year][month][day][hour][minute][second].[subsecond digits:6]"),
    )
    .ok()?;
    let offset = value[21..].parse::<i32>().ok()?.checked_mul(60)?;
    let utc = local.assume_offset(UtcOffset::from_whole_seconds(offset).ok()?);
    u64::try_from(
        utc.unix_timestamp_nanos()
            .div_euclid(100)
            .checked_add(116_444_736_000_000_000)?,
    )
    .ok()
    .map(|ticks| u128::from(ticks).to_le_bytes())
}

schema!(struct pub Marker derive [Clone, Copy, Debug, Eq, PartialEq] pub fields; generation: u32, incarnation: [u8; 16], pipe_length: [u8; 2], pipe: [u8; 46]);
binary_record!(RawMarker => Marker[80] error () = (); fixed { magic: [u8; 12] = *b"MOORMRK3\x01\0\0\0" } fields { generation: U32<LE>, incarnation: [u8; 16], pipe_length: [u8; 2], pipe: [u8; 46] });

impl Marker {
    pub fn new(generation: u32, incarnation: [u8; 16], random: [u8; 16]) -> Result<Self> {
        require(generation != 0, "zero marker generation")?;
        let pipe = format!(r"\\.\pipe\moor-{:032x}", u128::from_be_bytes(random))
            .into_bytes()
            .try_into()
            .map_err(|_| "invalid pipe name")?;
        Ok(Self {
            generation,
            incarnation,
            pipe_length: 46u16.to_le_bytes(),
            pipe,
        })
    }

    pub fn encode(&self) -> [u8; 84] {
        let mut out = [0; 84];
        out[..80].copy_from_slice(&(*self).encode_raw());
        let checksum = crc32c(&out[..80]);
        out[80..].copy_from_slice(&checksum.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        require(
            bytes.len() == 84
                && u32::from_le_bytes(bytes[80..84].try_into().unwrap()) == crc32c(&bytes[..80]),
            "malformed Windows marker",
        )?;
        let marker = Self::decode_raw(&bytes[..80]).map_err(|_| "malformed Windows marker")?;
        require(
            marker.generation != 0
                && marker.pipe_length == 46u16.to_le_bytes()
                && &marker.pipe[..14] == br"\\.\pipe\moor-"
                && crate::runtime::private::lowercase_hex(&marker.pipe[14..]),
            "malformed Windows marker",
        )?;
        Ok(marker)
    }
}

#[doc(hidden)]
schema!(struct pub BootstrapRecord derive [Clone, Copy, Debug, Eq, PartialEq] pub fields; nonce: [u8; 16], pid: u32, process: u64, thread: u64, created: u64);
binary_record!(RawBootstrapRecord => BootstrapRecord[56] error () = (); fixed { magic: [u8; 12] = *b"MOORBST1\x01\0\0\0" } fields { nonce: [u8; 16], pid: U32<LE>, process: U64<LE>, thread: U64<LE>, created: U64<LE> });

impl BootstrapRecord {
    pub fn encode(self) -> [u8; 56] {
        self.encode_raw()
    }

    pub fn decode(bytes: &[u8], nonce: [u8; 16]) -> Option<Self> {
        let value = Self::decode_raw(bytes)
            .ok()
            .filter(|value| value.nonce == nonce)?;
        (value.pid != 0
            && value.created != 0
            && value.process.min(value.thread) != 0
            && value.process != value.thread)
            .then_some(value)
    }
}

#[cfg(any(windows, test))]
schema!(enum ordinal DirectoryCause; Missing, NotDirectory, NotSearchable, Io);

#[cfg(any(windows, test))]
schema!(enum BootstrapFailure [Clone, Copy, Debug, Eq, PartialEq]; Directory(DirectoryCause), Execution(u32));
#[cfg(any(windows, test))]
schema!(struct BootstrapFailureRecord fields; nonce: [u8; 16], kind: u8, value: u32, reserved: [u8; 23]);
#[cfg(any(windows, test))]
binary_record!(RawBootstrapFailure => BootstrapFailureRecord[56] error () = (); fixed { magic: [u8; 12] = *b"MOORERR1\x01\0\0\0" } fields { nonce: [u8; 16], kind: u8, value: U32<LE>, reserved: [u8; 23] });

#[cfg(any(windows, test))]
fn bootstrap_failure_record(nonce: [u8; 16], failure: BootstrapFailure) -> [u8; 56] {
    let (kind, value) = match failure {
        BootstrapFailure::Directory(cause) => (1, cause as u32 + 1),
        BootstrapFailure::Execution(code) => (2, code),
    };
    BootstrapFailureRecord {
        nonce,
        kind,
        value,
        reserved: [0; 23],
    }
    .encode_raw()
}

#[cfg(any(windows, test))]
fn bootstrap_failure(bytes: &[u8], nonce: [u8; 16]) -> Option<BootstrapFailure> {
    let record = BootstrapFailureRecord::decode_raw(bytes)
        .ok()
        .filter(|record| record.nonce == nonce && record.reserved == [0; 23])?;
    match record.kind {
        1 if (1..=4).contains(&record.value) => Some(BootstrapFailure::Directory(
            DirectoryCause::from_ordinal(record.value as u8 - 1),
        )),
        2 if record.value != 0 => Some(BootstrapFailure::Execution(record.value)),
        _ => None,
    }
}

#[doc(hidden)]
pub fn bootstrap_command(kind: u8, nonce: [u8; 16]) -> [u8; 17] {
    let mut out = [0; 17];
    out[0] = kind;
    out[1..].copy_from_slice(&nonce);
    out
}

#[doc(hidden)]
pub fn accept_bootstrap_command(bytes: &[u8], nonce: [u8; 16], resumed: &mut bool) -> Option<u8> {
    let kind = *bytes.first()?;
    let valid = matches!((kind, *resumed), (1, false) | (2, true));
    crate::return_if!(bytes.len() != 17 || bytes[1..] != nonce || !valid, None);
    *resumed |= kind == 1;
    Some(kind)
}

#[cfg(windows)]
#[doc(hidden)]
pub fn console_control_kind(kind: u32) -> Option<bool> {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };
    match kind {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => Some(false),
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => Some(true),
        _ => None,
    }
}

#[cfg(any(windows, test))]
fn exact_descriptor_semantics(
    owner_matches: bool,
    dacl_protected: bool,
    actual_entries: usize,
    expected_entries: usize,
    mut same_entry: impl FnMut(usize, usize) -> bool,
) -> bool {
    if !owner_matches || !dacl_protected || actual_entries != expected_entries {
        return false;
    }
    let mut matched = vec![false; expected_entries];
    for actual in 0..actual_entries {
        let Some(expected) = (0..expected_entries)
            .find(|expected| !matched[*expected] && same_entry(actual, *expected))
        else {
            return false;
        };
        matched[expected] = true;
    }
    true
}

#[cfg(test)]
mod descriptor_semantics_tests {
    include!("../tests/unit/windows_descriptor.rs");
}

#[cfg(windows)]
#[allow(unused_unsafe)]
mod native {
    use super::*;
    use crate::{
        cli::*,
        name, require,
        runtime::{
            client::{Client, CommandError, CommandResult, missing, probe_session},
            holder::{Native as HolderNative, NativeExit},
            io::*,
            private::*,
        },
        wire::put_wide,
    };
    use interprocess::local_socket::{prelude::*, *};
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::*;
    use smallvec::SmallVec;
    use std::collections::VecDeque;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::os::windows::{ffi::*, fs::*, io::*};
    use std::sync::{OnceLock, atomic::*, mpsc};
    use std::{ffi::*, mem::*, path::*, ptr, thread, time::*};
    use windows_permissions::utilities::buf_from_os as wide;
    use windows_permissions::{LocalBox, SecurityDescriptor, constants::*, wrappers};
    use windows_spawn::{Command as SpawnCommand, Stdio as SpawnStdio, *};
    use windows_sys::Wdk::{Foundation::*, Storage::FileSystem::*};
    use windows_sys::Win32::{
        Foundation::*,
        Globalization::*,
        Security::{ACE_HEADER, *},
        Storage::FileSystem::*,
        System::{
            Console::*,
            Diagnostics::{Debug::*, ToolHelp::*},
            IO::IO_STATUS_BLOCK,
            JobObjects::*,
            LibraryLoader::*,
            Memory::*,
            Pipes::*,
            SystemInformation::*,
            Threading::*,
        },
        UI::Input::KeyboardAndMouse::{
            VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RMENU,
            VK_RSHIFT, VK_RWIN, VK_SHIFT, VkKeyScanW,
        },
    };

    fn check(ok: bool, what: &str) -> Result<()> {
        ok.then_some(())
            .ok_or_else(|| format!("{what}: {}", io::Error::last_os_error()))
    }
    fn coordinate((rows, columns): (u16, u16)) -> Result<COORD> {
        match (columns.try_into(), rows.try_into()) {
            (Ok(x), Ok(y)) => Ok(COORD { X: x, Y: y }),
            _ => Err("geometry exceeds the console interface limit".into()),
        }
    }
    macro_rules! win32 {
        ($call:expr, $what:expr) => {
            check(unsafe { $call } != 0, $what)
        };
    }
    macro_rules! transfer_handles {
        ($command:expr; $($name:expr => $handle:expr, $what:literal);+ $(;)?) => {$(
            ($command).env_remove($name);
            if let Some(handle) = $handle {
                win(($command).env_handle($name, handle), concat!("transfer ", $what))?;
            }
        )+};
    }
    fn win<T>(result: io::Result<T>, what: &str) -> Result<T> {
        result.map_err(|error| format!("{what}: {error}"))
    }
    fn string(error: impl std::fmt::Display) -> String {
        error.to_string()
    }
    fn os_bytes(value: &OsStr) -> Vec<u8> {
        wtf8_encode(&value.encode_wide().collect::<SmallVec<[u16; 256]>>())
    }
    fn process_birth(handle: HANDLE, what: &str) -> Result<u64> {
        let (mut created, mut exit, mut kernel, mut user) = Default::default();
        win32!(
            GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user),
            what
        )?;
        Ok(u64::from(created.dwLowDateTime) | u64::from(created.dwHighDateTime) << 32)
    }
    fn process_exit(handle: HANDLE) -> Result<Option<u32>> {
        require(
            !handle.is_null() && handle != INVALID_HANDLE_VALUE,
            "child process is unavailable",
        )?;
        let status = unsafe { WaitForSingleObject(handle, 0) };
        if status == WAIT_TIMEOUT {
            return Ok(None);
        }
        check(status == WAIT_OBJECT_0, "wait for child process")?;
        let mut code = 127;
        win32!(
            GetExitCodeProcess(handle, &mut code),
            "read child exit code"
        )?;
        Ok(Some(code))
    }
    const BOOTSTRAP_SELECTOR: &str = "MOOR_BOOTSTRAP";
    const BOOTSTRAP_CONTROL: &str = "MOOR_BOOTSTRAP_CONTROL";
    const BOOTSTRAP_RESULT: &str = "MOOR_BOOTSTRAP_RESULT";
    const BOOTSTRAP_STDERR: &str = "MOOR_BOOTSTRAP_STDERR";
    const BOOTSTRAP_INSTRUMENT: &str = "MOOR_BOOTSTRAP_INSTRUMENT";
    const BOOTSTRAP_DIRECTORY: &str = "MOOR_BOOTSTRAP_DIRECTORY";
    const INSTRUMENT_CHANNEL: &str = "MOOR_INSTRUMENT_CHANNEL";
    const INSTRUMENT_NONCE: &str = "MOOR_INSTRUMENT_NONCE";
    const SEMANTIC_TOKEN: &str = "MOOR_SESSION_SEMANTIC_TOKEN";
    const DETACHED_GEOMETRY: &str = "MOOR_DETACHED_GEOMETRY";
    const DETACHED_HOLDER: &str = "MOOR_DETACHED_HOLDER";
    fn path_buffer(what: &str, mut fill: impl FnMut(*mut u16, u32) -> u32) -> Result<PathBuf> {
        let size = fill(ptr::null_mut(), 0);
        check(size != 0, what)?;
        let mut out = vec![0; size as usize];
        let used = fill(out.as_mut_ptr(), size);
        check(used != 0 && used < size, what)?;
        Ok(OsString::from_wide(&out[..used as usize]).into())
    }
    fn temp() -> Result<PathBuf> {
        path_buffer("resolve Windows temporary directory", |out, size| unsafe {
            GetTempPathW(size, out)
        })
    }
    fn system() -> Result<PathBuf> {
        path_buffer("resolve Windows system directory", |out, size| unsafe {
            GetSystemDirectoryW(out, size)
        })
    }
    #[derive(Default)]
    struct Handle(Option<File>);
    impl Handle {
        fn checked(raw: HANDLE, what: &str) -> Result<Self> {
            let value = unsafe { Self::owned(raw) };
            check(!value.is_null(), what).map(|()| value)
        }
        unsafe fn owned(raw: HANDLE) -> Self {
            Self(
                (!raw.is_null() && raw != INVALID_HANDLE_VALUE)
                    .then(|| File::from(unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) })),
            )
        }
        fn is_null(&self) -> bool {
            self.0.is_none()
        }
        fn raw(&self) -> HANDLE {
            self.0
                .as_ref()
                .map_or(ptr::null_mut(), |value| value.as_raw_handle() as HANDLE)
        }
        fn pair(what: &str) -> Result<(Self, Self)> {
            let (read, write) = io::pipe().map_err(|error| format!("{what}: {error}"))?;
            Ok((
                Self(Some(OwnedHandle::from(read).into())),
                Self(Some(OwnedHandle::from(write).into())),
            ))
        }
        fn into_file(self) -> File {
            self.0.unwrap()
        }
        fn write(&self, bytes: &[u8], what: &str) -> Result<()> {
            let mut file = self.0.as_ref().unwrap();
            win(file.write_all(bytes), what)
        }
        fn read(&self, bytes: &mut [u8]) -> io::Result<usize> {
            let mut file = self.0.as_ref().unwrap();
            file.read(bytes)
        }
        fn record<const N: usize>(&self, eof: bool, what: &str) -> Result<[u8; N]> {
            let mut file = self.0.as_ref().unwrap();
            fixed_record(&mut file, what, "pipe record has wrong length", eof, |_| {
                pipe_available(self.raw())
            })
        }
    }
    fn validate_pipe(handle: HANDLE, what: &str) -> Result<()> {
        require(unsafe { GetFileType(handle) } == FILE_TYPE_PIPE, what)?;
        let mut flags = 0;
        win32!(
            GetNamedPipeInfo(
                handle,
                &mut flags,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            ),
            what
        )?;
        require(flags & PIPE_TYPE_MESSAGE == 0, what)
    }
    fn pipe_available(handle: HANDLE) -> io::Result<Option<usize>> {
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                handle,
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                &mut available,
                ptr::null_mut(),
            )
        } != 0
        {
            return Ok(Some(available as usize));
        }
        match unsafe { GetLastError() } {
            ERROR_BROKEN_PIPE => Ok(None),
            _ => Err(io::Error::last_os_error()),
        }
    }
    impl AsHandle for Pipe {
        fn as_handle(&self) -> BorrowedHandle<'_> {
            self.0.as_ref().unwrap().as_handle()
        }
    }
    type Pipe = Handle;
    #[derive(Default)]
    struct Pseudo(HPCON);
    fn retire_pseudo_with(handle: HPCON, close: impl FnOnce(HPCON) + Send + 'static) {
        if handle != 0 {
            let _ = thread::Builder::new()
                .name("moor-conpty-close".into())
                .spawn(move || close(handle));
        }
    }
    impl Pseudo {
        fn retire(&mut self) {
            let handle = std::mem::replace(&mut self.0, 0);
            retire_pseudo_with(handle, |handle| unsafe { ClosePseudoConsole(handle) });
        }
    }
    impl Drop for Pseudo {
        fn drop(&mut self) {
            self.retire();
        }
    }
    unsafe impl AsPseudoConsole for Pseudo {
        fn raw_pseudoconsole(&self) -> isize {
            self.0
        }
    }
    crate::schema!(tuple OpenPolicy [Clone, Copy]; fields; u32, u32);
    crate::schema!(tuple RelativePolicy [Clone, Copy]; fields; u32, u32, u32, u32);
    type StoreFile = (File, [u8; 24]);
    type StoreFileResult = std::result::Result<StoreFile, (io::Error, Option<StoreFile>)>;
    const SHARE_RW: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
    const SHARE_ALL: u32 = SHARE_RW | FILE_SHARE_DELETE;
    const NO_FOLLOW: u32 = FILE_FLAG_OPEN_REPARSE_POINT;
    const OPEN_SLOT: OpenPolicy = OpenPolicy(FILE_READ_ATTRIBUTES, SHARE_ALL);
    const OPEN_STDERR: OpenPolicy = OpenPolicy(
        FILE_APPEND_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
        FILE_SHARE_READ,
    );
    const CREATE_STAGE: OpenPolicy = OpenPolicy(
        GENERIC_READ | GENERIC_WRITE | DELETE,
        FILE_SHARE_READ | FILE_SHARE_DELETE,
    );
    const OPEN_STAGE: OpenPolicy = OpenPolicy(GENERIC_READ | DELETE, SHARE_ALL);
    const OPEN_MARKER: OpenPolicy = OpenPolicy(GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_DELETE);
    const OPEN_STORE: OpenPolicy = OpenPolicy(GENERIC_READ | GENERIC_WRITE, SHARE_RW);
    const OPEN_RB: OpenPolicy = OpenPolicy(FILE_READ_ATTRIBUTES | DELETE, SHARE_ALL);
    const CREATE_DIRECTORY: RelativePolicy = RelativePolicy(
        GENERIC_WRITE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL | DELETE,
        SHARE_RW,
        FILE_CREATE,
        FILE_DIRECTORY_FILE,
    );
    const CREATE_SLOT: RelativePolicy = RelativePolicy(
        GENERIC_READ | GENERIC_WRITE | DELETE,
        SHARE_ALL,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE,
    );
    const OPEN_ROLLBACK_SLOT: RelativePolicy = RelativePolicy(
        FILE_READ_ATTRIBUTES | DELETE,
        SHARE_ALL,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
    );
    unsafe fn open_handle(
        path: &Path,
        policy: OpenPolicy,
        security: Option<&SECURITY_ATTRIBUTES>,
        what: &str,
    ) -> Result<Handle> {
        let (security, creation) = security.map_or((ptr::null(), OPEN_EXISTING), |value| {
            (value as *const SECURITY_ATTRIBUTES, CREATE_NEW)
        });
        unsafe {
            Handle::checked(
                CreateFileW(
                    wide(path.as_os_str()).as_ptr(),
                    policy.0,
                    policy.1,
                    security,
                    creation,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    ptr::null_mut(),
                ),
                what,
            )
        }
    }
    fn reopen(file: &File, policy: OpenPolicy) -> Result<File> {
        let raw = unsafe { ReOpenFile(file.as_raw_handle(), policy.0, policy.1, NO_FOLLOW) };
        Handle::checked(raw, "reopen exact Windows object").map(Handle::into_file)
    }
    fn directory_cause(error: &io::Error) -> DirectoryCause {
        match error.raw_os_error().map(|code| code as u32) {
            Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND | ERROR_INVALID_NAME) => {
                DirectoryCause::Missing
            }
            Some(ERROR_DIRECTORY) => DirectoryCause::NotDirectory,
            Some(ERROR_ACCESS_DENIED) => DirectoryCause::NotSearchable,
            _ => DirectoryCause::Io,
        }
    }
    const CAUSES: [&str; 4] = ["missing", "not-directory", "not-searchable", "io-error"];
    fn read_reparse(path: &Path, share: u32) -> Result<File> {
        OpenOptions::new()
            .read(true)
            .share_mode(share)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .map_err(string)
    }
    unsafe fn file_info<T: Default>(
        handle: HANDLE,
        class: FILE_INFO_BY_HANDLE_CLASS,
        what: &str,
    ) -> Result<T> {
        let mut info = T::default();
        win32!(
            GetFileInformationByHandleEx(
                handle,
                class,
                (&mut info as *mut T).cast(),
                size_of::<T>() as u32,
            ),
            what
        )?;
        Ok(info)
    }
    unsafe fn token_user(token: Handle) -> Result<String> {
        let mut size = 0;
        unsafe { GetTokenInformation(token.raw(), TokenUser, ptr::null_mut(), 0, &mut size) };
        let mut words = vec![0usize; size as usize / size_of::<usize>() + 1];
        win32!(
            GetTokenInformation(
                token.raw(),
                TokenUser,
                words.as_mut_ptr().cast(),
                size,
                &mut size
            ),
            "read user SID"
        )?;
        let sid = unsafe {
            &*(*(words.as_ptr().cast::<TOKEN_USER>()))
                .User
                .Sid
                .cast::<windows_permissions::Sid>()
        };
        Ok(sid.to_string())
    }
    fn sid() -> Result<&'static str> {
        static USER: OnceLock<Result<String>> = OnceLock::new();
        USER.get_or_init(|| {
            windows_permissions::utilities::current_process_sid()
                .map(|sid| sid.to_string())
                .map_err(|error| format!("read user SID: {error}"))
        })
        .as_deref()
        .map_err(Clone::clone)
    }
    fn descriptor(
        sid: &str,
        access: &str,
    ) -> Result<(LocalBox<SecurityDescriptor>, SECURITY_ATTRIBUTES)> {
        let descriptor: LocalBox<SecurityDescriptor> =
            format!("O:{sid}D:P(A;;{access};;;SY)(A;;{access};;;{sid})")
                .parse()
                .map_err(|error: io::Error| format!("build protected DACL: {error}"))?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: (&*descriptor as *const SecurityDescriptor)
                .cast_mut()
                .cast(),
            bInheritHandle: 0,
        };
        Ok((descriptor, attributes))
    }
    fn descriptor_control(descriptor: &SecurityDescriptor) -> Result<u16> {
        let (mut control, mut revision) = (0, 0);
        win32!(
            GetSecurityDescriptorControl(
                (descriptor as *const SecurityDescriptor).cast_mut().cast(),
                &mut control,
                &mut revision,
            ),
            "inspect Windows security descriptor control"
        )?;
        Ok(control)
    }
    fn acl_entries(acl: &windows_permissions::Acl) -> Option<usize> {
        let acl = (acl as *const windows_permissions::Acl).cast::<ACL>();
        (unsafe { IsValidAcl(acl) } != 0).then(|| unsafe { (*acl).AceCount as usize })
    }
    fn ace_bytes(acl: &windows_permissions::Acl, index: usize) -> Option<&[u8]> {
        let raw_acl = (acl as *const windows_permissions::Acl).cast::<ACL>();
        if unsafe { IsValidAcl(raw_acl) } == 0 {
            return None;
        }
        let mut ace: *mut c_void = ptr::null_mut();
        if unsafe { GetAce(raw_acl, index.try_into().ok()?, &mut ace) } == 0 || ace.is_null() {
            return None;
        }
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        let size = usize::from(header.AceSize);
        let start = (ace as usize).checked_sub(raw_acl as usize)?;
        let end = start.checked_add(size)?;
        let acl_size = usize::from(unsafe { (*raw_acl).AclSize });
        if size < size_of::<ACE_HEADER>() || start < size_of::<ACL>() || end > acl_size {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts(ace.cast::<u8>(), size) })
    }
    fn descriptor_matches(
        actual: &SecurityDescriptor,
        expected: &SecurityDescriptor,
    ) -> Result<bool> {
        if unsafe {
            IsValidSecurityDescriptor((actual as *const SecurityDescriptor).cast_mut().cast()) == 0
                || IsValidSecurityDescriptor(
                    (expected as *const SecurityDescriptor).cast_mut().cast(),
                ) == 0
        } {
            return Ok(false);
        }
        let protected = descriptor_control(actual)? & SE_DACL_PROTECTED != 0;
        let (Some(actual_owner), Some(expected_owner)) = (actual.owner(), expected.owner()) else {
            return Ok(false);
        };
        let (Some(actual_dacl), Some(expected_dacl)) = (actual.dacl(), expected.dacl()) else {
            return Ok(false);
        };
        let (Some(actual_entries), Some(expected_entries)) =
            (acl_entries(actual_dacl), acl_entries(expected_dacl))
        else {
            return Ok(false);
        };
        Ok(exact_descriptor_semantics(
            actual_owner == expected_owner,
            protected,
            actual_entries,
            expected_entries,
            |left, right| match (
                ace_bytes(actual_dacl, left),
                ace_bytes(expected_dacl, right),
            ) {
                (Some(actual), Some(expected)) => actual == expected,
                _ => false,
            },
        ))
    }
    fn instrument_descriptor_matches(
        actual: &SecurityDescriptor,
        expected: &SecurityDescriptor,
    ) -> Result<bool> {
        use windows_permissions::constants::AceType::{
            ACCESS_ALLOWED_ACE_TYPE, ACCESS_ALLOWED_CALLBACK_ACE_TYPE,
            ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE, ACCESS_ALLOWED_OBJECT_ACE_TYPE,
        };
        let protected = descriptor_control(actual)? & SE_DACL_PROTECTED != 0;
        let (Some(actual_owner), Some(expected_owner), Some(dacl)) =
            (actual.owner(), expected.owner(), actual.dacl())
        else {
            return Ok(false);
        };
        if !protected || actual_owner != expected_owner {
            return Ok(false);
        }
        let system: LocalBox<windows_permissions::Sid> = "S-1-5-18"
            .parse()
            .map_err(|error: io::Error| format!("build system SID: {error}"))?;
        let write = GENERIC_ALL
            | GENERIC_WRITE
            | FILE_WRITE_DATA
            | FILE_APPEND_DATA
            | FILE_WRITE_EA
            | FILE_WRITE_ATTRIBUTES
            | DELETE
            | WRITE_DAC
            | WRITE_OWNER;
        for at in 0..dacl.len() {
            let Some(ace) = dacl.get_ace(at) else {
                return Ok(false);
            };
            if !matches!(
                ace.ace_type(),
                ACCESS_ALLOWED_ACE_TYPE
                    | ACCESS_ALLOWED_CALLBACK_ACE_TYPE
                    | ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE
                    | ACCESS_ALLOWED_OBJECT_ACE_TYPE
            ) {
                continue;
            }
            let trusted = ace
                .sid()
                .is_some_and(|sid| sid == actual_owner || sid == &*system);
            if !trusted && ace.mask().bits() & write != 0 {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn handle_attributes(file: &File) -> std::result::Result<u32, &'static str> {
        let attributes: FILE_ATTRIBUTE_TAG_INFO = unsafe {
            file_info(
                file.as_raw_handle(),
                FileAttributeTagInfo,
                "inspect protected Windows object",
            )
            .map_err(|_| "io-error")?
        };
        Ok(attributes.FileAttributes)
    }
    fn validate_stderr_handle(file: &File, user: &str) -> std::result::Result<(), &'static str> {
        let attributes = handle_attributes(file).map_err(|_| "io-error")?;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("reparse-point");
        }
        if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return Err("wrong-type");
        }
        let selector = SecurityInformation::Owner | SecurityInformation::Dacl;
        let actual = wrappers::GetSecurityInfo(file, SeObjectType::SE_FILE_OBJECT, selector)
            .map_err(|_| "io-error")?;
        let (expected, _) = descriptor(user, "FA").map_err(|_| "io-error")?;
        if actual
            .owner()
            .zip(expected.owner())
            .is_none_or(|(actual, expected)| actual != expected)
        {
            return Err("wrong-owner");
        }
        if !descriptor_matches(&actual, &expected).map_err(|_| "io-error")? {
            return Err("broad-dacl");
        }
        Ok(())
    }
    fn validate_instrument_handle(
        file: &File,
        user: &str,
    ) -> std::result::Result<(), &'static str> {
        let attributes = handle_attributes(file).map_err(|_| "io-error")?;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("reparse-point");
        }
        if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return Err("wrong-type");
        }
        let selector = SecurityInformation::Owner | SecurityInformation::Dacl;
        let actual = wrappers::GetSecurityInfo(file, SeObjectType::SE_FILE_OBJECT, selector)
            .map_err(|_| "io-error")?;
        let (expected, _) = descriptor(user, "FA").map_err(|_| "io-error")?;
        if actual
            .owner()
            .zip(expected.owner())
            .is_none_or(|(actual, expected)| actual != expected)
        {
            return Err("wrong-owner");
        }
        if !instrument_descriptor_matches(&actual, &expected).map_err(|_| "io-error")? {
            return Err("broad-dacl");
        }
        Ok(())
    }
    fn validate_exact_handle(file: &File, user: &str, access: &str, directory: bool) -> Result<()> {
        let attributes = handle_attributes(file).map_err(str::to_owned)?;
        require(
            attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
                && (attributes & FILE_ATTRIBUTE_DIRECTORY != 0) == directory,
            "reject Windows reparse point",
        )?;
        let selector = SecurityInformation::Owner | SecurityInformation::Dacl;
        let actual = wrappers::GetSecurityInfo(file, SeObjectType::SE_FILE_OBJECT, selector)
            .map_err(|error| format!("read protected DACL: {error}"))?;
        let (expected, _) = descriptor(user, access)?;
        require(
            descriptor_matches(&actual, &expected)?,
            "unexpected Windows owner/DACL",
        )
    }
    #[cfg(test)]
    mod security_descriptor_tests {
        use super::*;
        include!("../tests/unit/windows_security.rs");
    }
    fn protect(path: &Path, sid: &str, access: &str) -> Result<()> {
        let (descriptor, _) = descriptor(sid, access)?;
        wrappers::SetNamedSecurityInfo(
            path.as_os_str(),
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
            None,
            None,
            descriptor.dacl(),
            None,
        )
        .map_err(|error| format!("protect staged object: {error}"))
    }
    pub(super) fn validate(path: &Path, user: &str, access: &str, directory: bool) -> Result<()> {
        let name = wide(path.as_os_str());
        let attributes = unsafe { GetFileAttributesW(name.as_ptr()) };
        check(
            attributes != INVALID_FILE_ATTRIBUTES
                && attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
                && (attributes & FILE_ATTRIBUTE_DIRECTORY != 0) == directory,
            "reject Windows reparse point",
        )?;
        let selector = SecurityInformation::Owner | SecurityInformation::Dacl;
        let actual = wrappers::GetNamedSecurityInfo(
            path.as_os_str(),
            SeObjectType::SE_FILE_OBJECT,
            selector,
        )
        .map_err(|error| format!("read protected DACL: {error}"))?;
        let (expected, _) = descriptor(user, access)?;
        require(
            descriptor_matches(&actual, &expected)?,
            "unexpected Windows owner/DACL",
        )
    }
    pub(crate) fn protected_store_path(path: &Path, directory: bool) -> bool {
        sid()
            .and_then(|user| validate(path, user, "FA", directory))
            .is_ok()
    }
    pub(crate) fn valid_store_slots(path: &Path, slots: &[File; 4]) -> bool {
        unsafe {
            let mut identities = [[0; 24]; 4];
            for (at, (name, file)) in ["body.0", "body.1", "commit.0", "commit.1"]
                .into_iter()
                .zip(slots)
                .enumerate()
            {
                let Ok(identity) = unique_file_identity(file.as_raw_handle()) else {
                    return false;
                };
                if identities[..at].contains(&identity)
                    || store_file_identity(&path.join(name)) != Ok(identity)
                {
                    return false;
                }
                identities[at] = identity;
            }
            true
        }
    }
    pub(crate) fn create_store_path(path: &Path) -> io::Result<()> {
        let result = (|| unsafe {
            let (_descriptor, sa) = descriptor(sid()?, "FA")?;
            check(
                CreateDirectoryW(wide(path.as_os_str()).as_ptr(), &sa) != 0,
                "create protected store directory",
            )
        })();
        result.map_err(io::Error::other)
    }
    fn directory_handle(path: &Path, delete: bool, share_delete: bool) -> io::Result<File> {
        let access = GENERIC_WRITE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL;
        OpenOptions::new()
            .access_mode(access | if delete { DELETE } else { 0 })
            .share_mode(
                FILE_SHARE_READ
                    | FILE_SHARE_WRITE
                    | if share_delete { FILE_SHARE_DELETE } else { 0 },
            )
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
    pub(crate) fn store_directory(path: &Path, delete: bool) -> io::Result<File> {
        directory_handle(path, delete, true)
    }
    fn relative_file(
        parent: &File,
        name: &OsStr,
        user: &str,
        policy: RelativePolicy,
    ) -> Result<File> {
        let RelativePolicy(access, share, disposition, options) = policy;
        let mut name = name.encode_wide().collect::<Vec<_>>();
        let length = u16::try_from(name.len().saturating_mul(2))
            .map_err(|_| "store object name is too long")?;
        let unicode = UNICODE_STRING {
            Length: length,
            MaximumLength: length,
            Buffer: name.as_mut_ptr(),
        };
        let (security, _) = descriptor(user, "FA")?;
        let attributes = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent.as_raw_handle(),
            ObjectName: &unicode,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: (&*security as *const SecurityDescriptor).cast(),
            SecurityQualityOfService: ptr::null(),
        };
        let (mut raw, mut status) = (INVALID_HANDLE_VALUE, IO_STATUS_BLOCK::default());
        let result = unsafe {
            NtCreateFile(
                &mut raw,
                access | SYNCHRONIZE,
                &attributes,
                &mut status,
                ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                share,
                disposition,
                FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT | options,
                ptr::null(),
                0,
            )
        };
        let handle = unsafe { Handle::owned(raw) };
        require(
            result == STATUS_SUCCESS && !handle.is_null(),
            "open protected store object",
        )?;
        Ok(handle.into_file())
    }
    pub(crate) fn create_store_directory(path: &Path, event: bool) -> io::Result<(File, bool)> {
        if event {
            let opened = store_directory(path, false);
            if !matches!(&opened, Err(error) if error.kind() == io::ErrorKind::NotFound) {
                return opened.map(|directory| (directory, false));
            }
        }
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| io::Error::other("store directory has no parent"))?;
        let parent = directory_handle(parent, false, false)?;
        relative_file(
            &parent,
            path.file_name()
                .ok_or_else(|| io::Error::other("store directory has no name"))?,
            sid().map_err(io::Error::other)?,
            CREATE_DIRECTORY,
        )
        .map(|directory| (directory, true))
        .map_err(io::Error::other)
    }
    pub(crate) fn create_store_file(directory: &File, name: &str) -> StoreFileResult {
        let failure = |error| (io::Error::other(error), None);
        let created = relative_file(
            directory,
            OsStr::new(name),
            sid().map_err(failure)?,
            CREATE_SLOT,
        )
        .map_err(failure)?;
        let rollback = || {
            delete_file(&created);
        };
        let identity = unsafe { unique_file_identity(created.as_raw_handle()) }
            .inspect_err(|_| rollback())
            .map_err(failure)?;
        let guard = reopen(&created, OPEN_SLOT)
            .inspect_err(|_| rollback())
            .map_err(failure)?;
        drop(created);
        reopen(&guard, OPEN_STORE)
            .map(|file| (file, identity))
            .map_err(|error| (io::Error::other(error), Some((guard, identity))))
    }
    pub(crate) fn valid_store_directory(path: &Path, directory: &File) -> bool {
        store_directory(path, false)
            .and_then(|current| {
                unsafe { file_identity(current.as_raw_handle()) }.map_err(io::Error::other)
            })
            .is_ok_and(|current| unsafe { file_identity(directory.as_raw_handle()) } == Ok(current))
    }
    pub(crate) fn rollback_store(directory: File, ids: &[[u8; 24]], state: (Option<File>, bool)) {
        let Ok(user) = sid() else {
            return;
        };
        if let Some(file) = state.0.as_ref().and_then(|file| reopen(file, OPEN_RB).ok()) {
            delete_file(&file);
        }
        for (name, expected) in ["body.0", "body.1", "commit.0", "commit.1"]
            .into_iter()
            .zip(ids)
        {
            let Ok(file) = relative_file(&directory, OsStr::new(name), user, OPEN_ROLLBACK_SLOT)
            else {
                continue;
            };
            if unsafe { unique_file_identity(file.as_raw_handle()) } == Ok(*expected) {
                delete_file(&file);
            }
        }
        drop(state.0);
        if state.1 {
            let deadline = Instant::now() + Duration::from_millis(250);
            while !delete_file(&directory) && Instant::now() < deadline {
                // Slot dispositions become visible asynchronously under
                // filesystem load. Keep the exact owned-directory handle
                // pinned while waiting for its namespace to become empty.
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    pub(crate) fn delete_file(file: &File) -> bool {
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            ) != 0
        }
    }
    fn stage_file<T>(
        path: &Path,
        user: &str,
        access: &str,
        what: &str,
        write: impl FnOnce(&mut File) -> Result<T>,
    ) -> Result<(File, [u8; 24], T)> {
        let mut file = unsafe {
            let (_descriptor, sa) = descriptor(user, "GA")?;
            open_handle(path, CREATE_STAGE, Some(&sa), what)?.into_file()
        };
        let staged: Result<([u8; 24], T)> = (|| {
            let value = write(&mut file)?;
            file.sync_all().map_err(string)?;
            let identity = unsafe { file_identity(file.as_raw_handle())? };
            protect(path, user, access)?;
            validate(path, user, access, false)?;
            Ok((identity, value))
        })();
        let (identity, value) = staged.inspect_err(|_| {
            delete_file(&file);
        })?;
        let exact = unsafe { open_handle(path, OPEN_STAGE, None, "reopen staged object") }
            .and_then(|exact| unsafe {
                require(
                    file_identity(exact.raw())? == identity,
                    "staged object identity changed",
                )?;
                Ok(exact.into_file())
            })
            .inspect_err(|_| {
                delete_file(&file);
            })?;
        Ok((exact, identity, value))
    }
    unsafe fn file_identity(handle: HANDLE) -> Result<[u8; 24]> {
        let info: FILE_ID_INFO =
            unsafe { file_info(handle, FileIdInfo, "query Windows file identity")? };
        let mut identity = [0; 24];
        identity[..8].copy_from_slice(&info.VolumeSerialNumber.to_le_bytes());
        identity[8..].copy_from_slice(&info.FileId.Identifier);
        Ok(identity)
    }
    unsafe fn unique_file_identity(handle: HANDLE) -> Result<[u8; 24]> {
        let info: FILE_STANDARD_INFO =
            unsafe { file_info(handle, FileStandardInfo, "inspect Windows store slot links")? };
        require(info.NumberOfLinks == 1, "hard-linked Windows store slot")?;
        unsafe { file_identity(handle) }
    }
    fn session_identity(file: [u8; 24]) -> [u8; 25] {
        let mut identity = [0; 25];
        identity[0] = 2;
        identity[1..].copy_from_slice(&file);
        identity
    }
    unsafe fn store_file_identity(path: &Path) -> Result<[u8; 24]> {
        let file = unsafe { open_handle(path, OPEN_SLOT, None, "open Windows store slot")? };
        unsafe { unique_file_identity(file.raw()) }
    }
    fn selector_decimal(text: &str) -> Result<usize> {
        crate::canonical_u64(text)
            .filter(|value| *value != 0)
            .and_then(|value| value.try_into().ok())
            .ok_or_else(|| "invalid bootstrap selector".into())
    }

    fn parse_nonce(text: &str) -> Result<[u8; 16]> {
        u128::from_str_radix(text, 16)
            .ok()
            .filter(|_| text.len() == 32 && lowercase_hex(text.as_bytes()))
            .map(u128::to_be_bytes)
            .ok_or_else(|| "invalid bootstrap nonce".into())
    }

    fn inherited(name: &str, pipe: bool) -> Result<Option<Handle>> {
        let Some(value) = std::env::var_os(name) else {
            return Ok(None);
        };
        unsafe { std::env::remove_var(name) };
        let text = value.to_str().ok_or("invalid bootstrap selector")?;
        let handle = unsafe { Handle::owned(selector_decimal(text)? as HANDLE) };
        let mut inherit = 0;
        let inspected = unsafe { GetHandleInformation(handle.raw(), &mut inherit) };
        check(inspected != 0, "inspect bootstrap handle")?;
        let inherited = inherit & HANDLE_FLAG_INHERIT != 0;
        require(inherited, "bootstrap handle was not inherited")?;
        if pipe {
            validate_pipe(handle.raw(), "bootstrap channel")?;
        }
        Ok(Some(handle))
    }
    unsafe fn transfer_handle(source: HANDLE, target: HANDLE) -> Result<u64> {
        let mut copy = ptr::null_mut();
        win32!(
            DuplicateHandle(
                GetCurrentProcess(),
                source,
                target,
                &mut copy,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            ),
            "transfer requested child handles"
        )?;
        Ok(copy as usize as u64)
    }

    fn bootstrap_inner(selector: OsString) -> Result<i32> {
        let text = selector.to_str().ok_or("invalid bootstrap selector")?;
        let (holder, nonces) = text.split_once(':').ok_or("invalid bootstrap selector")?;
        let (nonce, insertion) = nonces.split_once(':').ok_or("invalid bootstrap selector")?;
        let (nonce, instrument_nonce) = (parse_nonce(nonce)?, parse_nonce(insertion)?);
        let holder_pid =
            u32::try_from(selector_decimal(holder)?).map_err(|_| "invalid holder pid")?;
        let command = std::env::args_os().skip(1).collect::<Vec<_>>();
        let semantic_token = std::env::var_os(SEMANTIC_TOKEN);
        unsafe { std::env::remove_var(SEMANTIC_TOKEN) };
        require(!command.is_empty(), "empty bootstrap command")?;
        unsafe {
            let required =
                |name| inherited(name, true)?.ok_or_else(|| "missing bootstrap handle".to_string());
            let (control, result) = (required(BOOTSTRAP_CONTROL)?, required(BOOTSTRAP_RESULT)?);
            let stderr = inherited(BOOTSTRAP_STDERR, false)?;
            let instrument = inherited(BOOTSTRAP_INSTRUMENT, true)?;
            let raw = [
                control.raw(),
                result.raw(),
                stderr.as_ref().map_or(ptr::null_mut(), Handle::raw),
                instrument.as_ref().map_or(ptr::null_mut(), Pipe::raw),
            ];
            require(
                raw.iter()
                    .enumerate()
                    .all(|(at, handle)| handle.is_null() || !raw[..at].contains(handle)),
                "aliased bootstrap handles",
            )?;
            if let Some(directory) = std::env::var_os(BOOTSTRAP_DIRECTORY) {
                std::env::remove_var(BOOTSTRAP_DIRECTORY);
                if let Err(error) = std::env::set_current_dir(directory) {
                    result.write(
                        &bootstrap_failure_record(
                            nonce,
                            BootstrapFailure::Directory(directory_cause(&error)),
                        ),
                        "report rejected working directory",
                    )?;
                    return Ok(1);
                }
            }
            let (program, args) = command.split_first().unwrap();
            let mut requested = SpawnCommand::new(program);
            requested
                .args(args)
                .env_remove(INSTRUMENT_NONCE)
                .env_remove(SEMANTIC_TOKEN);
            requested.env_remove(INSTRUMENT_CHANNEL);
            if let Some(instrument) = instrument.as_ref() {
                win(
                    requested.env_handle_lower_hex(INSTRUMENT_CHANNEL, instrument),
                    "transfer instrumentation channel",
                )?;
            }
            if instrument.is_some() {
                requested.env(
                    INSTRUMENT_NONCE,
                    format!("{:032x}", u128::from_be_bytes(instrument_nonce)),
                );
            }
            if let Some(token) = semantic_token {
                requested.env(SEMANTIC_TOKEN, token);
            }
            if let Some(handle) = &stderr {
                requested.stderr(win(
                    SpawnStdio::from_borrowed(handle),
                    "transfer requested child stderr",
                )?);
            }
            let mut child = match requested.spawn_suspended_with(
                SpawnOptions::new().creation_flags(CreationFlags::NEW_PROCESS_GROUP),
            ) {
                Ok(child) => Some(child),
                Err(error) => {
                    let code = error
                        .raw_os_error()
                        .map_or(ERROR_GEN_FAILURE, |code| code as u32);
                    result.write(
                        &bootstrap_failure_record(nonce, BootstrapFailure::Execution(code)),
                        "report requested child start failure",
                    )?;
                    return Ok(127);
                }
            };
            drop((requested, instrument));
            let holder = Handle::checked(
                OpenProcess(PROCESS_DUP_HANDLE, 0, holder_pid),
                "open holder for handle transfer",
            )?;
            let requested = child.as_ref().unwrap();
            let process_handle = requested.as_handle().as_raw_handle() as HANDLE;
            let thread_handle = requested.primary_thread_handle().as_raw_handle() as HANDLE;
            let record = BootstrapRecord {
                nonce,
                process: transfer_handle(process_handle, holder.raw())?,
                thread: transfer_handle(thread_handle, holder.raw())?,
                created: process_birth(process_handle, "read requested child birth")?,
                pid: requested.id(),
            };
            result.write(&record.encode(), "report requested child")?;
            let mut resumed = false;
            loop {
                let mut command = [0; 17];
                let Ok(read @ 1..) = control.read(&mut command) else {
                    if !resumed {
                        TerminateProcess(process_handle, 0xc000013a);
                        return Ok(125);
                    }
                    return Ok(0);
                };
                let kind = accept_bootstrap_command(&command[..read], nonce, &mut resumed)
                    .ok_or("invalid bootstrap command")?;
                let ok = if kind == 1 {
                    child.take().is_some_and(|child| child.resume().is_ok())
                } else {
                    GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, record.pid) != 0
                };
                result.write(&[u8::from(!ok)], "acknowledge bootstrap command")?;
            }
        }
    }

    pub fn bootstrap() -> Option<i32> {
        let selector = std::env::var_os(BOOTSTRAP_SELECTOR)?;
        unsafe { std::env::remove_var(BOOTSTRAP_SELECTOR) };
        Some(bootstrap_inner(selector).unwrap_or(125))
    }

    unsafe fn remote_call(process: HANDLE, entry: FARPROC, parameter: *mut c_void) -> Result<u32> {
        let start: LPTHREAD_START_ROUTINE = unsafe { std::mem::transmute(entry) };
        let thread = Handle::checked(
            unsafe {
                CreateRemoteThread(
                    process,
                    ptr::null(),
                    0,
                    start,
                    parameter,
                    0,
                    ptr::null_mut(),
                )
            },
            "start remote instrumentation call",
        )?;
        check(
            unsafe { WaitForSingleObject(thread.raw(), 2000) } == WAIT_OBJECT_0,
            "wait for remote instrumentation call",
        )?;
        let mut status = 0;
        win32!(
            GetExitCodeThread(thread.raw(), &mut status),
            "read remote instrumentation result"
        )?;
        Ok(status)
    }

    unsafe fn remote_module(pid: u32, path: &Path) -> Result<usize> {
        let snapshot = Handle::checked(
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) },
            "enumerate remote modules",
        )?;
        let wanted = path
            .file_name()
            .ok_or("instrumentation path has no basename")?;
        let mut module = MODULEENTRY32W {
            dwSize: size_of::<MODULEENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = unsafe { Module32FirstW(snapshot.raw(), &mut module) } != 0;
        while more {
            let used = module
                .szModule
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(module.szModule.len());
            if OsString::from_wide(&module.szModule[..used]).eq_ignore_ascii_case(wanted) {
                return Ok(module.modBaseAddr as usize);
            }
            more = unsafe { Module32NextW(snapshot.raw(), &mut module) } != 0;
        }
        Err("instrumentation module was not loaded into requested child".into())
    }

    fn stream_handle(stream: &Stream) -> HANDLE {
        let Stream::NamedPipe(pipe) = stream;
        pipe.inner().as_handle().as_raw_handle()
    }

    fn local_name(pipe: &[u8; 46]) -> Result<Name<'_>> {
        let name = std::str::from_utf8(pipe).map_err(|_| "invalid pipe name")?;
        OsStr::new(name)
            .to_fs_name::<GenericFilePath>()
            .map_err(string)
    }

    fn same_user(stream: &Stream, user: &str) -> std::result::Result<bool, ()> {
        unsafe {
            if ImpersonateNamedPipeClient(stream_handle(stream)) == 0 {
                return Err(());
            }
            let mut token = ptr::null_mut();
            let same = if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) != 0 {
                token_user(Handle::owned(token))
                    .map(|sid| sid == user)
                    .map_err(|_| ())
            } else {
                Err(())
            };
            if RevertToSelf() == 0 { Err(()) } else { same }
        }
    }

    type Authentication = (Stream, [u8; 4], bool, Option<u32>);
    fn authenticate(mut stream: Stream, user: &str) -> Option<Authentication> {
        let handle = stream_handle(&stream);
        let preface = fixed_record(
            &mut stream,
            "authenticate client",
            "pipe record has wrong length",
            false,
            |_| pipe_available(handle),
        )
        .ok()?;
        let owner = same_user(&stream, user).ok()?;
        let pid = if owner {
            Some(stream.peer_creds().ok()?.pid().filter(|pid| *pid != 0)?)
        } else {
            None
        };
        Some((stream, preface, owner, pid))
    }

    fn live_holder_ancestor(mut pid: u32) -> bool {
        let holder = unsafe { GetCurrentProcessId() };
        if pid == 0 || holder == 0 {
            return false;
        }
        if pid == holder {
            return true;
        }
        let Ok(snapshot) = Handle::checked(
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) },
            "enumerate process ancestry",
        ) else {
            return false;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if unsafe { Process32FirstW(snapshot.raw(), &mut entry) } == 0 {
            return false;
        }
        let mut entries = Vec::with_capacity(4096);
        loop {
            if entries.len() == 4096 {
                return false;
            }
            entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
            if unsafe { Process32NextW(snapshot.raw(), &mut entry) } == 0 {
                if unsafe { GetLastError() } != ERROR_NO_MORE_FILES {
                    return false;
                }
                break;
            }
        }
        entries.sort_unstable_by_key(|entry| entry.0);
        for _ in 0..entries.len() {
            let Ok(index) = entries.binary_search_by_key(&pid, |entry| entry.0) else {
                return false;
            };
            let parent = entries[index].1;
            if parent == holder {
                return true;
            }
            if parent == 0 || parent == pid {
                return false;
            }
            pid = parent;
        }
        false
    }

    fn cancel(stream: &Stream) {
        unsafe { windows_sys::Win32::System::IO::CancelIoEx(stream_handle(stream), ptr::null()) };
    }
    fn stop_job(job: &Job) -> bool {
        if job.terminate(0xc000013a).is_err() {
            return false;
        }
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            let mut state = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            if unsafe {
                QueryInformationJobObject(
                    job.as_handle().as_raw_handle(),
                    JobObjectBasicAccountingInformation,
                    (&mut state as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    ptr::null_mut(),
                )
            } == 0
            {
                return false;
            }
            if state.ActiveProcesses == 0 {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
    static SHUTDOWN: AtomicU8 = AtomicU8::new(0);
    unsafe extern "system" fn shutdown_handler(kind: u32) -> i32 {
        let Some(terminal) = super::console_control_kind(kind) else {
            return FALSE;
        };
        let previous = SHUTDOWN.fetch_or(1, Ordering::Relaxed);
        SHUTDOWN.fetch_or(u8::from(previous != 0) << 1, Ordering::Relaxed);
        if terminal {
            // Returning TRUE for CLOSE/LOGOFF/SHUTDOWN authorizes Windows to
            // terminate the process immediately. This callback owns no
            // cleanup; it gives the normal loop a short graceful interval,
            // then requests force early enough for durable retirement before
            // Windows' shorter terminal-control deadline. Ctrl-C/Break retain
            // the ordinary five-/ten-second schedule in the state machine.
            unsafe { Sleep(2_000) };
            SHUTDOWN.fetch_or(2, Ordering::Release);
            loop {
                unsafe { Sleep(INFINITE) };
            }
        }
        TRUE
    }

    crate::schema!(struct Instrument fields; path: PathBuf, file: File, identity: [u8; 24], digest: [u8; 32], read: Pipe, write: Pipe);
    crate::schema!(struct Staged fields; path: PathBuf, file: File, identity: [u8; 24]);
    crate::schema!(struct Bootstrap derive [Default] fields; child: Option<Child>, control: Pipe, result: Pipe, nonce: [u8; 16]);
    crate::schema!(struct EventTarget fields; path: PathBuf, present: bool, created: bool, guards: Vec<File>);
    crate::schema!(struct Native derive [Default] fields; marker: PathBuf, stage_root: PathBuf, sid: String, generation: u32, options: Options, incarnation: [u8; 16], semantic_token: [u8; 16], synthetic: u8, geometry: (u16, u16), conpty: Pseudo, job: Option<Job>, bootstrap: Bootstrap, process: Handle, pid: u32, child_released: bool, early_exit: Option<u32>, birth: [u8; 16], input: Pipe, output: Pipe, stage: Option<Staged>, instrument: Option<Instrument>, stderr: Handle, ready: LaunchReporter<File>, identity: [u8; 25], event: Option<EventTarget>, artifacts: Option<PreparedArtifacts>);
    impl Bootstrap {
        fn exchange(&self, kind: u8) -> Result<()> {
            self.control
                .write(&bootstrap_command(kind, self.nonce), "command bootstrap")?;
            require(
                self.result
                    .record::<1>(false, "bootstrap acknowledgement")?
                    == [0],
                "bootstrap command failed",
            )
        }
    }
    fn publish_marker_stage(
        stage: &Staged,
        marker: &Path,
        user: &str,
        after_move: impl FnOnce(&Path),
    ) -> Result<()> {
        unsafe {
            win32!(
                MoveFileExW(
                    wide(stage.path.as_os_str()).as_ptr(),
                    wide(marker.as_os_str()).as_ptr(),
                    MOVEFILE_WRITE_THROUGH,
                ),
                "publish protected marker"
            )?;
            after_move(marker);
            let final_file = open_handle(marker, OPEN_MARKER, None, "reopen protected marker")?;
            validate_exact_handle(final_file.0.as_ref().unwrap(), user, "FR", false)?;
            require(
                file_identity(final_file.raw())? == stage.identity,
                "marker identity changed",
            )
        }
    }
    impl Native {
        fn prepare_storage(&mut self, marker_identity: [u8; 24]) -> Result<()> {
            self.identity = session_identity(marker_identity);
            let start = (now(), unsafe { GetTickCount64() }, boot_identity());
            let event_path = self.event.as_ref().map(|target| target.path.as_path());
            let event_directory = self.event.as_ref().and_then(|target| target.guards.last());
            let event_identity = event_path.map(|path| os_bytes(path.as_os_str()));
            let instrument_identity = self
                .instrument
                .as_ref()
                .map(|instrument| os_bytes(instrument.path.as_os_str()));
            self.artifacts = Some(holder_artifacts(
                &self.identity,
                (
                    (self.generation != 1).then_some(self.generation),
                    self.generation,
                ),
                self.incarnation,
                self.semantic_token,
                start,
                ArtifactConfig {
                    marker: &self.marker,
                    event_path,
                    encoding: "windows-wtf8",
                    event_identity: event_identity.as_deref(),
                    instrument_identity: instrument_identity.as_deref(),
                    event_store: None,
                    event_directory,
                    stores: None,
                    event_layout: 2,
                    log_cap: self.options.log_cap,
                },
            )?);
            let artifacts = self.artifacts.as_mut().unwrap();
            let cwd = absolute(self.options.directory.as_deref().unwrap_or(Path::new(".")))?;
            let containment = u32::from_le_bytes(random_array::<4>()?).max(1);
            put_wide(&mut artifacts.status, &os_bytes(cwd.as_os_str())).map_err(crate::protocol)?;
            for bytes in [self.pid.to_le_bytes(), containment.to_le_bytes()] {
                artifacts.status.extend_from_slice(&bytes);
            }
            artifacts.status.extend_from_slice(&self.birth);
            Ok(())
        }
        fn launch(
            &mut self,
            marker: &Marker,
            command: &[OsString],
            nonce: [u8; 16],
        ) -> Result<Listener> {
            let instrument = self.options.instrument.take();
            let listener = self.first_protected_pipe(&marker.pipe)?;
            self.stage = Some(self.stage_marker(&marker.encode())?);
            let marker_identity = self.stage.as_ref().unwrap().identity;
            if let Some(path) = &instrument {
                self.stage_instrument(path, &session_identity(marker_identity))?;
            }
            let pid = self.conpty_job_bootstrap(command, nonce)?;
            if instrument.is_some() {
                self.inject_and_ack(pid, nonce)?;
            }
            self.bootstrap.exchange(1)?;
            self.child_released = true;
            self.prepublication_alive()?;
            self.publish_marker()?;
            Ok(listener)
        }

        fn first_protected_pipe(&self, pipe: &[u8; 46]) -> Result<Listener> {
            let (descriptor, _) = descriptor(&self.sid, "0x12019f")?;
            let pipe_descriptor = unsafe {
                BorrowedSecurityDescriptor::from_ptr(
                    (&*descriptor as *const SecurityDescriptor).cast(),
                )
            }
            .to_owned_sd()
            .map_err(string)?;
            ListenerOptions::new()
                .name(local_name(pipe)?)
                .security_descriptor(pipe_descriptor)
                .nonblocking(ListenerNonblockingMode::Accept)
                .create_sync()
                .map_err(string)
        }
        fn conpty_job_bootstrap(
            &mut self,
            command: &[OsString],
            instrument_nonce: [u8; 16],
        ) -> Result<u32> {
            unsafe {
                let (cin, input) = Pipe::pair("create ConPTY input")?;
                let (output, cout) = Pipe::pair("create ConPTY output")?;
                let mut conpty = 0;
                check(
                    CreatePseudoConsole(
                        coordinate(self.geometry)?,
                        cin.raw(),
                        cout.raw(),
                        0,
                        &mut conpty,
                    ) >= 0,
                    "create ConPTY",
                )?;
                (self.input, self.output, self.conpty) = (input, output, Pseudo(conpty));
                let job = win(Job::create(), "create containment job")?;
                win(
                    job.set_kill_on_close(true),
                    "set no-breakaway kill-on-close job",
                )?;
                self.job = Some(job);
                if let Some(path) = &self.options.stderr {
                    self.stderr = open_stderr_operand(path, &self.sid, |_| {})?;
                }
                let (command_read, bootstrap_control) =
                    Pipe::pair("create bootstrap control channel")?;
                let (bootstrap_result, result_write) =
                    Pipe::pair("create bootstrap result channel")?;
                self.bootstrap = Bootstrap {
                    control: bootstrap_control,
                    result: bootstrap_result,
                    nonce: random_array()?,
                    ..Bootstrap::default()
                };
                let executable = std::env::current_exe().map_err(string)?;
                let mut bootstrap = SpawnCommand::new(executable);
                bootstrap.args(command);
                bootstrap.env_remove(BOOTSTRAP_DIRECTORY);
                if let Some(directory) = &self.options.directory {
                    // The bootstrap is single-threaded and performs the one
                    // authoritative directory change in its own process before
                    // resolving the requested executable. The parent never
                    // mutates its process-wide cwd, and no check/use pathname
                    // gap exists between acceptance and child inheritance.
                    bootstrap.env(BOOTSTRAP_DIRECTORY, directory);
                }
                bootstrap
                    .env_remove(BOOTSTRAP_SELECTOR)
                    .env_remove(INSTRUMENT_CHANNEL)
                    .env_remove(INSTRUMENT_NONCE)
                    .env_remove(SEMANTIC_TOKEN);
                if self.semantic_token != [0; 16] {
                    bootstrap.env(
                        SEMANTIC_TOKEN,
                        format!("{:032x}", u128::from_be_bytes(self.semantic_token)),
                    );
                }
                bootstrap.env(
                    BOOTSTRAP_SELECTOR,
                    format!(
                        "{}:{:032x}:{:032x}",
                        GetCurrentProcessId(),
                        u128::from_be_bytes(self.bootstrap.nonce),
                        u128::from_be_bytes(instrument_nonce)
                    ),
                );
                transfer_handles!(bootstrap;
                    BOOTSTRAP_CONTROL => Some(&command_read), "bootstrap control channel";
                    BOOTSTRAP_RESULT => Some(&result_write), "bootstrap result channel";
                    BOOTSTRAP_STDERR => (!self.stderr.is_null()).then_some(&self.stderr), "bootstrap standard-error sink";
                    BOOTSTRAP_INSTRUMENT => self.instrument.as_ref().map(|value| &value.write), "instrumentation channel";
                );
                drop((command_read, result_write));
                let launched = win(
                    bootstrap.spawn_suspended_with(
                        SpawnOptions::new()
                            .job(self.job.as_ref().unwrap())
                            .pseudoconsole(&self.conpty),
                    ),
                    "start Windows bootstrap",
                )?;
                drop(bootstrap);
                self.bootstrap.child = Some(win(launched.resume(), "resume contained bootstrap")?);
                if let Some(instrument) = &mut self.instrument {
                    drop(std::mem::take(&mut instrument.write));
                }
                let endpoint = &self.bootstrap;
                let identity = endpoint.result.record::<56>(false, "bootstrap identity")?;
                if let Some(BootstrapFailure::Directory(cause)) =
                    bootstrap_failure(&identity, endpoint.nonce)
                {
                    let directory = self
                        .options
                        .directory
                        .as_deref()
                        .expect("directory failure");
                    return Err(format!(
                        "could not enter {} ({})",
                        name::render(directory.as_os_str()),
                        CAUSES[cause as usize]
                    ));
                }
                if let Some(BootstrapFailure::Execution(code)) =
                    bootstrap_failure(&identity, endpoint.nonce)
                {
                    return Err(format!(
                        "could not execute {}: {}",
                        name::render(command[0].as_os_str()),
                        io::Error::from_raw_os_error(code as i32)
                    ));
                }
                let record = BootstrapRecord::decode(&identity, endpoint.nonce)
                    .ok_or("bootstrap identity was invalid")?;
                let process = Handle::owned(record.process as HANDLE);
                let thread = Handle::owned(record.thread as HANDLE);
                check(
                    GetProcessId(process.raw()) == record.pid
                        && GetProcessIdOfThread(thread.raw()) == record.pid,
                    "validate requested child identity",
                )?;
                let created = process_birth(process.raw(), "validate requested child identity")?;
                let mut contained = 0;
                let job = self.job.as_ref().unwrap().as_handle().as_raw_handle() as HANDLE;
                check(
                    created == record.created
                        && IsProcessInJob(process.raw(), job, &mut contained) != 0
                        && contained != 0,
                    "validate requested child containment",
                )?;
                self.birth[..8].copy_from_slice(&created.to_le_bytes());
                self.birth[8..12].copy_from_slice(&record.pid.to_le_bytes());
                self.birth[12..].copy_from_slice(b"WIN1");
                (self.process, self.pid) = (process, record.pid);
                Ok(record.pid)
            }
        }
        fn stage_instrument(&mut self, source: &Path, identity: &[u8]) -> Result<()> {
            let mut input = open_instrument_operand(source, &self.sid, |_| {})?;
            let stage = instrument_stage(
                &self.stage_root,
                identity,
                self.generation,
                self.incarnation,
            )?;
            let (mut staged, staged_identity, digest) = stage_file(
                &stage,
                &self.sid,
                "FRFX",
                "stage instrumentation object",
                |output| copy_digest(&mut input, Some(output)),
            )?;
            let verified: Result<()> = (|| {
                let published = read_reparse(&stage, FILE_SHARE_READ | FILE_SHARE_DELETE)?;
                require(
                    unsafe { file_identity(published.as_raw_handle())? } == staged_identity
                        && copy_digest(&mut staged, None)? == digest,
                    "instrumentation identity or content changed",
                )
            })();
            verified.inspect_err(|_| {
                delete_file(&staged);
            })?;
            let (read, write) = Pipe::pair("create instrumentation acknowledgement pipe")
                .inspect_err(|_| {
                    delete_file(&staged);
                })?;
            self.instrument = Some(Instrument {
                path: stage,
                file: staged,
                identity: staged_identity,
                digest,
                read,
                write,
            });
            Ok(())
        }
        fn inject_and_ack(&mut self, pid: u32, nonce: [u8; 16]) -> Result<()> {
            let instrument = self
                .instrument
                .as_mut()
                .ok_or("instrumentation was not staged")?;
            unsafe {
                let path = wide(instrument.path.as_os_str());
                let bytes = path.len() * size_of::<u16>();
                let remote = VirtualAllocEx(
                    self.process.raw(),
                    ptr::null(),
                    bytes,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_READWRITE,
                );
                check(!remote.is_null(), "allocate remote instrumentation path")?;
                win32!(
                    WriteProcessMemory(
                        self.process.raw(),
                        remote,
                        path.as_ptr().cast(),
                        bytes,
                        ptr::null_mut(),
                    ),
                    "write remote instrumentation path"
                )?;
                let kernel = GetModuleHandleW(wide(OsStr::new("kernel32.dll")).as_ptr());
                let load = GetProcAddress(kernel, c"LoadLibraryW".as_ptr().cast());
                let status = remote_call(self.process.raw(), load, remote)?;
                check(status != 0, "load instrumentation object")?;
                let local =
                    LoadLibraryExW(path.as_ptr(), ptr::null_mut(), DONT_RESOLVE_DLL_REFERENCES);
                check(!local.is_null(), "inspect instrumentation export")?;
                let init = GetProcAddress(local, c"MoorInstrumentationInitV1".as_ptr().cast());
                let init =
                    init.ok_or("instrumentation export is missing")? as usize - local as usize;
                FreeLibrary(local);
                let base = remote_module(pid, &instrument.path)?;
                let status = remote_call(
                    self.process.raw(),
                    Some(std::mem::transmute::<
                        usize,
                        unsafe extern "system" fn() -> isize,
                    >(base + init)),
                    ptr::null_mut(),
                )?;
                let bytes = instrument
                    .read
                    .record::<36>(true, "instrumentation acknowledgement")?;
                drop(std::mem::take(&mut instrument.read));
                validate_instrument_ack(&bytes, true, self.generation, pid, nonce)?;
                require(status == 0, "instrumentation initializer failed")?;
            }
            require(
                unsafe { file_identity(instrument.file.as_raw_handle())? } == instrument.identity,
                "instrumentation identity changed after load",
            )?;
            require(
                copy_digest(&mut instrument.file, None)? == instrument.digest,
                "instrumentation content changed after load",
            )
        }
        fn stage_marker(&self, marker: &[u8; 84]) -> Result<Staged> {
            let mut stage = self.marker.as_os_str().to_owned();
            stage.push(format!(".stage-{}", unsafe { GetCurrentProcessId() }));
            let stage = PathBuf::from(stage);
            let (file, identity, ()) =
                stage_file(&stage, &self.sid, "FR", "stage protected marker", |file| {
                    win(file.write_all(marker), "write protected marker")
                })?;
            Ok(Staged {
                path: stage,
                file,
                identity,
            })
        }
        fn publish_marker(&mut self) -> Result<()> {
            let staged_identity = self.stage.as_ref().unwrap().identity;
            self.prepare_storage(staged_identity)?;
            self.prepublication_alive()?;
            self.ready.notice(1, 0);
            publish_marker_stage(
                self.stage.as_ref().unwrap(),
                &self.marker,
                &self.sid,
                |_| {},
            )
        }

        fn rollback_unpublished(&mut self) {
            if !self.job.as_ref().is_none_or(stop_job) {
                return;
            }
            if let Some(artifacts) = self.artifacts.take() {
                rollback_stores([
                    artifacts.storage.log.map(|(store, _)| store),
                    artifacts.storage.events.map(|events| events.store),
                    Some(artifacts.storage.lifecycle),
                ]);
            }
            if let Some(instrument) = self.instrument.take() {
                delete_file(&instrument.file);
            }
            if let Some(stage) = self.stage.take() {
                delete_file(&stage.file);
            }
            if let Some(event) = self.event.as_ref().filter(|event| event.created) {
                delete_file(event.guards.last().unwrap());
            }
        }

        fn prepublication_alive(&mut self) -> Result<()> {
            if let Some(code) = process_exit(self.process.raw())? {
                self.early_exit = Some(code);
                return Err("requested child exited before publication".into());
            }
            let child = self.bootstrap.child.as_mut();
            let bootstrap = child.ok_or("bootstrap is unavailable")?;
            require(
                win(bootstrap.try_wait(), "inspect Windows bootstrap")?.is_none(),
                "bootstrap exited before publication",
            )
        }
    }

    impl HolderNative for Native {
        fn resize(&mut self, rows: u16, columns: u16) -> Result<()> {
            check(
                unsafe { ResizePseudoConsole(self.conpty.0, coordinate((rows, columns))?) } >= 0,
                "resize ConPTY",
            )
        }
        fn holder_ancestor(&self, pid: u32) -> bool {
            live_holder_ancestor(pid)
        }
        fn terminate(&mut self, force: bool) -> (u8, bool) {
            crate::return_if!(!force && self.bootstrap.exchange(2).is_ok(), (0, false));
            let job = self.job.as_ref();
            let killed = job.is_some_and(|job| job.terminate(0xc000013a).is_ok());
            (u8::from(killed) << 1, true)
        }
        fn exited(&mut self) -> Result<Option<NativeExit>> {
            let exit = process_exit(self.process.raw())?;
            if exit.is_some() {
                // Closing HPCON can wait on older Windows, so the reader must
                // keep draining concurrently while this detached close first
                // releases the bootstrap and then the console ownership.
                self.bootstrap.control = Pipe::default();
                self.conpty.retire();
            }
            Ok(exit.map(NativeExit::Code))
        }
        fn abandon(&mut self) {
            self.bootstrap.control = Pipe::default();
            drop(self.job.take());
            self.conpty.retire();
        }
    }

    fn absolute(path: &Path) -> Result<PathBuf> {
        path_buffer("resolve absolute Windows path", |out, size| unsafe {
            GetFullPathNameW(wide(path.as_os_str()).as_ptr(), size, out, ptr::null_mut())
        })
    }
    fn caller_rejection(surface: &str, path: &Path, cause: &str) -> String {
        format!(
            "{surface} rejected: {} ({cause})",
            name::render(path.as_os_str())
        )
    }
    fn caller_open_cause(error: &io::Error) -> &'static str {
        match error.raw_os_error().map(|value| value as u32) {
            Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) => "missing",
            Some(ERROR_DIRECTORY) => "wrong-type",
            Some(ERROR_CANT_ACCESS_FILE) => "reparse-point",
            _ => "io-error",
        }
    }
    fn open_stderr_operand(
        operand: &Path,
        user: &str,
        after_open: impl FnOnce(&File),
    ) -> Result<Handle> {
        let reject = |cause| caller_rejection("standard-error sink", operand, cause);
        let path = absolute(operand).map_err(|_| reject("io-error"))?;
        let file = OpenOptions::new()
            .access_mode(OPEN_STDERR.0)
            .share_mode(OPEN_STDERR.1)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .map_err(|error| reject(caller_open_cause(&error)))?;
        after_open(&file);
        validate_stderr_handle(&file, user).map_err(reject)?;
        Ok(Handle(Some(file)))
    }
    fn open_instrument_operand(
        operand: &Path,
        user: &str,
        after_open: impl FnOnce(&File),
    ) -> Result<File> {
        let reject = |cause| caller_rejection("instrumentation", operand, cause);
        crate::ensure!(operand.is_absolute(), reject("not-absolute"));
        let file = OpenOptions::new()
            .read(true)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(operand)
            .map_err(|error| reject(caller_open_cause(&error)))?;
        after_open(&file);
        validate_instrument_handle(&file, user).map_err(reject)?;
        Ok(file)
    }
    fn event_rejection(path: &Path, cause: &str) -> String {
        format!(
            "event store rejected: {} ({cause})",
            name::render(path.as_os_str())
        )
    }
    fn event_component(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
    fn event_attributes(handle: &File) -> Result<u32> {
        let info: FILE_ATTRIBUTE_TAG_INFO = unsafe {
            file_info(
                handle.as_raw_handle(),
                FileAttributeTagInfo,
                "inspect event path component",
            )?
        };
        Ok(info.FileAttributes)
    }
    fn event_target(operand: &Path, root: &Path) -> Result<EventTarget> {
        let reject = |cause| event_rejection(operand, cause);
        let event = absolute(operand).map_err(|_| reject("io-error"))?;
        let root_handle = event_component(root)
            .map_err(|error| reject(CAUSES[directory_cause(&error) as usize]))?;
        let root_identity = unsafe { file_identity(root_handle.as_raw_handle()) }
            .map_err(|_| reject("io-error"))?;
        let mut all = event.components();
        let base: PathBuf = all.by_ref().take(root.components().count()).collect();
        let mut components = all.peekable();
        crate::ensure!(components.peek().is_some(), reject("outside-root"));
        let mut prefix = PathBuf::new();
        let mut guards = vec![root_handle];
        for component in base.components() {
            prefix.push(component.as_os_str());
            if !prefix.is_absolute() {
                continue;
            }
            let handle = event_component(&prefix).map_err(|_| reject("outside-root"))?;
            let attributes = event_attributes(&handle).map_err(|_| reject("io-error"))?;
            crate::ensure!(
                attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0,
                reject("reparse-point")
            );
            crate::ensure!(
                attributes & FILE_ATTRIBUTE_DIRECTORY != 0,
                reject("outside-root")
            );
            guards.push(handle);
        }
        let base_handle = guards.last().ok_or_else(|| reject("outside-root"))?;
        let base_identity = unsafe { file_identity(base_handle.as_raw_handle()) }
            .map_err(|_| reject("io-error"))?;
        crate::ensure!(root_identity == base_identity, reject("outside-root"));
        let mut current = base;
        let mut present = true;
        while let Some(component) = components.next() {
            let std::path::Component::Normal(name) = component else {
                return Err(reject("outside-root"));
            };
            current.push(name);
            let handle = match event_component(&current) {
                Ok(handle) => handle,
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound && components.peek().is_none() =>
                {
                    present = false;
                    break;
                }
                Err(error) => return Err(reject(CAUSES[directory_cause(&error) as usize])),
            };
            let attributes = event_attributes(&handle).map_err(|_| reject("io-error"))?;
            crate::ensure!(
                attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0,
                reject("reparse-point")
            );
            crate::ensure!(
                attributes & FILE_ATTRIBUTE_DIRECTORY != 0,
                reject("not-directory")
            );
            guards.push(handle);
        }
        Ok(EventTarget {
            path: operand.to_owned(),
            present,
            created: false,
            guards,
        })
    }
    fn validate_event(target: &EventTarget, user: &str) -> Result<()> {
        crate::return_if!(!target.present, Ok(()));
        let reject = |cause| event_rejection(&target.path, cause);
        let selector = SecurityInformation::Owner | SecurityInformation::Dacl;
        let actual = wrappers::GetSecurityInfo(
            target.guards.last().unwrap(),
            SeObjectType::SE_FILE_OBJECT,
            selector,
        )
        .map_err(|_| reject("io-error"))?;
        let (expected, _) = descriptor(user, "FA").map_err(|_| reject("io-error"))?;
        crate::ensure!(
            actual
                .owner()
                .zip(expected.owner())
                .is_some_and(|(a, b)| a == b),
            reject("wrong-owner")
        );
        crate::ensure!(
            descriptor_matches(&actual, &expected).map_err(|_| reject("io-error"))?,
            reject("broad-dacl")
        );
        Ok(())
    }
    fn materialize_event(
        target: &mut EventTarget,
        user: &str,
        after_create: impl FnOnce(&Path),
    ) -> Result<()> {
        crate::return_if!(target.present, validate_event(target, user));
        let rejected = event_rejection(&target.path, "identity-changed");
        let name = target.path.file_name().ok_or_else(|| rejected.clone())?;
        let handle = relative_file(target.guards.last().unwrap(), name, user, CREATE_DIRECTORY)
            .map_err(|_| rejected.clone())?;
        after_create(&target.path);
        target.guards.push(handle);
        target.created = true;
        let verified = (|| {
            let attributes =
                event_attributes(target.guards.last().unwrap()).map_err(|_| rejected.clone())?;
            crate::ensure!(
                attributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY)
                    == FILE_ATTRIBUTE_DIRECTORY,
                rejected.clone()
            );
            target.present = true;
            validate_event(target, user).map_err(|_| rejected.clone())
        })();
        verified.inspect_err(|_| {
            delete_file(target.guards.last().unwrap());
        })
    }
    fn os_string(bytes: &[u8]) -> Result<OsString> {
        wtf8_decode(bytes).map(|wide| OsString::from_wide(&wide))
    }
    fn wmi_boot_identity() -> Option<[u8; 16]> {
        #[derive(serde::Deserialize)]
        struct OperatingSystem {
            #[serde(rename = "LastBootUpTime")]
            boot: String,
        }
        wmi::WMIConnection::new()
            .ok()?
            .raw_query::<OperatingSystem>("SELECT LastBootUpTime FROM Win32_OperatingSystem")
            .ok()?
            .into_iter()
            .next()
            .and_then(|system| cim_boot_identity(&system.boot))
    }
    fn boot_identity() -> [u8; 16] {
        static BOOT: OnceLock<[u8; 16]> = OnceLock::new();
        *BOOT.get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || tx.send(wmi_boot_identity()).ok());
            rx.recv_timeout(Duration::from_secs(2))
                .ok()
                .flatten()
                .unwrap_or([0; 16])
        })
    }
    fn root(invoked: &OsStr) -> Result<PathBuf> {
        let text = sid()?;
        let mut name = OsString::from(".");
        name.push(Path::new(invoked).file_name().unwrap_or(OsStr::new("moor")));
        name.push("-");
        name.push(text);
        let path = temp()?.join(name);
        require(
            crate::store::private_directory(&path, true).map_err(string)?,
            "unexpected Windows owner/DACL",
        )?;
        Ok(path)
    }
    pub(crate) fn resolve(session: &OsStr, invoked: &OsStr) -> Result<PathBuf> {
        let path = PathBuf::from(session);
        if session.encode_wide().any(|unit| [47, 92].contains(&unit)) {
            absolute(&path)
        } else {
            Ok(root(invoked)?.join(path))
        }
    }
    pub(crate) fn current_paths(invoked: &OsStr) -> Result<Vec<PathBuf>> {
        ancestry_paths(invoked, os_string)
    }
    fn read_marker(path: &Path) -> Result<(Marker, [u8; 25])> {
        validate(path, sid()?, "FR", false)?;
        let mut file = read_reparse(path, FILE_SHARE_READ | FILE_SHARE_DELETE)?;
        let identity = unsafe { session_identity(file_identity(file.as_raw_handle())?) };
        require(
            file.metadata().map_err(string)?.len() == 84,
            "malformed Windows marker",
        )?;
        let mut bytes = [0; 84];
        file.read_exact(&mut bytes).map_err(string)?;
        Ok((Marker::decode(&bytes)?, identity))
    }
    fn pipe_name(marker: &Marker) -> [u16; 47] {
        std::array::from_fn(|at| marker.pipe.get(at).copied().map(u16::from).unwrap_or(0))
    }
    fn controller(path: &Path, timeout: u32) -> Result<Client> {
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout));
        let (marker, identity) = read_marker(path)?;
        let pipe = pipe_name(&marker);
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(u128::from(u32::MAX)) as u32;
        require(remaining != 0, "holder handshake timed out")?;
        win32!(
            WaitNamedPipeW(pipe.as_ptr(), remaining),
            "wait for holder pipe"
        )?;
        let stream = Stream::connect(local_name(&marker.pipe)?).map_err(string)?;
        require(
            read_marker(path)?.1 == identity,
            "marker identity changed during connection",
        )?;
        Client::from_stream(stream, identity.to_vec(), deadline, cancel)
    }
    pub(crate) fn connect(path: &Path) -> Result<Client> {
        controller(path, 2000)
    }
    fn inspect(path: &Path, status: bool, timeout: u32) -> SessionState {
        probe_session(
            path,
            status,
            || read_marker(path).is_ok(),
            || {
                let marker = read_marker(path).map_err(|_| false)?.0;
                let pipe = pipe_name(&marker);
                if unsafe { WaitNamedPipeW(pipe.as_ptr(), timeout) } == 0 {
                    return Err(unsafe { GetLastError() } == ERROR_FILE_NOT_FOUND);
                }
                controller(path, timeout).map_err(|_| false)
            },
        )
    }
    pub(crate) fn classify(path: &Path) -> SessionState {
        // Schema §9.3 freezes the identity exchange at 2 s (see unix::classify).
        inspect(path, false, 2_000)
    }
    pub(crate) fn sessions(invoked: &OsStr, status: bool) -> Result<Vec<SessionEntry>> {
        let root = root(invoked)?;
        discover_sessions(
            &root,
            |name| session_name(name, true),
            // OB-8 bounds the whole listing at 2 s (see unix::sessions).
            |path, remaining| inspect(path, status, remaining.as_millis().min(250) as u32),
        )
    }
    pub(crate) fn cleanup(path: &Path) -> Result<()> {
        let (external, expected) = cleanup_artifacts(path, None, |bytes| {
            os_string(&bytes).ok().map(PathBuf::from)
        });
        let instrument = external[1].clone();
        let user = sid()?;
        if path.exists() {
            read_marker(path)?;
            fs::remove_file(path).map_err(string)?;
        }
        cleanup_companions(path, external, false, |target| {
            let directory = target.is_dir();
            let access = if directory { "FA" } else { "FRFX" };
            (instrument.as_deref() != Some(target) || expected.as_deref() == Some(target))
                && validate(target, user, access, directory).is_ok()
        })
    }
    const WIN32_INPUT_ENABLE: &[u8] = b"\x1b[?9001h";
    const WIN32_INPUT_DISABLE: &[u8] = b"\x1b[?9001l";

    fn viewer_modes(input: u32, output: u32) -> [u32; 2] {
        let raw =
            ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT | ENABLE_QUICK_EDIT_MODE;
        [
            (input
                | ENABLE_EXTENDED_FLAGS
                | ENABLE_VIRTUAL_TERMINAL_INPUT
                | ENABLE_WINDOW_INPUT
                | ENABLE_MOUSE_INPUT)
                & !raw,
            output
                | ENABLE_PROCESSED_OUTPUT
                | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                | DISABLE_NEWLINE_AUTO_RETURN,
        ]
    }
    fn console_geometry(output: HANDLE) -> Result<(u16, u16)> {
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        let read = unsafe { GetConsoleScreenBufferInfo(output, &mut info) };
        check(read != 0, "inspect viewer console geometry")?;
        let invalid = "viewer console geometry is invalid";
        let extent = |a, b| u16::try_from(i32::from(a) - i32::from(b) + 1).map_err(|_| invalid);
        let window = info.srWindow;
        let rows = extent(window.Bottom, window.Top)?;
        let size = (rows, extent(window.Right, window.Left)?);
        require(crate::wire::valid_size(size), invalid).map(|_| size)
    }
    struct ViewerConsole([HANDLE; 2], [u32; 2]);
    struct ViewerInputMode(Option<HANDLE>);
    impl ViewerInputMode {
        fn write_handle(handle: HANDLE, bytes: &[u8]) -> Result<()> {
            let mut written = 0;
            win32!(
                WriteFile(
                    handle,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut written,
                    ptr::null_mut(),
                ),
                "write viewer input-mode control"
            )?;
            require(
                written == bytes.len() as u32,
                "short viewer input-mode control write",
            )
        }
        fn write(output: &mut dyn Write, bytes: &[u8]) -> Result<()> {
            win(output.write_all(bytes), "write viewer input-mode control")?;
            win(output.flush(), "flush viewer input-mode control")
        }
        fn enable(handle: HANDLE, output: &mut dyn Write) -> Result<Self> {
            Self::write(output, WIN32_INPUT_ENABLE).map(|()| Self(Some(handle)))
        }
        fn disable(&mut self, output: &mut dyn Write) -> Result<()> {
            if self.0.is_none() {
                return Ok(());
            }
            Self::write(output, WIN32_INPUT_DISABLE)?;
            self.0 = None;
            Ok(())
        }
    }
    impl Drop for ViewerInputMode {
        fn drop(&mut self) {
            if let Some(handle) = self.0 {
                Self::write_handle(handle, WIN32_INPUT_DISABLE).ok();
            }
        }
    }
    impl ViewerConsole {
        fn detect() -> Option<Self> {
            let handles =
                [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE].map(|kind| unsafe { GetStdHandle(kind) });
            let mut modes = [0; 2];
            (0..2)
                .all(|at| unsafe { GetConsoleMode(handles[at], &mut modes[at]) } != 0)
                .then_some(Self(handles, modes))
        }
        fn set(&self, modes: [u32; 2]) -> Result<()> {
            let set = |at| unsafe { SetConsoleMode(self.0[at], modes[at]) } != 0;
            (0..2).try_for_each(|at| check(set(at), "configure viewer console"))
        }
    }
    impl Drop for ViewerConsole {
        fn drop(&mut self) {
            self.set(self.1).ok();
        }
    }

    fn console_wide_with_nul(key: KEY_EVENT_RECORD, nul: i16) -> Option<(u16, u16)> {
        let unit = unsafe { key.uChar.UnicodeChar };
        let null = unit == 0 && {
            // Windows 10 1809 and Server 2019 predate conhost's reconstructed
            // VkKeyScanW chord for a NUL emitted by its VT input engine. They
            // enqueue one otherwise-empty key-down record instead.
            let legacy =
                key.wVirtualKeyCode == 0 && key.wVirtualScanCode == 0 && key.dwControlKeyState == 0;
            let expected = nul as u16 & 0x7ff;
            let chord = key.wVirtualKeyCode
                | u16::from(key.dwControlKeyState & SHIFT_PRESSED != 0) << 8
                | u16::from(key.dwControlKeyState & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0)
                    << 9
                | u16::from(key.dwControlKeyState & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0)
                    << 10;
            legacy || chord == expected
        };
        (key.bKeyDown != 0 && key.wRepeatCount != 0 && (unit != 0 || null))
            .then_some((unit, key.wRepeatCount))
    }

    fn console_wide(key: KEY_EVENT_RECORD) -> Option<(u16, u16)> {
        console_wide_with_nul(key, unsafe { VkKeyScanW(0) })
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct Win32InputCarrier {
        virtual_key: u16,
        scan_code: u16,
        unicode: u16,
        key_down: bool,
        control_state: u32,
        repeat: u16,
        c1: bool,
    }

    impl Win32InputCarrier {
        fn key(self) -> KEY_EVENT_RECORD {
            KEY_EVENT_RECORD {
                bKeyDown: i32::from(self.key_down),
                wRepeatCount: self.repeat,
                wVirtualKeyCode: self.virtual_key,
                wVirtualScanCode: self.scan_code,
                uChar: KEY_EVENT_RECORD_0 {
                    UnicodeChar: self.unicode,
                },
                dwControlKeyState: self.control_state,
            }
        }

        fn modifier(self) -> bool {
            matches!(
                self.virtual_key,
                VK_SHIFT
                    | VK_CONTROL
                    | VK_MENU
                    | VK_LSHIFT
                    | VK_RSHIFT
                    | VK_LCONTROL
                    | VK_RCONTROL
                    | VK_LMENU
                    | VK_RMENU
                    | VK_LWIN
                    | VK_RWIN
            )
        }

        fn once(self) -> Vec<u8> {
            let mut bytes = if self.c1 {
                vec![0xc2, 0x9b]
            } else {
                b"\x1b[".to_vec()
            };
            bytes.extend_from_slice(
                format!(
                    "{};{};{};{};{};1_",
                    self.virtual_key,
                    self.scan_code,
                    self.unicode,
                    u8::from(self.key_down),
                    self.control_state
                )
                .as_bytes(),
            );
            bytes
        }

        fn frame(self, bytes: Vec<u8>) -> InputFrame {
            if !self.key_down || self.modifier() {
                return InputFrame::Meta(bytes);
            }
            let semantic = console_wide(self.key()).and_then(|(unit, _)| u8::try_from(unit).ok());
            InputFrame::Key(bytes, self.once(), semantic, self.repeat)
        }
    }

    fn win32_input_carrier(bytes: &[u8]) -> Option<Win32InputCarrier> {
        let (body, c1) = if let Some(body) = bytes.strip_prefix(b"\x1b[") {
            (body, false)
        } else {
            (bytes.strip_prefix(b"\xc2\x9b")?, true)
        };
        let body = body.strip_suffix(b"_")?;
        let values = body
            .split(|byte| *byte == b';')
            .map(|field| {
                let text = std::str::from_utf8(field).ok()?;
                crate::canonical_u64(text)
            })
            .collect::<Option<Vec<_>>>()?;
        crate::return_if!(values.len() != 6 || values[3] > 1, None);
        let carrier = Win32InputCarrier {
            virtual_key: values[0].try_into().ok()?,
            scan_code: values[1].try_into().ok()?,
            unicode: values[2].try_into().ok()?,
            key_down: values[3] != 0,
            control_state: values[4].try_into().ok()?,
            repeat: values[5].try_into().ok()?,
            c1,
        };
        (carrier.repeat != 0).then_some(carrier)
    }

    fn generated_carrier_unit(event: INPUT_RECORD) -> Option<(u16, u16)> {
        crate::return_if!(event.EventType != KEY_EVENT as u16, None);
        let key = unsafe { event.Event.KeyEvent };
        let unit = unsafe { key.uChar.UnicodeChar };
        (key.bKeyDown != 0
            && key.wRepeatCount != 0
            && key.wVirtualKeyCode == 0
            && key.wVirtualScanCode == 0
            && key.dwControlKeyState == 0)
            .then_some((unit, key.wRepeatCount))
    }

    const CONSOLE_READ_RECORDS: usize = 256;
    const CONSOLE_RECORD_BUDGET: usize = 16_384;
    const CONSOLE_WIDE_TARGET: usize = 16 * 1024;
    const CONSOLE_BYTE_TARGET: usize = 64 * 1024;
    const WIN32_CARRIER_LIMIT: usize = 64;

    enum ConsoleUnit {
        Event(INPUT_RECORD),
        Bytes(SmallVec<[u8; WIN32_CARRIER_LIMIT]>),
        Framed(InputFrame),
    }

    struct ConsoleInput {
        handle: HANDLE,
        recognize_carriers: bool,
        pending_high: Option<(u16, u32)>,
        deferred: Option<InputState>,
        closed: bool,
        carrier: SmallVec<[u8; WIN32_CARRIER_LIMIT]>,
        carrier_started: Option<Instant>,
        units: VecDeque<u16>,
        replay: VecDeque<ConsoleUnit>,
        records: [INPUT_RECORD; CONSOLE_READ_RECORDS],
        next: usize,
        count: usize,
    }

    impl ConsoleInput {
        #[cfg(test)]
        fn new(handle: HANDLE) -> Self {
            Self::with_detach(handle, None)
        }

        fn with_detach(handle: HANDLE, detach: Option<u8>) -> Self {
            Self {
                handle,
                recognize_carriers: detach.is_some(),
                pending_high: None,
                deferred: None,
                closed: false,
                carrier: SmallVec::new(),
                carrier_started: None,
                units: VecDeque::new(),
                replay: VecDeque::new(),
                records: [INPUT_RECORD::default(); CONSOLE_READ_RECORDS],
                next: 0,
                count: 0,
            }
        }

        fn raw_unit(unit: u16) -> SmallVec<[u8; WIN32_CARRIER_LIMIT]> {
            if unit <= 0x7f {
                SmallVec::from_slice(&[unit as u8])
            } else {
                debug_assert_eq!(unit, 0x9b);
                SmallVec::from_slice(&[0xc2, 0x9b])
            }
        }

        fn take_carrier(&mut self) -> SmallVec<[u8; WIN32_CARRIER_LIMIT]> {
            self.carrier_started = None;
            std::mem::take(&mut self.carrier)
        }

        fn carrier_wait(&self) -> u32 {
            let elapsed = self
                .carrier_started
                .map_or(Duration::MAX, |at| at.elapsed());
            let remaining = Duration::from_millis(50).saturating_sub(elapsed);
            u32::try_from(remaining.as_nanos().div_ceil(1_000_000)).unwrap()
        }

        fn scan_unit(&mut self, unit: u16) -> Option<ConsoleUnit> {
            if self.carrier.is_empty() {
                match unit {
                    0x1b => self.carrier.push(0x1b),
                    0x9b => self.carrier.extend_from_slice(&[0xc2, 0x9b]),
                    _ => return Some(ConsoleUnit::Bytes(Self::raw_unit(unit))),
                }
                self.carrier_started = Some(Instant::now());
                return None;
            }
            if self.carrier.as_slice() == b"\x1b" {
                if unit == u16::from(b'[') {
                    self.carrier.push(b'[');
                    return None;
                }
                let prior = self.take_carrier();
                self.units.push_front(unit);
                return Some(ConsoleUnit::Bytes(prior));
            }
            let byte = u8::try_from(unit).ok();
            if byte.is_none_or(|byte| !byte.is_ascii_digit() && byte != b';' && byte != b'_') {
                let bytes = self.take_carrier();
                self.units.push_front(unit);
                return Some(ConsoleUnit::Bytes(bytes));
            }
            let byte = byte.unwrap();
            self.carrier.push(byte);
            if self.carrier.len() > WIN32_CARRIER_LIMIT {
                return Some(ConsoleUnit::Bytes(self.take_carrier()));
            }
            if byte == b'_' {
                let bytes = self.take_carrier();
                return match win32_input_carrier(&bytes) {
                    Some(carrier) => Some(ConsoleUnit::Framed(carrier.frame(bytes.to_vec()))),
                    None => Some(ConsoleUnit::Bytes(bytes)),
                };
            }
            None
        }

        fn next_unit(&mut self) -> Option<ConsoleUnit> {
            if let Some(unit) = self.replay.pop_front() {
                return Some(unit);
            }
            loop {
                if let Some(unit) = self.units.pop_front() {
                    if let Some(unit) = self.scan_unit(unit) {
                        return Some(unit);
                    }
                    continue;
                }
                crate::return_if!(self.next == self.count, None);
                let event = self.records[self.next];
                self.next += 1;
                crate::return_if!(!self.recognize_carriers, Some(ConsoleUnit::Event(event)));
                let Some((unit, repeat)) = generated_carrier_unit(event) else {
                    if !self.carrier.is_empty() {
                        self.replay.push_back(ConsoleUnit::Event(event));
                        return Some(ConsoleUnit::Bytes(self.take_carrier()));
                    }
                    return Some(ConsoleUnit::Event(event));
                };
                if self.carrier.is_empty() && !matches!(unit, 0x1b | 0x9b) {
                    return Some(ConsoleUnit::Event(event));
                }
                if unit > 0x7f && unit != 0x9b {
                    self.replay.push_back(ConsoleUnit::Event(event));
                    return Some(ConsoleUnit::Bytes(self.take_carrier()));
                }
                self.units.extend(std::iter::repeat_n(unit, repeat.into()));
            }
        }

        fn finish(&mut self, output: Vec<u8>, next: InputState) -> InputState {
            if output.is_empty() {
                next
            } else {
                if next != InputState::Pending {
                    self.deferred = Some(next);
                }
                InputState::Bytes(output)
            }
        }

        fn record(
            &mut self,
            event: INPUT_RECORD,
            codepage: u32,
            wide: &mut Vec<u16>,
        ) -> std::result::Result<Option<(u16, u16)>, ()> {
            if self
                .pending_high
                .is_some_and(|(_, high_codepage)| high_codepage != codepage)
            {
                self.pending_high = None;
            }
            if event.EventType != KEY_EVENT as u16 {
                if event.EventType == WINDOW_BUFFER_SIZE_EVENT as u16 {
                    self.pending_high = None;
                    let size = unsafe { event.Event.WindowBufferSizeEvent.dwSize };
                    let size = (size.Y as u16, size.X as u16);
                    return Ok(crate::wire::valid_size(size).then_some(size));
                }
                return Ok(None);
            }
            let key = unsafe { event.Event.KeyEvent };
            let Some((unit, repeat)) = console_wide(key) else {
                return Ok(None);
            };
            if codepage == 0 {
                return Err(());
            }
            if (0xd800..=0xdbff).contains(&unit) {
                self.pending_high = (repeat == 1).then_some((unit, codepage));
                return Ok(None);
            }
            let high = self.pending_high.take();
            let low = (0xdc00..=0xdfff).contains(&unit);
            let mut scalar = [unit; 2];
            if low {
                let Some((high, high_codepage)) = high else {
                    return Ok(None);
                };
                if repeat != 1 || high_codepage != codepage {
                    return Ok(None);
                }
                scalar[0] = high;
                wide.extend_from_slice(&scalar);
            } else {
                wide.extend(std::iter::repeat_n(unit, repeat.into()));
            }
            Ok(None)
        }

        fn encode(
            codepage: u32,
            wide: &[u16],
            output: &mut Vec<u8>,
        ) -> std::result::Result<(), ()> {
            if wide.is_empty() {
                return Ok(());
            }
            let wide_length = i32::try_from(wide.len()).map_err(|_| ())?;
            let (default, used) = (ptr::null(), ptr::null_mut());
            let required = unsafe {
                WideCharToMultiByte(
                    codepage,
                    0,
                    wide.as_ptr(),
                    wide_length,
                    ptr::null_mut(),
                    0,
                    default,
                    used,
                )
            };
            if required <= 0 {
                return Err(());
            }
            let mut bytes = SmallVec::<[u8; 8]>::new();
            bytes.resize(required as usize, 0);
            let length = unsafe {
                WideCharToMultiByte(
                    codepage,
                    0,
                    wide.as_ptr(),
                    wide_length,
                    bytes.as_mut_ptr(),
                    required,
                    default,
                    used,
                )
            };
            if length != required {
                return Err(());
            }
            output.extend_from_slice(&bytes);
            Ok(())
        }

        fn refill(&mut self, timeout: u32) -> std::result::Result<bool, ()> {
            let wait = unsafe { WaitForSingleObject(self.handle, timeout) };
            if wait == WAIT_TIMEOUT {
                return Ok(false);
            }
            if wait != WAIT_OBJECT_0 {
                return Err(());
            }
            let mut count = 0;
            let read = unsafe {
                ReadConsoleInputW(
                    self.handle,
                    self.records.as_mut_ptr(),
                    self.records.len() as u32,
                    &mut count,
                )
            };
            if read == 0 || count == 0 || count as usize > self.records.len() {
                return Err(());
            }
            self.next = 0;
            self.count = count as usize;
            Ok(true)
        }

        fn flush(
            &mut self,
            codepage: &mut Option<u32>,
            wide: &mut Vec<u16>,
            output: &mut Vec<u8>,
        ) -> std::result::Result<(), ()> {
            if let Some(codepage) = codepage.take() {
                Self::encode(codepage, wide, output)?;
                wide.clear();
            }
            Ok(())
        }

        fn state_with(
            &mut self,
            mut refill: impl FnMut(&mut Self, u32) -> std::result::Result<bool, ()>,
        ) -> InputState {
            if let Some(next) = self.deferred.take() {
                return next;
            }
            let mut output = Vec::new();
            let mut wide = Vec::new();
            let mut codepage = None;
            let mut frames = Vec::new();
            let mut frame_bytes = 0;
            // A larger processing budget coalesces ordinary paste input across
            // many native reads. The fixed budget still yields periodically so
            // ignored-event floods cannot starve keepalives or detach timing.
            for _ in 0..CONSOLE_RECORD_BUDGET {
                let unit = loop {
                    if let Some(unit) = self.next_unit() {
                        break unit;
                    }
                    if self.closed {
                        if !frames.is_empty() {
                            return InputState::Framed(frames);
                        }
                        self.flush(&mut codepage, &mut wide, &mut output).ok();
                        output.extend_from_slice(&self.take_carrier());
                        return self.finish(output, InputState::Closed);
                    }
                    if !frames.is_empty() {
                        match refill(self, 0) {
                            Ok(true) => continue,
                            Ok(false) => return InputState::Framed(frames),
                            Err(()) => {
                                self.closed = true;
                                return InputState::Framed(frames);
                            }
                        }
                    }
                    if !self.carrier.is_empty() && (!output.is_empty() || !wide.is_empty()) {
                        let next = match self.flush(&mut codepage, &mut wide, &mut output) {
                            Ok(()) => InputState::Pending,
                            Err(()) => InputState::Closed,
                        };
                        return self.finish(output, next);
                    }
                    let wait = if self.carrier.is_empty() {
                        u32::from(output.is_empty() && wide.is_empty()) * 50
                    } else {
                        self.carrier_wait()
                    };
                    if wait == 0 && !self.carrier.is_empty() {
                        let encoded = self.flush(&mut codepage, &mut wide, &mut output);
                        output.extend_from_slice(&self.take_carrier());
                        let next = if encoded.is_ok() {
                            InputState::Pending
                        } else {
                            InputState::Closed
                        };
                        return self.finish(output, next);
                    }
                    match refill(self, wait) {
                        Ok(true) => continue,
                        Ok(false) => {
                            let encoded = self.flush(&mut codepage, &mut wide, &mut output);
                            output.extend_from_slice(&self.take_carrier());
                            let next = if encoded.is_ok() {
                                InputState::Pending
                            } else {
                                InputState::Closed
                            };
                            return self.finish(output, next);
                        }
                        Err(()) => {
                            self.closed = true;
                            continue;
                        }
                    }
                };
                if !frames.is_empty() && !matches!(unit, ConsoleUnit::Framed(_)) {
                    self.replay.push_front(unit);
                    return InputState::Framed(frames);
                }
                match unit {
                    ConsoleUnit::Event(event) => {
                        // The console VT engine exposes UTF-16 records, while
                        // ConPTY's input boundary is always UTF-8. The viewer's
                        // legacy input code page must not alter serialization.
                        codepage = Some(CP_UTF8);
                        match self.record(event, CP_UTF8, &mut wide) {
                            Ok(Some((rows, columns))) => {
                                if self.flush(&mut codepage, &mut wide, &mut output).is_err() {
                                    return self.finish(output, InputState::Closed);
                                }
                                return self.finish(output, InputState::Resize(rows, columns));
                            }
                            Ok(None) => {}
                            Err(()) => {
                                self.flush(&mut codepage, &mut wide, &mut output).ok();
                                return self.finish(output, InputState::Closed);
                            }
                        }
                    }
                    ConsoleUnit::Bytes(bytes) => {
                        if self.flush(&mut codepage, &mut wide, &mut output).is_err() {
                            return self.finish(output, InputState::Closed);
                        }
                        output.extend_from_slice(&bytes);
                    }
                    ConsoleUnit::Framed(frame) => {
                        if !output.is_empty() || !wide.is_empty() {
                            self.replay.push_front(ConsoleUnit::Framed(frame));
                            let next = match self.flush(&mut codepage, &mut wide, &mut output) {
                                Ok(()) => InputState::Pending,
                                Err(()) => InputState::Closed,
                            };
                            return self.finish(output, next);
                        }
                        frame_bytes += match &frame {
                            InputFrame::Meta(bytes) | InputFrame::Key(bytes, ..) => bytes.len(),
                        };
                        frames.push(frame);
                        crate::return_if!(
                            frame_bytes >= CONSOLE_BYTE_TARGET,
                            InputState::Framed(frames)
                        );
                        continue;
                    }
                }
                if wide.len() >= CONSOLE_WIDE_TARGET
                    && self.flush(&mut codepage, &mut wide, &mut output).is_err()
                {
                    return self.finish(output, InputState::Closed);
                }
                crate::return_if!(
                    output.len() >= CONSOLE_BYTE_TARGET,
                    InputState::Bytes(output)
                );
            }
            crate::return_if!(!frames.is_empty(), InputState::Framed(frames));
            let encoded = self.flush(&mut codepage, &mut wide, &mut output);
            output.extend_from_slice(&self.take_carrier());
            let next = if encoded.is_ok() {
                InputState::Pending
            } else {
                InputState::Closed
            };
            self.finish(output, next)
        }

        fn state(&mut self) -> InputState {
            self.state_with(|input, timeout| input.refill(timeout))
        }
    }

    pub(crate) fn attach(path: &Path, options: Options) -> CommandResult<i32> {
        let terminal = ViewerConsole::detect()
            .ok_or_else(|| CommandError::output("no controlling terminal"))?;
        terminal.set(viewer_modes(terminal.1[0], terminal.1[1]))?;
        let mut output = io::stdout();
        let mut input_mode = ViewerInputMode::enable(terminal.0[1], &mut output)?;
        let mut client = controller(path, 2000).map_err(|_| missing(path))?;
        let geometry = console_geometry(terminal.0[1]).unwrap_or((0, 0));
        let attached = attach_viewer_to(
            &mut client,
            &options,
            geometry,
            &mut output,
            Duration::from_secs(15),
            |remaining| controller(path, remaining.as_millis().min(u128::from(u32::MAX)) as u32),
            |sender| viewer_input(sender, options.detach, geometry),
        );
        let disabled = input_mode.disable(&mut output);
        let status = attached?;
        disabled?;
        Ok(status)
    }
    fn viewer_input(sender: ViewerSender, detach: Option<u8>, geometry: (u16, u16)) {
        thread::spawn(move || {
            let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) } as usize;
            let mut input = ConsoleInput::with_detach(input as HANDLE, detach);
            run_viewer_input(
                io::empty(),
                sender,
                InputConfig {
                    detach,
                    pass_suspend: true,
                    last_size: crate::wire::valid_size(geometry).then_some(geometry),
                },
                move || input.state(),
                || None,
                || {},
                Instant::now,
            );
        });
    }
    fn detached(geometry: (u16, u16)) -> Result<i32> {
        let mut command = SpawnCommand::new(std::env::current_exe().map_err(string)?);
        command
            .args(std::env::args_os().skip(1))
            .env(DETACHED_HOLDER, "1")
            .env(DETACHED_GEOMETRY, format!("{}:{}", geometry.0, geometry.1))
            .stdout(SpawnStdio::piped());
        let flags = CreationFlags::DETACHED_PROCESS | CreationFlags::NEW_PROCESS_GROUP;
        let mut child = win(
            command.spawn_with(SpawnOptions::new().creation_flags(flags)),
            "start detached holder",
        )?;
        let output = child.stdout.take();
        let output = output.ok_or("launch result pipe is unavailable")?;
        Ok(i32::from(await_launch(output)?.0))
    }
    fn creation_size(required: bool, geometry: Option<(u16, u16)>) -> Result<(u16, u16)> {
        crate::ensure!(geometry.is_some() || !required, "no controlling terminal");
        Ok(geometry.unwrap_or((24, 80)))
    }
    fn parse_geometry(value: &OsStr) -> Option<(u16, u16)> {
        let (rows, columns) = value.to_str()?.split_once(':')?;
        let size = (rows.parse().ok()?, columns.parse().ok()?);
        crate::wire::valid_size(size).then_some(size)
    }
    fn creation_geometry(required: bool, capture: bool, child: bool) -> Result<(u16, u16)> {
        let inherited = std::env::var_os(DETACHED_GEOMETRY);
        unsafe { std::env::remove_var(DETACHED_GEOMETRY) };
        if child {
            return inherited
                .as_deref()
                .and_then(parse_geometry)
                .ok_or_else(|| "detached holder geometry is invalid".into());
        }
        let geometry = capture
            .then(ViewerConsole::detect)
            .flatten()
            .map(|console| console_geometry(console.0[1]))
            .transpose()?;
        creation_size(required, geometry)
    }
    fn holder(mut host: Native, listener: Listener) -> Result<i32> {
        let marker_path = std::mem::take(&mut host.marker);
        let marker = host.stage.take().unwrap();
        let user = std::mem::take(&mut host.sid);
        let reader = std::mem::take(&mut host.output).into_file();
        let writer = std::mem::take(&mut host.input).into_file();
        let pty = Duplex::tracked(reader, writer, 1 << 20);
        let mut artifacts = host.artifacts.take().unwrap();
        let running = std::mem::take(&mut artifacts.running);
        let (authenticated, clients) = mpsc::channel::<(bool, Option<Authentication>)>();
        let (mut authenticating, mut overflow_authenticating) = (0, false);
        let (synthetic, geometry) = (host.synthetic, host.geometry);
        let (mut handled, mut ready) = (false, std::mem::take(&mut host.ready));
        let mut runtime = artifacts.runtime(pty, (synthetic, host));
        runtime.set_geometry(geometry.0, geometry.1);
        let Some(NativeExit::Code(code)) = runtime.drive(
            |pending, overflow| {
                ready.notice(2, 0);
                while let Ok((exhausted, client)) = clients.try_recv() {
                    if exhausted {
                        overflow_authenticating = false;
                    } else {
                        authenticating -= 1;
                    }
                    let Some((stream, preface, trusted, pid)) = client else {
                        continue;
                    };
                    return Some((
                        Duplex::socket(stream, preface, cancel).ok()?,
                        trusted,
                        pid,
                        exhausted,
                    ));
                }
                while authenticating + pending < 16 {
                    let stream = listener.accept().ok()?;
                    let (send, user) = (authenticated.clone(), user.clone());
                    thread::spawn(move || send.send((false, authenticate(stream, &user))).ok());
                    authenticating += 1;
                }
                if !overflow && !overflow_authenticating {
                    let stream = listener.accept().ok()?;
                    let (send, user) = (authenticated.clone(), user.clone());
                    thread::spawn(move || send.send((true, authenticate(stream, &user))).ok());
                    overflow_authenticating = true;
                }
                None
            },
            || {
                let count = SHUTDOWN.swap(0, Ordering::AcqRel);
                (count != 0).then(|| count > 1 || std::mem::replace(&mut handled, true))
            },
        )?
        else {
            return Ok(125);
        };
        let termination = runtime.termination_method();
        let (exit, durable) = runtime.finish_exit(&running, NativeExit::Code(code), termination);
        let unlinked = durable
            && {
                let deleted = delete_file(&marker.file);
                drop(marker);
                deleted
                    && matches!(fs::symlink_metadata(marker_path), Err(error) if error.kind() == io::ErrorKind::NotFound)
            };
        runtime.retired(unlinked, false);
        Ok(exit)
    }

    fn observe_unpublished_exit(host: &mut Native) -> Result<Option<NativeExit>> {
        if host.process.is_null() {
            return Ok(None);
        }
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            if let Some(code) = process_exit(host.process.raw())? {
                return Ok(Some(NativeExit::Code(code)));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn finalizable_unpublished_exit(host: &mut Native) -> Result<Option<NativeExit>> {
        if !host.child_released {
            return Ok(None);
        }
        host.early_exit
            .map(NativeExit::Code)
            .map_or_else(|| observe_unpublished_exit(host), |exit| Ok(Some(exit)))
    }

    fn finalize_unpublished_exit(
        mut host: Native,
        observed: NativeExit,
        invoked: &OsStr,
        report: bool,
    ) -> Result<i32> {
        require(
            host.artifacts.is_some(),
            "unpublished child artifacts are unavailable",
        )?;
        if let Some(stage) = host.stage.take() {
            delete_file(&stage.file);
        }
        let reader = std::mem::take(&mut host.output).into_file();
        let writer = std::mem::take(&mut host.input).into_file();
        let pty = Duplex::tracked(reader, writer, 1 << 20);
        let mut artifacts = host.artifacts.take().unwrap();
        let running = std::mem::take(&mut artifacts.running);
        let (synthetic, geometry) = (host.synthetic, host.geometry);
        let mut ready = std::mem::take(&mut host.ready);
        let mut runtime = artifacts.runtime(pty, (synthetic, host));
        runtime.set_geometry(geometry.0, geometry.1);
        let status = runtime.drive(|_, _| None, || None)?.unwrap_or(observed);
        let (exit, durable) = runtime.finish_exit(&running, status, runtime.termination_method());
        require(durable, "prepublication child exit was not durable")?;
        crate::return_if!(!report, Ok(exit));
        eprintln!(
            "{}: child exited before session publication",
            name::program(invoked)
        );
        ready.notice(3, 1);
        Ok(1)
    }

    pub(crate) fn create(
        mode: CreateMode,
        path: &Path,
        mut command: Vec<OsString>,
        options: &Options,
        invoked: &OsStr,
    ) -> CommandResult<i32> {
        let foreground = matches!(mode, CreateMode::Run);
        let interactive = matches!(mode, CreateMode::Bare | CreateMode::New);
        let selected = std::env::var_os(DETACHED_HOLDER).as_deref() == Some(OsStr::new("1"));
        unsafe { std::env::remove_var(DETACHED_HOLDER) };
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let output =
            (selected && !handle.is_null() && unsafe { GetFileType(handle) } == FILE_TYPE_PIPE)
                .then(|| unsafe { File::from_raw_handle(handle) });
        let mut ready = LaunchReporter {
            output,
            generation: 1,
        };
        let child = ready.output.is_some();
        let geometry = creation_geometry(interactive, interactive || foreground, child)?;
        crate::return_if!(!foreground && !child, Ok(detached(geometry)?));
        SHUTDOWN.store(0, Ordering::Release);
        check(
            unsafe {
                SetConsoleCtrlHandler(None, FALSE) != 0
                    && SetConsoleCtrlHandler(Some(shutdown_handler), TRUE) != 0
            },
            "install console-control handler",
        )?;
        if command.is_empty() {
            command.push(
                std::env::var_os("SHELL")
                    .filter(|value| !value.is_empty())
                    .or_else(|| std::env::var_os("COMSPEC").filter(|value| !value.is_empty()))
                    .unwrap_or(system()?.join("cmd.exe").into()),
            );
        }
        let synthetic = terminal_environment(invoked);
        extend_ancestry(invoked, absolute(path)?, os_string, os_bytes)?;
        let user = sid()?;
        let stage_root = root(invoked)?;
        let random = random_array::<64>()?;
        let generation = supervised_generation(
            invoked,
            false,
            "supervised-launch acknowledgement was invalid",
            |selector| {
                let text = selector
                    .to_str()
                    .ok_or("invalid supervised-launch handle")?;
                let raw = usize::from_str_radix(text, 16)
                    .ok()
                    .filter(|raw| {
                        *raw != 0
                            && text.len() <= 16
                            && !text.starts_with('0')
                            && lowercase_hex(text.as_bytes())
                    })
                    .ok_or("invalid supervised-launch handle")?;
                let channel = unsafe { Handle::owned(raw as HANDLE) };
                validate_pipe(channel.raw(), "supervised-launch channel")?;
                decode_launch_record(&channel.record::<32>(true, "supervised-launch record")?)
                    .ok_or_else(|| "supervised-launch acknowledgement was invalid".into())
            },
        )?
        .0;
        ready.generation = generation;
        let marker = Marker::new(
            generation,
            random[..16].try_into().unwrap(),
            random[16..32].try_into().unwrap(),
        )?;
        let semantic = if options.events.is_some() {
            random[48..64].try_into().unwrap()
        } else {
            Default::default()
        };
        let event = options
            .events
            .as_deref()
            .map(|operand| -> Result<EventTarget> {
                let mut target = event_target(operand, &stage_root)?;
                materialize_event(&mut target, user, |_| {})?;
                Ok(target)
            })
            .transpose()?;
        let mut host = Native {
            marker: path.to_owned(),
            stage_root,
            sid: user.to_owned(),
            generation,
            options: options.clone(),
            incarnation: marker.incarnation,
            semantic_token: semantic,
            synthetic,
            geometry,
            ready,
            event,
            ..Native::default()
        };
        let listener = match host.launch(&marker, &command, random[32..48].try_into().unwrap()) {
            Ok(listener) => listener,
            Err(error) => {
                let observed = finalizable_unpublished_exit(&mut host)?;
                if let Some(observed) = observed {
                    if host.artifacts.is_none() {
                        let marker_identity = host
                            .stage
                            .as_ref()
                            .ok_or_else(|| "unpublished marker stage is unavailable".to_string())?
                            .identity;
                        if let Err(error) = host.prepare_storage(marker_identity) {
                            host.rollback_unpublished();
                            host.ready.notice(3, 1);
                            return Err(error.into());
                        }
                    }
                    return Ok(finalize_unpublished_exit(host, observed, invoked, child)?);
                }
                host.rollback_unpublished();
                let result = if error.starts_with("could not execute ") {
                    127
                } else {
                    1
                };
                host.ready.notice(3, result);
                return if result == 127 {
                    // 127 returns Ok, bypassing the common run()->report()
                    // layer, so this path owns its single diagnostic.
                    let _ = write!(io::stderr(), "{}: {error}\r\n", name::program(invoked));
                    Ok(127)
                } else {
                    // status 1 propagates as Err; the common report layer
                    // prints it exactly once. Printing here too would double it.
                    Err(error.into())
                };
            }
        };
        Ok(holder(host, listener)?)
    }
    pub(crate) fn preflight_create(
        options: &Options,
        session: &OsStr,
        invoked: &OsStr,
    ) -> Result<PathBuf> {
        if let Some(event) = options.events.as_deref() {
            if !event.is_absolute() {
                return Err(event_rejection(event, "not-absolute"));
            }
            event_target(event, &root(invoked)?)?;
        }
        resolve(session, invoked)
    }
    pub(crate) fn clock() -> Result<(u64, [u8; 16])> {
        Ok((unsafe { GetTickCount64() }, boot_identity()))
    }
}

#[cfg(windows)]
pub use native::bootstrap;
#[cfg(windows)]
pub(crate) use native::{
    attach, classify, cleanup, clock, connect, create, create_store_directory, create_store_file,
    create_store_path, current_paths, preflight_create, protected_store_path, resolve,
    rollback_store, sessions, valid_store_directory, valid_store_slots,
};
