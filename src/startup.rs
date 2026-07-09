//! Idempotent startup for one Tabby Session Refresher per Herdr Session.
//!
//! `tabby start` is the long-running refresher loop. Normal startup entrypoints
//! call `ensure-started`, which serializes startup per Herdr socket, validates
//! existing refresher metadata, and only spawns `tabby start` when needed.

use crate::paths::{
    HERDR_PLUGIN_CONFIG_DIR_ENV, HERDR_PLUGIN_STATE_DIR_ENV, HOME_ENV, PLUGIN_ID,
    PluginStateDirInputs, PluginStateDirSource, StatePathError, XDG_STATE_HOME_ENV,
    herdr_plugin_config_dir, plugin_state_dir_from_inputs, should_remove_stale_herdr_socket_path,
};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::string::FromUtf8Error;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[cfg(unix)]
unsafe extern "C" {
    fn setsid() -> i32;
}

const HERDR_SOCKET_PATH_ENV: &str = "HERDR_SOCKET_PATH";
const REFRESHERS_DIR_NAME: &str = "refreshers";
const METADATA_SCHEMA_VERSION: u8 = 1;
const REFRESHER_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

pub fn ensure_started_from_env() -> Result<String, StartupError> {
    let socket = resolve_socket_from_env()?;
    let state_base = state_base_from_runtime()?;
    let current_exe = std::env::current_exe().map_err(StartupError::CurrentExe)?;
    let mut runtime = SystemStartupRuntime;
    let outcome = ensure_started_with(&socket, &state_base, &current_exe, &mut runtime)?;
    Ok(format!("tabby ensure-started: {outcome}"))
}

pub fn ensure_started_with<R>(
    socket: &SessionSocket,
    state_base: &Path,
    binary_path: &Path,
    runtime: &mut R,
) -> Result<EnsureStartedOutcome, StartupError>
where
    R: StartupRuntime,
{
    let refresher_dir = state_base.join(REFRESHERS_DIR_NAME);
    fs::create_dir_all(&refresher_dir).map_err(StartupError::Io)?;
    let lock_path = refresher_dir.join(format!("{}.lock", socket.session_key));
    let metadata_path = refresher_dir.join(format!("{}.json", socket.session_key));
    let _lock = runtime.acquire_lock(&lock_path)?;

    if let Some(metadata) = read_metadata_if_present(&metadata_path)?
        && metadata_is_live_for_socket(&metadata, socket, runtime)
    {
        return Ok(EnsureStartedOutcome::AlreadyRunning { pid: metadata.pid });
    }

    let child = runtime.spawn_refresher(binary_path, &socket.socket_path)?;
    let metadata = RefresherMetadata {
        schema_version: METADATA_SCHEMA_VERSION,
        pid: child.pid,
        session_key: socket.session_key.clone(),
        socket_path: socket.socket_path.to_string_lossy().to_string(),
        started_at: unix_timestamp_secs(),
        tabby_version: env!("CARGO_PKG_VERSION").to_string(),
        binary_path: Some(binary_path.to_string_lossy().to_string()),
    };
    write_metadata(&metadata_path, &metadata)?;
    Ok(EnsureStartedOutcome::Started { pid: child.pid })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSocket {
    pub socket_path: PathBuf,
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
            session_key,
        })
    }
}

pub fn session_key_for_socket_path(path: &Path) -> String {
    format!("v1-{:016x}", fnv1a64(path.to_string_lossy().as_bytes()))
}

fn resolve_socket_from_env() -> Result<SessionSocket, StartupError> {
    resolve_socket_with_env(std::env::var_os(HERDR_SOCKET_PATH_ENV), herdr_status_json)
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

fn state_base_from_runtime() -> Result<PathBuf, StartupError> {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefresherMetadata {
    pub schema_version: u8,
    pub pid: u32,
    pub session_key: String,
    pub socket_path: String,
    pub started_at: u64,
    pub tabby_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
}

fn read_metadata_if_present(path: &Path) -> Result<Option<RefresherMetadata>, StartupError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(serde_json::from_str(&contents).ok()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StartupError::Io(error)),
    }
}

fn write_metadata(path: &Path, metadata: &RefresherMetadata) -> Result<(), StartupError> {
    let contents = serde_json::to_string_pretty(metadata)?;
    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, contents).map_err(StartupError::Io)?;
    fs::rename(&temp_path, path).map_err(StartupError::Io)?;
    Ok(())
}

pub fn metadata_is_live_for_socket<R>(
    metadata: &RefresherMetadata,
    socket: &SessionSocket,
    runtime: &mut R,
) -> bool
where
    R: StartupRuntime,
{
    metadata.schema_version == METADATA_SCHEMA_VERSION
        && metadata.session_key == socket.session_key
        && runtime.process_appears_to_be_tabby(metadata.pid)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureStartedOutcome {
    AlreadyRunning { pid: u32 },
    Started { pid: u32 },
}

impl fmt::Display for EnsureStartedOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning { pid } => {
                write!(
                    formatter,
                    "Tabby Session Refresher already running with pid {pid}"
                )
            }
            Self::Started { pid } => {
                write!(formatter, "started Tabby Session Refresher with pid {pid}")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnedRefresher {
    pub pid: u32,
}

pub trait StartupRuntime {
    fn acquire_lock(&mut self, path: &Path) -> Result<RefresherLock, StartupError>;
    fn process_appears_to_be_tabby(&mut self, pid: u32) -> bool;
    fn spawn_refresher(
        &mut self,
        binary_path: &Path,
        socket_path: &Path,
    ) -> Result<SpawnedRefresher, StartupError>;
}

struct SystemStartupRuntime;

impl StartupRuntime for SystemStartupRuntime {
    fn acquire_lock(&mut self, path: &Path) -> Result<RefresherLock, StartupError> {
        RefresherLock::acquire(path)
    }

    fn process_appears_to_be_tabby(&mut self, pid: u32) -> bool {
        process_appears_to_be_tabby(pid)
    }

    fn spawn_refresher(
        &mut self,
        binary_path: &Path,
        socket_path: &Path,
    ) -> Result<SpawnedRefresher, StartupError> {
        spawn_detached_refresher(binary_path, socket_path)
    }
}

pub struct RefresherLock {
    path: PathBuf,
}

impl RefresherLock {
    fn acquire(path: &Path) -> Result<Self, StartupError> {
        Self::acquire_with_timeout(path, REFRESHER_LOCK_TIMEOUT, process_appears_to_be_tabby)
    }

    fn acquire_with_timeout(
        path: &Path,
        timeout: Duration,
        mut lock_holder_is_live: impl FnMut(u32) -> bool,
    ) -> Result<Self, StartupError> {
        let deadline = Instant::now() + timeout;
        loop {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    use std::io::Write;
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if let Some(pid) = read_lock_holder_pid(path)?
                        && !lock_holder_is_live(pid)
                    {
                        fs::remove_file(path).map_err(StartupError::Io)?;
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(StartupError::RefresherLockBusy(path.to_path_buf()));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => return Err(StartupError::Io(error)),
            }
        }
    }
}

fn read_lock_holder_pid(path: &Path) -> Result<Option<u32>, StartupError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents.trim().parse().ok()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StartupError::Io(error)),
    }
}

impl Drop for RefresherLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn spawn_detached_refresher(
    binary_path: &Path,
    socket_path: &Path,
) -> Result<SpawnedRefresher, StartupError> {
    let mut command = Command::new(binary_path);
    command
        .arg("start")
        .env(HERDR_SOCKET_PATH_ENV, socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        // SAFETY: `pre_exec` runs in the child after fork and before exec. The
        // closure only calls async-signal-safe `setsid` and reads errno via
        // `last_os_error` when it fails.
        unsafe {
            command.pre_exec(|| {
                if setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let child: Child = command.spawn().map_err(StartupError::SpawnRefresher)?;
    Ok(SpawnedRefresher { pid: child.id() })
}

fn process_appears_to_be_tabby(pid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm=", "-o", "command="])
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&output.stdout).to_lowercase();
    text.split_whitespace().any(|part| {
        Path::new(part)
            .file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "tabby")
            || part == "tabby"
    })
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
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
    CurrentExe(io::Error),
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
    MetadataJson(serde_json::Error),
    Io(io::Error),
    RefresherLockBusy(PathBuf),
    SpawnRefresher(io::Error),
    StatePath(StatePathError),
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentExe(error) => write!(
                formatter,
                "failed to locate the running tabby executable: {error}"
            ),
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
                "could not resolve a Herdr socket from HERDR_SOCKET_PATH or `herdr status --json`"
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
                "{source} resolved relative Tabby state directory `{}`; refusing to write refresher metadata outside an explicit absolute path",
                path.display()
            ),
            Self::HerdrConfigDirIo(error) => write!(
                formatter,
                "failed to run `herdr plugin config-dir {PLUGIN_ID}` for Tabby refresher state path: {error}"
            ),
            Self::HerdrConfigDirFailed { status, stderr } => write!(
                formatter,
                "`herdr plugin config-dir {PLUGIN_ID}` failed with {status}: {stderr}"
            ),
            Self::HerdrConfigDirUtf8(error) => write!(
                formatter,
                "`herdr plugin config-dir {PLUGIN_ID}` returned non-UTF-8 output: {error}"
            ),
            Self::MetadataJson(error) => {
                write!(formatter, "refresher metadata is invalid JSON: {error}")
            }
            Self::Io(error) => write!(
                formatter,
                "refresher startup state operation failed: {error}"
            ),
            Self::RefresherLockBusy(path) => write!(
                formatter,
                "Tabby Session Refresher startup lock `{}` is still held; refusing to remove a live lock and risk duplicate Tabby Session Refreshers",
                path.display()
            ),
            Self::SpawnRefresher(error) => {
                write!(formatter, "failed to spawn detached `tabby start`: {error}")
            }
            Self::StatePath(error) => {
                write!(formatter, "failed to resolve Tabby state path: {error}")
            }
        }
    }
}

impl std::error::Error for StartupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CurrentExe(error)
            | Self::HerdrStatusIo(error)
            | Self::HerdrConfigDirIo(error)
            | Self::Io(error)
            | Self::SpawnRefresher(error) => Some(error),
            Self::HerdrStatusJson(error) | Self::MetadataJson(error) => Some(error),
            Self::HerdrConfigDirUtf8(error) => Some(error),
            Self::StatePath(error) => Some(error),
            Self::EmptySocketPath
            | Self::RelativeSocketPath(_)
            | Self::MissingSocketPath
            | Self::RefresherLockBusy(_)
            | Self::HerdrStatusFailed { .. }
            | Self::EmptyStateBase { .. }
            | Self::RelativeStateBase { .. }
            | Self::HerdrConfigDirFailed { .. } => None,
        }
    }
}

impl From<serde_json::Error> for StartupError {
    fn from(error: serde_json::Error) -> Self {
        Self::MetadataJson(error)
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
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs::File;
    use std::sync::atomic::{AtomicU64, Ordering};
    #[cfg(unix)]
    use std::thread;

    static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn derives_stable_session_key_from_socket_path() {
        let first = SessionSocket::resolve("/tmp/herdr.sock").expect("socket");
        let second = SessionSocket::resolve("/tmp/herdr.sock").expect("socket");
        let named = SessionSocket::resolve("/tmp/other-herdr.sock").expect("socket");

        assert_eq!(first.session_key, second.session_key);
        assert_ne!(first.session_key, named.session_key);
        assert!(first.session_key.starts_with("v1-"));
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
            Ok(serde_json::json!({
                "server": { "running": true, "socket": "/tmp/live-herdr.sock" }
            }))
        })
        .expect("socket from herdr status");

        assert_eq!(socket.socket_path, PathBuf::from("/tmp/live-herdr.sock"));
    }

    #[test]
    fn herdr_status_must_report_running_server_before_socket_is_used() {
        let temp_dir = TestTempDir::new();
        let stale_socket = temp_dir.path().join("missing.sock");

        let error = resolve_socket_with_env(Some(stale_socket.into_os_string()), || {
            Ok(serde_json::json!({
                "server": { "running": false, "socket": "/tmp/stale-herdr.sock" }
            }))
        })
        .expect_err("not-running Herdr status must not resolve a socket");

        assert!(matches!(error, StartupError::MissingSocketPath));
    }

    #[test]
    fn resolves_refresher_state_base_from_state_dir_env_first() {
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
    fn refuses_relative_refresher_state_base() {
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

    #[test]
    fn stale_refresher_lock_with_dead_holder_is_replaced() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("refresher.lock");
        fs::write(&lock_path, "424242\n").expect("stale lock");

        let lock = RefresherLock::acquire_with_timeout(&lock_path, Duration::ZERO, |_| false)
            .expect("replace stale lock");
        let holder = fs::read_to_string(&lock_path).expect("lock holder");

        assert_eq!(holder.trim(), std::process::id().to_string());
        drop(lock);
        assert!(!lock_path.exists());
    }

    #[test]
    fn live_refresher_lock_is_not_removed_after_timeout() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("refresher.lock");
        fs::write(&lock_path, "424242\n").expect("live lock");

        let error = match RefresherLock::acquire_with_timeout(&lock_path, Duration::ZERO, |_| true)
        {
            Ok(_) => panic!("live lock must not be acquired"),
            Err(error) => error,
        };

        assert!(matches!(error, StartupError::RefresherLockBusy(path) if path == lock_path));
        assert_eq!(
            fs::read_to_string(&lock_path).expect("lock holder"),
            "424242\n"
        );
    }

    #[test]
    fn ensure_started_does_not_spawn_duplicate_for_same_herdr_session() {
        let temp_dir = TestTempDir::new();
        let socket = SessionSocket::resolve("/tmp/herdr.sock").expect("socket");
        let metadata_path = write_test_metadata(temp_dir.path(), &socket, 123);
        let mut runtime = FakeStartupRuntime::default().with_live_tabby_pid(123);

        let outcome = ensure_started_with(
            &socket,
            temp_dir.path(),
            Path::new("/tmp/tabby"),
            &mut runtime,
        )
        .expect("ensure started");

        assert_eq!(outcome, EnsureStartedOutcome::AlreadyRunning { pid: 123 });
        assert!(runtime.spawns.is_empty());
        assert!(metadata_path.exists());
    }

    #[test]
    fn stale_metadata_is_replaced_and_spawns_refresher() {
        let temp_dir = TestTempDir::new();
        let socket = SessionSocket::resolve("/tmp/herdr.sock").expect("socket");
        let metadata_path = write_test_metadata(temp_dir.path(), &socket, 123);
        let mut runtime = FakeStartupRuntime::default().with_spawn_pid(456);

        let outcome = ensure_started_with(
            &socket,
            temp_dir.path(),
            Path::new("/tmp/tabby"),
            &mut runtime,
        )
        .expect("ensure started");
        let metadata = read_metadata_if_present(&metadata_path)
            .expect("read metadata")
            .expect("metadata present");

        assert_eq!(outcome, EnsureStartedOutcome::Started { pid: 456 });
        assert_eq!(metadata.pid, 456);
        assert_eq!(metadata.session_key, socket.session_key);
        assert_eq!(
            runtime.spawns,
            vec![(
                PathBuf::from("/tmp/tabby"),
                PathBuf::from("/tmp/herdr.sock")
            )]
        );
    }

    #[test]
    fn malformed_metadata_is_replaced_and_spawns_refresher() {
        let temp_dir = TestTempDir::new();
        let socket = SessionSocket::resolve("/tmp/herdr.sock").expect("socket");
        let metadata_path = metadata_path_for(temp_dir.path(), &socket);
        fs::create_dir_all(metadata_path.parent().expect("metadata parent"))
            .expect("refresher dir");
        fs::write(&metadata_path, "{not valid json").expect("write malformed metadata");
        let mut runtime = FakeStartupRuntime::default().with_spawn_pid(456);

        let outcome = ensure_started_with(
            &socket,
            temp_dir.path(),
            Path::new("/tmp/tabby"),
            &mut runtime,
        )
        .expect("ensure started");
        let metadata = read_metadata_if_present(&metadata_path)
            .expect("read metadata")
            .expect("metadata present");

        assert_eq!(outcome, EnsureStartedOutcome::Started { pid: 456 });
        assert_eq!(metadata.pid, 456);
        assert_eq!(metadata.session_key, socket.session_key);
    }

    #[test]
    fn metadata_with_mismatched_session_key_is_not_live() {
        let socket = SessionSocket::resolve("/tmp/herdr.sock").expect("socket");
        let metadata = RefresherMetadata {
            schema_version: METADATA_SCHEMA_VERSION,
            pid: 123,
            session_key: "other".to_string(),
            socket_path: socket.socket_path.to_string_lossy().to_string(),
            started_at: 1,
            tabby_version: "test".to_string(),
            binary_path: None,
        };
        let mut runtime = FakeStartupRuntime::default().with_live_tabby_pid(123);

        assert!(!metadata_is_live_for_socket(
            &metadata,
            &socket,
            &mut runtime
        ));
    }

    #[cfg(unix)]
    #[test]
    fn spawned_refresher_is_isolated_from_parent_process_group() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TestTempDir::new();
        let helper_path = temp_dir.path().join("fake-tabby");
        let pid_path = temp_dir.path().join("child.pid");
        let socket_path = temp_dir.path().join("herdr.sock");

        fs::write(
            &helper_path,
            format!(
                r#"#!/bin/sh
test "$1" = "start" || exit 2
test "$HERDR_SOCKET_PATH" = "{}" || exit 3
echo "$$" > "{}"
sleep 30
"#,
                socket_path.display(),
                pid_path.display()
            ),
        )
        .expect("write helper");
        let mut permissions = fs::metadata(&helper_path)
            .expect("helper metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&helper_path, permissions).expect("make helper executable");

        let spawned = spawn_detached_refresher(&helper_path, &socket_path).expect("spawn helper");
        wait_for_file(&pid_path);

        let recorded_pid: u32 = fs::read_to_string(&pid_path)
            .expect("read child pid")
            .trim()
            .parse()
            .expect("parse child pid");
        assert_eq!(recorded_pid, spawned.pid);
        assert!(pid_is_running(spawned.pid));
        assert_ne!(
            process_group_id(spawned.pid),
            process_group_id(std::process::id()),
            "detached refresher must not remain in the short-lived ensure-started process group"
        );

        terminate_process(spawned.pid);
    }

    fn metadata_path_for(state_base: &Path, socket: &SessionSocket) -> PathBuf {
        state_base
            .join(REFRESHERS_DIR_NAME)
            .join(format!("{}.json", socket.session_key))
    }

    fn write_test_metadata(state_base: &Path, socket: &SessionSocket, pid: u32) -> PathBuf {
        let path = metadata_path_for(state_base, socket);
        fs::create_dir_all(path.parent().expect("metadata parent")).expect("refresher dir");
        write_metadata(
            &path,
            &RefresherMetadata {
                schema_version: METADATA_SCHEMA_VERSION,
                pid,
                session_key: socket.session_key.clone(),
                socket_path: socket.socket_path.to_string_lossy().to_string(),
                started_at: 1,
                tabby_version: "test".to_string(),
                binary_path: Some("/tmp/tabby".to_string()),
            },
        )
        .expect("metadata");
        path
    }

    #[derive(Default)]
    struct FakeStartupRuntime {
        live_tabby_pids: BTreeSet<u32>,
        spawn_pid: u32,
        spawns: Vec<(PathBuf, PathBuf)>,
        locks: BTreeMap<PathBuf, File>,
    }

    impl FakeStartupRuntime {
        fn with_live_tabby_pid(mut self, pid: u32) -> Self {
            self.live_tabby_pids.insert(pid);
            self
        }

        fn with_spawn_pid(mut self, pid: u32) -> Self {
            self.spawn_pid = pid;
            self
        }
    }

    impl StartupRuntime for FakeStartupRuntime {
        fn acquire_lock(&mut self, path: &Path) -> Result<RefresherLock, StartupError> {
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .map_err(StartupError::Io)?;
            self.locks.insert(path.to_path_buf(), file);
            Ok(RefresherLock {
                path: path.to_path_buf(),
            })
        }

        fn process_appears_to_be_tabby(&mut self, pid: u32) -> bool {
            self.live_tabby_pids.contains(&pid)
        }

        fn spawn_refresher(
            &mut self,
            binary_path: &Path,
            socket_path: &Path,
        ) -> Result<SpawnedRefresher, StartupError> {
            self.spawns
                .push((binary_path.to_path_buf(), socket_path.to_path_buf()));
            Ok(SpawnedRefresher {
                pid: self.spawn_pid,
            })
        }
    }

    #[cfg(unix)]
    fn wait_for_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if path.exists() {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting for {}", path.display());
    }

    #[cfg(unix)]
    fn process_group_id(pid: u32) -> u32 {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "pgid="])
            .output()
            .expect("run ps");
        assert!(output.status.success(), "ps failed for pid {pid}");
        String::from_utf8(output.stdout)
            .expect("pgid output utf8")
            .trim()
            .parse()
            .expect("parse pgid")
    }

    #[cfg(unix)]
    fn pid_is_running(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }

    #[cfg(unix)]
    fn terminate_process(pid: u32) {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }

    struct TestTempDir {
        path: PathBuf,
    }

    impl TestTempDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after unix epoch")
                .as_nanos();
            let id = NEXT_TEMP_DIR_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "tabby-startup-test-{}-{unique}-{id}",
                std::process::id()
            ));
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
