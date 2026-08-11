use std::process::{Command, Output};

fn program() -> String {
    std::path::Path::new(env!("CARGO_BIN_EXE_moor"))
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .expect("Cargo test binary has an ASCII basename")
        .to_owned()
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_moor"))
        .args(args)
        .output()
        .unwrap()
}

fn parses(args: &[&str]) -> bool {
    let mut argv = vec![std::ffi::OsString::from("moor")];
    argv.extend(args.iter().map(std::ffi::OsString::from));
    moor::cli::parse(&argv).is_ok()
}

const HELP: &str = "Usage:\n  moor <session> [options] [command [argument...]]\n  moor new|start|run [options] <session> [options] [command [argument...]]\n  moor attach [options] <session>\n  moor push <session>\n  moor kill [-f] [-q] <session>\n  moor rm [-q] <session> | moor rm -a [-q]\n  moor list [-a]\n  moor current\n  moor tail [-f] [-n N] <session>\n  moor clear [<session>]\n\nAttach/create options:\n  -e <char>  detach byte (default ^\\)\n  -E         disable detach\n  -r <mode>  child redraw: none, ctrl_l, winch (default none)\n  -R <mode>  viewer reset: none, move (default none)\n  -z         pass ^Z to the child\n  -q         suppress informational messages\n  -t         viewer is not VT-compatible\n\nCreate-only options:\n  -C <size>  log cap (default 1m; 0 disables)\n  -2 <path>  redirect child standard error\n  -T <path>  event store directory\n  -S <path>  launch-time instrumentation object\n  -d <path>  child working directory\n";

#[test]
fn help_and_no_args_are_identical_success() {
    let program = program();
    let expected = format!(
        "{program} {}\n{}",
        env!("CARGO_PKG_VERSION"),
        HELP.replace("moor", &program)
    );
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
        format!("{} {}\n", program(), env!("CARGO_PKG_VERSION"))
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn invalid_mode_uses_frozen_two_line_stdout() {
    let out = run(&["--typo"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!(
            "{}: Invalid mode '--typo'\nTry '{} --help' for more information.\n",
            program(),
            program()
        )
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn strict_numeric_and_reserved_names_are_rejected() {
    for args in [
        &["start", "s", "-C", "-1"][..],
        &["start", "s", "-C", "1kk"][..],
        &["start", "s", "-C", ""][..],
        &["tail", "-n", "garbage", "s"][..],
        &["start", "session.log"][..],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(out.stderr.is_empty(), "{args:?}");
    }
    // OB-3 freezes the size suffix as case-insensitive, so the uppercase forms
    // are values, not argument errors. They reach dispatch and fail later on
    // the session rather than on the operand.
    for args in [
        &["start", "s", "-C", "1K"][..],
        &["start", "s", "-C", "4M"][..],
        &["start", "s", "-C", "1G"][..],
    ] {
        assert!(parses(args), "{args:?}");
    }
}

#[test]
fn valid_create_grammars_reach_runtime_dispatch() {
    for args in [
        &["start", "s", "-C", "1m", "--", "/bin/sh", "-c", "exit 0"][..],
        &["s", "-q", "/bin/true"][..],
    ] {
        assert!(parses(args), "{args:?}");
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
        assert!(parses(args), "{args:?}");
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
        format!(
            "{}: Option '-C' is not valid for 'attach'\nTry '{} --help' for more information.\n",
            program(),
            program()
        )
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
    assert!(parses(&["--", "-session", "--child-option"]));

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
        format!(
            "{}: Invalid number of arguments\nTry '{} --help' for more information.\n",
            program(),
            program()
        )
    );
}

#[test]
fn repeated_flags_and_known_option_ownership_are_exact() {
    assert!(parses(&["list", "-a", "-a"]));
    let out = run(&["tail", "-q", "session"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!(
            "{}: Option '-q' is not valid for 'tail'\nTry '{} --help' for more information.\n",
            program(),
            program()
        )
    );
}

#[test]
fn every_frozen_spelling_and_option_phase_reaches_dispatch() {
    for token in ["new", "n", "-c", "start", "s", "-n", "run", "-N", "-A"] {
        for args in [vec![token, "-q", "session"], vec![token, "session", "-q"]] {
            assert!(parses(&args), "{args:?}");
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
        assert!(parses(&args), "{args:?}");
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
            assert!(parses(&before_session), "{before_session:?}");
            let mut after_session = vec![token, "session"];
            after_session.extend_from_slice(option);
            assert!(parses(&after_session), "{after_session:?}");
        }
        assert_eq!(run(&["-q", token, "session"]).status.code(), Some(1));
    }
    for token in ["attach", "a", "-a"] {
        for option in viewer {
            let mut args = vec![token];
            args.extend_from_slice(option);
            args.push("session");
            assert!(parses(&args), "{args:?}");
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

#[test]
fn parsed_action_preserves_defaults_repetition_paths_and_child_arguments() {
    use moor::cli::{Action, CreateMode, Redraw};
    use std::ffi::OsString;
    let args: Vec<_> = [
        "moor", "start", "-C", "1k", "session", "-C", "2m", "-r", "winch", "-d", "work", "--",
        "child", "-x",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let Action::Create {
        mode,
        session,
        command,
        options,
    } = moor::cli::parse(&args).unwrap()
    else {
        panic!()
    };
    assert_eq!(mode, CreateMode::Start);
    assert_eq!(session, "session");
    assert_eq!(command, ["child", "-x"]);
    assert_eq!(options.log_cap, 2 << 20);
    assert_eq!(options.redraw, Redraw::Winch);
    assert_eq!(options.detach, Some(0x1c));
    assert_eq!(options.directory.unwrap(), std::path::PathBuf::from("work"));
}

#[cfg(unix)]
#[test]
fn invoked_dot_is_not_normalized_to_moor() {
    use std::os::unix::process::CommandExt;
    let out = Command::new(env!("CARGO_BIN_EXE_moor"))
        .arg0(".")
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!(". {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[cfg(unix)]
#[test]
fn invoked_renamed_copy_name_drives_help_and_diagnostics() {
    use std::os::unix::fs::symlink;
    let dir = std::env::temp_dir().join(format!("moor-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    let renamed = dir.join("moor-copy");
    symlink(env!("CARGO_BIN_EXE_moor"), &renamed).unwrap();
    let out = Command::new(&renamed).arg("--version").output().unwrap();
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("moor-copy {}\n", env!("CARGO_PKG_VERSION"))
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

#[test]
fn v32_cli_numeric_fixtures_are_parsed_exactly() {
    // The ratified §16 V32 numeric table. These are the fixtures that decide
    // the OB-3 case-insensitivity reading, so they are the independent check on
    // it rather than a restatement: 1k and 1K must both be 1024.
    for operand in [
        "0",
        "1k",
        "1K",
        "2m",
        "2M",
        "3g",
        "3G",
        "18446744073709551615",
        "18014398509481983k",
    ] {
        assert!(
            parses(&["start", "s", "-C", operand]),
            "-C {operand} must be a value"
        );
    }
    // Overflow of the checked multiplication, a leading zero, a two-letter
    // suffix, and a suffixed tail count are all invalid.
    for args in [
        &["start", "s", "-C", "18014398509481984k"][..],
        &["start", "s", "-C", "01k"][..],
        &["start", "s", "-C", "1kb"][..],
        &["tail", "-n", "1k", "s"][..],
    ] {
        assert!(!parses(args), "{args:?} must be rejected");
    }
}

#[test]
fn action_spellings_preserve_their_distinct_modes_and_payloads() {
    use moor::cli::{Action, CreateMode};
    use std::ffi::OsString;

    let action = |args: &[&str]| {
        let mut argv = vec![OsString::from("moor")];
        argv.extend(args.iter().map(OsString::from));
        moor::cli::parse(&argv).unwrap()
    };
    for (token, mode) in [
        ("new", CreateMode::New),
        ("n", CreateMode::New),
        ("start", CreateMode::Start),
        ("s", CreateMode::Start),
        ("run", CreateMode::Run),
        ("-A", CreateMode::LegacyA),
        ("-c", CreateMode::LegacyC),
        ("-n", CreateMode::LegacyStart),
        ("-N", CreateMode::LegacyRun),
    ] {
        let Action::Create {
            mode: actual,
            session,
            command,
            ..
        } = action(&[token, "session", "child", "-x"])
        else {
            panic!("{token} did not create")
        };
        assert_eq!(actual, mode, "{token}");
        assert_eq!(session, "session", "{token}");
        assert_eq!(command, ["child", "-x"], "{token}");
    }

    assert!(matches!(action(&["a", "session"]), Action::Attach { .. }));
    assert_eq!(action(&["p", "session"]), Action::Push("session".into()));
    assert_eq!(action(&["ls", "-a"]), Action::List { all: true });
    assert_eq!(action(&["-i"]), Action::Current);
    assert_eq!(
        action(&["tail", "-f", "-n", "4294967295", "session"]),
        Action::Tail {
            session: "session".into(),
            follow: true,
            lines: u32::MAX,
        }
    );
}

#[cfg(unix)]
#[test]
fn native_name_rendering_and_final_component_rules_are_byte_exact() {
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStringExt;

    let hostile = OsString::from_vec(b"dir/na\0me\xff".to_vec());
    assert_eq!(moor::name::render(&hostile), "dir/na\\x00me\\xFF");
    assert_eq!(moor::name::program(&hostile), "na\\x00me\\xFF");
    assert_eq!(moor::name::program(OsStr::new("dir/")), "moor");

    for valid in ["session", "dir/session", "session.LOG", "has:colon"] {
        assert!(moor::name::valid_session(OsStr::new(valid)), "{valid}");
    }
    for invalid in [
        "",
        ".",
        "..",
        "dir/",
        "session.log",
        "dir/session.events",
        "session.exit",
        "session.instrument",
    ] {
        assert!(!moor::name::valid_session(OsStr::new(invalid)), "{invalid}");
    }
}

#[test]
fn trailing_terminator_is_accepted_by_every_non_creating_command() {
    // OB-4: `--` is a grammar terminator in every phase, never an
    // operand-count participant. Parser-only on purpose: exercising these at
    // runtime would read (and for `rm -a` mutate) the shared default session
    // root under the parallel suite.
    for args in [
        vec!["list", "--"],
        vec!["rm", "-a", "--"],
        vec!["clear", "--"],
        vec!["kill", "terminator-check", "--"],
        vec!["current", "--"],
        vec!["tail", "-f", "terminator-check", "--"],
        vec!["attach", "terminator-check", "--"],
    ] {
        assert!(parses(&args), "{args:?} rejected the trailing terminator");
    }
}
