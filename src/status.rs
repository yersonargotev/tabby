use crate::herdr_client::{HerdrApi, HerdrClient, HerdrError, UnixSocketTransport};
use crate::labeler::LabelPolicy;
use crate::locks::{SessionTabStateInspection, SessionTabStateStore};
use crate::paths::PLUGIN_ID;
use crate::session_runtime::{self, RuntimeInspection, SessionRuntimeError};
use crate::startup::{self, SessionSocket, StartupError};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRegistration {
    pub enabled: bool,
    pub manifest_path: PathBuf,
    pub command_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusedTabInspection {
    pub workspace_id: String,
    pub tab_id: String,
    pub number: Option<u64>,
    pub label: String,
    pub pane_id: Option<String>,
    pub cwd: Option<String>,
    pub candidate_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentAction {
    pub command: String,
    pub status: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSnapshot {
    pub session_name: Option<String>,
    pub socket_path: PathBuf,
    pub current_binary: PathBuf,
    pub plugin: Option<PluginRegistration>,
    pub runtime: RuntimeInspection,
    pub focused_tab: Option<FocusedTabInspection>,
    pub tab_state: SessionTabStateInspection,
    pub recent_actions: Vec<RecentAction>,
}

pub fn run_from_env() -> Result<String, StatusError> {
    collect_from_env().map(|snapshot| render_status(&snapshot))
}

pub fn render_status(snapshot: &StatusSnapshot) -> String {
    let session = snapshot.session_name.as_deref().unwrap_or("<unknown>");
    let mut lines = vec![
        format!("Tabby status for Herdr Session {session}"),
        format!("Socket: {}", snapshot.socket_path.display()),
        format!(
            "Session Identity: {}",
            SessionSocket::resolve(&snapshot.socket_path)
                .map(|socket| socket.identity_hex())
                .unwrap_or_else(|_| "<unresolved>".to_string())
        ),
        format!("Current executable: {}", snapshot.current_binary.display()),
    ];

    match &snapshot.plugin {
        Some(plugin) => {
            let state = if plugin.enabled {
                "enabled"
            } else {
                "disabled"
            };
            lines.push(format!(
                "Plugin: {state}, {}",
                plugin.manifest_path.display()
            ));
            let commands = if plugin.command_paths.is_empty() {
                "<none>".to_string()
            } else {
                plugin
                    .command_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            lines.push(format!("Commands: {commands}"));
            lines.push(format!("Registered command binary: {commands}"));
        }
        None => lines.push(format!("Plugin: {PLUGIN_ID} is not registered")),
    }

    render_runtime(&mut lines, &snapshot.runtime);

    match &snapshot.focused_tab {
        Some(tab) => {
            let number = tab
                .number
                .map(|number| number.to_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            lines.push(format!(
                "Focused tab: {} workspace={} number={number} label={}",
                tab.tab_id, tab.workspace_id, tab.label
            ));
            let pane = tab.pane_id.as_deref().unwrap_or("<none>");
            let cwd = tab.cwd.as_deref().unwrap_or("<unknown>");
            let candidate = tab.candidate_label.as_deref().unwrap_or("<none>");
            lines.push(format!(
                "Focused pane: {pane} cwd={cwd} candidate={candidate}"
            ));
        }
        None => lines.push("Focused tab: <none>".to_string()),
    }

    render_tab_state(&mut lines, &snapshot.tab_state);

    let failed_actions = snapshot
        .recent_actions
        .iter()
        .filter(|action| action.status != "succeeded")
        .count();
    let lock_skips = snapshot
        .recent_actions
        .iter()
        .filter(|action| action_mentions_lock_skip(action))
        .count();
    if failed_actions == 0 && lock_skips == 0 {
        lines.push(format!(
            "Recent plugin actions: {} inspected, no failures or lock skips",
            snapshot.recent_actions.len()
        ));
    } else {
        lines.push(format!(
            "Recent plugin actions: {} inspected, {failed_actions} failures, {lock_skips} lock skips",
            snapshot.recent_actions.len()
        ));
    }

    let (warnings, fixes) = warnings_and_fixes(snapshot);
    if warnings.is_empty() {
        lines.push("Warnings: none".to_string());
    } else {
        lines.push("Warnings:".to_string());
        lines.extend(warnings.into_iter().map(|warning| format!("- {warning}")));
    }
    if !fixes.is_empty() {
        lines.push("Suggested fixes:".to_string());
        lines.extend(fixes.into_iter().map(|fix| format!("- {fix}")));
    }

    lines.join("\n")
}

fn render_tab_state(lines: &mut Vec<String>, tab_state: &SessionTabStateInspection) {
    match tab_state {
        SessionTabStateInspection::Missing => lines.push(
            "State: 0 Manually Locked Tabs, 0 baselines, 0 unresolved rename intents (not yet persisted)"
                .to_string(),
        ),
        SessionTabStateInspection::Valid {
            manual_locks,
            baselines,
            unresolved_rename_intents,
        } => lines.push(format!(
            "State: {manual_locks} Manually Locked Tabs, {baselines} baselines, {unresolved_rename_intents} unresolved rename intents"
        )),
        SessionTabStateInspection::Fault { diagnostic } => lines.push(format!(
            "State: unavailable (State Integrity Fault: {diagnostic})"
        )),
    }
}

fn render_runtime(lines: &mut Vec<String>, runtime: &RuntimeInspection) {
    match runtime {
        RuntimeInspection::Absent => lines.push("Session Runtime: Absent".to_string()),
        RuntimeInspection::Starting { lease_held } => {
            lines.push(format!("Session Runtime: Starting lease_held={lease_held}"))
        }
        RuntimeInspection::Ready {
            pid,
            launch_id,
            version,
            binary_path,
            lease_held,
            last_evaluation_unix_ms,
            last_failure,
            next_periodic_unix_ms,
            config_path,
            config_schema_version,
            config_source,
            selected_profile,
            latest_config_error,
        } => {
            lines.push(format!(
                "Session Runtime: Ready pid={pid} version={version} lease_held={lease_held}"
            ));
            lines.push(format!(
                "Configuration: path={} active_schema_version={} active_source={} selected_profile={} policy_source={} latest_error={}",
                config_path.display(),
                config_schema_version
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "<none>".to_string()),
                config_source.as_deref().unwrap_or("<none>"),
                config_schema_version
                    .map(|_| selected_profile.as_deref().unwrap_or("global"))
                    .unwrap_or("<none>"),
                config_schema_version
                    .map(|_| selected_profile.as_deref().map(|profile| format!("profile:{profile}")).unwrap_or_else(|| "global".to_string()))
                    .unwrap_or_else(|| "<none>".to_string()),
                latest_config_error.as_deref().unwrap_or("<none>"),
            ));
            lines.push(format!("Ready owner binary: {}", binary_path.display()));
            lines.push(format!(
                "Session Runtime details: launch_id={launch_id} binary={} last_evaluation_unix_ms={} next_periodic_unix_ms={} last_failure={}",
                binary_path.display(),
                last_evaluation_unix_ms.map(|value| value.to_string()).unwrap_or_else(|| "<none>".to_string()),
                next_periodic_unix_ms.map(|value| value.to_string()).unwrap_or_else(|| "<none>".to_string()),
                last_failure.as_deref().unwrap_or("<none>"),
            ));
        }
        RuntimeInspection::Faulted {
            diagnostic,
            lease_held,
            config_path,
            config_schema_version,
            config_source,
            selected_profile,
            latest_config_error,
        } => {
            lines.push(format!(
                "Session Runtime: Faulted lease_held={lease_held} diagnostic={diagnostic}"
            ));
            lines.push(format!(
                "Configuration: path={} active_schema_version={} active_source={} selected_profile={} policy_source={} latest_error={}",
                config_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "<unresolved>".to_string()),
                config_schema_version
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "<none>".to_string()),
                config_source.as_deref().unwrap_or("<none>"),
                config_schema_version
                    .map(|_| selected_profile.as_deref().unwrap_or("global"))
                    .unwrap_or("<none>"),
                config_schema_version
                    .map(|_| selected_profile.as_deref().map(|profile| format!("profile:{profile}")).unwrap_or_else(|| "global".to_string()))
                    .unwrap_or_else(|| "<none>".to_string()),
                latest_config_error.as_deref().unwrap_or("<none>"),
            ));
        }
    }
}

fn warnings_and_fixes(snapshot: &StatusSnapshot) -> (Vec<String>, BTreeSet<String>) {
    let mut warnings = Vec::new();
    let mut fixes = BTreeSet::new();

    match &snapshot.plugin {
        None => {
            warnings.push(format!(
                "plugin {PLUGIN_ID} is not registered in this Herdr Session"
            ));
            fixes
                .insert("run `tabby install` to refresh the Herdr plugin registration".to_string());
        }
        Some(plugin) if !plugin.enabled => {
            warnings.push(format!("plugin {PLUGIN_ID} is registered but disabled"));
        }
        Some(_) => {}
    }

    match &snapshot.runtime {
        RuntimeInspection::Absent => {
            warnings.push("Session Runtime is absent".to_string());
            fixes.insert("run `tabby ensure-started` for this Herdr Session".to_string());
        }
        RuntimeInspection::Starting { .. } => {
            warnings.push("Session Runtime is starting but not Ready".to_string());
        }
        RuntimeInspection::Faulted {
            diagnostic,
            latest_config_error,
            ..
        } => {
            warnings.push(format!("Session Runtime is Faulted: {diagnostic}"));
            if latest_config_error.is_some() {
                fixes.insert("run `tabby ensure-started` for this Herdr Session".to_string());
            }
        }
        RuntimeInspection::Ready { binary_path, .. } => {
            let binary_identity = startup::binary_identity(binary_path);
            let mut binary_mismatch = false;
            if binary_identity != snapshot.current_binary {
                binary_mismatch = true;
                warnings.push(format!(
                    "Session Runtime binary {} does not match current executable {}",
                    binary_path.display(),
                    snapshot.current_binary.display()
                ));
            }
            if let Some(plugin) = &snapshot.plugin
                && !plugin.command_paths.is_empty()
                && !plugin
                    .command_paths
                    .iter()
                    .any(|path| path == &binary_identity)
            {
                binary_mismatch = true;
                warnings.push(format!(
                    "Session Runtime binary {} does not match registered command {}",
                    binary_path.display(),
                    plugin
                        .command_paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if binary_mismatch {
                fixes.insert(format!(
                    "run `herdr plugin action invoke start --plugin {PLUGIN_ID}` to activate the registered Tabby binary"
                ));
            }
        }
    }

    if let SessionTabStateInspection::Fault { diagnostic } = &snapshot.tab_state {
        warnings.push(format!("State Integrity Fault: {diagnostic}"));
        fixes.insert(
            "run `tabby repair-state --discard` to preserve evidence and discard invalid Session-Scoped Tab State"
                .to_string(),
        );
    }

    for action in &snapshot.recent_actions {
        if action.status != "succeeded" {
            warnings.push(format!(
                "recent plugin action failed: {} ({}){}",
                action.command,
                action.status,
                action_error_suffix(action)
            ));
        }
        for outcome in matching_lock_skip_outcomes(action) {
            warnings.push(format!(
                "recent plugin action reported {outcome}: {}",
                action.command
            ));
        }
    }

    (warnings, fixes)
}

fn action_mentions_lock_skip(action: &RecentAction) -> bool {
    matching_lock_skip_outcomes(action).next().is_some()
}

fn matching_lock_skip_outcomes(action: &RecentAction) -> impl Iterator<Item = &'static str> + '_ {
    ["SkippedLocked", "SkippedManualLockCreated"]
        .into_iter()
        .filter(|outcome| action.stdout.contains(outcome) || action.stderr.contains(outcome))
}

fn action_error_suffix(action: &RecentAction) -> String {
    let detail = if !action.stderr.trim().is_empty() {
        action.stderr.trim()
    } else {
        action.stdout.trim()
    };
    if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    }
}

fn collect_from_env() -> Result<StatusSnapshot, StatusError> {
    let socket = startup::resolve_socket_from_env()?;
    let current_binary =
        startup::binary_identity(&std::env::current_exe().map_err(StatusError::CurrentExe)?);
    let plugin_list = run_herdr_json(&socket, &["plugin", "list", "--json"])?;
    let plugin = parse_plugin_registration(&plugin_list)?;
    let recent_actions = if plugin.is_some() {
        let logs = run_herdr_json(
            &socket,
            &[
                "plugin", "log", "list", "--plugin", PLUGIN_ID, "--limit", "10",
            ],
        )?;
        parse_recent_actions(&logs)
    } else {
        Vec::new()
    };

    let runtime = session_runtime::inspect_runtime_from_env()?;
    let focused_tab = inspect_focused_tab(&socket)?;
    let state_base = startup::state_base_from_runtime()?;
    let tab_state = SessionTabStateStore::inspect_read_only(&state_base, &socket);

    Ok(StatusSnapshot {
        session_name: session_name_from_socket(&socket.socket_path),
        socket_path: socket.socket_path,
        current_binary,
        plugin,
        runtime,
        focused_tab,
        tab_state,
        recent_actions,
    })
}

fn session_name_from_socket(socket_path: &Path) -> Option<String> {
    let session_dir = socket_path.parent()?;
    (session_dir.parent()?.file_name()?.to_str() == Some("sessions"))
        .then(|| session_dir.file_name()?.to_str().map(str::to_string))
        .flatten()
}

fn inspect_focused_tab(
    socket: &SessionSocket,
) -> Result<Option<FocusedTabInspection>, StatusError> {
    let transport = UnixSocketTransport::new(&socket.socket_path);
    let mut client = HerdrClient::new(transport);
    let Some(observation) = client.observe_focused_tab()? else {
        return Ok(None);
    };
    let tab = observation.tab;
    let pane = observation.pane;
    let process_info = pane
        .focused
        .then(|| client.pane_process_info(&pane.pane_id).ok())
        .flatten();
    let candidate_label = LabelPolicy::default()
        .candidate_for_pane(&pane, process_info.as_ref())
        .map(|candidate| candidate.label().to_string());

    Ok(Some(FocusedTabInspection {
        workspace_id: tab.workspace_id,
        tab_id: tab.tab_id,
        number: tab.number,
        label: tab.label,
        pane_id: Some(pane.pane_id),
        cwd: observation.working_directory,
        candidate_label,
    }))
}

fn run_herdr_json(socket: &SessionSocket, args: &[&str]) -> Result<Value, StatusError> {
    let output = Command::new("herdr")
        .args(args)
        .env("HERDR_SOCKET_PATH", &socket.socket_path)
        .output()
        .map_err(|source| StatusError::HerdrCommandIo {
            command: command_text(args),
            source,
        })?;
    if !output.status.success() {
        return Err(StatusError::HerdrCommandFailed {
            command: command_text(args),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    serde_json::from_slice(&output.stdout).map_err(|source| StatusError::HerdrCommandJson {
        command: command_text(args),
        source,
    })
}

fn command_text(args: &[&str]) -> String {
    std::iter::once("herdr")
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_plugin_registration(value: &Value) -> Result<Option<PluginRegistration>, StatusError> {
    let plugins = value
        .pointer("/result/plugins")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            StatusError::Protocol("plugin list has no result.plugins array".to_string())
        })?;
    let Some(plugin) = plugins
        .iter()
        .find(|plugin| plugin.get("plugin_id").and_then(Value::as_str) == Some(PLUGIN_ID))
    else {
        return Ok(None);
    };
    let manifest_path = required_string(plugin, "manifest_path")?;
    let plugin_root = required_string(plugin, "plugin_root")?;
    let mut command_paths = BTreeSet::new();
    for collection in ["actions", "events"] {
        if let Some(entries) = plugin.get(collection).and_then(Value::as_array) {
            for entry in entries {
                if let Some(command) = entry
                    .get("command")
                    .and_then(Value::as_array)
                    .and_then(|command| command.first())
                    .and_then(Value::as_str)
                {
                    command_paths.insert(command_path(Path::new(&plugin_root), command));
                }
            }
        }
    }
    Ok(Some(PluginRegistration {
        enabled: plugin
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        manifest_path: PathBuf::from(manifest_path),
        command_paths: command_paths.into_iter().collect(),
    }))
}

fn required_string(value: &Value, field: &str) -> Result<String, StatusError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| StatusError::Protocol(format!("plugin registration has no {field}")))
}

fn command_path(plugin_root: &Path, command: &str) -> PathBuf {
    let path = Path::new(command);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        plugin_root.join(path)
    };
    startup::binary_identity(&normalize_path(&path))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn parse_recent_actions(value: &Value) -> Vec<RecentAction> {
    value
        .pointer("/result/logs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|log| RecentAction {
            command: log
                .get("command")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
            status: log
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            stdout: log
                .get("stdout")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            stderr: log
                .get("stderr")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
        .collect()
}

#[derive(Debug)]
pub enum StatusError {
    CurrentExe(io::Error),
    Startup(StartupError),
    Herdr(HerdrError),
    SessionRuntime(SessionRuntimeError),
    HerdrCommandIo {
        command: String,
        source: io::Error,
    },
    HerdrCommandFailed {
        command: String,
        status: ExitStatus,
        stderr: String,
    },
    HerdrCommandJson {
        command: String,
        source: serde_json::Error,
    },
    Protocol(String),
}

impl fmt::Display for StatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentExe(error) => write!(
                formatter,
                "failed to locate current tabby executable: {error}"
            ),
            Self::Startup(error) => write!(
                formatter,
                "failed to resolve Herdr Session runtime inputs: {error}"
            ),
            Self::Herdr(error) => {
                write!(formatter, "failed to inspect focused Herdr state: {error}")
            }
            Self::SessionRuntime(error) => {
                write!(
                    formatter,
                    "failed to inspect Session Runtime state: {error}"
                )
            }
            Self::HerdrCommandIo { command, source } => {
                write!(formatter, "failed to run `{command}`: {source}")
            }
            Self::HerdrCommandFailed {
                command,
                status,
                stderr,
            } => write!(formatter, "`{command}` failed with {status}: {stderr}"),
            Self::HerdrCommandJson { command, source } => {
                write!(formatter, "`{command}` returned invalid JSON: {source}")
            }
            Self::Protocol(message) => {
                write!(formatter, "Herdr diagnostics protocol error: {message}")
            }
        }
    }
}

impl std::error::Error for StatusError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CurrentExe(error) => Some(error),
            Self::Startup(error) => Some(error),
            Self::Herdr(error) => Some(error),
            Self::SessionRuntime(error) => Some(error),
            Self::HerdrCommandIo { source, .. } => Some(source),
            Self::HerdrCommandJson { source, .. } => Some(source),
            Self::HerdrCommandFailed { .. } | Self::Protocol(_) => None,
        }
    }
}

impl From<StartupError> for StatusError {
    fn from(error: StartupError) -> Self {
        Self::Startup(error)
    }
}

impl From<HerdrError> for StatusError {
    fn from(error: HerdrError) -> Self {
        Self::Herdr(error)
    }
}

impl From<SessionRuntimeError> for StatusError {
    fn from(error: SessionRuntimeError) -> Self {
        Self::SessionRuntime(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::session_tab_state_path;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_STATUS_TEST_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn reports_required_healthy_status_sections() {
        let snapshot = healthy_snapshot();

        let output = render_status(&snapshot);

        assert!(output.contains("Tabby status for Herdr Session work"));
        assert!(output.contains("Socket: /tmp/herdr/work.sock"));
        assert!(output.contains("Plugin: enabled, /opt/tabby/herdr-plugin.toml"));
        assert!(output.contains("Commands: /opt/tabby/bin/tabby"));
        assert!(output.contains("Registered command binary: /opt/tabby/bin/tabby"));
        assert!(output.contains("Session Runtime: Ready pid=42 version=0.1.10 lease_held=true"));
        assert!(output.contains("Ready owner binary: /opt/tabby/bin/tabby"));
        assert!(output.contains("Configuration: path=/tmp/config.toml active_schema_version=1 active_source=built-in defaults selected_profile=global policy_source=global latest_error=<none>"));
        assert!(output.contains("Focused tab: w1:t1 workspace=w1 number=1 label=codex"));
        assert!(output.contains("Focused pane: w1:p1 cwd=/repo candidate=codex"));
        assert!(
            output.contains(
                "State: 0 Manually Locked Tabs, 0 baselines, 0 unresolved rename intents"
            )
        );
        assert!(output.contains("Recent plugin actions: 1 inspected, no failures or lock skips"));
        assert!(output.contains("Warnings: none"));
    }

    #[test]
    fn reports_every_required_warning_from_injected_data() {
        let snapshot = StatusSnapshot {
            plugin: None,
            runtime: RuntimeInspection::Faulted {
                diagnostic: "control endpoint is absent".to_string(),
                lease_held: false,
                config_path: None,
                config_schema_version: None,
                config_source: None,
                selected_profile: None,
                latest_config_error: None,
            },
            focused_tab: Some(FocusedTabInspection {
                workspace_id: "w1".to_string(),
                tab_id: "w1:t1".to_string(),
                number: Some(1),
                label: "1".to_string(),
                pane_id: Some("w1:p1".to_string()),
                cwd: Some("/repo".to_string()),
                candidate_label: Some("codex".to_string()),
            }),
            tab_state: SessionTabStateInspection::Fault {
                diagnostic: "invalid JSON in state.json".to_string(),
            },
            recent_actions: vec![RecentAction {
                command: "../../bin/tabby refresh".to_string(),
                status: "failed".to_string(),
                stdout: "SkippedLocked".to_string(),
                stderr: "boom".to_string(),
            }],
            ..healthy_snapshot()
        };

        let output = render_status(&snapshot);

        assert!(output.contains("plugin yersonargotev.tabby is not registered"));
        assert!(output.contains("Session Runtime is Faulted: control endpoint is absent"));
        assert!(
            output
                .contains("State: unavailable (State Integrity Fault: invalid JSON in state.json)")
        );
        assert!(output.contains("State Integrity Fault: invalid JSON in state.json"));
        assert!(output.contains("recent plugin action failed"));
        assert!(output.contains("recent plugin action reported SkippedLocked"));
        assert!(output.contains("tabby repair-state --discard"));
        assert!(
            !output.contains("run `tabby ensure-started`"),
            "status must not recommend an ineffective recovery for a terminal fault"
        );
    }

    #[test]
    fn warns_when_ready_runtime_does_not_match_registered_binary() {
        let mut snapshot = healthy_snapshot();
        snapshot.runtime = ready_runtime("/tmp/local/tabby");

        let output = render_status(&snapshot);

        assert!(output.contains("Session Runtime binary /tmp/local/tabby does not match current executable /opt/tabby/bin/tabby"));
        assert!(output.contains("Session Runtime binary /tmp/local/tabby does not match registered command /opt/tabby/bin/tabby"));
        assert!(output.contains("herdr plugin action invoke start --plugin yersonargotev.tabby"));
    }

    #[test]
    fn reports_the_active_configuration_and_latest_rejected_reload() {
        let mut snapshot = healthy_snapshot();
        let RuntimeInspection::Ready {
            config_source,
            selected_profile,
            latest_config_error,
            ..
        } = &mut snapshot.runtime
        else {
            panic!("healthy fixture has a Ready runtime");
        };
        *config_source = Some("config.toml".to_string());
        *selected_profile = Some("work".to_string());
        *latest_config_error =
            Some("field `labels.max_length` is invalid: must be between 1 and 128".to_string());

        let output = render_status(&snapshot);

        assert!(output.contains("active_schema_version=1 active_source=config.toml"));
        assert!(output.contains("selected_profile=work"));
        assert!(output.contains("policy_source=profile:work"));
        assert!(output.contains("latest_error=field `labels.max_length` is invalid"));
    }

    #[test]
    fn derives_session_name_from_the_selected_socket_instead_of_an_env_selector() {
        assert_eq!(
            session_name_from_socket(Path::new(
                "/Users/me/.config/herdr/sessions/dots/herdr.sock"
            )),
            Some("dots".to_string())
        );
        assert_eq!(
            session_name_from_socket(Path::new("/tmp/custom.sock")),
            None
        );
    }

    #[test]
    fn missing_state_path_remains_nonexistent_during_read_only_status_inspection() {
        let state_base = test_state_base();
        let session = SessionSocket::resolve("/tmp/tabby-status-missing.sock").expect("session");

        let inspection = SessionTabStateStore::inspect_read_only(&state_base, &session);

        assert_eq!(inspection, SessionTabStateInspection::Missing);
        assert!(
            !state_base.exists(),
            "a read-only status inspection must not create the state directory"
        );
    }

    #[test]
    fn invalid_state_bytes_render_an_actionable_state_integrity_fault() {
        let state_base = test_state_base();
        let session = SessionSocket::resolve("/tmp/tabby-status-invalid.sock").expect("session");
        let path = session_tab_state_path(&state_base, &session.session_key).expect("state path");
        fs::create_dir_all(path.parent().expect("state parent")).expect("state parent");
        fs::write(&path, b"not JSON").expect("invalid state bytes");

        let inspection = SessionTabStateStore::inspect_read_only(&state_base, &session);
        let mut snapshot = healthy_snapshot();
        snapshot.tab_state = inspection;
        let output = render_status(&snapshot);

        assert!(output.contains("State: unavailable (State Integrity Fault:"));
        assert!(output.contains("invalid JSON"));
        assert!(output.contains("tabby repair-state --discard"));

        fs::remove_dir_all(&state_base).expect("remove test state");
    }

    fn healthy_snapshot() -> StatusSnapshot {
        StatusSnapshot {
            session_name: Some("work".to_string()),
            socket_path: PathBuf::from("/tmp/herdr/work.sock"),
            current_binary: PathBuf::from("/opt/tabby/bin/tabby"),
            plugin: Some(PluginRegistration {
                enabled: true,
                manifest_path: PathBuf::from("/opt/tabby/herdr-plugin.toml"),
                command_paths: vec![PathBuf::from("/opt/tabby/bin/tabby")],
            }),
            runtime: ready_runtime("/opt/tabby/bin/tabby"),
            focused_tab: Some(FocusedTabInspection {
                workspace_id: "w1".to_string(),
                tab_id: "w1:t1".to_string(),
                number: Some(1),
                label: "codex".to_string(),
                pane_id: Some("w1:p1".to_string()),
                cwd: Some("/repo".to_string()),
                candidate_label: Some("codex".to_string()),
            }),
            tab_state: SessionTabStateInspection::Valid {
                manual_locks: 0,
                baselines: 0,
                unresolved_rename_intents: 0,
            },
            recent_actions: vec![RecentAction {
                command: "../../bin/tabby ensure-started".to_string(),
                status: "succeeded".to_string(),
                stdout: String::new(),
                stderr: String::new(),
            }],
        }
    }

    fn ready_runtime(binary_path: &str) -> RuntimeInspection {
        RuntimeInspection::Ready {
            pid: 42,
            launch_id: "launch-42".to_string(),
            version: "0.1.10".to_string(),
            binary_path: PathBuf::from(binary_path),
            lease_held: true,
            last_evaluation_unix_ms: None,
            last_failure: None,
            next_periodic_unix_ms: None,
            config_path: PathBuf::from("/tmp/config.toml"),
            config_schema_version: Some(crate::config::SCHEMA_VERSION),
            config_source: Some("built-in defaults".to_string()),
            selected_profile: None,
            latest_config_error: None,
        }
    }

    fn test_state_base() -> PathBuf {
        let id = NEXT_STATUS_TEST_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("tabby-status-test-{}-{id}", std::process::id()))
    }
}
