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
    require(
        Wtf8Buf::from_wide(&wide).as_bytes() == bytes,
        "noncanonical WTF-8",
    )?;
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
binary_record!(RawMarker => Marker[80] error () = ();
    fixed { magic: [u8; 12] = *b"MOORMRK3\x01\0\0\0" }
    fields { generation: U32<LE>, incarnation: [u8; 16], pipe_length: [u8; 2], pipe: [u8; 46] });

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
schema!(struct pub BootstrapRecord derive [Clone, Copy, Debug, Eq, PartialEq] pub fields; nonce: [u8; 16], pid: u32, process: u64,
    thread: u64, created: u64);
binary_record!(RawBootstrapRecord => BootstrapRecord[56] error () = ();
    fixed { magic: [u8; 12] = *b"MOORBST1\x01\0\0\0" }
    fields { nonce: [u8; 16], pid: U32<LE>, process: U64<LE>, thread: U64<LE>, created: U64<LE> });

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
    crate::return_if!(bytes.len() != 17 || bytes[1..] != nonce, None);
    match (kind, *resumed) {
        (1, false) => *resumed = true,
        (2, true) => {}
        _ => return None,
    }
    Some(kind)
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
    use super::exact_descriptor_semantics;

    #[test]
    fn exact_dacl_semantics_ignore_order_and_reject_every_difference() {
        let expected = [(0u8, 0u8, 0x1f01ffu32, 18u32), (0, 0, 0x1f01ff, 42)];
        let reversed = [expected[1], expected[0]];
        let matches = |actual: &[_], owner, protected| {
            exact_descriptor_semantics(
                owner,
                protected,
                actual.len(),
                expected.len(),
                |left, right| actual[left] == expected[right],
            )
        };

        assert!(matches(&reversed, true, true));
        assert!(!matches(&expected, false, true));
        assert!(!matches(&expected, true, false));
        assert!(!matches(&expected[..1], true, true));
        assert!(!matches(
            &[expected[0], expected[1], (0, 0, 0x1f01ff, 1)],
            true,
            true
        ));
        assert!(!matches(&[expected[0], expected[0]], true, true));
        assert!(!matches(&[expected[0], (0, 0, 0x120089, 42)], true, true));
        assert!(!matches(&[expected[0], (0, 2, 0x1f01ff, 42)], true, true));
        assert!(!matches(&[expected[0], (1, 0, 0x1f01ff, 42)], true, true));
    }
}

#[cfg(windows)]
#[allow(unused_unsafe)]
mod native {
    use super::{
        BootstrapRecord, Marker, Result, accept_bootstrap_command, bootstrap_command,
        cim_boot_identity, exact_descriptor_semantics, wtf8_decode, wtf8_encode,
    };
    use crate::{
        cli::{CreateMode, Options},
        name, require,
        runtime::{
            client::{Client as WireClient, CommandError, CommandResult, missing, probe_session},
            holder::{Native as HolderNative, NativeExit},
            io::{
                Duplex, InputConfig, InputState, ViewerSender, attach_viewer_to, run_viewer_input,
            },
            private::*,
        },
        wire::put_wide,
    };
    use interprocess::local_socket::prelude::*;
    use interprocess::local_socket::{
        GenericFilePath, Listener as LocalListener, ListenerNonblockingMode, ListenerOptions, Name,
        Stream as LocalStream,
    };
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    use interprocess::os::windows::security_descriptor::{
        AsSecurityDescriptorExt, BorrowedSecurityDescriptor,
    };
    use smallvec::SmallVec;
    use std::{
        ffi::{OsStr, OsString, c_void},
        fs::{self, File, OpenOptions},
        io::{self, Read, Write},
        mem::size_of,
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            fs::OpenOptionsExt,
            io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle, RawHandle},
        },
        path::{Path, PathBuf},
        ptr,
        sync::OnceLock,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };
    use windows_permissions::{
        LocalBox, SecurityDescriptor,
        constants::{SeObjectType, SecurityInformation},
        utilities::buf_from_os as wide,
        wrappers,
    };
    use windows_spawn::{
        AsPseudoConsole, Child, Command as SpawnCommand, CreationFlags, Job, SpawnOptions,
        Stdio as SpawnStdio,
    };
    use windows_sys::Win32::{
        Foundation::*,
        Security::*,
        Storage::FileSystem::*,
        System::{
            Console::*,
            Diagnostics::{Debug::*, ToolHelp::*},
            JobObjects::*,
            LibraryLoader::*,
            Memory::*,
            Pipes::*,
            SystemInformation::*,
            Threading::*,
        },
    };

    fn check(ok: bool, what: &str) -> Result<()> {
        ok.then_some(())
            .ok_or_else(|| format!("{what}: {}", io::Error::last_os_error()))
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
        let wide: SmallVec<[u16; 256]> = value.encode_wide().collect();
        wtf8_encode(&wide)
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
    const BOOTSTRAP_SELECTOR: &str = "DESK_MOOR_BOOTSTRAP";
    const BOOTSTRAP_CONTROL: &str = "DESK_MOOR_BOOTSTRAP_CONTROL";
    const BOOTSTRAP_RESULT: &str = "DESK_MOOR_BOOTSTRAP_RESULT";
    const BOOTSTRAP_STDERR: &str = "DESK_MOOR_BOOTSTRAP_STDERR";
    const BOOTSTRAP_INSTRUMENT: &str = "DESK_MOOR_BOOTSTRAP_INSTRUMENT";
    const INSTRUMENT_CHANNEL: &str = "DESK_MOOR_INSTRUMENT_CHANNEL";
    const INSTRUMENT_NONCE: &str = "DESK_MOOR_INSTRUMENT_NONCE";
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
    impl Drop for Pseudo {
        fn drop(&mut self) {
            if self.0 != 0 {
                unsafe { ClosePseudoConsole(self.0) };
            }
        }
    }
    unsafe impl AsPseudoConsole for Pseudo {
        fn raw_pseudoconsole(&self) -> isize {
            self.0
        }
    }
    crate::schema!(tuple OpenPolicy [Clone, Copy]; fields; u32, u32);
    const CREATE_STORE: OpenPolicy = OpenPolicy(GENERIC_READ | GENERIC_WRITE, 0);
    const OPEN_SLOT: OpenPolicy = OpenPolicy(
        FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    );
    const OPEN_STDERR: OpenPolicy = OpenPolicy(FILE_APPEND_DATA | SYNCHRONIZE, FILE_SHARE_READ);
    const CREATE_STAGE: OpenPolicy = OpenPolicy(
        GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_DELETE,
    );
    const OPEN_MARKER: OpenPolicy = OpenPolicy(GENERIC_READ, FILE_SHARE_READ);
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
    fn launch_reporter() -> LaunchReporter<File> {
        let selected =
            std::env::var_os("DESK_MOOR_DETACHED_HOLDER").as_deref() == Some(OsStr::new("1"));
        unsafe { std::env::remove_var("DESK_MOOR_DETACHED_HOLDER") };
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let output =
            (selected && !handle.is_null() && unsafe { GetFileType(handle) } == FILE_TYPE_PIPE)
                .then(|| unsafe { File::from_raw_handle(handle) });
        LaunchReporter {
            output,
            generation: 1,
        }
    }
    fn launch_generation(invoked: &OsStr) -> Result<u32> {
        supervised_generation(
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
        )
        .map(|result| result.0)
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
    #[cfg(test)]
    mod security_descriptor_tests {
        use super::*;

        const USER: &str = "S-1-5-21-1-2-3-42";

        fn parsed(value: impl AsRef<str>) -> LocalBox<SecurityDescriptor> {
            value.as_ref().parse().unwrap()
        }

        fn first_ace(descriptor: &SecurityDescriptor) -> *mut ACE_HEADER {
            let acl = descriptor.dacl().unwrap() as *const windows_permissions::Acl;
            let mut ace = ptr::null_mut();
            assert_ne!(unsafe { GetAce(acl.cast(), 0, &mut ace) }, 0);
            ace.cast()
        }

        #[test]
        fn structural_validation_accepts_only_the_exact_protected_owner_and_aces() {
            let (expected, _) = descriptor(USER, "FA").unwrap();
            let reordered = parsed(format!("O:{USER}D:PAI(A;;FA;;;{USER})(A;;FA;;;SY)"));
            assert!(descriptor_matches(&expected, &expected).unwrap());
            assert!(descriptor_matches(&reordered, &expected).unwrap());

            for invalid in [
                format!("O:S-1-5-21-1-2-3-43D:P(A;;FA;;;SY)(A;;FA;;;{USER})"),
                format!("O:{USER}D:AI(A;;FA;;;SY)(A;;FA;;;{USER})"),
                format!("O:{USER}D:P(A;;FA;;;{USER})"),
                format!("O:{USER}D:P(A;;FA;;;{USER})(A;;FA;;;{USER})"),
                format!("O:{USER}D:P(A;;FA;;;SY)(A;;FA;;;{USER})(A;;FA;;;WD)"),
                format!("O:{USER}D:P(A;;FA;;;SY)(A;;FR;;;{USER})"),
                format!("O:{USER}D:P(D;;FA;;;SY)(A;;FA;;;{USER})"),
                format!("O:{USER}D:P(A;CI;FA;;;SY)(A;;FA;;;{USER})"),
            ] {
                assert!(
                    !descriptor_matches(&parsed(&invalid), &expected).unwrap(),
                    "accepted {invalid}"
                );
            }

            let invalid_flags = parsed(format!("O:{USER}D:P(A;;FA;;;SY)(A;;FA;;;{USER})"));
            unsafe { (*first_ace(&invalid_flags)).AceFlags = 0x20 };
            assert!(!descriptor_matches(&invalid_flags, &expected).unwrap());

            let invalid_type = parsed(format!("O:{USER}D:P(A;;FA;;;SY)(A;;FA;;;{USER})"));
            unsafe { (*first_ace(&invalid_type)).AceType = u8::MAX };
            assert!(!descriptor_matches(&invalid_type, &expected).unwrap());
        }

        #[test]
        fn file_descriptor_query_validates_a_created_store_directory() {
            let path = std::env::temp_dir().join(format!(
                "moor-windows-descriptor-{}-{}",
                std::process::id(),
                now()
            ));
            create_store_path(&path, true).unwrap();
            validate(&path, sid().unwrap(), "FA", true).unwrap();
            fs::remove_dir(path).unwrap();
        }
    }
    fn pipe_descriptor(
        sid: &str,
    ) -> Result<interprocess::os::windows::security_descriptor::SecurityDescriptor> {
        let (descriptor, _) = descriptor(sid, "0x12019b")?;
        unsafe {
            BorrowedSecurityDescriptor::from_ptr((&*descriptor as *const SecurityDescriptor).cast())
        }
        .to_owned_sd()
        .map_err(string)
    }
    fn protect(path: &Path, sid: &str, access: &str) -> Result<()> {
        use windows_permissions::constants::SeObjectType;
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
                let info: Result<FILE_STANDARD_INFO> = file_info(
                    file.as_raw_handle(),
                    FileStandardInfo,
                    "inspect Windows store slot links",
                );
                let Ok(identity) = file_identity(file.as_raw_handle()) else {
                    return false;
                };
                if !info.is_ok_and(|info| info.NumberOfLinks == 1)
                    || identities[..at].contains(&identity)
                    || store_file_identity(&path.join(name)) != Ok(identity)
                {
                    return false;
                }
                identities[at] = identity;
            }
            true
        }
    }
    pub(crate) fn create_store_path(path: &Path, directory: bool) -> io::Result<()> {
        let result = (|| unsafe {
            let user = sid()?;
            let (_descriptor, sa) = descriptor(user, "FA")?;
            if directory {
                check(
                    CreateDirectoryW(wide(path.as_os_str()).as_ptr(), &sa) != 0,
                    "create protected store directory",
                )
            } else {
                open_handle(path, CREATE_STORE, Some(&sa), "create protected store file").map(drop)
            }
        })();
        result.map_err(io::Error::other)
    }
    unsafe fn file_identity(handle: HANDLE) -> Result<[u8; 24]> {
        let info: FILE_ID_INFO =
            unsafe { file_info(handle, FileIdInfo, "query Windows file identity")? };
        let mut identity = [0; 24];
        identity[..8].copy_from_slice(&info.VolumeSerialNumber.to_le_bytes());
        identity[8..].copy_from_slice(&info.FileId.Identifier);
        Ok(identity)
    }
    fn session_identity(file: [u8; 24]) -> [u8; 25] {
        let mut identity = [0; 25];
        identity[0] = 2;
        identity[1..].copy_from_slice(&file);
        identity
    }
    unsafe fn store_file_identity(path: &Path) -> Result<[u8; 24]> {
        let file = unsafe { open_handle(path, OPEN_SLOT, None, "open Windows store slot")? };
        let info: FILE_STANDARD_INFO = unsafe {
            file_info(
                file.raw(),
                FileStandardInfo,
                "inspect Windows store slot links",
            )?
        };
        require(info.NumberOfLinks == 1, "hard-linked Windows store slot")?;
        unsafe { file_identity(file.raw()) }
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
            let (program, args) = command.split_first().unwrap();
            let mut requested = SpawnCommand::new(program);
            requested.args(args).env_remove(INSTRUMENT_NONCE);
            transfer_handles!(requested;
                INSTRUMENT_CHANNEL => instrument.as_ref(), "instrumentation channel"
            );
            if instrument.is_some() {
                requested.env(
                    INSTRUMENT_NONCE,
                    format!("{:032x}", u128::from_be_bytes(instrument_nonce)),
                );
            }
            if let Some(handle) = &stderr {
                requested.stderr(win(
                    SpawnStdio::from_borrowed(handle),
                    "transfer requested child stderr",
                )?);
            }
            let mut child = Some(win(
                requested.spawn_suspended_with(
                    SpawnOptions::new().creation_flags(CreationFlags::NEW_PROCESS_GROUP),
                ),
                "start requested child",
            )?);
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

    fn stream_handle(stream: &LocalStream) -> HANDLE {
        let LocalStream::NamedPipe(pipe) = stream;
        pipe.inner().as_handle().as_raw_handle()
    }

    fn local_name(pipe: &[u8; 46]) -> Result<Name<'_>> {
        let name = std::str::from_utf8(pipe).map_err(|_| "invalid pipe name")?;
        OsStr::new(name)
            .to_fs_name::<GenericFilePath>()
            .map_err(string)
    }

    fn same_user(stream: &LocalStream, user: &str) -> std::result::Result<bool, ()> {
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

    type Authentication = (LocalStream, [u8; 4], bool, Option<u32>);
    fn authenticate(mut stream: LocalStream, user: &str) -> Option<Authentication> {
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

    fn cancel(stream: &LocalStream) {
        unsafe { windows_sys::Win32::System::IO::CancelIoEx(stream_handle(stream), ptr::null()) };
    }

    crate::schema!(struct Instrument fields; path: PathBuf, file: File, identity: [u8; 24], digest: [u8; 32], read: Pipe, write: Pipe);
    crate::schema!(struct Bootstrap derive [Default] fields; child: Option<Child>, control: Pipe, result: Pipe, nonce: [u8; 16]);
    crate::schema!(struct Native derive [Default] fields; marker: PathBuf, stage_root: PathBuf, sid: String,
        generation: u32, options: Options, incarnation: [u8; 16], semantic_token: [u8; 16], synthetic: u8,
        conpty: Pseudo, job: Option<Job>, bootstrap: Bootstrap, process: Handle, pid: u32,
        early_exit: Option<u32>, birth: [u8; 16],
        input: Pipe, output: Pipe, instrument: Option<Instrument>, stderr: Handle, ready: LaunchReporter<File>,
        identity: [u8; 25], artifacts: Option<PreparedArtifacts>);
    impl Bootstrap {
        fn exchange(&self, kind: u8) -> Result<()> {
            let (write, read, rejected) = match kind {
                1 => (
                    "command bootstrap to resume child",
                    "bootstrap resume acknowledgement",
                    "bootstrap failed to resume child",
                ),
                _ => (
                    "command bootstrap to break child",
                    "bootstrap break acknowledgement",
                    "bootstrap failed to break child",
                ),
            };
            self.control
                .write(&bootstrap_command(kind, self.nonce), write)?;
            require(self.result.record::<1>(false, read)? == [0], rejected)
        }
    }
    impl Native {
        fn prepare_storage(&mut self, marker_identity: [u8; 24]) -> Result<()> {
            self.identity = session_identity(marker_identity);
            let start = (now(), unsafe { GetTickCount64() }, boot_identity());
            let event_path = self.options.events.as_deref().map(absolute).transpose()?;
            let event_identity = event_path.as_deref().map(|path| os_bytes(path.as_os_str()));
            let instrument_identity = self
                .instrument
                .as_ref()
                .map(|instrument| os_bytes(instrument.path.as_os_str()));
            let mut artifacts = holder_artifacts(
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
                    event_path: event_path.as_deref(),
                    encoding: "windows-wtf8",
                    event_identity: event_identity.as_deref(),
                    instrument_identity: instrument_identity.as_deref(),
                    event_store: None,
                    stores: None,
                    event_layout: 2,
                    log_cap: self.options.log_cap,
                },
            )?;
            let cwd = absolute(self.options.directory.as_deref().unwrap_or(Path::new(".")))?;
            let containment = u32::from_le_bytes(random_array::<4>()?).max(1);
            put_wide(&mut artifacts.status, &os_bytes(cwd.as_os_str())).map_err(crate::protocol)?;
            for bytes in [self.pid.to_le_bytes(), containment.to_le_bytes()] {
                artifacts.status.extend_from_slice(&bytes);
            }
            artifacts.status.extend_from_slice(&self.birth);
            self.artifacts = Some(artifacts);
            Ok(())
        }
        fn launch(
            &mut self,
            marker: &Marker,
            command: &[OsString],
            nonce: [u8; 16],
        ) -> Result<LocalListener> {
            let instrument = self.options.instrument.take();
            let listener = self.first_protected_pipe(&marker.pipe)?;
            let (marker_stage, marker_identity) = self.stage_marker(&marker.encode())?;
            if let Some(path) = &instrument {
                self.stage_instrument(path, &session_identity(marker_identity))?;
            }
            let pid = self.conpty_job_bootstrap(command, nonce)?;
            if instrument.is_some() {
                self.inject_and_ack(pid, nonce)?;
            }
            self.bootstrap.exchange(1)?;
            self.prepublication_alive()?;
            self.publish_marker(&marker_stage, marker_identity)?;
            Ok(listener)
        }

        fn first_protected_pipe(&self, pipe: &[u8; 46]) -> Result<LocalListener> {
            ListenerOptions::new()
                .name(local_name(pipe)?)
                .security_descriptor(pipe_descriptor(&self.sid)?)
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
                        COORD { X: 80, Y: 24 },
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
                    let path = absolute(path)?;
                    validate(&path, &self.sid, "FA", false)?;
                    self.stderr =
                        open_handle(&path, OPEN_STDERR, None, "open protected stderr sink")?;
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
                if let Some(directory) = &self.options.directory {
                    bootstrap.current_dir(directory);
                }
                bootstrap
                    .env_remove(BOOTSTRAP_SELECTOR)
                    .env_remove(INSTRUMENT_CHANNEL)
                    .env_remove(INSTRUMENT_NONCE);
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
            require(source.is_absolute(), "instrumentation path is not absolute")?;
            let mut input = read_reparse(
                source,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            )?;
            let attributes: FILE_ATTRIBUTE_TAG_INFO = unsafe {
                file_info(
                    input.as_raw_handle(),
                    FileAttributeTagInfo,
                    "inspect instrumentation object",
                )?
            };
            require(
                attributes.FileAttributes
                    & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
                    == 0,
                "validate instrumentation object",
            )?;
            let stage = instrument_stage(
                &self.stage_root,
                identity,
                self.generation,
                self.incarnation,
            )?;
            let (digest, staged_identity) = unsafe {
                let (_descriptor, sa) = descriptor(&self.sid, "GA")?;
                let mut output = open_handle(
                    &stage,
                    CREATE_STAGE,
                    Some(&sa),
                    "stage instrumentation object",
                )?
                .into_file();
                let digest = copy_digest(&mut input, Some(&mut output))?;
                output.sync_all().map_err(string)?;
                (digest, file_identity(output.as_raw_handle())?)
            };
            protect(&stage, &self.sid, "FRFX")?;
            validate(&stage, &self.sid, "FRFX", false)?;
            let mut staged = read_reparse(&stage, FILE_SHARE_READ | FILE_SHARE_DELETE)?;
            require(
                unsafe { file_identity(staged.as_raw_handle())? } == staged_identity,
                "instrumentation identity changed",
            )?;
            require(
                copy_digest(&mut staged, None)? == digest,
                "instrumentation content changed",
            )?;
            let (read, write) = Pipe::pair("create instrumentation acknowledgement pipe")?;
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
        fn stage_marker(&self, marker: &[u8; 84]) -> Result<(PathBuf, [u8; 24])> {
            unsafe {
                let mut stage = self.marker.as_os_str().to_owned();
                stage.push(format!(".stage-{}", GetCurrentProcessId()));
                let stage = PathBuf::from(stage);
                let (_descriptor, sa) = descriptor(&self.sid, "FR")?;
                let mut file =
                    open_handle(&stage, CREATE_STAGE, Some(&sa), "stage protected marker")?
                        .into_file();
                win(file.write_all(marker), "write protected marker")?;
                win32!(
                    FlushFileBuffers(file.as_raw_handle()),
                    "write protected marker"
                )?;
                let staged_identity = file_identity(file.as_raw_handle())?;
                Ok((stage, staged_identity))
            }
        }
        fn publish_marker(&mut self, stage: &Path, staged_identity: [u8; 24]) -> Result<()> {
            unsafe {
                self.prepare_storage(staged_identity)?;
                self.prepublication_alive()?;
                self.ready.notice(1, 0);
                win32!(
                    MoveFileExW(
                        wide(stage.as_os_str()).as_ptr(),
                        wide(self.marker.as_os_str()).as_ptr(),
                        MOVEFILE_WRITE_THROUGH,
                    ),
                    "publish protected marker"
                )?;
                validate(&self.marker, &self.sid, "FR", false)?;
                let final_file =
                    open_handle(&self.marker, OPEN_MARKER, None, "reopen protected marker")?;
                require(
                    file_identity(final_file.raw())? == staged_identity,
                    "marker identity changed",
                )?;
                Ok(())
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
            let (Ok(x), Ok(y)) = (i16::try_from(columns), i16::try_from(rows)) else {
                return Err("geometry exceeds the console interface limit".into());
            };
            check(
                unsafe { ResizePseudoConsole(self.conpty.0, COORD { X: x, Y: y }) } >= 0,
                "resize ConPTY",
            )
        }
        fn holder_ancestor(&self, pid: u32) -> bool {
            live_holder_ancestor(pid)
        }
        fn terminate(&mut self, force: bool) -> (u8, bool) {
            if !force && self.bootstrap.exchange(2).is_ok() {
                return (0, false);
            }
            let terminated = self
                .job
                .as_ref()
                .is_some_and(|job| job.terminate(0xc000013a).is_ok());
            (u8::from(terminated) << 1, true)
        }
        fn exited(&mut self) -> Result<Option<NativeExit>> {
            process_exit(self.process.raw()).map(|value| value.map(NativeExit::Code))
        }
    }

    fn absolute(path: &Path) -> Result<PathBuf> {
        path_buffer("resolve absolute Windows path", |out, size| unsafe {
            GetFullPathNameW(wide(path.as_os_str()).as_ptr(), size, out, ptr::null_mut())
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
    fn child_environment(invoked: &OsStr, path: &Path) -> Result<()> {
        extend_ancestry(invoked, absolute(path)?, os_string, os_bytes)
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
    fn controller(path: &Path, timeout: u32) -> Result<WireClient> {
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
        let stream = LocalStream::connect(local_name(&marker.pipe)?).map_err(string)?;
        require(
            read_marker(path)?.1 == identity,
            "marker identity changed during connection",
        )?;
        WireClient::from_stream(stream, identity.to_vec(), deadline, cancel)
    }
    pub(crate) fn connect(path: &Path) -> Result<WireClient> {
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
    pub(crate) fn attach(path: &Path, options: Options) -> CommandResult<i32> {
        let (input, output) = unsafe {
            (
                GetStdHandle(STD_INPUT_HANDLE),
                GetStdHandle(STD_OUTPUT_HANDLE),
            )
        };
        let mut mode = 0;
        crate::return_if!(
            unsafe { GetConsoleMode(input, &mut mode) } == 0
                || unsafe { GetConsoleMode(output, &mut mode) } == 0,
            Err(CommandError::output("no controlling terminal"))
        );
        let mut client = controller(path, 2000).map_err(|_| missing(path))?;
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        let geometry = if unsafe { GetConsoleScreenBufferInfo(output, &mut info) } != 0 {
            (
                (info.srWindow.Bottom - info.srWindow.Top + 1) as u16,
                (info.srWindow.Right - info.srWindow.Left + 1) as u16,
            )
        } else {
            (0, 0)
        };
        let mut output = io::stdout();
        Ok(attach_viewer_to(
            &mut client,
            &options,
            geometry,
            &mut output,
            Duration::from_secs(15),
            |remaining| controller(path, remaining.as_millis().min(u128::from(u32::MAX)) as u32),
            |sender| viewer_input(sender, options.detach),
        )?)
    }
    fn viewer_input(sender: ViewerSender, detach: Option<u8>) {
        thread::spawn(move || {
            let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) } as usize;
            run_viewer_input(
                io::stdin(),
                sender,
                InputConfig {
                    detach,
                    pass_suspend: true,
                    last_size: None,
                },
                move || match unsafe { WaitForSingleObject(input as HANDLE, 50) } {
                    WAIT_OBJECT_0 => InputState::Ready,
                    WAIT_TIMEOUT => InputState::Pending,
                    _ => InputState::Closed,
                },
                || None,
                || {},
                Instant::now,
            );
        });
    }
    fn detached() -> Result<i32> {
        let mut command = SpawnCommand::new(std::env::current_exe().map_err(string)?);
        command
            .args(std::env::args_os().skip(1))
            .env("DESK_MOOR_DETACHED_HOLDER", "1")
            .stdout(SpawnStdio::piped());
        let flags = CreationFlags::DETACHED_PROCESS | CreationFlags::NEW_PROCESS_GROUP;
        let mut child = win(
            command.spawn_with(SpawnOptions::new().creation_flags(flags)),
            "start detached holder",
        )?;
        Ok(i32::from(
            await_launch(
                child
                    .stdout
                    .take()
                    .ok_or("launch result pipe is unavailable")?,
            )?
            .0,
        ))
    }
    fn holder(mut host: Native, listener: LocalListener) -> Result<i32> {
        let user = std::mem::take(&mut host.sid);
        let reader = std::mem::take(&mut host.output).into_file();
        let writer = std::mem::take(&mut host.input).into_file();
        let pty = Duplex::tracked(reader, writer, 1 << 20);
        let marker_path = std::mem::take(&mut host.marker);
        let identity = host.identity;
        let mut artifacts = host.artifacts.take().unwrap();
        let running = std::mem::take(&mut artifacts.running);
        let (authenticated, clients) = mpsc::channel::<(bool, Option<Authentication>)>();
        let mut authenticating = 0;
        let mut overflow_authenticating = false;
        let synthetic = host.synthetic;
        let mut runtime = artifacts.runtime(pty, (synthetic, host));
        let Some(NativeExit::Code(code)) = runtime.drive(
            |pending, overflow| {
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
            || None,
        )?
        else {
            return Ok(125);
        };
        let termination = runtime.termination_method();
        let (exit, durable) = runtime.finish_exit(&running, NativeExit::Code(code), termination);
        let unlinked = durable
            && read_marker(&marker_path).is_ok_and(|(_, final_id)| final_id == identity)
            && fs::remove_file(&marker_path).is_ok();
        runtime.retired(unlinked, false);
        Ok(exit)
    }
    pub(crate) fn create(
        mode: CreateMode,
        path: &Path,
        mut command: Vec<OsString>,
        options: &Options,
        invoked: &OsStr,
    ) -> CommandResult<i32> {
        let foreground = matches!(mode, CreateMode::Run | CreateMode::LegacyRun);
        let mut ready = launch_reporter();
        let child = ready.output.is_some();
        crate::return_if!(!foreground && !child, Ok(detached()?));
        if command.is_empty() {
            command.push(
                std::env::var_os("SHELL")
                    .filter(|value| !value.is_empty())
                    .or_else(|| std::env::var_os("COMSPEC").filter(|value| !value.is_empty()))
                    .unwrap_or(system()?.join("cmd.exe").into()),
            );
        }
        let synthetic = terminal_environment(invoked);
        child_environment(invoked, path)?;
        let user = sid()?;
        let stage_root = root(invoked)?;
        if let Some(event) = options.events.as_deref().map(absolute).transpose()? {
            let namespace = path
                .parent()
                .ok_or_else(|| "session has no parent".to_string())?;
            require(
                event.starts_with(namespace),
                "event store is outside the session root",
            )?;
            validate(&event, user, "FA", true)?;
        }
        let random = random_array::<64>()?;
        let generation = launch_generation(invoked)?;
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
        let mut host = Native {
            marker: path.to_owned(),
            stage_root,
            sid: user.to_owned(),
            generation,
            options: options.clone(),
            incarnation: marker.incarnation,
            semantic_token: semantic,
            synthetic,
            ready,
            ..Native::default()
        };
        let listener = match host.launch(&marker, &command, random[32..48].try_into().unwrap()) {
            Ok(listener) => listener,
            Err(error) => {
                if let Some(code) = host.early_exit {
                    if child {
                        host.ready.notice(
                            if code == 0 { 1 } else { 3 },
                            code.min(u16::MAX as u32) as u16,
                        );
                        if code == 0 {
                            host.ready.notice(2, 0);
                        }
                    }
                    return Ok(code as i32);
                }
                let result = if error.starts_with("could not execute ") {
                    127
                } else {
                    1
                };
                host.ready.notice(3, result);
                eprintln!("{}: {error}", name::program(invoked));
                return if result == 127 {
                    Ok(127)
                } else {
                    Err(error.into())
                };
            }
        };
        host.ready.notice(2, 0);
        Ok(holder(host, listener)?)
    }
    pub(crate) fn preflight_create(
        options: &Options,
        session: &OsStr,
        invoked: &OsStr,
    ) -> Result<PathBuf> {
        if let Some(event) = options
            .events
            .as_deref()
            .filter(|event| !event.is_absolute())
        {
            return Err(format!(
                "event store rejected: {} (not-absolute)",
                name::render(event.as_os_str())
            ));
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
    attach, classify, cleanup, clock, connect, create, create_store_path, current_paths,
    preflight_create, protected_store_path, resolve, sessions, valid_store_slots,
};
