pub mod daemon;
pub mod herdr_client;
pub mod install;
pub mod labeler;
pub mod locks;
pub mod paths;
pub mod session_runtime;
pub mod stability;
pub mod startup;
pub mod status;

use std::fmt;

pub const USAGE: &str = "Usage: tabby <status|refresh|start|ensure-started|signal-focus|signal-created|install [--start]|unlock-focused|unlock-all>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Status,
    Refresh,
    Start,
    EnsureStarted,
    SignalFocus,
    SignalCreated,
    Runtime { launch_id: String },
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
    SessionRuntime(session_runtime::SessionRuntimeError),
    Status(status::StatusError),
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => write!(formatter, "{error}"),
            Self::Install(error) => write!(formatter, "install failed: {error}"),
            Self::Startup(error) => write!(formatter, "startup failed: {error}"),
            Self::SessionRuntime(error) => write!(formatter, "session runtime failed: {error}"),
            Self::Status(error) => write!(formatter, "status failed: {error}"),
        }
    }
}

impl std::error::Error for CommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Runtime(error) => Some(error),
            Self::Install(error) => Some(error),
            Self::Startup(error) => Some(error),
            Self::SessionRuntime(error) => Some(error),
            Self::Status(error) => Some(error),
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

impl From<session_runtime::SessionRuntimeError> for CommandError {
    fn from(error: session_runtime::SessionRuntimeError) -> Self {
        Self::SessionRuntime(error)
    }
}

impl From<status::StatusError> for CommandError {
    fn from(error: status::StatusError) -> Self {
        Self::Status(error)
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
        "runtime" => {
            let flag = args.next().ok_or_else(|| CliError::UnexpectedArgument {
                command: command.clone(),
                argument: "<missing --launch-id>".to_string(),
            })?;
            if flag != "--launch-id" {
                return Err(CliError::UnexpectedArgument {
                    command,
                    argument: flag,
                });
            }
            let launch_id = args.next().ok_or_else(|| CliError::UnexpectedArgument {
                command: command.clone(),
                argument: "<missing launch id>".to_string(),
            })?;
            if let Some(argument) = args.next() {
                return Err(CliError::UnexpectedArgument { command, argument });
            }
            Ok(Command::Runtime { launch_id })
        }
        "status" | "refresh" | "start" | "ensure-started" | "signal-focus" | "signal-created"
        | "unlock-focused" | "unlock-all" => {
            if let Some(argument) = args.next() {
                return Err(CliError::UnexpectedArgument { command, argument });
            }
            match command.as_str() {
                "status" => Ok(Command::Status),
                "refresh" => Ok(Command::Refresh),
                "start" => Ok(Command::Start),
                "ensure-started" => Ok(Command::EnsureStarted),
                "signal-focus" => Ok(Command::SignalFocus),
                "signal-created" => Ok(Command::SignalCreated),
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
        Command::Status => "tabby status runtime: use run_command for read-only diagnostics",
        Command::Refresh => "tabby refresh runtime: use run_command for a one-shot label refresh",
        Command::Start => {
            "tabby start runtime: use run_command to ensure one Ready Session Runtime"
        }
        Command::EnsureStarted => {
            "tabby ensure-started runtime: use run_command to ensure one Ready Session Runtime"
        }
        Command::SignalFocus => "tabby signal-focus runtime: deliver a focus trigger",
        Command::SignalCreated => "tabby signal-created runtime: deliver a creation trigger",
        Command::Runtime { .. } => "tabby internal Session Runtime",
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
        Command::Status => status::run_from_env().map_err(CommandError::from),
        Command::Refresh => daemon::run_one_shot_refresh_from_env().map_err(CommandError::from),
        Command::Start | Command::EnsureStarted => {
            session_runtime::ensure_ready_owner_from_env(session_runtime::RefreshTrigger::Startup)
                .map_err(CommandError::from)
        }
        Command::SignalFocus => {
            session_runtime::ensure_ready_owner_from_env(session_runtime::RefreshTrigger::Focus)
                .map_err(CommandError::from)
        }
        Command::SignalCreated => {
            session_runtime::ensure_ready_owner_from_env(session_runtime::RefreshTrigger::Creation)
                .map_err(CommandError::from)
        }
        Command::Runtime { launch_id } => {
            session_runtime::run_owned_session_from_env(launch_id).map_err(CommandError::from)
        }
        Command::Install { start } => {
            let install_message = install::relink_from_current_exe()?;
            if start {
                let startup_message = session_runtime::ensure_ready_owner_from_env(
                    session_runtime::RefreshTrigger::Startup,
                )?;
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
    fn parses_refresh_start_and_ensure_started_commands() {
        assert_eq!(parse_command(["status"]), Ok(Command::Status));
        assert_eq!(parse_command(["refresh"]), Ok(Command::Refresh));
        assert_eq!(parse_command(["start"]), Ok(Command::Start));
        assert_eq!(
            parse_command(["ensure-started"]),
            Ok(Command::EnsureStarted)
        );
        assert_eq!(parse_command(["signal-focus"]), Ok(Command::SignalFocus));
        assert_eq!(
            parse_command(["signal-created"]),
            Ok(Command::SignalCreated)
        );
        assert_eq!(
            parse_command(["runtime", "--launch-id", "launch-1"]),
            Ok(Command::Runtime {
                launch_id: "launch-1".to_string()
            })
        );
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
            parse_command(["daemon"]),
            Err(CliError::UnknownCommand("daemon".to_string()))
        );
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
            parse_command(["refresh", "now"]),
            Err(CliError::UnexpectedArgument {
                command: "refresh".to_string(),
                argument: "now".to_string(),
            })
        );
    }

    #[test]
    fn refresh_stub_points_to_runtime_command() {
        let outcome = run_stub(Command::Refresh);
        assert!(outcome.message.contains("one-shot label refresh"));
    }
}
