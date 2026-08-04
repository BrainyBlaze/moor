use crate::name;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Redraw {
    None,
    CtrlL,
    Winch,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reset {
    None,
    Move,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Options {
    pub detach: Option<u8>,
    pub redraw: Redraw,
    pub reset: Reset,
    pub pass_suspend: bool,
    pub quiet: bool,
    pub non_vt: bool,
    pub log_cap: u64,
    pub stderr: Option<PathBuf>,
    pub events: Option<PathBuf>,
    pub instrument: Option<PathBuf>,
    pub directory: Option<PathBuf>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            detach: Some(0x1c),
            redraw: Redraw::None,
            reset: Reset::None,
            pass_suspend: false,
            quiet: false,
            non_vt: false,
            log_cap: 1 << 20,
            stderr: None,
            events: None,
            instrument: None,
            directory: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateMode {
    Bare,
    New,
    Start,
    Run,
    LegacyA,
    LegacyC,
    LegacyStart,
    LegacyRun,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Help,
    Version,
    Create {
        mode: CreateMode,
        session: OsString,
        command: Vec<OsString>,
        options: Options,
    },
    Attach {
        session: OsString,
        options: Options,
    },
    Push(OsString),
    Kill {
        session: OsString,
        force: bool,
        quiet: bool,
    },
    Remove {
        session: Option<OsString>,
        all: bool,
        quiet: bool,
    },
    List {
        all: bool,
    },
    Current,
    Tail {
        session: OsString,
        follow: bool,
        lines: u32,
    },
    Clear(Option<OsString>),
}

#[derive(Debug)]
pub struct Error(pub String);

fn eq(arg: &OsStr, text: &str) -> bool {
    arg == OsStr::new(text)
}
#[cfg(unix)]
fn leading_dash(arg: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    arg.as_bytes().first() == Some(&b'-')
}
#[cfg(not(unix))]
fn leading_dash(arg: &OsStr) -> bool {
    arg.to_string_lossy().starts_with('-')
}
fn shown(arg: &OsString) -> String {
    name::rendered(arg)
}
fn invalid_mode(arg: &OsString) -> Error {
    Error(format!("Invalid mode '{}'", shown(arg)))
}
fn invalid_args() -> Error {
    Error("Invalid number of arguments".into())
}
fn need(opt: &str) -> Error {
    Error(format!("Option '{opt}' requires an argument"))
}
fn invalid_value(value: &OsString, opt: &str) -> Error {
    Error(format!(
        "Invalid value '{}' for option '{opt}'",
        shown(value)
    ))
}
fn known_option(arg: &OsStr) -> bool {
    [
        "-e", "-E", "-r", "-R", "-z", "-q", "-t", "-C", "-2", "-T", "-S", "-d", "-f", "-a", "-n",
    ]
    .iter()
    .any(|option| eq(arg, option))
}
fn invalid_option(arg: &OsStr, command: &str) -> Error {
    Error(format!(
        "Option '{}' is not valid for '{command}'",
        name::render(arg)
    ))
}
fn extra_error(args: &[OsString], command: &str) -> Error {
    let args = &args[..args
        .iter()
        .position(|arg| eq(arg, "--"))
        .unwrap_or(args.len())];
    match args.iter().find(|arg| leading_dash(arg)) {
        Some(arg) if known_option(arg) => invalid_option(arg, command),
        Some(arg) => invalid_mode(arg),
        None => invalid_args(),
    }
}

fn parse_u32(value: &OsString, opt: &str) -> Result<u32, Error> {
    let s = value.to_str().ok_or_else(|| invalid_value(value, opt))?;
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(invalid_value(value, opt));
    }
    s.parse().map_err(|_| invalid_value(value, opt))
}
fn parse_size(value: &OsString) -> Result<u64, Error> {
    let s = value.to_str().ok_or_else(|| invalid_value(value, "-C"))?;
    let (digits, scale) = match s.as_bytes().last() {
        Some(b'k') => (&s[..s.len() - 1], 1024u64),
        Some(b'm') => (&s[..s.len() - 1], 1 << 20),
        Some(b'g') => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    if digits.is_empty()
        || (digits.len() > 1 && digits.starts_with('0'))
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(invalid_value(value, "-C"));
    }
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(scale))
        .ok_or_else(|| invalid_value(value, "-C"))
}
fn parse_detach(value: &OsString) -> Result<u8, Error> {
    let s = value
        .to_str()
        .ok_or_else(|| invalid_value(value, "-e"))?
        .as_bytes();
    match s {
        [b @ 0x20..=0x7e] => Ok(*b),
        [b'^', b'?'] => Ok(0x7f),
        [b'^', b @ b'@'..=b'_'] => Ok(*b - b'@'),
        _ => Err(invalid_value(value, "-e")),
    }
}
fn session(value: OsString) -> Result<OsString, Error> {
    if name::valid_session(&value) {
        Ok(value)
    } else {
        Err(Error(format!("Invalid session name '{}'", shown(&value))))
    }
}

fn create_only(arg: &OsStr) -> bool {
    ["-C", "-2", "-T", "-S", "-d"].iter().any(|s| eq(arg, s))
}

fn option(
    args: &[OsString],
    i: &mut usize,
    out: &mut Options,
    create: bool,
) -> Result<bool, Error> {
    let a = &args[*i];
    macro_rules! value {
        ($opt:literal) => {{
            *i += 1;
            args.get(*i).ok_or_else(|| need($opt))?.clone()
        }};
    }
    if eq(a, "-e") {
        out.detach = Some(parse_detach(&value!("-e"))?);
    } else if eq(a, "-E") {
        out.detach = None;
    } else if eq(a, "-r") {
        let v = value!("-r");
        out.redraw = match v.to_str() {
            Some("none") => Redraw::None,
            Some("ctrl_l") => Redraw::CtrlL,
            Some("winch") => Redraw::Winch,
            _ => return Err(invalid_value(&v, "-r")),
        };
    } else if eq(a, "-R") {
        let v = value!("-R");
        out.reset = match v.to_str() {
            Some("none") => Reset::None,
            Some("move") => Reset::Move,
            _ => return Err(invalid_value(&v, "-R")),
        };
    } else if eq(a, "-z") {
        out.pass_suspend = true;
    } else if eq(a, "-q") {
        out.quiet = true;
    } else if eq(a, "-t") {
        out.non_vt = true;
    } else if create && eq(a, "-C") {
        out.log_cap = parse_size(&value!("-C"))?;
    } else if create && eq(a, "-2") {
        out.stderr = Some(value!("-2").into());
    } else if create && eq(a, "-T") {
        out.events = Some(value!("-T").into());
    } else if create && eq(a, "-S") {
        out.instrument = Some(value!("-S").into());
    } else if create && eq(a, "-d") {
        out.directory = Some(value!("-d").into());
    } else {
        return Ok(false);
    }
    *i += 1;
    Ok(true)
}

fn create(
    args: &[OsString],
    mode: CreateMode,
    command_name: &str,
    mut i: usize,
    preset: Option<OsString>,
    options_done: bool,
) -> Result<Action, Error> {
    let mut options = Options::default();
    let mut sess = preset;
    let mut command = Vec::new();
    if options_done {
        command = args[i..].to_vec();
        return Ok(Action::Create {
            mode,
            session: session(sess.ok_or_else(invalid_args)?)?,
            command,
            options,
        });
    }
    while i < args.len() {
        if eq(&args[i], "--") {
            i += 1;
            if sess.is_none() {
                sess = Some(args.get(i).ok_or_else(invalid_args)?.clone());
                i += 1;
            }
            command = args[i..].to_vec();
            break;
        }
        if leading_dash(&args[i]) {
            if option(args, &mut i, &mut options, true)? {
                continue;
            }
            if known_option(&args[i]) {
                return Err(invalid_option(&args[i], command_name));
            }
            return Err(invalid_mode(&args[i]));
        }
        if sess.is_none() {
            sess = Some(args[i].clone());
            i += 1;
        } else {
            command = args[i..].to_vec();
            break;
        }
    }
    if options.non_vt && options.reset == Reset::Move {
        return Err(invalid_value(&OsString::from("move"), "-R"));
    }
    Ok(Action::Create {
        mode,
        session: session(sess.ok_or_else(invalid_args)?)?,
        command,
        options,
    })
}
fn attach(args: &[OsString], mut i: usize) -> Result<Action, Error> {
    let mut options = Options::default();
    let mut sess = None;
    while i < args.len() {
        if eq(&args[i], "--") {
            i += 1;
            if sess.is_some() || i >= args.len() {
                return Err(invalid_args());
            }
            sess = Some(args[i].clone());
            i += 1;
            if i != args.len() {
                return Err(invalid_args());
            }
            break;
        }
        if leading_dash(&args[i]) {
            if create_only(&args[i]) {
                return Err(Error(format!(
                    "Option '{}' is not valid for 'attach'",
                    name::render(&args[i])
                )));
            }
            if option(args, &mut i, &mut options, false)? {
                continue;
            }
            if known_option(&args[i]) {
                return Err(invalid_option(&args[i], "attach"));
            }
            return Err(invalid_mode(&args[i]));
        }
        if sess.replace(args[i].clone()).is_some() {
            return Err(invalid_args());
        }
        i += 1;
    }
    if options.non_vt && options.reset == Reset::Move {
        return Err(invalid_value(&OsString::from("move"), "-R"));
    }
    Ok(Action::Attach {
        session: session(sess.ok_or_else(invalid_args)?)?,
        options,
    })
}
fn one_session(args: &[OsString], i: usize, command: &str) -> Result<OsString, Error> {
    if args.len() == i + 1 {
        if eq(&args[i], "--") {
            return Err(invalid_args());
        }
        if leading_dash(&args[i]) {
            return Err(if known_option(&args[i]) {
                invalid_option(&args[i], command)
            } else {
                invalid_mode(&args[i])
            });
        }
        session(args[i].clone())
    } else if args.len() == i + 2 && eq(&args[i], "--") {
        session(args[i + 1].clone())
    } else {
        Err(extra_error(&args[i..], command))
    }
}

pub fn parse(args: Vec<OsString>) -> Result<Action, Error> {
    if args.len() == 1 {
        return Ok(Action::Help);
    }
    let first = &args[1];
    if eq(first, "--help") || eq(first, "-h") || eq(first, "?") {
        return if args.len() == 2 {
            Ok(Action::Help)
        } else {
            Err(invalid_args())
        };
    }
    if eq(first, "--version") {
        return if args.len() == 2 {
            Ok(Action::Version)
        } else {
            Err(invalid_args())
        };
    }
    let command_name = first.to_str().unwrap_or("create");
    let modern = |mode| create(&args, mode, command_name, 2, None, false);
    if eq(first, "new") || eq(first, "n") {
        return modern(CreateMode::New);
    }
    if eq(first, "start") || eq(first, "s") {
        return modern(CreateMode::Start);
    }
    if eq(first, "run") {
        return modern(CreateMode::Run);
    }
    if eq(first, "-A") {
        return modern(CreateMode::LegacyA);
    }
    if eq(first, "-c") {
        return modern(CreateMode::LegacyC);
    }
    if eq(first, "-n") {
        return modern(CreateMode::LegacyStart);
    }
    if eq(first, "-N") {
        return modern(CreateMode::LegacyRun);
    }
    if eq(first, "attach") || eq(first, "a") || eq(first, "-a") {
        return attach(&args, 2);
    }
    if eq(first, "push") || eq(first, "p") || eq(first, "-p") {
        return Ok(Action::Push(one_session(&args, 2, "push")?));
    }
    if eq(first, "list") || eq(first, "l") || eq(first, "ls") || eq(first, "-l") {
        let mut all = false;
        for arg in &args[2..] {
            if eq(arg, "-a") {
                all = true;
            } else if leading_dash(arg) {
                return Err(if known_option(arg) {
                    invalid_option(arg, "list")
                } else {
                    invalid_mode(arg)
                });
            } else {
                return Err(invalid_args());
            }
        }
        return Ok(Action::List { all });
    }
    if eq(first, "current") || eq(first, "-i") {
        return if args.len() == 2 {
            Ok(Action::Current)
        } else {
            Err(extra_error(&args[2..], "current"))
        };
    }
    if eq(first, "clear") {
        return match args.len() {
            2 => Ok(Action::Clear(None)),
            3 if leading_dash(&args[2]) => Err(if known_option(&args[2]) {
                invalid_option(&args[2], "clear")
            } else {
                invalid_mode(&args[2])
            }),
            3 => Ok(Action::Clear(Some(session(args[2].clone())?))),
            4 if eq(&args[2], "--") => Ok(Action::Clear(Some(session(args[3].clone())?))),
            _ => Err(extra_error(&args[2..], "clear")),
        };
    }
    if eq(first, "kill") || eq(first, "k") || eq(first, "-k") {
        let legacy = eq(first, "-k");
        let mut force = false;
        let mut quiet = false;
        let mut sess = None;
        let mut literal = false;
        for a in &args[2..] {
            if !literal && eq(a, "--") {
                literal = true
            } else if !literal && eq(a, "-f") && !legacy {
                force = true
            } else if !literal && eq(a, "-q") && !legacy {
                quiet = true
            } else if !literal && leading_dash(a) {
                return Err(if legacy && known_option(a) {
                    invalid_args()
                } else if known_option(a) {
                    invalid_option(a, "kill")
                } else {
                    invalid_mode(a)
                });
            } else if sess.replace(a.clone()).is_some() {
                return Err(invalid_args());
            }
        }
        return Ok(Action::Kill {
            session: session(sess.ok_or_else(invalid_args)?)?,
            force,
            quiet,
        });
    }
    if eq(first, "rm") {
        let mut all = false;
        let mut quiet = false;
        let mut sess = None;
        let mut literal = false;
        for a in &args[2..] {
            if !literal && eq(a, "--") {
                literal = true
            } else if !literal && eq(a, "-a") {
                all = true
            } else if !literal && eq(a, "-q") {
                quiet = true
            } else if !literal && leading_dash(a) {
                return Err(if known_option(a) {
                    invalid_option(a, "rm")
                } else {
                    invalid_mode(a)
                });
            } else if sess.replace(a.clone()).is_some() {
                return Err(invalid_args());
            }
        }
        if all == sess.is_some() || (!all && sess.is_none()) {
            return Err(invalid_args());
        }
        return Ok(Action::Remove {
            session: sess.map(session).transpose()?,
            all,
            quiet,
        });
    }
    if eq(first, "tail") {
        let mut follow = false;
        let mut lines = 10;
        let mut sess = None;
        let mut i = 2;
        while i < args.len() {
            if eq(&args[i], "--") {
                i += 1;
                if sess.is_some() || i + 1 != args.len() {
                    return Err(invalid_args());
                }
                sess = Some(args[i].clone());
                i += 1;
            } else if eq(&args[i], "-f") {
                follow = true;
                i += 1
            } else if eq(&args[i], "-n") {
                i += 1;
                let v = args.get(i).ok_or_else(|| need("-n"))?;
                lines = parse_u32(v, "-n")?;
                i += 1
            } else if leading_dash(&args[i]) {
                return Err(if known_option(&args[i]) {
                    invalid_option(&args[i], "tail")
                } else {
                    invalid_mode(&args[i])
                });
            } else if sess.replace(args[i].clone()).is_some() {
                return Err(invalid_args());
            } else {
                i += 1
            }
        }
        return Ok(Action::Tail {
            session: session(sess.ok_or_else(invalid_args)?)?,
            follow,
            lines,
        });
    }
    if eq(first, "--") {
        let s = args.get(2).ok_or_else(invalid_args)?.clone();
        return create(&args, CreateMode::Bare, "bare", 3, Some(s), true);
    }
    if leading_dash(first) {
        return Err(invalid_mode(first));
    }
    create(
        &args,
        CreateMode::Bare,
        "bare",
        2,
        Some(first.clone()),
        false,
    )
}

pub fn help(program: &str, version: &str) -> String {
    format!(
        "{program} {version}\nUsage:\n  {program} <session> [options] [command [argument...]]\n  {program} new|start|run [options] <session> [options] [command [argument...]]\n  {program} attach [options] <session>\n  {program} push <session>\n  {program} kill [-f] [-q] <session>\n  {program} rm [-q] <session> | {program} rm -a [-q]\n  {program} list [-a]\n  {program} current\n  {program} tail [-f] [-n N] <session>\n  {program} clear [<session>]\n\nAttach/create options:\n  -e <char>  detach byte (default ^\\)\n  -E         disable detach\n  -r <mode>  child redraw: none, ctrl_l, winch (default none)\n  -R <mode>  viewer reset: none, move (default none)\n  -z         pass ^Z to the child\n  -q         suppress informational messages\n  -t         viewer is not VT-compatible\n\nCreate-only options:\n  -C <size>  log cap (default 1m; 0 disables)\n  -2 <path>  redirect child standard error\n  -T <path>  event store directory\n  -S <path>  launch-time instrumentation object\n  -d <path>  child working directory\n"
    )
}
