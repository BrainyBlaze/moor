use moor::{cli, name};
use std::io::Write;

fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    let program = name::program(args.first().map(|s| s.as_os_str()).unwrap_or_default());
    match cli::parse(args) {
        Ok(cli::Action::Help) => print!("{}", cli::help(&program, env!("CARGO_PKG_VERSION"))),
        Ok(cli::Action::Version) => println!("{program} {}", env!("CARGO_PKG_VERSION")),
        Ok(_) => {
            let _ = writeln!(std::io::stderr(), "{program}: runtime not implemented");
            std::process::exit(125);
        }
        Err(error) => {
            print!(
                "{program}: {}\nTry '{program} --help' for more information.\n",
                error.0
            );
            std::process::exit(1);
        }
    }
}
