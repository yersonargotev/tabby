pub mod daemon;
pub mod herdr_client;
pub mod install;
pub mod labeler;
pub mod locks;
pub mod paths;
pub mod stability;
pub mod startup;

use std::fmt;

pub const USAGE: &str =
    "Usage: tabby <daemon|start|ensure-started|install [--start]|unlock-focused|unlock-all>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Daemon,
    Start,
    EnsureStarted,
    Install { start: bool },
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
    Startup(startup::StartupError),
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => write!(formatter, "{error}"),
            Self::Install(error) => write!(formatter, "install failed: {error}"),
            Self::Startup(error) => write!(formatter, "startup failed: {error}"),
        }
    }
}

impl std::error::Error for CommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Runtime(error) => Some(error),
            Self::Install(error) => Some(error),
            Self::Startup(error) => Some(error),
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

impl From<startup::StartupError> for CommandError {
    fn from(error: startup::StartupError) -> Self {
        Self::Startup(error)
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
        "install" => {
            let start = match args.next() {
                Some(argument) if argument == "--start" => true,
                Some(argument) => return Err(CliError::UnexpectedArgument { command, argument }),
                None => false,
            };
            if let Some(argument) = args.next() {
                return Err(CliError::UnexpectedArgument { command, argument });
            }
            Ok(Command::Install { start })
        }
        "daemon" | "start" | "ensure-started" | "unlock-focused" | "unlock-all" => {
            if let Some(argument) = args.next() {
                return Err(CliError::UnexpectedArgument { command, argument });
            }
            match command.as_str() {
                "daemon" => Ok(Command::Daemon),
                "start" => Ok(Command::Start),
                "ensure-started" => Ok(Command::EnsureStarted),
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
        Command::Daemon | Command::Start => {
            "tabby daemon runtime: use run_command to start the rename loop"
        }
        Command::EnsureStarted => {
            "tabby ensure-started runtime: use run_command to start one Tabby Session Daemon"
        }
        Command::Install { .. } => {
            "tabby install runtime: use run_command to relink the Herdr plugin"
        }
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
        Command::EnsureStarted => startup::ensure_started_from_env().map_err(CommandError::from),
        Command::Install { start } => {
            let install_message = install::relink_from_current_exe()?;
            if start {
                let startup_message = startup::ensure_started_from_env()?;
                Ok(format!("{install_message}\n{startup_message}"))
            } else {
                Ok(install_message)
            }
        }
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
        assert_eq!(
            parse_command(["install"]),
            Ok(Command::Install { start: false })
        );
    }

    #[test]
    fn parses_install_start_command() {
        assert_eq!(
            parse_command(["install", "--start"]),
            Ok(Command::Install { start: true })
        );
    }

    #[test]
    fn parses_ensure_started_command() {
        assert_eq!(
            parse_command(["ensure-started"]),
            Ok(Command::EnsureStarted)
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
            parse_command(["install", "--start", "again"]),
            Err(CliError::UnexpectedArgument {
                command: "install".to_string(),
                argument: "again".to_string(),
            })
        );
    }

    #[test]
    fn daemon_stub_points_to_runtime_command() {
        let outcome = run_stub(Command::Daemon);
        assert!(outcome.message.contains("run_command"));
    }
}
