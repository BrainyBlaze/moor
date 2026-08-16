use crate::name;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

schema!(enum pub Redraw [Clone, Copy, Debug, Eq, PartialEq]; None, CtrlL, Winch);
schema!(enum pub Reset [Clone, Copy, Debug, Eq, PartialEq]; None, Move);
schema!(enum pub CreateMode [Clone, Copy, Debug, Eq, PartialEq]; Bare, New, Start, Run);

schema!(struct default pub Options derive [Clone, Debug, Eq, PartialEq] pub fields; detach: Option<u8> = Some(0x1c), redraw: Redraw = Redraw::None, reset: Reset = Reset::None, pass_suspend: bool = false, quiet: bool = false, non_vt: bool = false, log_cap: u64 = 1 << 20, stderr: Option<PathBuf> = None, events: Option<PathBuf> = None, instrument: Option<PathBuf> = None, directory: Option<PathBuf> = None);

schema!(enum pub Action [Clone, Debug, Eq, PartialEq]; Help, Version, Create { mode: CreateMode, session: OsString, command: Vec<OsString>, options: Options }, Attach { session: OsString, options: Options }, Push(OsString), Kill { session: OsString, force: bool, quiet: bool }, Remove { session: Option<OsString>, all: bool, quiet: bool }, List { all: bool }, Current, Tail { session: OsString, follow: bool, lines: u32 }, Clear(Option<OsString>));

schema!(tuple pub Error [Debug]; fields pub; String);
type CliResult<T> = Result<T, Error>;
type Scan<'a> = (Options, u16, u32, smallvec::SmallVec<[&'a OsString; 4]>);

const OPTIONS: [&str; 15] = [
    "-e", "-E", "-r", "-R", "-z", "-q", "-t", "-C", "-2", "-T", "-S", "-d", "-f", "-a", "-n",
];
const VIEW: u16 = (1 << 7) - 1;
const CREATE: u16 = (1 << 12) - 1;
const FORCE: u16 = 1 << 12;
const ALL: u16 = 1 << 13;
const NUMBER: u16 = 1 << 14;
const VALUES: u16 = 1 | 1 << 2 | 1 << 3 | 1 << 7 | 1 << 8 | 1 << 9 | 1 << 10 | 1 << 11 | NUMBER;

fn leading_dash(arg: &OsStr) -> bool {
    arg.as_encoded_bytes().first() == Some(&b'-')
}
fn invalid_mode(arg: &OsStr) -> Error {
    Error(format!("Invalid mode '{}'", name::render(arg)))
}
fn invalid_args() -> Error {
    Error("Invalid number of arguments".into())
}
fn bad_value(value: &OsString, option: &str) -> Error {
    Error(format!(
        "Invalid value '{}' for option '{option}'",
        name::render(value)
    ))
}
fn bad_option(arg: &OsStr, command: &str) -> Error {
    Error(format!(
        "Option '{}' is not valid for '{command}'",
        name::render(arg)
    ))
}
fn parse_size(value: &OsString) -> CliResult<u64> {
    let s = value.to_str().ok_or_else(|| bad_value(value, "-C"))?;
    // OB-3 freezes the suffix as case-insensitive; the closure draft's
    // lowercase-only reading loses to the authoritative register.
    let (digits, scale) = match s.as_bytes().last().map(u8::to_ascii_lowercase) {
        Some(b'k') => (&s[..s.len() - 1], 1024u64),
        Some(b'm') => (&s[..s.len() - 1], 1 << 20),
        Some(b'g') => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    number(digits, value, "-C")?
        .checked_mul(scale)
        .ok_or_else(|| bad_value(value, "-C"))
}
fn number(text: &str, value: &OsString, option: &str) -> CliResult<u64> {
    crate::canonical_u64(text).ok_or_else(|| bad_value(value, option))
}
fn parse_detach(value: &OsString) -> CliResult<u8> {
    match value.to_str().map(str::as_bytes) {
        Some([b @ 0x20..=0x7e]) => Ok(*b),
        Some([b'^', b'?']) => Ok(0x7f),
        Some([b'^', b @ b'@'..=b'_']) => Ok(*b - b'@'),
        _ => Err(bad_value(value, "-e")),
    }
}
fn redraw(value: &OsString) -> CliResult<Redraw> {
    match value.to_str() {
        Some("none") => Ok(Redraw::None),
        Some("ctrl_l") => Ok(Redraw::CtrlL),
        Some("winch") => Ok(Redraw::Winch),
        _ => Err(bad_value(value, "-r")),
    }
}
fn reset(value: &OsString) -> CliResult<Reset> {
    match value.to_str() {
        Some("none") => Ok(Reset::None),
        Some("move") => Ok(Reset::Move),
        _ => Err(bad_value(value, "-R")),
    }
}
fn session(value: &OsStr) -> CliResult<OsString> {
    name::valid_session(value)
        .then(|| value.to_owned())
        .ok_or_else(|| Error(format!("Invalid session name '{}'", name::render(value))))
}

fn set_option(
    id: usize,
    value: Option<&OsString>,
    out: &mut Options,
    lines: &mut u32,
) -> CliResult<()> {
    match id {
        0 => out.detach = Some(parse_detach(value.unwrap())?),
        1 => out.detach = None,
        2 => out.redraw = redraw(value.unwrap())?,
        3 => out.reset = reset(value.unwrap())?,
        4..=6 => *[&mut out.pass_suspend, &mut out.quiet, &mut out.non_vt][id - 4] = true,
        7 => out.log_cap = parse_size(value.unwrap())?,
        8..=11 => {
            *[
                &mut out.stderr,
                &mut out.events,
                &mut out.instrument,
                &mut out.directory,
            ][id - 8] = value.map(Into::into)
        }
        12 | 13 => {}
        14 => {
            let value = value.unwrap();
            *lines = u32::try_from(number(value.to_str().unwrap_or(""), value, "-n")?)
                .map_err(|_| bad_value(value, "-n"))?
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn scan<'a>(
    args: &'a [OsString],
    command: &str,
    allowed: u16,
    creating: bool,
) -> CliResult<Scan<'a>> {
    let (mut options, mut seen, mut lines, mut operands) =
        (Options::default(), 0, 10, smallvec::SmallVec::new());
    let mut input = args.iter();
    while let Some(arg) = input.next() {
        if arg == "--" {
            let rest = input.as_slice();
            // OB-4 makes `--` a grammar terminator in every phase, so it may
            // introduce a dash-leading operand but never participates in the
            // operand count: a trailing `--` with nothing after it just ends
            // option recognition.
            return_if!(
                !creating && (rest.len() > 1 || !operands.is_empty() && !rest.is_empty()),
                Err(invalid_args())
            );
            operands.extend(rest);
            break;
        } else if leading_dash(arg) {
            let Some(id) = OPTIONS.iter().position(|option| arg == *option) else {
                return Err(invalid_mode(arg));
            };
            let bit = 1 << id;
            return_if!(allowed & bit == 0, Err(bad_option(arg, command)));
            seen |= bit;
            let value = if VALUES & bit == 0 {
                None
            } else {
                Some(input.next().ok_or_else(|| {
                    Error(format!("Option '{}' requires an argument", OPTIONS[id]))
                })?)
            };
            set_option(id, value, &mut options, &mut lines)?;
        } else {
            operands.push(arg);
            if creating && operands.len() == 2 {
                operands.extend(input);
                break;
            }
        }
    }
    return_if!(
        options.non_vt && options.reset == Reset::Move,
        Err(bad_value(&OsString::from("move"), "-R"))
    );
    Ok((options, seen, lines, operands))
}

fn fixed(
    args: &[OsString],
    command: &str,
    allowed: u16,
    required: bool,
) -> CliResult<(Options, u16, u32, Option<OsString>)> {
    let (options, seen, lines, mut operands) = scan(args, command, allowed, false)?;
    return_if!(
        operands.len() > 1 || required && operands.is_empty(),
        Err(invalid_args())
    );
    Ok((
        options,
        seen,
        lines,
        operands.pop().map(|value| session(value)).transpose()?,
    ))
}

fn flags(args: &[OsString], command: &str, allowed: u16) -> CliResult<u16> {
    let (_, seen, _, operands) = scan(args, command, allowed, false)?;
    operands.is_empty().then_some(seen).ok_or_else(invalid_args)
}

fn create(args: &[OsString], mode: CreateMode, command: &str) -> CliResult<Action> {
    let (options, _, _, operands) = scan(args, command, CREATE, true)?;
    let mut operands = operands.into_iter();
    let session = session(operands.next().ok_or_else(invalid_args)?)?;
    Ok(Action::Create {
        mode,
        session,
        command: operands.cloned().collect(),
        options,
    })
}

fn remove(args: &[OsString]) -> CliResult<Action> {
    let (options, seen, _, session) = fixed(args, "rm", ALL | 1 << 5, false)?;
    let all = seen & ALL != 0;
    return_if!(all == session.is_some(), Err(invalid_args()));
    Ok(Action::Remove {
        session,
        all,
        quiet: options.quiet,
    })
}

pub fn parse(args: &[OsString]) -> Result<Action, Error> {
    return_if!(args.len() == 1, Ok(Action::Help));
    let first = &args[1];
    let rest = &args[2..];
    match first.to_str() {
        Some(command @ ("new" | "n")) => create(rest, CreateMode::New, command),
        Some(command @ ("start" | "s")) => create(rest, CreateMode::Start, command),
        Some(command @ "run") => create(rest, CreateMode::Run, command),
        Some("--help" | "-h" | "?") if args.len() == 2 => Ok(Action::Help),
        Some("--version") if args.len() == 2 => Ok(Action::Version),
        Some("--help" | "-h" | "?" | "--version") => Err(invalid_args()),
        Some("attach" | "a") => {
            let (options, _, _, session) = fixed(rest, "attach", VIEW, true)?;
            Ok(Action::Attach {
                session: session.unwrap(),
                options,
            })
        }
        Some("push" | "p") => Ok(Action::Push(fixed(rest, "push", 0, true)?.3.unwrap())),
        Some("list" | "l" | "ls") => Ok(Action::List {
            all: flags(rest, "list", ALL)? & ALL != 0,
        }),
        Some("current") => flags(rest, "current", 0).map(|_| Action::Current),
        Some("clear") => Ok(Action::Clear(fixed(rest, "clear", 0, false)?.3)),
        Some("kill" | "k") => {
            let (options, seen, _, session) = fixed(rest, "kill", FORCE | 1 << 5, true)?;
            Ok(Action::Kill {
                session: session.unwrap(),
                force: seen & FORCE != 0,
                quiet: options.quiet,
            })
        }
        Some("rm") => remove(rest),
        Some("tail") => {
            let (_, seen, lines, session) = fixed(rest, "tail", FORCE | NUMBER, true)?;
            Ok(Action::Tail {
                session: session.unwrap(),
                follow: seen & FORCE != 0,
                lines,
            })
        }
        Some("--") => create(&args[1..], CreateMode::Bare, "bare"),
        _ if leading_dash(first) => Err(invalid_mode(first)),
        _ => create(&args[1..], CreateMode::Bare, "bare"),
    }
}

pub fn help(program: &str, version: &str) -> String {
    format!(
        "{program} {version}\nUsage:\n  {program} <session> [options] [command [argument...]]\n  {program} new|start|run [options] <session> [options] [command [argument...]]\n  {program} attach [options] <session>\n  {program} push <session>\n  {program} kill [-f] [-q] <session>\n  {program} rm [-q] <session> | {program} rm -a [-q]\n  {program} list [-a]\n  {program} current\n  {program} tail [-f] [-n N] <session>\n  {program} clear [<session>]\n\nAttach/create options:\n  -e <char>  detach byte (default ^\\)\n  -E         disable detach\n  -r <mode>  child redraw: none, ctrl_l, winch (default none)\n  -R <mode>  viewer reset: none, move (default none)\n  -z         pass ^Z to the child\n  -q         suppress informational messages\n  -t         viewer is not VT-compatible\n\nCreate-only options:\n  -C <size>  log cap (default 1m; 0 disables)\n  -2 <path>  redirect child standard error\n  -T <path>  event store directory\n  -S <path>  launch-time instrumentation object\n  -d <path>  child working directory\n"
    )
}
