use std::ffi::{OsStr, OsString};
use std::path::Path;

#[cfg(unix)]
fn bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn bytes(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}

pub fn render(value: &OsStr) -> String {
    let mut out = String::new();
    for byte in bytes(value) {
        if byte.is_ascii_alphanumeric() || b"._/-".contains(&byte) {
            out.push(byte as char);
        } else {
            use std::fmt::Write;
            write!(out, "\\x{byte:02X}").unwrap();
        }
    }
    out
}

pub fn program(value: &OsStr) -> String {
    Path::new(value)
        .file_name()
        .map(render)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "moor".into())
}

pub fn valid_session(value: &OsStr) -> bool {
    let raw = bytes(value);
    if raw.is_empty() || raw.contains(&0) || raw.last() == Some(&b'/') {
        return false;
    }
    let final_part = raw.rsplit(|b| *b == b'/').next().unwrap();
    if final_part.is_empty() || final_part == b"." || final_part == b".." {
        return false;
    }
    ![b".log".as_slice(), b".events", b".exit", b".instrument"]
        .iter()
        .any(|suffix| final_part.ends_with(suffix))
}

pub fn rendered(value: &OsString) -> String {
    render(value.as_os_str())
}
