pub mod herdr_client;
pub mod labeler;
pub mod locks;
pub mod stability;

use std::fmt;

pub const USAGE: &str = "Usage: tabby <daemon|start|unlock-focused|unlock-all>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Daemon,
    Start,
    UnlockFocused,
    UnlockAll,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    UnknownCommand(String),
    UnexpectedArgument { command: String, argument: String },
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand(command) => {
                write!(formatter, "unknown command `{command}`\n{USAGE}")
            }
            Self::UnexpectedArgument { command, argument } => write!(
                formatter,
                "unexpected argument `{argument}` for command `{command}`\n{USAGE}"
            ),
        }
    }
}

impl std::error::Error for CliError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandOutcome {
    pub message: &'static str,
}

pub fn parse_command<I, S>(args: I) -> Result<Command, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut args = args.into_iter().map(Into::into);
    let command = args.next().unwrap_or_else(|| "help".to_string());

    if let Some(argument) = args.next() {
        return Err(CliError::UnexpectedArgument { command, argument });
    }

    match command.as_str() {
        "daemon" => Ok(Command::Daemon),
        "start" => Ok(Command::Start),
        "unlock-focused" => Ok(Command::UnlockFocused),
        "unlock-all" => Ok(Command::UnlockAll),
        "help" | "--help" | "-h" => Ok(Command::Help),
        _ => Err(CliError::UnknownCommand(command)),
    }
}

pub fn run_stub(command: Command) -> CommandOutcome {
    let message = match command {
        Command::Daemon | Command::Start => "tabby daemon stub: rename loop is not implemented yet",
        Command::UnlockFocused => {
            "tabby unlock-focused stub: daemon state path wiring is not implemented yet"
        }
        Command::UnlockAll => {
            "tabby unlock-all stub: daemon state path wiring is not implemented yet"
        }
        Command::Help => USAGE,
    };

    CommandOutcome { message }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_daemon_and_start_commands() {
        assert_eq!(parse_command(["daemon"]), Ok(Command::Daemon));
        assert_eq!(parse_command(["start"]), Ok(Command::Start));
    }

    #[test]
    fn parses_unlock_commands() {
        assert_eq!(
            parse_command(["unlock-focused"]),
            Ok(Command::UnlockFocused)
        );
        assert_eq!(parse_command(["unlock-all"]), Ok(Command::UnlockAll));
    }

    #[test]
    fn defaults_to_help_without_arguments() {
        assert_eq!(parse_command(std::iter::empty::<&str>()), Ok(Command::Help));
        assert_eq!(run_stub(Command::Help).message, USAGE);
    }

    #[test]
    fn rejects_unknown_commands() {
        assert_eq!(
            parse_command(["rename-now"]),
            Err(CliError::UnknownCommand("rename-now".to_string()))
        );
    }

    #[test]
    fn rejects_extra_arguments() {
        assert_eq!(
            parse_command(["unlock-all", "now"]),
            Err(CliError::UnexpectedArgument {
                command: "unlock-all".to_string(),
                argument: "now".to_string(),
            })
        );
    }

    #[test]
    fn daemon_stub_does_not_claim_rename_logic_exists() {
        let outcome = run_stub(Command::Daemon);
        assert!(outcome.message.contains("not implemented yet"));
    }
}
