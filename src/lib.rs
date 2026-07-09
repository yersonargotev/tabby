pub mod daemon;
pub mod herdr_client;
pub mod install;
pub mod labeler;
pub mod locks;
pub mod paths;
pub mod stability;

use std::fmt;

pub const USAGE: &str = "Usage: tabby <refresh|install|unlock-focused|unlock-all>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Refresh,
    Install,
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

#[derive(Debug)]
pub enum CommandError {
    Runtime(daemon::RuntimeError),
    Install(install::InstallError),
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => write!(formatter, "{error}"),
            Self::Install(error) => write!(formatter, "install failed: {error}"),
        }
    }
}

impl std::error::Error for CommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Runtime(error) => Some(error),
            Self::Install(error) => Some(error),
        }
    }
}

impl From<daemon::RuntimeError> for CommandError {
    fn from(error: daemon::RuntimeError) -> Self {
        Self::Runtime(error)
    }
}

impl From<install::InstallError> for CommandError {
    fn from(error: install::InstallError) -> Self {
        Self::Install(error)
    }
}

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

    match command.as_str() {
        "refresh" | "install" | "unlock-focused" | "unlock-all" => {
            if let Some(argument) = args.next() {
                return Err(CliError::UnexpectedArgument { command, argument });
            }
            match command.as_str() {
                "refresh" => Ok(Command::Refresh),
                "install" => Ok(Command::Install),
                "unlock-focused" => Ok(Command::UnlockFocused),
                "unlock-all" => Ok(Command::UnlockAll),
                _ => unreachable!(),
            }
        }
        "help" | "--help" | "-h" => Ok(Command::Help),
        _ => Err(CliError::UnknownCommand(command)),
    }
}

pub fn run_stub(command: Command) -> CommandOutcome {
    let message = match command {
        Command::Refresh => "tabby refresh runtime: use run_command for a one-shot label refresh",
        Command::Install => "tabby install runtime: use run_command to relink the Herdr plugin",
        Command::UnlockFocused => {
            "tabby unlock-focused runtime: use run_command with injected state path"
        }
        Command::UnlockAll => "tabby unlock-all runtime: use run_command with injected state path",
        Command::Help => USAGE,
    };

    CommandOutcome { message }
}

pub fn run_command(command: Command) -> Result<String, CommandError> {
    match command {
        Command::Refresh => daemon::run_one_shot_refresh_from_env().map_err(CommandError::from),
        Command::Install => install::relink_from_current_exe().map_err(CommandError::from),
        Command::UnlockFocused => daemon::unlock_focused_from_env().map_err(CommandError::from),
        Command::UnlockAll => daemon::unlock_all_from_env().map_err(CommandError::from),
        Command::Help => Ok(USAGE.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_refresh_command() {
        assert_eq!(parse_command(["refresh"]), Ok(Command::Refresh));
    }

    #[test]
    fn parses_install_command() {
        assert_eq!(parse_command(["install"]), Ok(Command::Install));
    }

    #[test]
    fn rejects_removed_daemon_startup_commands() {
        assert_eq!(
            parse_command(["ensure-started"]),
            Err(CliError::UnknownCommand("ensure-started".to_string()))
        );
        assert_eq!(
            parse_command(["start"]),
            Err(CliError::UnknownCommand("start".to_string()))
        );
        assert_eq!(
            parse_command(["daemon"]),
            Err(CliError::UnknownCommand("daemon".to_string()))
        );
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
        assert_eq!(
            parse_command(["install", "--start"]),
            Err(CliError::UnexpectedArgument {
                command: "install".to_string(),
                argument: "--start".to_string(),
            })
        );
    }

    #[test]
    fn refresh_stub_points_to_runtime_command() {
        let outcome = run_stub(Command::Refresh);
        assert!(outcome.message.contains("one-shot label refresh"));
    }
}
