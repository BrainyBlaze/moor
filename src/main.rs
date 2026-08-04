use moor::{cli, name};

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    let invoked = args.first().cloned().unwrap_or_default();
    let program = name::program(args.first().map(|s| s.as_os_str()).unwrap_or_default());
    match cli::parse(args) {
        Ok(cli::Action::Help) => print!("{}", cli::help(&program, env!("CARGO_PKG_VERSION"))),
        Ok(cli::Action::Version) => println!("{program} {}", env!("CARGO_PKG_VERSION")),
        #[cfg(unix)]
        Ok(action) => std::process::exit(moor::unix::run(action, &invoked, &program)),
        #[cfg(windows)]
        Ok(action) => std::process::exit(moor::windows::run(action, &invoked, &program)),
        Err(error) => {
            print!(
                "{program}: {}\nTry '{program} --help' for more information.\n",
                error.0
            );
            std::process::exit(1);
        }
    }
}
