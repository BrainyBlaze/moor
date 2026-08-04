use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_moor"))
        .args(args)
        .output()
        .unwrap()
}

const HELP: &str = "Usage:\n  moor <session> [options] [command [argument...]]\n  moor new|start|run [options] <session> [options] [command [argument...]]\n  moor attach [options] <session>\n  moor push <session>\n  moor kill [-f] [-q] <session>\n  moor rm [-q] <session> | moor rm -a [-q]\n  moor list [-a]\n  moor current\n  moor tail [-f] [-n N] <session>\n  moor clear [<session>]\n\nAttach/create options:\n  -e <char>  detach byte (default ^\\)\n  -E         disable detach\n  -r <mode>  child redraw: none, ctrl_l, winch (default none)\n  -R <mode>  viewer reset: none, move (default none)\n  -z         pass ^Z to the child\n  -q         suppress informational messages\n  -t         viewer is not VT-compatible\n\nCreate-only options:\n  -C <size>  log cap (default 1m; 0 disables)\n  -2 <path>  redirect child standard error\n  -T <path>  event store directory\n  -S <path>  launch-time instrumentation object\n  -d <path>  child working directory\n";

#[test]
fn help_and_no_args_are_identical_success() {
    let expected = format!("moor {}\n{HELP}", env!("CARGO_PKG_VERSION"));
    for args in [&[][..], &["--help"][..], &["-h"][..], &["?"][..]] {
        let out = run(args);
        assert!(out.status.success());
        assert_eq!(String::from_utf8(out.stdout).unwrap(), expected);
        assert!(out.stderr.is_empty());
    }
}

#[test]
fn version_is_one_lf_line() {
    let out = run(&["--version"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("moor {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn invalid_mode_uses_frozen_two_line_stdout() {
    let out = run(&["--typo"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "moor: Invalid mode '--typo'\nTry 'moor --help' for more information.\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn strict_numeric_and_reserved_names_are_rejected() {
    for args in [
        &["start", "s", "-C", "-1"][..],
        &["start", "s", "-C", "1K"][..],
        &["tail", "-n", "garbage", "s"][..],
        &["start", "session.log"][..],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(out.stderr.is_empty(), "{args:?}");
    }
}

#[test]
fn valid_create_grammars_reach_runtime_dispatch() {
    for args in [
        &["start", "s", "-C", "1m", "--", "/bin/sh", "-c", "exit 0"][..],
        &["s", "-q", "/bin/true"][..],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(125), "{args:?}");
        assert!(out.stdout.is_empty());
        assert_eq!(
            String::from_utf8(out.stderr).unwrap(),
            "moor: runtime not implemented\n"
        );
    }
}

#[test]
fn double_dash_introduces_dash_leading_session_for_every_shape() {
    for args in [
        &["--", "-bare", "/bin/true"][..],
        &["attach", "--", "-attach"][..],
        &["push", "--", "-push"][..],
        &["clear", "--", "-clear"][..],
        &["tail", "-n", "0", "--", "-tail"][..],
        &["kill", "--", "-kill"][..],
        &["rm", "--", "-rm"][..],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(125), "{args:?}");
    }
}

#[test]
fn numerics_are_canonical_and_attach_reports_option_ownership() {
    for args in [
        &["start", "s", "-C", "00"][..],
        &["start", "s", "-C", "01m"][..],
        &["tail", "-n", "00", "s"][..],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
    }

    let out = run(&["attach", "-C", "1m", "s"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "moor: Option '-C' is not valid for 'attach'\nTry 'moor --help' for more information.\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn command_specific_arity_and_legacy_rules_are_strict() {
    for args in [
        &["-T", "events", "start", "s"][..],
        &["-k", "-f", "s"][..],
        &["rm", "-a", "s"][..],
        &["current", "ignored"][..],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
    }
}

#[cfg(unix)]
#[test]
fn invoked_atch_name_drives_help_and_diagnostics() {
    use std::os::unix::fs::symlink;
    let dir = std::env::temp_dir().join(format!("moor-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    let atch = dir.join("atch");
    symlink(env!("CARGO_BIN_EXE_moor"), &atch).unwrap();
    let out = Command::new(&atch).arg("--version").output().unwrap();
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("atch {}\n", env!("CARGO_PKG_VERSION"))
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn diagnostics_render_hostile_bytes_reversibly() {
    use std::os::unix::ffi::OsStringExt;
    let bad = std::ffi::OsString::from_vec(vec![b'-', b'\n', 0xff]);
    let out = Command::new(env!("CARGO_BIN_EXE_moor"))
        .arg(bad)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "moor: Invalid mode '-\\x0A\\xFF'\nTry 'moor --help' for more information.\n"
    );
}
