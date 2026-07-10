use crate::herdr_client::{HerdrApi, HerdrClient, HerdrError, UnixSocketTransport};
use crate::labeler::LabelPolicy;
use crate::locks::{LockStore, LockStoreError};
use crate::paths::{PLUGIN_ID, StatePathError, lock_store_path_from_runtime};
use crate::startup::{self, RefresherMetadata, SessionSocket, StartupError};
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
pub struct RefresherInspection {
    pub metadata: RefresherMetadata,
    pub running: bool,
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
    pub refresher: Option<RefresherInspection>,
    pub focused_tab: Option<FocusedTabInspection>,
    pub locks: LockStore,
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
        }
        None => lines.push(format!("Plugin: {PLUGIN_ID} is not registered")),
    }

    match &snapshot.refresher {
        Some(refresher) => {
            let metadata = &refresher.metadata;
            let state = if refresher.running {
                "running"
            } else {
                "not running"
            };
            let binary = metadata.binary_path.as_deref().unwrap_or("<unknown>");
            lines.push(format!(
                "Refresher: {state} pid {}, {binary}, version {}",
                metadata.pid, metadata.tabby_version
            ));
            lines.push(format!(
                "Refresher metadata: session_key={} socket={} started_at={}",
                metadata.session_key, metadata.socket_path, metadata.started_at
            ));
        }
        None => lines.push("Refresher: not running (metadata not found)".to_string()),
    }

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

    lines.push(format!(
        "Locks: {} Manually Locked Tabs",
        snapshot.locks.len()
    ));
    for lock in snapshot.locks.locks() {
        lines.push(format!(
            "- {} label={}",
            lock.tab_id(),
            lock.label().unwrap_or("<unknown>")
        ));
    }

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

    match &snapshot.refresher {
        None => {
            warnings
                .push("Hybrid Session Refresher is not running (metadata not found)".to_string());
            fixes.insert("run `tabby ensure-started` for this Herdr Session".to_string());
        }
        Some(refresher) => {
            let metadata = &refresher.metadata;
            if !refresher.running {
                warnings.push(format!(
                    "Hybrid Session Refresher pid {} is not running",
                    metadata.pid
                ));
                fixes.insert("run `tabby ensure-started` for this Herdr Session".to_string());
            }
            if metadata.socket_path != snapshot.socket_path.to_string_lossy() {
                warnings.push(format!(
                    "refresher metadata socket {} does not match targeted socket {}",
                    metadata.socket_path,
                    snapshot.socket_path.display()
                ));
            }
            match metadata.binary_path.as_deref().map(Path::new) {
                None => warnings.push(
                    "refresher metadata has no binary path, so its executable cannot be verified"
                        .to_string(),
                ),
                Some(binary) => {
                    let binary_identity = startup::binary_identity(binary);
                    if binary_identity != snapshot.current_binary {
                        warnings.push(format!(
                            "refresher binary {} does not match current executable {}",
                            binary.display(),
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
                        warnings.push(format!(
                            "refresher binary {} does not match registered command {}",
                            binary.display(),
                            plugin
                                .command_paths
                                .iter()
                                .map(|path| path.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
            }
            if warnings
                .iter()
                .any(|warning| warning.starts_with("refresher binary"))
            {
                fixes.insert(format!(
                    "stop only recorded refresher pid {} with `kill {}`; then rerun `tabby ensure-started`",
                    metadata.pid, metadata.pid
                ));
            }
        }
    }

    if let Some(tab) = &snapshot.focused_tab {
        if tab.label.parse::<u64>().is_ok() && snapshot.locks.is_locked(&tab.tab_id) {
            warnings.push(format!(
                "focused tab {} has numeric label {} and is manually locked",
                tab.tab_id, tab.label
            ));
            fixes.insert("run `tabby unlock-focused` to clear its lock and baseline".to_string());
        }
        if let Some(baseline) = snapshot.locks.last_plugin_label(&tab.tab_id)
            && baseline != tab.label
        {
            warnings.push(format!(
                "baseline for {} is {baseline} but the visible label is {}; possible stale tab_id reuse",
                tab.tab_id, tab.label
            ));
            fixes.insert("run `tabby unlock-focused` to clear its lock and baseline".to_string());
        }
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
    let state_base = startup::state_base_from_runtime()?;
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

    let metadata = startup::read_refresher_metadata(&state_base, &socket)?;
    let refresher = metadata.map(|metadata| RefresherInspection {
        running: startup::metadata_process_is_live(&metadata, &socket),
        metadata,
    });
    let focused_tab = inspect_focused_tab(&socket)?;
    let locks = LockStore::load(lock_store_path_from_runtime()?)?;

    Ok(StatusSnapshot {
        session_name: session_name_from_socket(&socket.socket_path),
        socket_path: socket.socket_path,
        current_binary,
        plugin,
        refresher,
        focused_tab,
        locks,
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
    let Some(tab) = client.list_tabs()?.into_iter().find(|tab| tab.focused) else {
        return Ok(None);
    };
    let panes = client.list_panes()?;
    let pane = panes
        .iter()
        .find(|pane| pane.tab_id == tab.tab_id && pane.focused)
        .or_else(|| panes.iter().find(|pane| pane.tab_id == tab.tab_id));
    let (pane_id, cwd, candidate_label) = match pane {
        Some(pane) => {
            let process_info = client.pane_process_info(&pane.pane_id).ok();
            let candidate = LabelPolicy::default()
                .candidate_for_pane(pane, process_info.as_ref())
                .map(|candidate| candidate.label().to_string());
            (
                Some(pane.pane_id.clone()),
                pane.foreground_cwd.clone().or_else(|| pane.cwd.clone()),
                candidate,
            )
        }
        None => (None, None, None),
    };

    Ok(Some(FocusedTabInspection {
        workspace_id: tab.workspace_id,
        tab_id: tab.tab_id,
        number: tab.number,
        label: tab.label,
        pane_id,
        cwd,
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
    StatePath(StatePathError),
    LockStore(LockStoreError),
    Herdr(HerdrError),
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
                "failed to inspect refresher startup state: {error}"
            ),
            Self::StatePath(error) => {
                write!(formatter, "failed to resolve Tabby state path: {error}")
            }
            Self::LockStore(error) => {
                write!(formatter, "failed to inspect Manually Locked Tabs: {error}")
            }
            Self::Herdr(error) => {
                write!(formatter, "failed to inspect focused Herdr state: {error}")
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
            Self::StatePath(error) => Some(error),
            Self::LockStore(error) => Some(error),
            Self::Herdr(error) => Some(error),
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

impl From<StatePathError> for StatusError {
    fn from(error: StatePathError) -> Self {
        Self::StatePath(error)
    }
}

impl From<LockStoreError> for StatusError {
    fn from(error: LockStoreError) -> Self {
        Self::LockStore(error)
    }
}

impl From<HerdrError> for StatusError {
    fn from(error: HerdrError) -> Self {
        Self::Herdr(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_required_healthy_status_sections() {
        let snapshot = healthy_snapshot();

        let output = render_status(&snapshot);

        assert!(output.contains("Tabby status for Herdr Session work"));
        assert!(output.contains("Socket: /tmp/herdr/work.sock"));
        assert!(output.contains("Plugin: enabled, /opt/tabby/herdr-plugin.toml"));
        assert!(output.contains("Commands: /opt/tabby/bin/tabby"));
        assert!(output.contains("Refresher: running pid 42, /opt/tabby/bin/tabby, version 0.1.8"));
        assert!(output.contains("Focused tab: w1:t1 workspace=w1 number=1 label=codex"));
        assert!(output.contains("Focused pane: w1:p1 cwd=/repo candidate=codex"));
        assert!(output.contains("Locks: 0 Manually Locked Tabs"));
        assert!(output.contains("Recent plugin actions: 1 inspected, no failures or lock skips"));
        assert!(output.contains("Warnings: none"));
    }

    #[test]
    fn reports_every_required_warning_from_injected_data() {
        let mut locks = LockStore::default();
        locks.lock_tab("w1:t1", Some("1".to_string()));
        locks.record_plugin_label("w1:t1", "codex");
        let snapshot = StatusSnapshot {
            plugin: None,
            refresher: Some(RefresherInspection {
                metadata: metadata("/tmp/local/tabby"),
                running: false,
            }),
            focused_tab: Some(FocusedTabInspection {
                workspace_id: "w1".to_string(),
                tab_id: "w1:t1".to_string(),
                number: Some(1),
                label: "1".to_string(),
                pane_id: Some("w1:p1".to_string()),
                cwd: Some("/repo".to_string()),
                candidate_label: Some("codex".to_string()),
            }),
            locks,
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
        assert!(output.contains("Hybrid Session Refresher pid 42 is not running"));
        assert!(output.contains("does not match current executable /opt/tabby/bin/tabby"));
        assert!(output.contains("focused tab w1:t1 has numeric label 1 and is manually locked"));
        assert!(output.contains("baseline for w1:t1 is codex but the visible label is 1"));
        assert!(output.contains("recent plugin action failed"));
        assert!(output.contains("recent plugin action reported SkippedLocked"));
        assert!(output.contains("tabby unlock-focused"));
    }

    #[test]
    fn warns_when_live_refresher_does_not_match_registered_binary() {
        let mut snapshot = healthy_snapshot();
        snapshot.refresher = Some(RefresherInspection {
            metadata: metadata("/tmp/local/tabby"),
            running: true,
        });

        let output = render_status(&snapshot);

        assert!(output.contains("does not match current executable /opt/tabby/bin/tabby"));
        assert!(output.contains("does not match registered command /opt/tabby/bin/tabby"));
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
            refresher: Some(RefresherInspection {
                metadata: metadata("/opt/tabby/bin/tabby"),
                running: true,
            }),
            focused_tab: Some(FocusedTabInspection {
                workspace_id: "w1".to_string(),
                tab_id: "w1:t1".to_string(),
                number: Some(1),
                label: "codex".to_string(),
                pane_id: Some("w1:p1".to_string()),
                cwd: Some("/repo".to_string()),
                candidate_label: Some("codex".to_string()),
            }),
            locks: LockStore::default(),
            recent_actions: vec![RecentAction {
                command: "../../bin/tabby ensure-started".to_string(),
                status: "succeeded".to_string(),
                stdout: String::new(),
                stderr: String::new(),
            }],
        }
    }

    fn metadata(binary_path: &str) -> RefresherMetadata {
        RefresherMetadata {
            schema_version: 2,
            pid: 42,
            session_key: "v1-test".to_string(),
            socket_path: "/tmp/herdr/work.sock".to_string(),
            started_at: 1,
            tabby_version: "0.1.8".to_string(),
            binary_path: Some(binary_path.to_string()),
        }
    }
}
