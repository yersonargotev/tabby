use std::{env, process::ExitCode};

fn main() -> ExitCode {
    match tabby::parse_command(env::args().skip(1)) {
        Ok(command) => match tabby::run_command(command) {
            Ok(message) => {
                println!("{message}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
