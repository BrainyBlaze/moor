use moor::{cli, name};

fn main() {
    #[cfg(windows)]
    if let Some(status) = moor::windows::bootstrap() {
        std::process::exit(status);
    }
    let mut args: Vec<_> = std::env::args_os().collect();
    let invoked = args.first_mut().map(std::mem::take).unwrap_or_default();
    let program = name::program(&invoked);
    let parsed = cli::parse(&args);
    drop(args);
    match parsed {
        Ok(cli::Action::Help) => print!("{}", cli::help(&program, env!("CARGO_PKG_VERSION"))),
        Ok(cli::Action::Version) => println!("{program} {}", env!("CARGO_PKG_VERSION")),
        Ok(action) => std::process::exit(moor::runtime::client::run(action, &invoked, &program)),
        Err(error) => {
            print!(
                "{program}: {}\nTry '{program} --help' for more information.\n",
                error.0
            );
            std::process::exit(1);
        }
    }
}
