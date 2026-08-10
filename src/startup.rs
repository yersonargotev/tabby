//! Herdr Session identity and Tabby state-base resolution.

use crate::paths::{
    HERDR_PLUGIN_CONFIG_DIR_ENV, HERDR_PLUGIN_STATE_DIR_ENV, HOME_ENV, PLUGIN_ID,
    PluginStateDirInputs, PluginStateDirSource, StatePathError, XDG_STATE_HOME_ENV,
    herdr_plugin_config_dir, plugin_state_dir_from_inputs, should_remove_stale_herdr_socket_path,
};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::string::FromUtf8Error;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

const HERDR_SOCKET_PATH_ENV: &str = "HERDR_SOCKET_PATH";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSocket {
    pub socket_path: PathBuf,
    pub identity_path: PathBuf,
    pub session_key: String,
}

impl SessionSocket {
    pub fn resolve(path: impl Into<PathBuf>) -> Result<Self, StartupError> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(StartupError::EmptySocketPath);
        }
        if !path.is_absolute() {
            return Err(StartupError::RelativeSocketPath(path));
        }

        let identity_path = path.canonicalize().unwrap_or_else(|_| path.clone());
        let session_key = session_key_for_socket_path(&identity_path);
        Ok(Self {
            socket_path: path,
            identity_path,
            session_key,
        })
    }

    #[cfg(unix)]
    pub fn identity_hex(&self) -> String {
        encode_hex(self.identity_path.as_os_str().as_bytes())
    }
}

#[cfg(unix)]
pub fn session_key_for_socket_path(path: &Path) -> String {
    let digest = Sha256::digest(path.as_os_str().as_bytes());
    format!("v2-{}", encode_hex(&digest))
}

#[cfg(unix)]
fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

pub(crate) fn resolve_socket_from_env() -> Result<SessionSocket, StartupError> {
    resolve_socket_with_env(std::env::var_os(HERDR_SOCKET_PATH_ENV), herdr_status_json)
}

/// Resolves the explicit identity of a stopped session without falling back to
/// another currently running Herdr session.
///
/// A stopped session no longer has a connectable socket, so `forget-session`
/// must retain the exact absolute path supplied by Herdr/the operator instead
/// of applying the normal stale-socket fallback.
pub(crate) fn resolve_stopped_socket_from_env() -> Result<SessionSocket, StartupError> {
    resolve_stopped_socket(std::env::var_os(HERDR_SOCKET_PATH_ENV))
}

fn resolve_stopped_socket(socket_path: Option<OsString>) -> Result<SessionSocket, StartupError> {
    let socket_path = socket_path
        .filter(|value| !value.is_empty())
        .ok_or(StartupError::MissingSocketPath)?;
    SessionSocket::resolve(PathBuf::from(socket_path))
}

fn resolve_socket_with_env(
    socket_path: Option<OsString>,
    load_status: impl FnOnce() -> Result<serde_json::Value, StartupError>,
) -> Result<SessionSocket, StartupError> {
    if let Some(socket_path) = socket_path.filter(|value| !value.is_empty()) {
        let socket_path = PathBuf::from(socket_path);
        if !should_remove_stale_herdr_socket_path(Some(socket_path.as_os_str())) {
            return SessionSocket::resolve(socket_path);
        }
    }

    let status = load_status()?;
    let socket = herdr_status_socket_path(&status).ok_or(StartupError::MissingSocketPath)?;
    SessionSocket::resolve(socket)
}

fn herdr_status_socket_path(status: &serde_json::Value) -> Option<&str> {
    let server = status.get("server")?;
    if !server.get("running")?.as_bool()? {
        return None;
    }
    server
        .get("socket")?
        .as_str()
        .filter(|socket| !socket.is_empty())
}

pub(crate) fn state_base_from_runtime() -> Result<PathBuf, StartupError> {
    resolve_state_base_with(RuntimeStateInputs::from_env(), || {
        herdr_plugin_config_dir(PLUGIN_ID).map_err(StartupError::from)
    })
}

pub type RuntimeStateInputs = PluginStateDirInputs;

pub fn resolve_state_base_with(
    inputs: RuntimeStateInputs,
    discover_plugin_config_dir: impl FnOnce() -> Result<PathBuf, StartupError>,
) -> Result<PathBuf, StartupError> {
    if let Some((path, source)) = plugin_state_dir_from_inputs(&inputs) {
        return absolute_state_base(path, source.into());
    }
    absolute_state_base(
        discover_plugin_config_dir()?,
        StateBaseSource::HerdrPluginConfigDirCommand,
    )
}

fn absolute_state_base(path: PathBuf, source: StateBaseSource) -> Result<PathBuf, StartupError> {
    if path.as_os_str().is_empty() {
        return Err(StartupError::EmptyStateBase { source });
    }
    if !path.is_absolute() {
        return Err(StartupError::RelativeStateBase { source, path });
    }
    Ok(path)
}

fn herdr_status_json() -> Result<serde_json::Value, StartupError> {
    let mut command = Command::new("herdr");
    command.args(["status", "--json"]);
    if should_remove_stale_herdr_socket_path(std::env::var_os(HERDR_SOCKET_PATH_ENV).as_deref()) {
        command.env_remove(HERDR_SOCKET_PATH_ENV);
    }

    let output = command.output().map_err(StartupError::HerdrStatusIo)?;
    if !output.status.success() {
        return Err(StartupError::HerdrStatusFailed {
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    serde_json::from_slice(&output.stdout).map_err(StartupError::HerdrStatusJson)
}

pub(crate) fn binary_identity(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateBaseSource {
    HerdrPluginStateDir,
    HerdrPluginConfigDir,
    XdgStateHome,
    Home,
    HerdrPluginConfigDirCommand,
}

impl From<PluginStateDirSource> for StateBaseSource {
    fn from(source: PluginStateDirSource) -> Self {
        match source {
            PluginStateDirSource::HerdrPluginStateDir => Self::HerdrPluginStateDir,
            PluginStateDirSource::HerdrPluginConfigDir => Self::HerdrPluginConfigDir,
            PluginStateDirSource::XdgStateHome => Self::XdgStateHome,
            PluginStateDirSource::Home => Self::Home,
        }
    }
}

impl fmt::Display for StateBaseSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::HerdrPluginStateDir => HERDR_PLUGIN_STATE_DIR_ENV,
            Self::HerdrPluginConfigDir => HERDR_PLUGIN_CONFIG_DIR_ENV,
            Self::XdgStateHome => XDG_STATE_HOME_ENV,
            Self::Home => HOME_ENV,
            Self::HerdrPluginConfigDirCommand => "herdr plugin config-dir",
        };
        formatter.write_str(name)
    }
}

#[derive(Debug)]
pub enum StartupError {
    EmptySocketPath,
    RelativeSocketPath(PathBuf),
    MissingSocketPath,
    HerdrStatusIo(io::Error),
    HerdrStatusFailed {
        status: ExitStatus,
        stderr: String,
    },
    HerdrStatusJson(serde_json::Error),
    EmptyStateBase {
        source: StateBaseSource,
    },
    RelativeStateBase {
        source: StateBaseSource,
        path: PathBuf,
    },
    HerdrConfigDirIo(io::Error),
    HerdrConfigDirFailed {
        status: ExitStatus,
        stderr: String,
    },
    HerdrConfigDirUtf8(FromUtf8Error),
    StatePath(StatePathError),
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySocketPath => write!(
                formatter,
                "HERDR_SOCKET_PATH resolved an empty Herdr socket path"
            ),
            Self::RelativeSocketPath(path) => write!(
                formatter,
                "Herdr socket path `{}` is relative; refusing to derive a Herdr Session identity",
                path.display()
            ),
            Self::MissingSocketPath => write!(
                formatter,
                "could not resolve a socket from HERDR_SOCKET_PATH or `herdr status --json`"
            ),
            Self::HerdrStatusIo(error) => write!(
                formatter,
                "failed to run `herdr status --json` for Herdr socket resolution: {error}"
            ),
            Self::HerdrStatusFailed { status, stderr } => write!(
                formatter,
                "`herdr status --json` failed with {status}: {stderr}"
            ),
            Self::HerdrStatusJson(error) => write!(
                formatter,
                "`herdr status --json` returned invalid JSON: {error}"
            ),
            Self::EmptyStateBase { source } => write!(
                formatter,
                "{source} resolved an empty Tabby state directory"
            ),
            Self::RelativeStateBase { source, path } => write!(
                formatter,
                "{source} resolved relative Tabby state directory `{}`; refusing to use a non-absolute runtime state base",
                path.display()
            ),
            Self::HerdrConfigDirIo(error) => write!(
                formatter,
                "failed to run `herdr plugin config-dir {PLUGIN_ID}` for Tabby runtime state: {error}"
            ),
            Self::HerdrConfigDirFailed { status, stderr } => write!(
                formatter,
                "`herdr plugin config-dir {PLUGIN_ID}` failed with {status}: {stderr}"
            ),
            Self::HerdrConfigDirUtf8(error) => write!(
                formatter,
                "`herdr plugin config-dir {PLUGIN_ID}` returned non-UTF-8 output: {error}"
            ),
            Self::StatePath(error) => {
                write!(formatter, "failed to resolve Tabby state path: {error}")
            }
        }
    }
}

impl std::error::Error for StartupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HerdrStatusIo(error) | Self::HerdrConfigDirIo(error) => Some(error),
            Self::HerdrStatusJson(error) => Some(error),
            Self::HerdrConfigDirUtf8(error) => Some(error),
            Self::StatePath(error) => Some(error),
            Self::EmptySocketPath
            | Self::RelativeSocketPath(_)
            | Self::MissingSocketPath
            | Self::HerdrStatusFailed { .. }
            | Self::EmptyStateBase { .. }
            | Self::RelativeStateBase { .. }
            | Self::HerdrConfigDirFailed { .. } => None,
        }
    }
}

impl From<FromUtf8Error> for StartupError {
    fn from(error: FromUtf8Error) -> Self {
        Self::HerdrConfigDirUtf8(error)
    }
}

impl From<StatePathError> for StartupError {
    fn from(error: StatePathError) -> Self {
        Self::StatePath(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn derives_stable_session_key_from_socket_path() {
        let first = SessionSocket::resolve("/tmp/herdr.sock").expect("socket");
        let second = SessionSocket::resolve("/tmp/herdr.sock").expect("socket");
        let named = SessionSocket::resolve("/tmp/other-herdr.sock").expect("socket");
        assert_eq!(first.session_key, second.session_key);
        assert_ne!(first.session_key, named.session_key);
        assert!(first.session_key.starts_with("v2-"));
    }

    #[cfg(unix)]
    #[test]
    fn session_identity_keeps_non_utf8_socket_bytes_distinct() {
        use std::os::unix::ffi::OsStringExt;
        let first = PathBuf::from(OsString::from_vec(b"/tmp/herdr-\x80.sock".to_vec()));
        let second = PathBuf::from(OsString::from_vec(b"/tmp/herdr-\x81.sock".to_vec()));
        let first = SessionSocket::resolve(first).expect("first socket identity");
        let second = SessionSocket::resolve(second).expect("second socket identity");
        assert_ne!(first.session_key, second.session_key);
        assert_ne!(first.identity_hex(), second.identity_hex());
    }

    #[test]
    fn rejects_relative_socket_path() {
        let error = SessionSocket::resolve("relative/herdr.sock").expect_err("relative socket");
        assert!(matches!(error, StartupError::RelativeSocketPath(_)));
    }

    #[test]
    fn existing_socket_env_wins_without_status_lookup() {
        let temp_dir = TestTempDir::new();
        let socket_path = temp_dir.path().join("herdr.sock");
        fs::write(&socket_path, "").expect("socket placeholder");
        let socket = resolve_socket_with_env(Some(socket_path.clone().into_os_string()), || {
            panic!("existing HERDR_SOCKET_PATH must win")
        })
        .expect("socket from env");
        assert_eq!(socket.socket_path, socket_path);
    }

    #[test]
    fn stale_absolute_socket_env_falls_back_to_herdr_status() {
        let temp_dir = TestTempDir::new();
        let stale_socket = temp_dir.path().join("missing.sock");
        let socket = resolve_socket_with_env(Some(stale_socket.into_os_string()), || {
            Ok(serde_json::json!({"server": {"running": true, "socket": "/tmp/live-herdr.sock"}}))
        })
        .expect("socket from herdr status");
        assert_eq!(socket.socket_path, PathBuf::from("/tmp/live-herdr.sock"));
    }

    #[test]
    fn stopped_session_resolution_preserves_an_explicit_missing_socket_identity() {
        let temp_dir = TestTempDir::new();
        let stopped_socket = temp_dir.path().join("stopped.sock");
        let socket = resolve_stopped_socket(Some(stopped_socket.clone().into_os_string()))
            .expect("stopped session identity");
        assert_eq!(socket.socket_path, stopped_socket);
    }

    #[test]
    fn stopped_session_resolution_requires_an_explicit_identity() {
        let error = resolve_stopped_socket(None).expect_err("explicit stopped session identity");
        assert!(matches!(error, StartupError::MissingSocketPath));
    }

    #[test]
    fn herdr_status_must_report_running_server_before_socket_is_used() {
        let temp_dir = TestTempDir::new();
        let stale_socket = temp_dir.path().join("missing.sock");
        let error = resolve_socket_with_env(Some(stale_socket.into_os_string()), || {
            Ok(serde_json::json!({"server": {"running": false, "socket": "/tmp/stale-herdr.sock"}}))
        })
        .expect_err("not-running Herdr status must not resolve a socket");
        assert!(matches!(error, StartupError::MissingSocketPath));
    }

    #[test]
    fn resolves_runtime_state_base_from_state_dir_env_first() {
        let path = resolve_state_base_with(
            RuntimeStateInputs {
                herdr_plugin_state_dir: Some(OsString::from("/tmp/tabby-state")),
                herdr_plugin_config_dir: Some(OsString::from("/tmp/tabby-config")),
                ..RuntimeStateInputs::default()
            },
            || panic!("state dir must win"),
        )
        .expect("state base");
        assert_eq!(path, PathBuf::from("/tmp/tabby-state"));
    }

    #[test]
    fn refuses_relative_runtime_state_base() {
        let error = resolve_state_base_with(
            RuntimeStateInputs {
                herdr_plugin_state_dir: Some(OsString::from("relative/state")),
                herdr_plugin_config_dir: None,
                ..RuntimeStateInputs::default()
            },
            || panic!("relative state dir must fail"),
        )
        .expect_err("relative state dir");
        assert!(matches!(error, StartupError::RelativeStateBase { .. }));
    }

    #[test]
    fn xdg_state_home_matches_herdr_plugin_state_layout_without_plugin_env() {
        let path = resolve_state_base_with(
            RuntimeStateInputs {
                xdg_state_home: Some(OsString::from("/tmp/tabby-state")),
                home: Some(OsString::from("/tmp/tabby-home")),
                ..RuntimeStateInputs::default()
            },
            || panic!("XDG_STATE_HOME should avoid Herdr config-dir discovery"),
        )
        .expect("state base");
        assert_eq!(
            path,
            PathBuf::from("/tmp/tabby-state/herdr/plugins/yersonargotev.tabby")
        );
    }

    #[test]
    fn home_state_fallback_matches_herdr_plugin_state_layout_without_plugin_env() {
        let path = resolve_state_base_with(
            RuntimeStateInputs {
                home: Some(OsString::from("/tmp/tabby-home")),
                ..RuntimeStateInputs::default()
            },
            || panic!("HOME state fallback should avoid Herdr config-dir discovery"),
        )
        .expect("state base");
        assert_eq!(
            path,
            PathBuf::from("/tmp/tabby-home/.local/state/herdr/plugins/yersonargotev.tabby")
        );
    }

    struct TestTempDir {
        path: PathBuf,
    }
    impl TestTempDir {
        fn new() -> Self {
            let id = NEXT_TEMP_DIR_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("tabby-startup-test-{}-{id}", std::process::id()));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
        fn path(&self) -> &Path {
            &self.path
        }
    }
    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
