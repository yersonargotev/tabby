pub mod daemon;
pub mod herdr_client;
pub mod install;
pub mod labeler;
pub mod locks;
pub mod paths;
pub mod stability;

use std::fmt;

pub const USAGE: &str = "Usage: tabby <daemon|start|install|unlock-focused|unlock-all>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Daemon,
    Start,
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

    if let Some(argument) = args.next() {
        return Err(CliError::UnexpectedArgument { command, argument });
    }

    match command.as_str() {
        "daemon" => Ok(Command::Daemon),
        "start" => Ok(Command::Start),
        "install" => Ok(Command::Install),
        "unlock-focused" => Ok(Command::UnlockFocused),
        "unlock-all" => Ok(Command::UnlockAll),
        "help" | "--help" | "-h" => Ok(Command::Help),
        _ => Err(CliError::UnknownCommand(command)),
    }
}

pub fn run_stub(command: Command) -> CommandOutcome {
    let message = match command {
        Command::Daemon | Command::Start => {
            "tabby daemon runtime: use run_command to start the rename loop"
        }
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
        Command::Daemon | Command::Start => {
            daemon::run_daemon_loop_from_env()?;
            Ok("tabby daemon stopped".to_string())
        }
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
    fn parses_daemon_and_start_commands() {
        assert_eq!(parse_command(["daemon"]), Ok(Command::Daemon));
        assert_eq!(parse_command(["start"]), Ok(Command::Start));
    }

    #[test]
    fn parses_install_command() {
        assert_eq!(parse_command(["install"]), Ok(Command::Install));
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
    fn daemon_stub_points_to_runtime_command() {
        let outcome = run_stub(Command::Daemon);
        assert!(outcome.message.contains("run_command"));
    }
}
