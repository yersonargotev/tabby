use std::{env, process::ExitCode};

fn main() -> ExitCode {
    match tabby::parse_command(env::args().skip(1)) {
        Ok(command) => {
            println!("{}", tabby::run_stub(command).message);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
