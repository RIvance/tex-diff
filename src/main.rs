mod app;
mod cli;
mod doctor;
mod git;
mod output;
mod viewer;
mod worker;

use clap::Parser;
use cli::Cli;
use std::process::ExitCode;

fn main() -> ExitCode {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == worker::COMMAND)
    {
        return match worker::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("LaTeX source comparison: {error:#}");
                ExitCode::from(2)
            }
        };
    }

    let cli = Cli::parse();
    match app::run(&cli) {
        Ok(changed) => ExitCode::from(u8::from(cli.exit_code && changed)),
        Err(error) => {
            eprintln!("tex-diff: {error:#}");
            ExitCode::from(2)
        }
    }
}
