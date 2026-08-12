use std::ffi::c_void;

type Handle = *mut c_void;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CloseHandle(handle: Handle) -> i32;
    fn ExitProcess(code: u32) -> !;
    fn GetCurrentProcessId() -> u32;
    fn WriteFile(
        handle: Handle,
        buffer: *const c_void,
        bytes: u32,
        written: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
}

fn hex(text: &str) -> Option<Vec<u8>> {
    (text.len() % 2 == 0)
        .then(|| {
            text.as_bytes()
                .chunks_exact(2)
                .map(|pair| {
                    let digit = |byte| match byte {
                        b'0'..=b'9' => Some(byte - b'0'),
                        b'a'..=b'f' => Some(byte - b'a' + 10),
                        _ => None,
                    };
                    Some(digit(pair[0])? << 4 | digit(pair[1])?)
                })
                .collect::<Option<Vec<_>>>()
        })
        .flatten()
}

#[unsafe(no_mangle)]
pub extern "system" fn MoorInstrumentationInitV1() -> u32 {
    let (Some(channel), Some(nonce)) = (
        std::env::var_os("MOOR_INSTRUMENT_CHANNEL"),
        std::env::var_os("MOOR_INSTRUMENT_NONCE"),
    ) else {
        return 1;
    };
    unsafe {
        std::env::remove_var("MOOR_INSTRUMENT_CHANNEL");
        std::env::remove_var("MOOR_INSTRUMENT_NONCE");
    }
    let Some(handle) = channel
        .to_str()
        .and_then(|text| usize::from_str_radix(text, 16).ok())
        .filter(|handle| *handle != 0)
        .map(|handle| handle as Handle)
    else {
        return 2;
    };
    if std::env::var_os("MOOR_TEST_INSTRUMENT_EXIT").is_some() {
        unsafe { ExitProcess(66) };
    }
    let Some(nonce) = nonce.to_str().and_then(hex).filter(|value| value.len() == 16) else {
        return 3;
    };
    let generation = std::env::var("MOOR_SESSION_GENERATION")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1u32);
    let mut record = [0; 36];
    record[..8].copy_from_slice(b"MOORINS3");
    record[8] = 1;
    record[12..16].copy_from_slice(&generation.to_le_bytes());
    record[16..20].copy_from_slice(&unsafe { GetCurrentProcessId() }.to_le_bytes());
    record[20..].copy_from_slice(&nonce);
    let mut written = 0;
    let success = unsafe {
        WriteFile(
            handle,
            record.as_ptr().cast(),
            record.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        ) != 0
            && written == record.len() as u32
    };
    unsafe { CloseHandle(handle) };
    u32::from(!success) * 4
}
