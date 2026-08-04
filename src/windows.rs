use crate::wire::crc32c;
use std::ffi::OsString;
use std::path::Path;

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Marker {
    pub generation: u32,
    pub incarnation: [u8; 16],
    pub pipe: [u8; 46],
}

impl Marker {
    pub fn new(generation: u32, incarnation: [u8; 16], random: [u8; 16]) -> Result<Self> {
        if generation == 0 { return Err("zero marker generation".into()); }
        let mut pipe = [0; 46]; pipe[..14].copy_from_slice(br"\\.\pipe\moor-");
        for (n, byte) in random.into_iter().enumerate() {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            pipe[14 + n * 2] = HEX[(byte >> 4) as usize]; pipe[15 + n * 2] = HEX[(byte & 15) as usize];
        }
        Ok(Self { generation, incarnation, pipe })
    }

    pub fn encode(&self) -> [u8; 84] {
        let mut out = [0; 84]; out[..8].copy_from_slice(b"MOORMRK3"); out[8] = 1;
        out[12..16].copy_from_slice(&self.generation.to_le_bytes()); out[16..32].copy_from_slice(&self.incarnation);
        out[32..34].copy_from_slice(&46u16.to_le_bytes()); out[34..80].copy_from_slice(&self.pipe);
        let checksum = crc32c(&out[..80]); out[80..].copy_from_slice(&checksum.to_le_bytes()); out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 84 || &bytes[..8] != b"MOORMRK3" || bytes[8] != 1 || bytes[9..12] != [0; 3]
            || bytes[32..34] != 46u16.to_le_bytes() || &bytes[34..48] != br"\\.\pipe\moor-"
            || !bytes[48..80].iter().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            || u32::from_le_bytes(bytes[80..84].try_into().unwrap()) != crc32c(&bytes[..80]) { return Err("malformed Windows marker".into()); }
        let generation = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        if generation == 0 { return Err("zero marker generation".into()); }
        Ok(Self { generation, incarnation: bytes[16..32].try_into().unwrap(), pipe: bytes[34..80].try_into().unwrap() })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstrumentAck { pub generation: u32, pub pid: u32, pub nonce: [u8; 16] }

impl InstrumentAck {
    pub fn new(generation: u32, pid: u32, nonce: [u8; 16]) -> Result<Self> {
        if generation == 0 || pid == 0 { Err("zero instrumentation identity".into()) } else { Ok(Self { generation, pid, nonce }) }
    }
    pub fn encode(&self) -> [u8; 36] {
        let mut out = [0; 36]; out[..8].copy_from_slice(b"MOORINS3"); out[8] = 1;
        out[12..16].copy_from_slice(&self.generation.to_le_bytes()); out[16..20].copy_from_slice(&self.pid.to_le_bytes()); out[20..].copy_from_slice(&self.nonce); out
    }
    pub fn validate(bytes: &[u8], eof: bool, generation: u32, pid: u32, nonce: [u8; 16]) -> Result<()> {
        let expected = Self::new(generation, pid, nonce)?.encode();
        if eof && bytes == expected { Ok(()) } else { Err("instrumentation acknowledgement was invalid".into()) }
    }
}

pub trait LaunchHost {
    fn protected_root_marker(&mut self) -> Result<()>;
    fn first_protected_pipe(&mut self, pipe: &[u8; 46]) -> Result<()>;
    fn conpty_job_bootstrap(&mut self, command: &[OsString]) -> Result<u32>;
    fn stage_instrument(&mut self, path: &Path) -> Result<()>;
    fn inject_and_ack(&mut self, pid: u32, nonce: [u8; 16]) -> Result<(u32, Vec<u8>, bool)>;
    fn resume_child(&mut self, pid: u32) -> Result<()>;
    fn publish_marker(&mut self, marker: &[u8; 84]) -> Result<()>;
    fn authenticate_same_user(&mut self, preface: [u8; 4]) -> Result<()>;
}

pub struct LaunchRequest<'a> {
    pub marker: &'a Marker,
    pub command: &'a [OsString],
    pub instrument: Option<&'a Path>,
    pub nonce: [u8; 16],
}

pub fn launch(host: &mut impl LaunchHost, request: LaunchRequest<'_>) -> Result<u32> {
    host.protected_root_marker()?; let marker = request.marker.encode(); Marker::decode(&marker)?; host.first_protected_pipe(&request.marker.pipe)?;
    if let Some(path) = request.instrument { host.stage_instrument(path)?; }
    let pid = host.conpty_job_bootstrap(request.command)?;
    if request.instrument.is_some() {
        let (status, bytes, eof) = host.inject_and_ack(pid, request.nonce)?;
        if status != 0 { return Err("instrumentation initializer failed".into()); }
        InstrumentAck::validate(&bytes, eof, request.marker.generation, pid, request.nonce)?;
    }
    host.resume_child(pid)?; host.publish_marker(&marker)?; Ok(pid)
}

pub fn admit(host: &mut impl LaunchHost, bytes: &[u8], preface: [u8; 4]) -> Result<Marker> {
    host.protected_root_marker()?; let marker = Marker::decode(bytes)?; host.authenticate_same_user(preface)?;
    if &preface != b"MOOR" && &preface != b"MOOS" { return Err("invalid authenticated preface".into()); } Ok(marker)
}

#[cfg(windows)]
mod native {
    use super::*;
    use crate::cli::Action;
    use std::{ffi::{c_void, OsStr, OsString}, fs::File, io, mem::size_of, os::windows::{ffi::{OsStrExt, OsStringExt}, io::FromRawHandle}, path::PathBuf, ptr, thread};
    use windows_sys::Win32::{Foundation::*, Security::{*, Authorization::*, Cryptography::*}, Storage::FileSystem::*, System::{Console::*, JobObjects::*, Pipes::*, SystemInformation::*, Threading::*}};

    fn wide(value: &OsStr) -> Vec<u16> { value.encode_wide().chain(Some(0)).collect() }
    fn check(ok: bool, what: &str) -> Result<()> { if ok { Ok(()) } else { Err(format!("{what}: {}", std::io::Error::last_os_error())) } }
    fn temp() -> Result<PathBuf> { unsafe { let n = GetTempPathW(0, ptr::null_mut()); let mut out = vec![0; n as usize]; let used = GetTempPathW(n, out.as_mut_ptr()); check(used != 0 && used < n, "resolve Windows temporary directory")?; Ok(OsString::from_wide(&out[..used as usize]).into()) } }
    fn system() -> Result<PathBuf> { unsafe { let mut out = vec![0; 32768]; let used = GetSystemDirectoryW(out.as_mut_ptr(), out.len() as u32); check(used != 0 && used < out.len() as u32, "resolve Windows system directory")?; Ok(OsString::from_wide(&out[..used as usize]).into()) } }
    unsafe fn sid() -> Result<(Vec<u8>, String)> {
        let mut token: HANDLE = ptr::null_mut(); check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } != 0, "open user token")?;
        let mut size = 0; unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size) }; let mut words = vec![0usize; size as usize / size_of::<usize>() + 1];
        check(unsafe { GetTokenInformation(token, TokenUser, words.as_mut_ptr().cast(), size, &mut size) } != 0, "read user SID")?; unsafe { CloseHandle(token) };
        let source = unsafe { (*(words.as_ptr().cast::<TOKEN_USER>())).User.Sid }; let mut bytes = vec![0; unsafe { GetLengthSid(source) } as usize]; check(unsafe { CopySid(bytes.len() as u32, bytes.as_mut_ptr().cast(), source) } != 0, "copy user SID")?;
        let mut text = ptr::null_mut(); check(unsafe { ConvertSidToStringSidW(bytes.as_mut_ptr().cast(), &mut text) } != 0, "render user SID")?; let n = (0..).find(|n| unsafe { *text.add(*n) } == 0).unwrap(); let rendered = String::from_utf16(unsafe { std::slice::from_raw_parts(text, n) }).map_err(|_| "invalid user SID".to_string())?; unsafe { LocalFree(text.cast()) }; Ok((bytes, rendered))
    }
    unsafe fn descriptor(sid: &str, access: &str, inherit: bool) -> Result<(PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES)> {
        let text = wide(OsStr::new(&format!("D:P(A;;{access};;;SY)(A;;{access};;;{sid})"))); let mut sd = ptr::null_mut();
        check(unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(text.as_ptr(), 1, &mut sd, ptr::null_mut()) } != 0, "build protected DACL")?;
        Ok((sd, SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: inherit.into() }))
    }
    unsafe fn validate(path: &Path, user: &[u8], mask: u32) -> Result<()> {
        let name = wide(path.as_os_str()); let attributes = unsafe { GetFileAttributesW(name.as_ptr()) }; check(attributes != INVALID_FILE_ATTRIBUTES && attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0, "reject Windows reparse point")?; let (mut owner, mut dacl, mut sd) = (ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
        check(unsafe { GetNamedSecurityInfoW(name.as_ptr(), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION, &mut owner, ptr::null_mut(), &mut dacl, ptr::null_mut(), &mut sd) } == 0, "read protected DACL")?;
        let mut control = 0; let mut revision = 0; let mut info = ACL_SIZE_INFORMATION::default(); let mut system = [0u8; SECURITY_MAX_SID_SIZE as usize]; let mut system_len = system.len() as u32;
        check(unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) } != 0 && control & SE_DACL_PROTECTED != 0 && unsafe { EqualSid(owner, user.as_ptr() as *mut c_void) } != 0 && unsafe { GetAclInformation(dacl, (&mut info as *mut ACL_SIZE_INFORMATION).cast(), size_of::<ACL_SIZE_INFORMATION>() as u32, AclSizeInformation) } != 0 && info.AceCount == 2 && unsafe { CreateWellKnownSid(WinLocalSystemSid, ptr::null_mut(), system.as_mut_ptr().cast(), &mut system_len) } != 0, "validate protected owner/DACL")?;
        let mut seen = 0; for n in 0..2 { let mut raw = ptr::null_mut(); check(unsafe { GetAce(dacl, n, &mut raw) } != 0, "read protected ACE")?; let ace = unsafe { &*(raw.cast::<ACCESS_ALLOWED_ACE>()) }; let principal = &ace.SidStart as *const u32 as *mut c_void; if ace.Header.AceType != 0 || ace.Header.AceFlags & 0x10 != 0 || ace.Mask != mask { return Err("unexpected Windows access grant".into()); } else if unsafe { EqualSid(principal, user.as_ptr() as *mut c_void) } != 0 { seen |= 1 } else if unsafe { EqualSid(principal, system.as_mut_ptr().cast()) } != 0 { seen |= 2 } else { return Err("unexpected Windows access principal".into()); } }
        unsafe { LocalFree(sd.cast()) }; if seen == 3 { Ok(()) } else { Err("incomplete protected Windows DACL".into()) }
    }
    fn command_line(args: &[OsString]) -> Result<Vec<u16>> {
        if args.is_empty() { return Err("empty Windows command".into()); } let mut out = Vec::new();
        for (at, arg) in args.iter().enumerate() { if at != 0 { out.push(32); } out.push(34); let mut slash = 0; for ch in arg.encode_wide() { if ch == 92 { slash += 1; continue; } out.extend(std::iter::repeat_n(92, slash * usize::from(ch == 34) + slash)); slash = 0; if ch == 34 { out.push(92); } out.push(ch); } out.extend(std::iter::repeat_n(92, slash * 2)); out.push(34); } out.push(0); Ok(out)
    }

    struct Native { marker: PathBuf, root: Option<PathBuf>, sid: Vec<u8>, sid_text: String, pipe: HANDLE, conpty: HPCON, job: HANDLE, process: PROCESS_INFORMATION, input: HANDLE, output: HANDLE, published: bool }
    impl Drop for Native { fn drop(&mut self) { unsafe { if !self.published && !self.job.is_null() { TerminateJobObject(self.job, 0xc000013a); } for handle in [self.pipe, self.job, self.process.hThread, self.process.hProcess, self.input, self.output] { if !handle.is_null() && handle != INVALID_HANDLE_VALUE { CloseHandle(handle); } } if self.conpty != 0 { ClosePseudoConsole(self.conpty); } } } }
    impl Native { fn new(marker: PathBuf, root: Option<PathBuf>, sid: Vec<u8>, sid_text: String) -> Self { Self { marker, root, sid, sid_text, pipe: INVALID_HANDLE_VALUE, conpty: 0, job: ptr::null_mut(), process: PROCESS_INFORMATION::default(), input: ptr::null_mut(), output: ptr::null_mut(), published: false } }
        fn wait(&mut self) -> Result<i32> { let output = self.output as usize; self.output = ptr::null_mut(); let drain = thread::spawn(move || { let mut file = unsafe { File::from_raw_handle(output as *mut c_void) }; io::copy(&mut file, &mut io::sink()) }); unsafe { check(WaitForSingleObject(self.process.hProcess, INFINITE) == WAIT_OBJECT_0, "wait for child")?; let mut code = 0; check(GetExitCodeProcess(self.process.hProcess, &mut code) != 0, "read child status")?; ClosePseudoConsole(self.conpty); self.conpty = 0; let _ = drain.join(); Ok(code as i32) } }
    }
    impl LaunchHost for Native {
        fn protected_root_marker(&mut self) -> Result<()> { let Some(root) = self.root.as_ref() else { return Ok(()); }; unsafe { if !root.exists() { let (sd, sa) = descriptor(&self.sid_text, "FA", false)?; let made = CreateDirectoryW(wide(root.as_os_str()).as_ptr(), &sa); LocalFree(sd.cast()); check(made != 0, "create session root")?; } validate(root, &self.sid, FILE_ALL_ACCESS) } }
        fn first_protected_pipe(&mut self, pipe: &[u8; 46]) -> Result<()> { unsafe { let (sd, sa) = descriptor(&self.sid_text, "0x12019b", false)?; let name: Vec<u16> = pipe.iter().map(|b| *b as u16).chain(Some(0)).collect(); self.pipe = CreateNamedPipeW(name.as_ptr(), PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS, PIPE_UNLIMITED_INSTANCES, 65536, 65536, 0, &sa); LocalFree(sd.cast()); check(self.pipe != INVALID_HANDLE_VALUE, "create protected first pipe") } }
        fn conpty_job_bootstrap(&mut self, command: &[OsString]) -> Result<u32> { unsafe { let (mut cin, mut cout): (HANDLE, HANDLE) = (ptr::null_mut(), ptr::null_mut()); check(CreatePipe(&mut cin, &mut self.input, ptr::null(), 0) != 0 && CreatePipe(&mut self.output, &mut cout, ptr::null(), 0) != 0, "create ConPTY streams")?; check(CreatePseudoConsole(COORD { X: 80, Y: 24 }, cin, cout, 0, &mut self.conpty) >= 0, "create ConPTY")?; CloseHandle(cin); CloseHandle(cout);
            self.job = CreateJobObjectW(ptr::null(), ptr::null()); check(!self.job.is_null(), "create containment job")?; let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default(); limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE; check(SetInformationJobObject(self.job, JobObjectExtendedLimitInformation, (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(), size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32) != 0, "set no-breakaway kill-on-close job")?;
            let mut bytes = 0; InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut bytes); let mut attrs = vec![0u8; bytes]; check(InitializeProcThreadAttributeList(attrs.as_mut_ptr().cast(), 1, 0, &mut bytes) != 0 && UpdateProcThreadAttribute(attrs.as_mut_ptr().cast(), 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize, self.conpty as *const c_void, size_of::<HPCON>(), ptr::null_mut(), ptr::null()) != 0, "configure ConPTY bootstrap")?;
            let mut startup = STARTUPINFOEXW::default(); startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32; startup.lpAttributeList = attrs.as_mut_ptr().cast(); let mut line = command_line(command)?; let made = CreateProcessW(ptr::null(), line.as_mut_ptr(), ptr::null(), ptr::null(), 0, CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP | EXTENDED_STARTUPINFO_PRESENT, ptr::null(), ptr::null(), &startup.StartupInfo, &mut self.process); DeleteProcThreadAttributeList(startup.lpAttributeList); check(made != 0 && AssignProcessToJobObject(self.job, self.process.hProcess) != 0, "start contained suspended bootstrap")?; Ok(self.process.dwProcessId) } }
        fn stage_instrument(&mut self, _: &Path) -> Result<()> { Err("Windows instrumentation insertion is not installed; launch refused before child creation".into()) }
        fn inject_and_ack(&mut self, _: u32, _: [u8; 16]) -> Result<(u32, Vec<u8>, bool)> { Err("Windows instrumentation acknowledgement channel was not created".into()) }
        fn resume_child(&mut self, pid: u32) -> Result<()> { unsafe { check(pid == self.process.dwProcessId && ResumeThread(self.process.hThread) != u32::MAX, "resume contained child") } }
        fn publish_marker(&mut self, marker: &[u8; 84]) -> Result<()> { unsafe { let mut stage = self.marker.as_os_str().to_owned(); stage.push(format!(".stage-{}", GetCurrentProcessId())); let stage = PathBuf::from(stage); let (sd, sa) = descriptor(&self.sid_text, "FR", false)?; let file = CreateFileW(wide(stage.as_os_str()).as_ptr(), GENERIC_WRITE, 0, &sa, CREATE_NEW, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT, ptr::null_mut()); LocalFree(sd.cast()); check(file != INVALID_HANDLE_VALUE, "stage protected marker")?; let mut wrote = 0; let ok = WriteFile(file, marker.as_ptr().cast(), marker.len() as u32, &mut wrote, ptr::null_mut()) != 0 && wrote == marker.len() as u32 && FlushFileBuffers(file) != 0; CloseHandle(file); check(ok && MoveFileExW(wide(stage.as_os_str()).as_ptr(), wide(self.marker.as_os_str()).as_ptr(), MOVEFILE_WRITE_THROUGH) != 0, "publish protected marker")?; validate(&self.marker, &self.sid, FILE_GENERIC_READ)?; self.published = true; Ok(()) } }
        fn authenticate_same_user(&mut self, _: [u8; 4]) -> Result<()> { unsafe { check(ImpersonateNamedPipeClient(self.pipe) != 0, "impersonate pipe client")?; let mut token = ptr::null_mut(); let opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 0, &mut token) != 0; let mut size = 0; if opened { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size); } let mut words = vec![0usize; size as usize / size_of::<usize>() + 1]; let got = opened && GetTokenInformation(token, TokenUser, words.as_mut_ptr().cast(), size, &mut size) != 0; let same = got && EqualSid((*(words.as_ptr().cast::<TOKEN_USER>())).User.Sid, self.sid.as_mut_ptr().cast()) != 0; let reverted = RevertToSelf() != 0; if opened { CloseHandle(token); } check(same && reverted, "authenticate same-user pipe client") } }
    }

    pub fn run(action: Action, invoked: &OsStr, program: &str) -> i32 { match execute(action, invoked) { Ok(code) => code, Err(error) => { eprintln!("{program}: {error}"); 1 } } }
    fn execute(action: Action, invoked: &OsStr) -> Result<i32> { let Action::Create { session, mut command, options, .. } = action else { return Err("Windows controller command requires an authenticated live-session connection".into()); }; if command.is_empty() { command.push(if let Some(shell) = std::env::var_os("COMSPEC").filter(|value| !value.is_empty()) { shell } else { system()?.join("cmd.exe").into() }); } let (user, sid_text) = unsafe { sid()? }; let explicit = session.encode_wide().any(|c| c == 47 || c == 92); let root = if explicit { None } else { let mut name = OsString::from("."); name.push(Path::new(invoked).file_name().unwrap_or(OsStr::new("moor"))); name.push("-"); name.push(&sid_text); Some(temp()?.join(name)) }; let marker_path = root.as_ref().map_or_else(|| PathBuf::from(&session), |path| path.join(&session)); let mut random = [0u8; 48]; check(unsafe { BCryptGenRandom(ptr::null_mut(), random.as_mut_ptr(), random.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) } >= 0, "generate launch identity")?; let marker = Marker::new(1, random[..16].try_into().unwrap(), random[16..32].try_into().unwrap())?; let mut host = Native::new(marker_path, root, user, sid_text); launch(&mut host, LaunchRequest { marker: &marker, command: &command, instrument: options.instrument.as_deref(), nonce: random[32..].try_into().unwrap() })?; host.wait() }
}

#[cfg(windows)]
pub use native::run;
