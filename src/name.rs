use std::ffi::{OsStr, OsString};
#[cfg(not(any(unix, windows)))]
compile_error!("Moor supports only Unix-family systems and Windows");

#[cfg(unix)]
fn bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn bytes(value: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<_> = value.encode_wide().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < wide.len() {
        let unit = wide[i] as u32;
        let code = if (0xd800..=0xdbff).contains(&unit)
            && wide
                .get(i + 1)
                .is_some_and(|n| (0xdc00..=0xdfff).contains(&(*n as u32)))
        {
            i += 1;
            0x10000 + ((unit - 0xd800) << 10) + wide[i] as u32 - 0xdc00
        } else {
            unit
        };
        if code <= 0x7f {
            out.push(code as u8);
        } else if code <= 0x7ff {
            out.extend([(0xc0 | code >> 6) as u8, (0x80 | code & 0x3f) as u8]);
        } else if code <= 0xffff {
            out.extend([
                (0xe0 | code >> 12) as u8,
                (0x80 | code >> 6 & 0x3f) as u8,
                (0x80 | code & 0x3f) as u8,
            ]);
        } else {
            out.extend([
                (0xf0 | code >> 18) as u8,
                (0x80 | code >> 12 & 0x3f) as u8,
                (0x80 | code >> 6 & 0x3f) as u8,
                (0x80 | code & 0x3f) as u8,
            ]);
        }
        i += 1;
    }
    out
}

fn render_bytes(raw: &[u8]) -> String {
    let mut out = String::new();
    for byte in raw.iter().copied() {
        if byte.is_ascii_alphanumeric() || b"._/-".contains(&byte) {
            out.push(byte as char);
        } else {
            use std::fmt::Write;
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
    #[cfg(unix)]
    let base = raw.rsplit(|byte| *byte == b'/').next().unwrap_or(&[]);
    #[cfg(windows)]
    let base = raw
        .rsplit(|byte| *byte == b'/' || *byte == b'\\')
        .next()
        .unwrap_or(&[]);
    if base.is_empty() {
        "moor".into()
    } else {
        render_bytes(base)
    }
}

#[cfg(unix)]
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

#[cfg(windows)]
pub fn valid_session(value: &OsStr) -> bool {
    let raw = bytes(value);
    if raw.is_empty() || raw.last().is_some_and(|b| *b == b'/' || *b == b'\\') {
        return false;
    }
    let final_part = raw.rsplit(|b| *b == b'/' || *b == b'\\').next().unwrap();
    if final_part.is_empty()
        || final_part == b"."
        || final_part == b".."
        || final_part.contains(&b':')
        || final_part.last().is_some_and(|b| *b == b' ' || *b == b'.')
    {
        return false;
    }
    ![b".log".as_slice(), b".events", b".exit", b".instrument"]
        .iter()
        .any(|suffix| {
            final_part.len() >= suffix.len()
                && final_part[final_part.len() - suffix.len()..]
                    .iter()
                    .zip(*suffix)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
        })
}

pub fn rendered(value: &OsString) -> String {
    render(value.as_os_str())
}
