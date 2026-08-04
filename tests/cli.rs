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

#[test]
fn option_terminator_is_permanent_and_legacy_k_has_frozen_error() {
    let out = run(&["--", "-session", "--child-option"]);
    assert_eq!(out.status.code(), Some(125));

    let out = run(&["attach", "--", "-session", "-q"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("Invalid number of arguments")
    );

    let out = run(&["-k", "-f", "session"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "moor: Invalid number of arguments\nTry 'moor --help' for more information.\n"
    );
}

#[test]
fn repeated_flags_and_known_option_ownership_are_exact() {
    assert_eq!(run(&["list", "-a", "-a"]).status.code(), Some(125));
    let out = run(&["tail", "-q", "session"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "moor: Option '-q' is not valid for 'tail'\nTry 'moor --help' for more information.\n"
    );
}

#[test]
fn every_frozen_spelling_and_option_phase_reaches_dispatch() {
    for token in ["new", "n", "-c", "start", "s", "-n", "run", "-N", "-A"] {
        for args in [vec![token, "-q", "session"], vec![token, "session", "-q"]] {
            assert_eq!(run(&args).status.code(), Some(125), "{args:?}");
        }
    }
    for token in [
        "attach", "a", "-a", "push", "p", "-p", "kill", "k", "-k", "list", "l", "ls", "-l",
        "current", "-i",
    ] {
        let args = match token {
            "list" | "l" | "ls" | "-l" | "current" | "-i" => vec![token],
            _ => vec![token, "session"],
        };
        assert_eq!(run(&args).status.code(), Some(125), "{args:?}");
    }
}

#[test]
fn remaining_argument_diagnostics_name_the_real_command() {
    for (args, message) in [
        (&["-k", "--bogus", "session"][..], "Invalid mode '--bogus'"),
        (
            &["start", "-f", "session"][..],
            "Option '-f' is not valid for 'start'",
        ),
        (&["push", "--"][..], "Invalid number of arguments"),
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(String::from_utf8(out.stdout).unwrap().contains(message));
    }
}

#[test]
fn known_options_after_fixed_operands_keep_ownership_diagnostics() {
    for (args, command) in [
        (&["current", "-q"][..], "current"),
        (&["push", "session", "-q"][..], "push"),
        (&["clear", "session", "-q"][..], "clear"),
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(1));
        assert!(
            String::from_utf8(out.stdout)
                .unwrap()
                .contains(&format!("Option '-q' is not valid for '{command}'"))
        );
    }
}

#[test]
fn excess_operands_after_terminator_are_always_literal() {
    for args in [
        &["push", "--", "-session", "-q"][..],
        &["clear", "--", "-session", "-q"][..],
        &["current", "--", "-q"][..],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(1));
        assert!(
            String::from_utf8(out.stdout)
                .unwrap()
                .contains("Invalid number of arguments")
        );
    }
}

#[test]
fn every_owned_option_is_accepted_in_each_legal_phase() {
    let viewer: &[&[&str]] = &[
        &["-e", "^A"],
        &["-E"],
        &["-r", "winch"],
        &["-R", "move"],
        &["-z"],
        &["-q"],
        &["-t"],
    ];
    let create: &[&[&str]] = &[
        &["-C", "2m"],
        &["-2", "stderr"],
        &["-T", "session.events"],
        &["-S", "module"],
        &["-d", "."],
    ];
    for token in ["new", "n", "-c", "start", "s", "-n", "run", "-N", "-A"] {
        for option in viewer.iter().chain(create) {
            let mut before_session = vec![token];
            before_session.extend_from_slice(option);
            before_session.push("session");
            assert_eq!(
                run(&before_session).status.code(),
                Some(125),
                "{before_session:?}"
            );
            let mut after_session = vec![token, "session"];
            after_session.extend_from_slice(option);
            assert_eq!(
                run(&after_session).status.code(),
                Some(125),
                "{after_session:?}"
            );
        }
        assert_eq!(run(&["-q", token, "session"]).status.code(), Some(1));
    }
    for token in ["attach", "a", "-a"] {
        for option in viewer {
            let mut args = vec![token];
            args.extend_from_slice(option);
            args.push("session");
            assert_eq!(run(&args).status.code(), Some(125), "{args:?}");
        }
    }
}

#[test]
fn every_reserved_suffix_is_rejected_in_bare_and_path_forms() {
    for suffix in [".log", ".events", ".exit", ".instrument"] {
        for name in [format!("session{suffix}"), format!("dir/session{suffix}")] {
            assert_eq!(run(&[&name]).status.code(), Some(1), "{name}");
        }
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
