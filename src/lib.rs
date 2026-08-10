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

pub const USAGE: &str = "Usage: tabby <status|refresh|start|ensure-started|signal-focus|signal-created|install|unlock-focused|unlock-all|repair-state --discard|forget-session>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Status,
    Refresh,
    Start,
    EnsureStarted,
    SignalFocus,
    SignalCreated,
    Runtime { launch_id: String },
    Install,
    UnlockFocused,
    UnlockAll,
    RepairStateDiscard,
    ForgetSession,
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
    Install(install::InstallError),
    SessionRuntime(session_runtime::SessionRuntimeError),
    Status(status::StatusError),
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Install(error) => write!(formatter, "install failed: {error}"),
            Self::SessionRuntime(error) => write!(formatter, "session runtime failed: {error}"),
            Self::Status(error) => write!(formatter, "status failed: {error}"),
        }
    }
}

impl std::error::Error for CommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Install(error) => Some(error),
            Self::SessionRuntime(error) => Some(error),
            Self::Status(error) => Some(error),
        }
    }
}

impl From<install::InstallError> for CommandError {
    fn from(error: install::InstallError) -> Self {
        Self::Install(error)
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
        "install" => no_arguments(command, args, Command::Install),
        "repair-state" => {
            let argument = args.next().ok_or_else(|| CliError::UnexpectedArgument {
                command: command.clone(),
                argument: "<missing --discard>".to_string(),
            })?;
            if argument != "--discard" {
                return Err(CliError::UnexpectedArgument { command, argument });
            }
            if let Some(argument) = args.next() {
                return Err(CliError::UnexpectedArgument { command, argument });
            }
            Ok(Command::RepairStateDiscard)
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
        | "unlock-focused" | "unlock-all" | "forget-session" => match command.as_str() {
            "status" => no_arguments(command, args, Command::Status),
            "refresh" => no_arguments(command, args, Command::Refresh),
            "start" => no_arguments(command, args, Command::Start),
            "ensure-started" => no_arguments(command, args, Command::EnsureStarted),
            "signal-focus" => no_arguments(command, args, Command::SignalFocus),
            "signal-created" => no_arguments(command, args, Command::SignalCreated),
            "unlock-focused" => no_arguments(command, args, Command::UnlockFocused),
            "unlock-all" => no_arguments(command, args, Command::UnlockAll),
            "forget-session" => no_arguments(command, args, Command::ForgetSession),
            _ => unreachable!(),
        },
        "help" | "--help" | "-h" => Ok(Command::Help),
        _ => Err(CliError::UnknownCommand(command)),
    }
}

fn no_arguments<S>(
    command: String,
    mut args: impl Iterator<Item = S>,
    parsed: Command,
) -> Result<Command, CliError>
where
    S: Into<String>,
{
    if let Some(argument) = args.next() {
        return Err(CliError::UnexpectedArgument {
            command,
            argument: argument.into(),
        });
    }
    Ok(parsed)
}

pub fn run_stub(command: Command) -> CommandOutcome {
    let message = match command {
        Command::Status => "tabby status runtime: use run_command for read-only diagnostics",
        Command::Refresh => {
            "tabby refresh runtime: use run_command to deliver a manual trigger to the Session Runtime"
        }
        Command::Start => {
            "tabby start runtime: use run_command to ensure one Ready Session Runtime"
        }
        Command::EnsureStarted => {
            "tabby ensure-started runtime: use run_command to ensure one Ready Session Runtime"
        }
        Command::SignalFocus => "tabby signal-focus runtime: deliver a focus trigger",
        Command::SignalCreated => "tabby signal-created runtime: deliver a creation trigger",
        Command::Runtime { .. } => "tabby internal Session Runtime",
        Command::Install => {
            "tabby install runtime: use run_command to relink the Herdr plugin and ensure the Session Runtime"
        }
        Command::UnlockFocused => {
            "tabby unlock-focused runtime: use run_command to request a control operation from the Session Runtime"
        }
        Command::UnlockAll => {
            "tabby unlock-all runtime: use run_command to request a control operation from the Session Runtime"
        }
        Command::RepairStateDiscard => {
            "tabby repair-state runtime: use run_command to discard invalid Session-Scoped Tab State"
        }
        Command::ForgetSession => {
            "tabby forget-session runtime: use run_command to remove Session-Scoped Tab State"
        }
        Command::Help => USAGE,
    };

    CommandOutcome { message }
}

fn runtime_trigger(command: &Command) -> Option<session_runtime::RefreshTrigger> {
    match command {
        Command::Refresh => Some(session_runtime::RefreshTrigger::Manual),
        Command::Start | Command::EnsureStarted => Some(session_runtime::RefreshTrigger::Startup),
        Command::SignalFocus => Some(session_runtime::RefreshTrigger::Focus),
        Command::SignalCreated => Some(session_runtime::RefreshTrigger::Creation),
        Command::Status
        | Command::Runtime { .. }
        | Command::Install
        | Command::UnlockFocused
        | Command::UnlockAll
        | Command::RepairStateDiscard
        | Command::ForgetSession
        | Command::Help => None,
    }
}

fn signal_runtime_trigger_from_env(
    trigger: session_runtime::RefreshTrigger,
) -> Result<String, session_runtime::SessionRuntimeError> {
    match trigger {
        session_runtime::RefreshTrigger::Manual => {
            session_runtime::signal_manual_refresh_from_env()
        }
        trigger => session_runtime::ensure_ready_owner_from_env(trigger),
    }
}

pub fn run_command(command: Command) -> Result<String, CommandError> {
    if let Some(trigger) = runtime_trigger(&command) {
        return signal_runtime_trigger_from_env(trigger).map_err(CommandError::from);
    }

    match command {
        Command::Status => status::run_from_env().map_err(CommandError::from),
        Command::Runtime { launch_id } => {
            session_runtime::run_owned_session_from_env(launch_id).map_err(CommandError::from)
        }
        Command::Install => {
            let install_message = install::relink_from_current_exe()?;
            let runtime_message = session_runtime::ensure_current_runtime_after_install_from_env()?;
            Ok(format!("{install_message}\n{runtime_message}"))
        }
        Command::UnlockFocused => {
            session_runtime::request_unlock_focused_from_env().map_err(CommandError::from)
        }
        Command::UnlockAll => {
            session_runtime::request_unlock_all_from_env().map_err(CommandError::from)
        }
        Command::RepairStateDiscard => {
            session_runtime::repair_session_state_from_env().map_err(CommandError::from)
        }
        Command::ForgetSession => {
            session_runtime::forget_session_from_env().map_err(CommandError::from)
        }
        Command::Help => Ok(USAGE.to_string()),
        Command::Refresh
        | Command::Start
        | Command::EnsureStarted
        | Command::SignalFocus
        | Command::SignalCreated => unreachable!("runtime ingress handled before dispatch"),
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
        assert_eq!(parse_command(["install"]), Ok(Command::Install));
    }

    #[test]
    fn rejects_legacy_install_start_argument() {
        assert_eq!(
            parse_command(["install", "--start"]),
            Err(CliError::UnexpectedArgument {
                command: "install".to_string(),
                argument: "--start".to_string(),
            })
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
    fn parses_state_repair_and_forget_session_commands() {
        assert_eq!(
            parse_command(["repair-state", "--discard"]),
            Ok(Command::RepairStateDiscard)
        );
        assert_eq!(
            parse_command(["forget-session"]),
            Ok(Command::ForgetSession)
        );
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
        assert!(outcome.message.contains("manual trigger"));
    }

    #[test]
    fn refresh_enters_the_session_runtime_as_a_manual_trigger() {
        assert_eq!(
            runtime_trigger(&Command::Refresh),
            Some(session_runtime::RefreshTrigger::Manual)
        );
    }
}
