//! End-to-end UTF-16 argument and environment round-trip test.

#[cfg(windows)]
mod windows {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    use windows_spawn::Command;

    const OUTPUT: &str = "WINDOWS_SPAWN_ROUNDTRIP_OUTPUT";
    const VALUE: &str = "WINDOWS_SPAWN_ROUNDTRIP_VALUE";
    const MIXED: &str = "WINDOWS_SPAWN_ROUNDTRIP_MIXED";
    const REMOVE: &str = "WINDOWS_SPAWN_ROUNDTRIP_REMOVE";

    fn append_os(buffer: &mut Vec<u8>, value: &OsStr) {
        let wide: Vec<u16> = value.encode_wide().collect();
        let length = u32::try_from(wide.len()).expect("test values fit in a DWORD");
        buffer.extend_from_slice(&length.to_le_bytes());
        for unit in wide {
            buffer.extend_from_slice(&unit.to_le_bytes());
        }
    }

    fn child() -> io::Result<()> {
        let output = std::env::var_os(OUTPUT).expect("parent supplies output path");
        let mut encoded = Vec::new();
        let arguments: Vec<OsString> = std::env::args_os().skip(2).collect();
        let count = u32::try_from(arguments.len()).expect("test corpus fits in a DWORD");
        encoded.extend_from_slice(&count.to_le_bytes());
        for argument in &arguments {
            append_os(&mut encoded, argument);
        }
        append_os(
            &mut encoded,
            &std::env::var_os(VALUE).expect("parent supplies environment value"),
        );
        append_os(
            &mut encoded,
            &std::env::var_os(MIXED).expect("case-insensitive replacement survives"),
        );
        encoded.push(u8::from(std::env::var_os(REMOVE).is_some()));
        fs::write(output, encoded)
    }

    fn corpus() -> Vec<OsString> {
        vec![
            OsString::new(),
            OsString::from("plain"),
            OsString::from(" "),
            OsString::from("\t"),
            OsString::from("space separated words"),
            OsString::from("\""),
            OsString::from("embedded\"quote"),
            OsString::from("\\"),
            OsString::from("\\\\"),
            OsString::from("trailing\\"),
            OsString::from("trailing\\\\"),
            OsString::from("slashes\\\\\"before quote"),
            OsString::from("shell metacharacters &|<>^%()!"),
            OsString::from("日本語-Καλημέρα-🚀"),
            OsString::from_wide(&[0x0061, 0xd800, 0x0062]),
            OsString::from_wide(&[0xd83d, 0xde80]),
        ]
    }

    fn parent() -> io::Result<()> {
        let arguments = corpus();
        let environment = OsString::from_wide(&[
            0x74, 0x61, 0x62, 0x09, 0x65, 0x71, 0x3d, 0xd83d, 0xde80, 0xdfff,
        ]);
        let mixed = OsString::from("second-日本語");
        let output = std::env::temp_dir().join(format!(
            "windows-spawn-roundtrip-{}-{}.bin",
            std::process::id(),
            arguments.len()
        ));

        let mut command = Command::new(std::env::current_exe()?);
        command.arg("--child");
        command.args(&arguments);
        command
            .env_clear()
            .env(OUTPUT, &output)
            .env(VALUE, &environment)
            .env(MIXED, "first")
            .env("windows_spawn_roundtrip_mixed", &mixed)
            .env(REMOVE, "must disappear")
            .env_remove("windows_spawn_roundtrip_remove");
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            // LLVM writes a default `*.profraw` in the working directory when
            // env_clear removes this instrumentation-only destination.
            command.env("LLVM_PROFILE_FILE", profile);
        }
        let status = command.status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "round-trip child failed with {status}"
            )));
        }

        let mut expected = Vec::new();
        let count = u32::try_from(arguments.len()).expect("test corpus fits in a DWORD");
        expected.extend_from_slice(&count.to_le_bytes());
        for argument in &arguments {
            append_os(&mut expected, argument);
        }
        append_os(&mut expected, &environment);
        append_os(&mut expected, &mixed);
        expected.push(0);

        let actual = fs::read(&output)?;
        fs::remove_file(output)?;
        if actual != expected {
            return Err(io::Error::other(format!(
                "UTF-16 round trip differed: expected {} bytes, got {}",
                expected.len(),
                actual.len()
            )));
        }
        Ok(())
    }

    pub(super) fn main() -> io::Result<()> {
        if std::env::args_os().nth(1).as_deref() == Some(OsStr::new("--child")) {
            child()
        } else {
            parent()
        }
    }
}

#[cfg(windows)]
fn main() -> std::io::Result<()> {
    windows::main()
}

#[cfg(not(windows))]
fn main() {}
