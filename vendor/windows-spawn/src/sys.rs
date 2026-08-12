//! The only Win32 FFI boundary in the crate.

use std::cmp::Ordering;
use std::ffi::{c_void, OsString};
use std::io;
use std::mem::{size_of, size_of_val};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::process::ExitStatus;
use std::ptr;

use windows_sys::Win32::Foundation::{
    DuplicateHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, ERROR_BROKEN_PIPE,
    ERROR_HANDLE_EOF, ERROR_INSUFFICIENT_BUFFER, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Globalization::{
    CompareStringOrdinal, CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetFileAttributesW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, INVALID_FILE_ATTRIBUTES, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Environment::{FreeEnvironmentStringsW, GetEnvironmentStringsW};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    GetProcessId, InitializeProcThreadAttributeList, OpenProcess, ResumeThread, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_CREATE_PROCESS,
    PROCESS_DUP_HANDLE, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY,
    PROC_THREAD_ATTRIBUTE_PARENT_PROCESS, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

pub(crate) const INVALID_RAW_HANDLE: isize = -1;

struct EnvironmentBlock(*mut u16);

#[cfg(test)]
static ENVIRONMENT_BLOCK_DROPS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
static ATTRIBUTE_LIST_DROPS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

impl Drop for EnvironmentBlock {
    fn drop(&mut self) {
        #[cfg(test)]
        ENVIRONMENT_BLOCK_DROPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: the pointer came from GetEnvironmentStringsW and is
        // released exactly once.
        unsafe {
            FreeEnvironmentStringsW(self.0);
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum StandardStream {
    Input,
    Output,
    Error,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum NullAccess {
    Read,
    Write,
}

pub(crate) fn duplicate_local(
    source: BorrowedHandle<'_>,
    inheritable: bool,
) -> io::Result<OwnedHandle> {
    // SAFETY: `GetCurrentProcess` takes no arguments, cannot fail, and returns
    // the current-process pseudo-handle. The value is a constant that stays
    // valid for the lifetime of the process and must never be closed.
    let current = unsafe { GetCurrentProcess() };
    duplicate_between(
        current,
        raw(source),
        current,
        inheritable,
        DUPLICATE_SAME_ACCESS,
    )
}

fn duplicate_between(
    source_process: HANDLE,
    source: HANDLE,
    target_process: HANDLE,
    inheritable: bool,
    options: u32,
) -> io::Result<OwnedHandle> {
    let mut duplicate = ptr::null_mut();
    // SAFETY: process and source handles are valid for the call; `duplicate`
    // points to writable storage and becomes uniquely owned on success.
    if unsafe {
        DuplicateHandle(
            source_process,
            source,
            target_process,
            &mut duplicate,
            0,
            i32::from(inheritable),
            options,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    owned(duplicate)
}

#[derive(Debug)]
pub(crate) struct RemoteHandle<'a> {
    process: BorrowedHandle<'a>,
    value: HANDLE,
}

impl RemoteHandle<'_> {
    pub(crate) fn value(&self) -> isize {
        self.value as isize
    }
}

impl Drop for RemoteHandle<'_> {
    fn drop(&mut self) {
        // SAFETY: `GetCurrentProcess` takes no arguments, cannot fail, and
        // returns the current-process pseudo-handle. The value is a constant
        // that stays valid for the lifetime of the process and is never closed.
        let current = unsafe { GetCurrentProcess() };
        // `duplicate_between` turns the temporary local copy into an
        // `OwnedHandle`; discarding the result closes it immediately. The
        // close-source option atomically removes the remote value.
        let _ = duplicate_between(
            raw(self.process),
            self.value,
            current,
            false,
            DUPLICATE_SAME_ACCESS | DUPLICATE_CLOSE_SOURCE,
        );
    }
}

pub(crate) fn duplicate_remote<'a>(
    source: BorrowedHandle<'_>,
    target_process: BorrowedHandle<'a>,
    inheritable: bool,
) -> io::Result<RemoteHandle<'a>> {
    let mut value = ptr::null_mut();
    // SAFETY: both process handles and `source` remain valid. The returned
    // numeric handle belongs to `target_process` and is owned by RemoteHandle.
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            raw(source),
            raw(target_process),
            &mut value,
            0,
            i32::from(inheritable),
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(RemoteHandle {
        process: target_process,
        value,
    })
}

pub(crate) fn standard_handle(stream: StandardStream) -> io::Result<Option<OwnedHandle>> {
    let id = match stream {
        StandardStream::Input => STD_INPUT_HANDLE,
        StandardStream::Output => STD_OUTPUT_HANDLE,
        StandardStream::Error => STD_ERROR_HANDLE,
    };
    // SAFETY: GetStdHandle has no pointer preconditions.
    let handle = unsafe { GetStdHandle(id) };
    if !is_valid_handle(handle) {
        return Ok(None);
    }
    // SAFETY: GetStdHandle returned a live borrowed handle. The borrow is used
    // only during DuplicateHandle and is never closed.
    let borrowed = unsafe { BorrowedHandle::borrow_raw(handle as RawHandle) };
    duplicate_local(borrowed, false).map(Some)
}

pub(crate) fn null_handle(access: NullAccess) -> io::Result<OwnedHandle> {
    let name = [u16::from(b'N'), u16::from(b'U'), u16::from(b'L'), 0];
    let desired = match access {
        NullAccess::Read => GENERIC_READ,
        NullAccess::Write => GENERIC_WRITE,
    };
    // SAFETY: `name` is NUL-terminated; optional pointers are null. The return
    // value is transferred into OwnedHandle on success.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            desired,
            null_share_mode(),
            ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    owned(handle)
}

fn null_share_mode() -> u32 {
    [FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_SHARE_DELETE]
        .into_iter()
        .fold(0, std::ops::BitOr::bitor)
}

pub(crate) struct Pipe {
    pub(crate) parent: OwnedHandle,
    pub(crate) child: OwnedHandle,
}

pub(crate) fn create_pipe(parent_reads: bool) -> io::Result<Pipe> {
    let mut read = ptr::null_mut();
    let mut write = ptr::null_mut();
    // SAFETY: both output pointers are valid. Null security attributes make
    // both initial handles private; the child end is duplicated immediately
    // before CreateProcessW.
    if unsafe { CreatePipe(&mut read, &mut write, ptr::null::<SECURITY_ATTRIBUTES>(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful CreatePipe guarantees two valid, distinct handles;
    // ownership of both is transferred together before either can be lost.
    let (read, write) = unsafe {
        (
            OwnedHandle::from_raw_handle(read as RawHandle),
            OwnedHandle::from_raw_handle(write as RawHandle),
        )
    };
    if parent_reads {
        Ok(Pipe {
            parent: read,
            child: write,
        })
    } else {
        Ok(Pipe {
            parent: write,
            child: read,
        })
    }
}

pub(crate) fn open_parent_process(pid: u32) -> io::Result<OwnedHandle> {
    // SAFETY: OpenProcess has no pointer preconditions.
    let handle = unsafe { OpenProcess(PROCESS_CREATE_PROCESS | PROCESS_DUP_HANDLE, 0, pid) };
    owned(handle)
}

pub(crate) fn validate_process_handle(handle: BorrowedHandle<'_>) -> io::Result<()> {
    // SAFETY: the borrowed handle remains valid for the query.
    if unsafe { GetProcessId(raw(handle)) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn create_job() -> io::Result<OwnedHandle> {
    // SAFETY: null arguments request an unnamed Job with default security.
    owned(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })
}

pub(crate) fn validate_job_handle(handle: BorrowedHandle<'_>) -> io::Result<()> {
    query_job_limits(handle).map(drop)
}

pub(crate) fn set_job_kill_on_close(handle: BorrowedHandle<'_>, enable: bool) -> io::Result<()> {
    let mut limits = query_job_limits(handle)?;
    if enable {
        limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    } else {
        limits.BasicLimitInformation.LimitFlags &= !JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    }
    // SAFETY: `limits` is the exact structure required by the information
    // class and remains readable for the call.
    if unsafe {
        SetInformationJobObject(
            raw(handle),
            JobObjectExtendedLimitInformation,
            ptr::addr_of!(limits).cast(),
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                .expect("Job limit structure size fits u32"),
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn query_job_limits(
    handle: BorrowedHandle<'_>,
) -> io::Result<JOBOBJECT_EXTENDED_LIMIT_INFORMATION> {
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // SAFETY: `limits` is correctly sized writable storage for the selected
    // information class; the optional returned-size pointer is null.
    if unsafe {
        QueryInformationJobObject(
            raw(handle),
            JobObjectExtendedLimitInformation,
            ptr::addr_of_mut!(limits).cast(),
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                .expect("Job limit structure size fits u32"),
            ptr::null_mut(),
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(limits)
    }
}

pub(crate) fn assign_job(job: BorrowedHandle<'_>, process: BorrowedHandle<'_>) -> io::Result<()> {
    // SAFETY: both handles remain valid for the call.
    bool_result(unsafe { AssignProcessToJobObject(raw(job), raw(process)) })
}

pub(crate) fn terminate_job(job: BorrowedHandle<'_>, exit_code: u32) -> io::Result<()> {
    // SAFETY: the Job handle remains valid for the call.
    bool_result(unsafe { TerminateJobObject(raw(job), exit_code) })
}

pub(crate) struct AttributeList {
    storage: Box<[usize]>,
}

impl AttributeList {
    pub(crate) fn new(count: u32) -> io::Result<Self> {
        let mut bytes = 0_usize;
        // SAFETY: the documented first call uses a null list to obtain size.
        let probe =
            unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), count, 0, &mut bytes) };
        let probe_error = io::Error::last_os_error();
        if probe != 0
            || probe_error.raw_os_error()
                != Some(
                    i32::try_from(ERROR_INSUFFICIENT_BUFFER).expect("Win32 error code fits i32"),
                )
            || bytes == 0
        {
            return Err(if probe != 0 {
                io::Error::other("attribute-list size probe unexpectedly succeeded")
            } else {
                probe_error
            });
        }
        let words = bytes
            .checked_add(size_of::<usize>() - 1)
            .ok_or_else(|| io::Error::other("attribute list is too large"))?
            / size_of::<usize>();
        let mut storage = vec![0_usize; words].into_boxed_slice();
        let pointer = storage.as_mut_ptr().cast();
        let mut actual = words * size_of::<usize>();
        // SAFETY: Box<[usize]> is word-aligned, stable, and at least `bytes`
        // bytes long. It remains owned by the returned AttributeList.
        if unsafe { InitializeProcThreadAttributeList(pointer, count, 0, &mut actual) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { storage })
    }

    pub(crate) fn set_handle_list(&mut self, handles: &[isize]) -> io::Result<()> {
        self.update(
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            handles.as_ptr().cast(),
            size_of_val(handles),
        )
    }

    pub(crate) fn set_parent(&mut self, parent: &isize) -> io::Result<()> {
        self.update(
            PROC_THREAD_ATTRIBUTE_PARENT_PROCESS as usize,
            (parent as *const isize).cast(),
            size_of::<isize>(),
        )
    }

    pub(crate) fn set_mitigation(&mut self, words: &[u64; 2]) -> io::Result<()> {
        self.update(
            PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY as usize,
            words.as_ptr().cast(),
            size_of::<[u64; 2]>(),
        )
    }

    pub(crate) fn set_jobs(&mut self, jobs: &[isize]) -> io::Result<()> {
        self.update(
            PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
            jobs.as_ptr().cast(),
            size_of_val(jobs),
        )
    }

    pub(crate) fn set_pseudoconsole(&mut self, pseudoconsole: isize) -> io::Result<()> {
        // PSEUDOCONSOLE is the sole attribute whose lpValue is the HPCON value
        // itself, matching Microsoft's ConPTY sample, not `&HPCON`.
        self.update(
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            pseudoconsole as *const c_void,
            size_of::<isize>(),
        )
    }

    fn update(&mut self, attribute: usize, value: *const c_void, bytes: usize) -> io::Result<()> {
        // SAFETY: the list is initialized, `value` points to `bytes` readable
        // bytes (or is the documented HPCON value), and the transaction keeps
        // every backing allocation stable through CreateProcessW.
        if unsafe {
            UpdateProcThreadAttribute(
                self.pointer(),
                0,
                attribute,
                value,
                bytes,
                ptr::null_mut(),
                ptr::null(),
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn pointer(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_ptr().cast_mut().cast()
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        #[cfg(test)]
        ATTRIBUTE_LIST_DROPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: initialization succeeded once and this is its sole owner.
        unsafe { DeleteProcThreadAttributeList(self.pointer()) };
    }
}

#[derive(Clone, Copy)]
pub(crate) struct StandardHandles {
    pub(crate) stdin: isize,
    pub(crate) stdout: isize,
    pub(crate) stderr: isize,
}

#[derive(Clone, Copy)]
pub(crate) enum StartupStdio {
    Ordinary(StandardHandles),
    PseudoConsole,
}

pub(crate) struct ProcessRequest<'a> {
    pub(crate) application: &'a [u16],
    pub(crate) command_line: &'a mut [u16],
    pub(crate) environment: Option<&'a [u16]>,
    pub(crate) current_dir: Option<&'a [u16]>,
    pub(crate) stdio: StartupStdio,
    pub(crate) inherit_handles: bool,
    pub(crate) creation_flags: u32,
    pub(crate) suspended: bool,
    pub(crate) attributes: Option<&'a AttributeList>,
}

pub(crate) struct CreatedProcess {
    pub(crate) process: OwnedHandle,
    pub(crate) thread: OwnedHandle,
    pub(crate) pid: u32,
}

pub(crate) fn create_process(request: &mut ProcessRequest<'_>) -> io::Result<CreatedProcess> {
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = if request.attributes.is_some() {
        u32::try_from(size_of::<STARTUPINFOEXW>()).expect("startup structure size fits u32")
    } else {
        u32::try_from(size_of::<windows_sys::Win32::System::Threading::STARTUPINFOW>())
            .expect("startup structure size fits u32")
    };
    set_standard_handles(&mut startup, request.stdio);
    startup.lpAttributeList = request
        .attributes
        .map_or(ptr::null_mut(), AttributeList::pointer);

    let mut flags = request.creation_flags | CREATE_UNICODE_ENVIRONMENT;
    if request.suspended {
        flags |= CREATE_SUSPENDED;
    }
    if request.attributes.is_some() {
        flags |= EXTENDED_STARTUPINFO_PRESENT;
    }
    let environment = request
        .environment
        .map_or(ptr::null(), |block| block.as_ptr().cast());
    let current_dir = request.current_dir.map_or(ptr::null(), <[u16]>::as_ptr);
    let mut information = PROCESS_INFORMATION::default();

    // SAFETY: all UTF-16 buffers are correctly terminated and remain live;
    // command_line is writable as required by CreateProcessW. Startup handles
    // and every attribute backing allocation remain live for the call.
    if unsafe {
        CreateProcessW(
            request.application.as_ptr(),
            request.command_line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            i32::from(request.inherit_handles),
            flags,
            environment,
            current_dir,
            &startup.StartupInfo,
            &mut information,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: successful CreateProcessW guarantees valid process and primary
    // thread handles. Adopt both in one step so no success-only handle can leak.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(information.hProcess as RawHandle),
            OwnedHandle::from_raw_handle(information.hThread as RawHandle),
        )
    };
    Ok(CreatedProcess {
        process,
        thread,
        pid: information.dwProcessId,
    })
}

fn set_standard_handles(startup: &mut STARTUPINFOEXW, stdio: StartupStdio) {
    startup.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
    if let StartupStdio::Ordinary(handles) = stdio {
        startup.StartupInfo.hStdInput = handles.stdin as HANDLE;
        startup.StartupInfo.hStdOutput = handles.stdout as HANDLE;
        startup.StartupInfo.hStdError = handles.stderr as HANDLE;
    }
}

pub(crate) fn wait_process(process: BorrowedHandle<'_>) -> io::Result<()> {
    // SAFETY: the process handle remains valid while waiting.
    match unsafe { WaitForSingleObject(raw(process), INFINITE) } {
        WAIT_OBJECT_0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

pub(crate) fn try_wait_process(process: BorrowedHandle<'_>) -> io::Result<bool> {
    // SAFETY: the process handle remains valid while querying.
    match unsafe { WaitForSingleObject(raw(process), 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

#[cfg(test)]
pub(crate) fn wait_process_for_test(
    process: BorrowedHandle<'_>,
    timeout_millis: u32,
) -> io::Result<bool> {
    // SAFETY: the process handle remains valid while querying.
    match unsafe { WaitForSingleObject(raw(process), timeout_millis) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

#[cfg(test)]
pub(crate) fn cleanup_process_for_test(process: BorrowedHandle<'_>) {
    // Bypass the production wrapper so its mutants cannot disable cleanup.
    // SAFETY: tests pass a duplicate with the source process handle's access.
    let _ = unsafe { TerminateProcess(raw(process), 1) };
    // SAFETY: the same borrowed process handle remains valid for the wait.
    let _ = unsafe { WaitForSingleObject(raw(process), 5_000) };
}

pub(crate) fn exit_status(process: BorrowedHandle<'_>) -> io::Result<ExitStatus> {
    use std::os::windows::process::ExitStatusExt;

    let mut code = 0_u32;
    // SAFETY: `code` is writable and the process handle remains valid.
    if unsafe { GetExitCodeProcess(raw(process), &mut code) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ExitStatus::from_raw(code))
    }
}

pub(crate) fn terminate_process(process: BorrowedHandle<'_>, exit_code: u32) -> io::Result<()> {
    // SAFETY: the process handle remains valid for the call.
    bool_result(unsafe { TerminateProcess(raw(process), exit_code) })
}

pub(crate) fn resume_thread(thread: BorrowedHandle<'_>) -> io::Result<u32> {
    // SAFETY: the thread handle remains valid for the call.
    let previous = unsafe { ResumeThread(raw(thread)) };
    if previous == u32::MAX {
        Err(io::Error::last_os_error())
    } else {
        Ok(previous)
    }
}

pub(crate) fn read_handle(handle: BorrowedHandle<'_>, buffer: &mut [u8]) -> io::Result<usize> {
    if buffer.is_empty() {
        return Ok(0);
    }
    let length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    let mut read = 0_u32;
    // SAFETY: buffer is writable for `length` bytes, the synchronous handle
    // remains valid, and a null OVERLAPPED requests synchronous I/O.
    if unsafe {
        ReadFile(
            raw(handle),
            buffer.as_mut_ptr(),
            length,
            &mut read,
            ptr::null_mut(),
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(code)
                if code == i32::try_from(ERROR_BROKEN_PIPE).expect("Win32 error code fits i32")
                    || code == i32::try_from(ERROR_HANDLE_EOF).expect("Win32 error code fits i32")
        ) {
            Ok(0)
        } else {
            Err(error)
        }
    } else {
        Ok(read as usize)
    }
}

pub(crate) fn write_handle(handle: BorrowedHandle<'_>, buffer: &[u8]) -> io::Result<usize> {
    if buffer.is_empty() {
        return Ok(0);
    }
    let length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    let mut written = 0_u32;
    // SAFETY: buffer is readable for `length` bytes, the synchronous handle
    // remains valid, and a null OVERLAPPED requests synchronous I/O.
    if unsafe {
        WriteFile(
            raw(handle),
            buffer.as_ptr(),
            length,
            &mut written,
            ptr::null_mut(),
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(written as usize)
    }
}

pub(crate) fn environment_strings() -> io::Result<Vec<(OsString, OsString)>> {
    // SAFETY: GetEnvironmentStringsW returns a process-owned double-NUL block
    // which remains valid until FreeEnvironmentStringsW below.
    let base = unsafe { GetEnvironmentStringsW() };
    if base.is_null() {
        return Err(io::Error::last_os_error());
    }
    let guard = EnvironmentBlock(base);
    let mut entries = Vec::new();
    let mut cursor = guard.0;
    loop {
        // SAFETY: cursor walks one NUL-terminated entry at a time inside the
        // double-NUL-terminated environment block.
        if unsafe { *cursor } == 0 {
            break;
        }
        let mut length = 0_usize;
        // SAFETY: the OS-provided current entry is NUL-terminated.
        while unsafe { *cursor.add(length) } != 0 {
            length = length
                .checked_add(1)
                .ok_or_else(|| io::Error::other("environment entry is too large"))?;
        }
        // SAFETY: the just-computed range lies within the current entry.
        let entry = unsafe { std::slice::from_raw_parts(cursor, length) };
        if let Some(separator) = entry[1..]
            .iter()
            .position(|unit| *unit == u16::from(b'='))
            .map(|index| index + 1)
        {
            entries.push((
                OsString::from_wide(&entry[..separator]),
                OsString::from_wide(&entry[separator + 1..]),
            ));
        }
        let advance = length
            .checked_add(1)
            .ok_or_else(|| io::Error::other("environment block is too large"))?;
        // SAFETY: `advance` moves to the first unit after this entry's
        // terminator, which is still inside the double-NUL-terminated block.
        cursor = unsafe { cursor.add(advance) };
    }
    Ok(entries)
}

pub(crate) fn compare_ordinal(left: &[u16], right: &[u16]) -> Ordering {
    let left_len = i32::try_from(left.len()).unwrap_or(i32::MAX);
    let right_len = i32::try_from(right.len()).unwrap_or(i32::MAX);
    // SAFETY: both pointers are readable for their checked lengths.
    match unsafe { CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) } {
        CSTR_LESS_THAN => Ordering::Less,
        CSTR_EQUAL => Ordering::Equal,
        CSTR_GREATER_THAN => Ordering::Greater,
        _ => left.cmp(right),
    }
}

pub(crate) fn program_exists(path: &[u16]) -> bool {
    // SAFETY: callers supply a NUL-terminated path buffer.
    unsafe { GetFileAttributesW(path.as_ptr()) != INVALID_FILE_ATTRIBUTES }
}

pub(crate) fn system_directory() -> io::Result<OsString> {
    system_path(false)
}

pub(crate) fn windows_directory() -> io::Result<OsString> {
    system_path(true)
}

fn system_path(windows: bool) -> io::Result<OsString> {
    // Windows paths cannot exceed 32,767 UTF-16 code units. A single maximum
    // sized allocation avoids a retry loop whose termination would otherwise
    // depend on a length reported by the operating system.
    let mut buffer = vec![0_u16; 32_768];
    // SAFETY: buffer is writable for its reported length.
    let length = unsafe {
        if windows {
            GetWindowsDirectoryW(
                buffer.as_mut_ptr(),
                u32::try_from(buffer.len()).expect("maximum Windows path fits u32"),
            )
        } else {
            GetSystemDirectoryW(
                buffer.as_mut_ptr(),
                u32::try_from(buffer.len()).expect("maximum Windows path fits u32"),
            )
        }
    } as usize;
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length >= buffer.len() {
        return Err(io::Error::other(
            "Windows directory exceeds the maximum path length",
        ));
    }
    buffer.truncate(length);
    Ok(OsString::from_wide(&buffer))
}

fn raw(handle: BorrowedHandle<'_>) -> HANDLE {
    handle.as_raw_handle() as HANDLE
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if is_valid_handle(handle) {
        // SAFETY: callers pass a newly-created or newly-duplicated handle and
        // transfer its sole local ownership into this function.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
    } else {
        Err(io::Error::last_os_error())
    }
}

fn is_valid_handle(handle: HANDLE) -> bool {
    !handle.is_null() && handle != INVALID_HANDLE_VALUE
}

fn bool_result(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsHandle, AsRawHandle};
    use std::path::PathBuf;

    use super::*;
    use windows_sys::Win32::System::Threading::GetProcessHandleCount;

    fn process_handle_count(process: BorrowedHandle<'_>) -> io::Result<u32> {
        let mut count = 0;
        // SAFETY: `process` remains valid and count is writable DWORD storage.
        if unsafe { GetProcessHandleCount(raw(process), &mut count) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(count)
        }
    }

    fn current_process() -> BorrowedHandle<'static> {
        // SAFETY: `GetCurrentProcess` cannot fail and returns the
        // current-process pseudo-handle, a constant that stays valid for the
        // whole process lifetime. `BorrowedHandle` never closes what it borrows,
        // so a `'static` borrow of it can never dangle or double-close.
        unsafe { BorrowedHandle::borrow_raw(GetCurrentProcess() as RawHandle) }
    }

    #[test]
    fn pipe_null_and_duplicate_primitives_preserve_ownership() -> io::Result<()> {
        assert_eq!(
            null_share_mode(),
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        );
        let readable_null = null_handle(NullAccess::Read)?;
        let writable_null = null_handle(NullAccess::Write)?;
        assert_eq!(read_handle(readable_null.as_handle(), &mut [])?, 0);
        assert_eq!(write_handle(writable_null.as_handle(), &[])?, 0);
        assert_eq!(write_handle(writable_null.as_handle(), b"discard")?, 7);

        let parent_reads = create_pipe(true)?;
        assert_eq!(write_handle(parent_reads.child.as_handle(), b"a")?, 1);
        let mut byte = [0_u8; 1];
        assert_eq!(read_handle(parent_reads.parent.as_handle(), &mut byte)?, 1);
        assert_eq!(byte, [b'a']);

        let parent_writes = create_pipe(false)?;
        assert_eq!(write_handle(parent_writes.parent.as_handle(), b"b")?, 1);
        assert_eq!(read_handle(parent_writes.child.as_handle(), &mut byte)?, 1);
        assert_eq!(byte, [b'b']);

        let private = duplicate_local(writable_null.as_handle(), false)?;
        let inheritable = duplicate_local(writable_null.as_handle(), true)?;
        drop((private, inheritable));

        let mut host = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "ping -n 5 127.0.0.1 >nul"])
            .spawn()?;
        let target = open_parent_process(host.id())?;
        drop(target);
        let before = process_handle_count(host.as_handle())?;
        let local_before = process_handle_count(current_process())?;
        let remote = duplicate_remote(writable_null.as_handle(), host.as_handle(), true)?;
        assert_ne!(remote.value(), 0);
        assert!(process_handle_count(host.as_handle())? > before);
        drop(remote);
        assert_eq!(process_handle_count(host.as_handle())?, before);
        assert_eq!(process_handle_count(current_process())?, local_before);
        let _ = host.kill();
        let _ = host.wait();
        Ok(())
    }

    #[test]
    fn jobs_attributes_and_standard_handles_cover_all_ffi_shapes() -> io::Result<()> {
        let drops_before = ATTRIBUTE_LIST_DROPS.load(std::sync::atomic::Ordering::Relaxed);
        let job = create_job()?;
        validate_job_handle(job.as_handle())?;
        set_job_kill_on_close(job.as_handle(), true)?;
        assert_ne!(
            query_job_limits(job.as_handle())?
                .BasicLimitInformation
                .LimitFlags
                & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            0
        );
        set_job_kill_on_close(job.as_handle(), false)?;
        assert_eq!(
            query_job_limits(job.as_handle())?
                .BasicLimitInformation
                .LimitFlags
                & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            0
        );
        let process = open_parent_process(std::process::id())?;
        let inherited = duplicate_local(job.as_handle(), true)?;
        let handles = [inherited.as_raw_handle() as isize];
        let jobs = [job.as_raw_handle() as isize];
        let parent = process.as_raw_handle() as isize;
        let words = [1_u64, 0_u64];
        let mut attributes = AttributeList::new(4)?;
        attributes.set_handle_list(&handles)?;
        attributes.set_parent(&parent)?;
        attributes.set_mitigation(&words)?;
        attributes.set_jobs(&jobs)?;
        drop(attributes);

        let mut pseudoconsole = AttributeList::new(1)?;
        let _ = pseudoconsole.set_pseudoconsole(1);
        drop(pseudoconsole);
        assert!(ATTRIBUTE_LIST_DROPS.load(std::sync::atomic::Ordering::Relaxed) > drops_before);

        let input = standard_handle(StandardStream::Input)?;
        let output = standard_handle(StandardStream::Output)?;
        let error = standard_handle(StandardStream::Error)?;
        assert!(input.is_some() || output.is_some() || error.is_some());
        assert_eq!(INVALID_RAW_HANDLE, -1);
        Ok(())
    }

    #[test]
    fn startup_info_distinguishes_pseudoconsole_and_ordinary_stdio() {
        let mut conpty = STARTUPINFOEXW::default();
        set_standard_handles(&mut conpty, StartupStdio::PseudoConsole);
        assert_ne!(conpty.StartupInfo.dwFlags & STARTF_USESTDHANDLES, 0);
        assert!(conpty.StartupInfo.hStdInput.is_null());
        assert!(conpty.StartupInfo.hStdOutput.is_null());
        assert!(conpty.StartupInfo.hStdError.is_null());

        let mut ordinary = STARTUPINFOEXW::default();
        set_standard_handles(
            &mut ordinary,
            StartupStdio::Ordinary(StandardHandles {
                stdin: 1,
                stdout: 2,
                stderr: 3,
            }),
        );
        assert_ne!(ordinary.StartupInfo.dwFlags & STARTF_USESTDHANDLES, 0);
        assert_eq!(ordinary.StartupInfo.hStdInput as isize, 1);
        assert_eq!(ordinary.StartupInfo.hStdOutput as isize, 2);
        assert_eq!(ordinary.StartupInfo.hStdError as isize, 3);
    }

    #[test]
    fn environment_paths_comparison_and_error_helpers_work() -> io::Result<()> {
        let drops_before = ENVIRONMENT_BLOCK_DROPS.load(std::sync::atomic::Ordering::Relaxed);
        assert!(!environment_strings()?.is_empty());
        assert!(ENVIRONMENT_BLOCK_DROPS.load(std::sync::atomic::Ordering::Relaxed) > drops_before);
        assert_eq!(
            compare_ordinal(&[u16::from(b'a')], &[u16::from(b'B')]),
            Ordering::Less
        );
        assert_eq!(
            compare_ordinal(&[u16::from(b'a')], &[u16::from(b'A')]),
            Ordering::Equal
        );
        assert_eq!(
            compare_ordinal(&[u16::from(b'z')], &[u16::from(b'A')]),
            Ordering::Greater
        );
        assert_eq!(
            compare_ordinal(&[u16::from(b'B')], &[u16::from(b'a')]),
            Ordering::Greater
        );

        let system = system_directory()?;
        let windows = windows_directory()?;
        assert!(!system.is_empty() && !windows.is_empty());
        let mut executable = PathBuf::from(system);
        executable.push("cmd.exe");
        let mut wide: Vec<u16> = executable.as_os_str().encode_wide().collect();
        wide.push(0);
        assert!(program_exists(&wide));
        let missing = std::env::temp_dir().join(format!(
            "windows-spawn-definitely-missing-{}",
            std::process::id()
        ));
        let mut missing_wide: Vec<u16> = missing.as_os_str().encode_wide().collect();
        missing_wide.push(0);
        assert!(!program_exists(&missing_wide));

        assert!(owned(ptr::null_mut()).is_err());
        assert!(owned(INVALID_HANDLE_VALUE).is_err());
        assert!(bool_result(0).is_err());
        bool_result(1)?;

        assert!(open_parent_process(u32::MAX).is_err());

        let file = File::open("NUL")?;
        assert!(is_valid_handle(file.as_raw_handle() as HANDLE));
        assert!(!is_valid_handle(ptr::null_mut()));
        assert!(!is_valid_handle(INVALID_HANDLE_VALUE));
        assert!(validate_process_handle(file.as_handle()).is_err());
        assert!(validate_job_handle(file.as_handle()).is_err());
        Ok(())
    }
}
