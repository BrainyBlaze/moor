use std::borrow::Cow;
use std::ffi::OsStr;
#[cfg(not(any(unix, windows)))]
compile_error!("Moor supports only Unix-family systems and Windows");

#[cfg(unix)]
fn bytes(value: &OsStr) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    Cow::Borrowed(value.as_bytes())
}

#[cfg(windows)]
fn bytes(value: &OsStr) -> Cow<'_, [u8]> {
    use std::os::windows::ffi::OsStrExt;
    Cow::Owned(crate::windows::wtf8_encode(
        &value.encode_wide().collect::<Vec<_>>(),
    ))
}

fn render_bytes(raw: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(raw.len().saturating_mul(4));
    for byte in raw.iter().copied() {
        if byte.is_ascii_alphanumeric() || b"._/-".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str("\\x");
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 15) as usize] as char);
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
    byte == b'/' || cfg!(windows) && byte == b'\\'
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
    #[cfg(windows)]
    if final_part.contains(&b':')
        || final_part
            .last()
            .is_some_and(|byte| matches!(*byte, b' ' | b'.'))
    {
        return false;
    }
    ![b".log".as_slice(), b".events", b".exit", b".instrument"]
        .iter()
        .any(|suffix| {
            let tail = final_part.get(final_part.len().saturating_sub(suffix.len())..);
            #[cfg(unix)]
            return tail == Some(*suffix);
            #[cfg(windows)]
            tail.is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
        })
}
