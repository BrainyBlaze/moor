use std::{borrow::Cow, ffi::OsStr, fmt::Write as _};
#[cfg(not(unix))]
compile_error!("Moor supports only Unix-family systems (Linux and macOS)");

fn bytes(value: &OsStr) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    Cow::Borrowed(value.as_bytes())
}

fn render_bytes(raw: &[u8]) -> String {
    let mut out = String::with_capacity(raw.len().saturating_mul(4));
    for byte in raw.iter().copied() {
        if byte.is_ascii_alphanumeric() || b"._/-".contains(&byte) {
            out.push(byte as char);
        } else {
            write!(out, "\\x{byte:02X}").unwrap();
        }
    }
    out
}
pub fn render(value: &OsStr) -> String {
    render_bytes(&bytes(value))
}

pub fn program(value: &OsStr) -> String {
    let raw = bytes(value);
    raw.rsplit(|byte| separator(*byte))
        .next()
        .filter(|base| !base.is_empty())
        .map_or_else(|| "moor".into(), render_bytes)
}

fn separator(byte: u8) -> bool {
    byte == b'/'
}

pub(crate) fn artifact_suffix_len(raw: &[u8]) -> Option<usize> {
    [b".log".as_slice(), b".events", b".exit", b".instrument"]
        .into_iter()
        .find_map(|suffix| {
            let tail = raw.get(raw.len().checked_sub(suffix.len())?..)?;
            (tail == suffix).then_some(suffix.len())
        })
}

pub fn valid_session(value: &OsStr) -> bool {
    let raw = bytes(value);
    if raw.is_empty() || raw.contains(&0) || raw.last().is_some_and(|byte| separator(*byte)) {
        return false;
    }
    let final_part = raw.rsplit(|byte| separator(*byte)).next().unwrap();
    if final_part.is_empty() || matches!(final_part, b"." | b"..") {
        return false;
    }
    artifact_suffix_len(final_part).is_none()
}
