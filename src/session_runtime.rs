//! One Ready Session Runtime per running Herdr Session.

mod unix;

use crate::daemon::{self, DaemonError, HybridRefresherState};
use crate::herdr_client::{HerdrClient, UnixSocketTransport};
use crate::paths::{HERDR_PLUGIN_STATE_DIR_ENV, lock_store_path_from_runtime};
use crate::startup::SessionSocket;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use self::unix::{LifetimeLease, bind_private_listener, ensure_private_directory, peer_uid};

const HERDR_SOCKET_PATH_ENV: &str = "HERDR_SOCKET_PATH";
const RUNTIMES_DIR_NAME: &str = "session-runtimes";
const CONTROL_SOCKET_NAME: &str = "control.sock";
const RUNTIME_METADATA_NAME: &str = "runtime.json";
const STARTUP_GATE_NAME: &str = "startup.lock";
const RUNTIME_LEASE_NAME: &str = "runtime.lease";
const CONTROL_SCHEMA_VERSION: u8 = 1;
const RUNTIME_METADATA_SCHEMA_VERSION: u8 = 1;
const STARTUP_GATE_TIMEOUT: Duration = Duration::from_secs(5);
const READINESS_TIMEOUT: Duration = Duration::from_secs(2);
const RUNTIME_LEASE_WAIT: Duration = Duration::from_secs(1);
const CONTROL_IO_TIMEOUT: Duration = Duration::from_secs(1);
const CONTROL_MAX_LINE_BYTES: u64 = 64 * 1024;

static NEXT_LAUNCH_ID: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
unsafe extern "C" {
    fn setsid() -> i32;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshTrigger {
    Startup,
    Focus,
    Creation,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadySessionRuntime {
    pub pid: u32,
    pub launch_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureRuntimeOutcome {
    ReadyOwnerSignaled(ReadySessionRuntime),
    NewOwnerReady(ReadySessionRuntime),
}

pub struct SessionRuntimeLaunch<'a> {
    pub socket: &'a SessionSocket,
    pub state_base: &'a Path,
    pub binary_path: &'a Path,
}

#[derive(Debug, Clone)]
struct RuntimePaths {
    directory: PathBuf,
    control_directory: PathBuf,
    startup_gate: PathBuf,
    lifetime_lease: PathBuf,
    control_socket: PathBuf,
    metadata: PathBuf,
}

impl RuntimePaths {
    fn for_launch(launch: &SessionRuntimeLaunch<'_>) -> Self {
        let directory = launch
            .state_base
            .join(RUNTIMES_DIR_NAME)
            .join(&launch.socket.session_key);
        let control_directory = PathBuf::from("/tmp")
            .join(format!("tby-{}", unsafe { libc::geteuid() }))
            .join(&launch.socket.session_key);
        Self {
            startup_gate: directory.join(STARTUP_GATE_NAME),
            lifetime_lease: directory.join(RUNTIME_LEASE_NAME),
            control_socket: control_directory.join(CONTROL_SOCKET_NAME),
            metadata: directory.join(RUNTIME_METADATA_NAME),
            control_directory,
            directory,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeMetadata {
    schema_version: u8,
    state: RuntimeMetadataState,
    pid: u32,
    session_key: String,
    socket_path: String,
    socket_identity_hex: String,
    launch_id: String,
    tabby_version: String,
    binary_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeMetadataState {
    Ready,
}

#[derive(Debug, Serialize, Deserialize)]
struct ControlRequest {
    schema_version: u8,
    session_key: String,
    launch_id: String,
    trigger: RefreshTrigger,
}

#[derive(Debug, Serialize, Deserialize)]
struct ControlReply {
    accepted: bool,
    pid: u32,
    launch_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Default)]
struct TriggerMailboxState {
    pending: Option<RefreshTrigger>,
}

#[derive(Clone)]
struct TriggerSender {
    shared: Arc<(Mutex<TriggerMailboxState>, Condvar)>,
}

struct TriggerReceiver {
    shared: Arc<(Mutex<TriggerMailboxState>, Condvar)>,
}

fn trigger_mailbox() -> (TriggerSender, TriggerReceiver) {
    let shared = Arc::new((Mutex::new(TriggerMailboxState::default()), Condvar::new()));
    (
        TriggerSender {
            shared: Arc::clone(&shared),
        },
        TriggerReceiver { shared },
    )
}

impl TriggerSender {
    fn send(&self, trigger: RefreshTrigger) {
        let (lock, ready) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if !matches!(
            (state.pending, trigger),
            (Some(existing), RefreshTrigger::Creation) if existing != RefreshTrigger::Creation
        ) {
            state.pending = Some(trigger);
        }
        ready.notify_one();
    }
}

impl TriggerReceiver {
    fn recv_until(&self, deadline: Option<Instant>) -> Option<RefreshTrigger> {
        let (lock, ready) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some(trigger) = state.pending.take() {
                return Some(trigger);
            }
            match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return None;
                    }
                    let (next_state, timeout) = ready
                        .wait_timeout(state, remaining)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state = next_state;
                    if timeout.timed_out() && state.pending.is_none() {
                        return None;
                    }
                }
                None => {
                    state = ready
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            }
        }
    }
}

pub trait StartupGateGuard {}

pub trait SessionRuntimeAdapter {
    fn acquire_startup_gate(
        &mut self,
        path: &Path,
    ) -> Result<Box<dyn StartupGateGuard>, SessionRuntimeError>;

    fn signal_ready_owner(
        &mut self,
        launch: &SessionRuntimeLaunch<'_>,
        trigger: RefreshTrigger,
    ) -> Result<Option<ReadySessionRuntime>, SessionRuntimeError>;

    fn start_owner_and_wait_ready(
        &mut self,
        launch: &SessionRuntimeLaunch<'_>,
        trigger: RefreshTrigger,
    ) -> Result<ReadySessionRuntime, SessionRuntimeError>;
}

pub fn ensure_ready_owner_with<A>(
    launch: &SessionRuntimeLaunch<'_>,
    trigger: RefreshTrigger,
    adapter: &mut A,
) -> Result<EnsureRuntimeOutcome, SessionRuntimeError>
where
    A: SessionRuntimeAdapter,
{
    let paths = RuntimePaths::for_launch(launch);
    let _gate = adapter.acquire_startup_gate(&paths.startup_gate)?;

    if let Some(owner) = adapter.signal_ready_owner(launch, trigger)? {
        return Ok(EnsureRuntimeOutcome::ReadyOwnerSignaled(owner));
    }

    adapter
        .start_owner_and_wait_ready(launch, trigger)
        .map(EnsureRuntimeOutcome::NewOwnerReady)
}

pub fn ensure_ready_owner_from_env(trigger: RefreshTrigger) -> Result<String, SessionRuntimeError> {
    let socket = crate::startup::resolve_socket_from_env()?;
    let state_base = crate::startup::state_base_from_runtime()?;
    let binary_path = std::env::current_exe().map_err(SessionRuntimeError::CurrentExe)?;
    let launch = SessionRuntimeLaunch {
        socket: &socket,
        state_base: &state_base,
        binary_path: &binary_path,
    };
    let mut adapter = SystemSessionRuntimeAdapter::default();
    let outcome = ensure_ready_owner_with(&launch, trigger, &mut adapter)?;
    Ok(match outcome {
        EnsureRuntimeOutcome::ReadyOwnerSignaled(owner) => format!(
            "tabby Session Runtime ready with pid {} (signal delivered)",
            owner.pid
        ),
        EnsureRuntimeOutcome::NewOwnerReady(owner) => format!(
            "started Tabby Session Runtime with pid {} and confirmed readiness",
            owner.pid
        ),
    })
}

#[derive(Default)]
struct SystemSessionRuntimeAdapter {
    child: Option<Child>,
}

impl StartupGateGuard for LifetimeLease {}

impl SessionRuntimeAdapter for SystemSessionRuntimeAdapter {
    fn acquire_startup_gate(
        &mut self,
        path: &Path,
    ) -> Result<Box<dyn StartupGateGuard>, SessionRuntimeError> {
        let parent = path.parent().ok_or_else(|| {
            SessionRuntimeError::Control("startup gate has no parent directory".to_string())
        })?;
        if let Some(runtimes_root) = parent.parent() {
            if let Some(state_base) = runtimes_root.parent() {
                ensure_private_directory(state_base)?;
            }
            ensure_private_directory(runtimes_root)?;
        }
        ensure_private_directory(parent)?;

        let deadline = Instant::now() + STARTUP_GATE_TIMEOUT;
        loop {
            if let Some(lease) = LifetimeLease::try_acquire(path)? {
                return Ok(Box::new(lease));
            }
            if Instant::now() >= deadline {
                return Err(SessionRuntimeError::StartupGateBusy(path.to_path_buf()));
            }
            thread::park_timeout(Duration::from_millis(25));
        }
    }

    fn signal_ready_owner(
        &mut self,
        launch: &SessionRuntimeLaunch<'_>,
        trigger: RefreshTrigger,
    ) -> Result<Option<ReadySessionRuntime>, SessionRuntimeError> {
        signal_ready_owner(launch, trigger)
    }

    fn start_owner_and_wait_ready(
        &mut self,
        launch: &SessionRuntimeLaunch<'_>,
        trigger: RefreshTrigger,
    ) -> Result<ReadySessionRuntime, SessionRuntimeError> {
        let launch_id = new_launch_id();
        let mut command = Command::new(launch.binary_path);
        command
            .args(["runtime", "--launch-id", &launch_id])
            .env(HERDR_SOCKET_PATH_ENV, &launch.socket.socket_path)
            .env(HERDR_PLUGIN_STATE_DIR_ENV, launch.state_base)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        // SAFETY: the child calls only the async-signal-safe `setsid` between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let child = command.spawn().map_err(SessionRuntimeError::SpawnRuntime)?;
        self.child = Some(child);
        let deadline = Instant::now() + READINESS_TIMEOUT;

        loop {
            if let Some(owner) = signal_ready_owner(launch, trigger)? {
                self.child.take();
                return Ok(owner);
            }

            if let Some(status) = self
                .child
                .as_mut()
                .expect("spawned child")
                .try_wait()
                .map_err(SessionRuntimeError::ChildWait)?
            {
                self.child.take();
                return Err(SessionRuntimeError::ChildExitedBeforeReady(status));
            }

            if Instant::now() >= deadline {
                let mut child = self.child.take().expect("spawned child");
                let _ = child.kill();
                let _ = child.wait();
                return Err(SessionRuntimeError::Readiness(format!(
                    "timed out after {} ms",
                    READINESS_TIMEOUT.as_millis()
                )));
            }
            thread::park_timeout(Duration::from_millis(25));
        }
    }
}

fn signal_ready_owner(
    launch: &SessionRuntimeLaunch<'_>,
    trigger: RefreshTrigger,
) -> Result<Option<ReadySessionRuntime>, SessionRuntimeError> {
    let paths = RuntimePaths::for_launch(launch);
    let metadata = match fs::read(&paths.metadata) {
        Ok(bytes) => serde_json::from_slice::<RuntimeMetadata>(&bytes)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };

    if metadata.schema_version != RUNTIME_METADATA_SCHEMA_VERSION
        || metadata.state != RuntimeMetadataState::Ready
        || metadata.session_key != launch.socket.session_key
        || metadata.socket_identity_hex != launch.socket.identity_hex()
    {
        return Err(SessionRuntimeError::Control(
            "runtime metadata contradicts the requested Session Identity".to_string(),
        ));
    }

    let mut stream = match UnixStream::connect(&paths.control_socket) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    stream.set_read_timeout(Some(CONTROL_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(CONTROL_IO_TIMEOUT))?;
    let request = ControlRequest {
        schema_version: CONTROL_SCHEMA_VERSION,
        session_key: launch.socket.session_key.clone(),
        launch_id: metadata.launch_id.clone(),
        trigger,
    };
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut line = String::new();
    BufReader::new(stream)
        .take(CONTROL_MAX_LINE_BYTES)
        .read_line(&mut line)?;
    let reply: ControlReply = serde_json::from_str(&line)?;
    if !reply.accepted {
        return Err(SessionRuntimeError::Control(
            reply
                .error
                .unwrap_or_else(|| "request rejected".to_string()),
        ));
    }
    if reply.launch_id != metadata.launch_id {
        return Err(SessionRuntimeError::Control(
            "control reply came from a stale runtime launch".to_string(),
        ));
    }
    Ok(Some(ReadySessionRuntime {
        pid: reply.pid,
        launch_id: reply.launch_id,
    }))
}

pub fn run_owned_session_from_env(launch_id: String) -> Result<String, SessionRuntimeError> {
    let socket = crate::startup::resolve_socket_from_env()?;
    let state_base = crate::startup::state_base_from_runtime()?;
    let binary_path = std::env::current_exe().map_err(SessionRuntimeError::CurrentExe)?;
    let launch = SessionRuntimeLaunch {
        socket: &socket,
        state_base: &state_base,
        binary_path: &binary_path,
    };
    run_owned_session(&launch, &launch_id)?;
    Ok("tabby Session Runtime stopped with its Herdr Session".to_string())
}

fn run_owned_session(
    launch: &SessionRuntimeLaunch<'_>,
    launch_id: &str,
) -> Result<(), SessionRuntimeError> {
    validate_live_herdr_contract(launch.socket)?;
    let paths = RuntimePaths::for_launch(launch);
    ensure_private_directory(launch.state_base)?;
    if let Some(runtimes_root) = paths.directory.parent() {
        ensure_private_directory(runtimes_root)?;
    }
    ensure_private_directory(&paths.directory)?;
    let _lease = acquire_runtime_lease(&paths.lifetime_lease, &launch.socket.session_key)?;

    let lock_store_path = lock_store_path_from_runtime()?;
    let mut refresher_state = HybridRefresherState::load(&lock_store_path)?;
    let listener = bind_private_listener(&paths.control_directory, &paths.control_socket)?;
    let (trigger_tx, trigger_rx) = trigger_mailbox();
    spawn_control_acceptor(
        listener,
        launch.socket.session_key.clone(),
        launch_id.to_string(),
        trigger_tx,
    );

    let metadata = RuntimeMetadata {
        schema_version: RUNTIME_METADATA_SCHEMA_VERSION,
        state: RuntimeMetadataState::Ready,
        pid: std::process::id(),
        session_key: launch.socket.session_key.clone(),
        socket_path: launch.socket.socket_path.to_string_lossy().into_owned(),
        socket_identity_hex: launch.socket.identity_hex(),
        launch_id: launch_id.to_string(),
        tabby_version: env!("CARGO_PKG_VERSION").to_string(),
        binary_path: crate::startup::binary_identity(launch.binary_path)
            .to_string_lossy()
            .into_owned(),
    };
    write_metadata(&paths.metadata, &metadata)?;
    let _artifacts = RuntimeArtifacts {
        metadata_path: paths.metadata.clone(),
        control_socket_path: paths.control_socket.clone(),
        launch_id: launch_id.to_string(),
    };

    let transport = UnixSocketTransport::new(&launch.socket.socket_path);
    let mut client = HerdrClient::new(transport);
    run_runtime_loop(
        &mut client,
        &mut refresher_state,
        &lock_store_path,
        trigger_rx,
    )
}

fn acquire_runtime_lease(
    path: &Path,
    session_key: &str,
) -> Result<LifetimeLease, SessionRuntimeError> {
    let deadline = Instant::now() + RUNTIME_LEASE_WAIT;
    loop {
        if let Some(lease) = LifetimeLease::try_acquire(path)? {
            return Ok(lease);
        }
        if Instant::now() >= deadline {
            return Err(SessionRuntimeError::AlreadyOwned(session_key.to_string()));
        }
        thread::park_timeout(Duration::from_millis(25));
    }
}

fn validate_live_herdr_contract(socket: &SessionSocket) -> Result<(), SessionRuntimeError> {
    let output = Command::new("herdr")
        .args(["status", "--json"])
        .env(HERDR_SOCKET_PATH_ENV, &socket.socket_path)
        .output()
        .map_err(SessionRuntimeError::HerdrStatusIo)?;
    if !output.status.success() {
        return Err(SessionRuntimeError::HerdrStatusFailed {
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let status: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    validate_herdr_status(&status, socket)
}

fn validate_herdr_status(
    status: &serde_json::Value,
    socket: &SessionSocket,
) -> Result<(), SessionRuntimeError> {
    let server = status
        .get("server")
        .ok_or_else(|| SessionRuntimeError::HerdrContract("missing server status".to_string()))?;
    let running = server
        .get("running")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let version = server
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let protocol = server.get("protocol").and_then(serde_json::Value::as_u64);
    let reported_socket = server
        .get("socket")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    if !running
        || !version_at_least_0_8_0(version)
        || protocol != Some(19)
        || reported_socket != socket.socket_path.to_string_lossy()
    {
        return Err(SessionRuntimeError::HerdrContract(format!(
            "expected running Herdr >=0.8.0 protocol 19 at `{}`, got version `{version}`, protocol {protocol:?}, socket `{reported_socket}`",
            socket.socket_path.display()
        )));
    }
    Ok(())
}

fn version_at_least_0_8_0(version: &str) -> bool {
    let mut parts = version.split('.');
    let major = parts.next().and_then(|part| part.parse::<u64>().ok());
    let minor = parts.next().and_then(|part| part.parse::<u64>().ok());
    matches!((major, minor), (Some(major), Some(minor)) if major > 0 || minor >= 8)
}

fn run_runtime_loop(
    herdr: &mut impl crate::herdr_client::HerdrApi,
    state: &mut HybridRefresherState,
    lock_store_path: &Path,
    triggers: TriggerReceiver,
) -> Result<(), SessionRuntimeError> {
    let mut next_tick_at = Some(Instant::now() + daemon::DEFAULT_HYBRID_IDLE_POLL_INTERVAL);

    loop {
        let trigger = triggers.recv_until(next_tick_at);

        if let Some(trigger) = trigger {
            if trigger == RefreshTrigger::Creation {
                continue;
            }
            let now = Instant::now();
            state.note_focus_or_create_event(now);
            next_tick_at = Some(now + daemon::DEFAULT_FOCUS_QUIET_WINDOW);
            continue;
        }

        let now = Instant::now();
        match daemon::hybrid_tick_and_save_locks(herdr, state, lock_store_path, now) {
            Ok(_) => {}
            Err(error) if error.proves_session_stop() => return Ok(()),
            Err(DaemonError::Herdr(_)) => {
                // Ambiguous transport/application failures end this evaluation only.
            }
            Err(error) => return Err(error.into()),
        }
        next_tick_at = Some(now + daemon::DEFAULT_HYBRID_IDLE_POLL_INTERVAL);
    }
}

fn spawn_control_acceptor(
    listener: UnixListener,
    session_key: String,
    launch_id: String,
    triggers: TriggerSender,
) {
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else {
                break;
            };
            let reply = handle_control_request(&mut stream, &session_key, &launch_id, &triggers);
            let _ = serde_json::to_writer(&mut stream, &reply);
            let _ = stream.write_all(b"\n");
            let _ = stream.flush();
        }
    });
}

fn handle_control_request(
    stream: &mut UnixStream,
    session_key: &str,
    launch_id: &str,
    triggers: &TriggerSender,
) -> ControlReply {
    let reject = |message: String| ControlReply {
        accepted: false,
        pid: std::process::id(),
        launch_id: launch_id.to_string(),
        error: Some(message),
    };

    match peer_uid(stream) {
        Ok(uid) if uid == unsafe { libc::geteuid() } => {}
        Ok(_) => return reject("control peer is not the runtime owner".to_string()),
        Err(error) => return reject(format!("could not validate control peer: {error}")),
    }
    if let Err(error) = stream.set_read_timeout(Some(CONTROL_IO_TIMEOUT)) {
        return reject(format!("could not bound control read: {error}"));
    }
    let mut line = String::new();
    if let Err(error) = BufReader::new(&mut *stream)
        .take(CONTROL_MAX_LINE_BYTES)
        .read_line(&mut line)
    {
        return reject(format!("could not read control request: {error}"));
    }
    let request: ControlRequest = match serde_json::from_str(&line) {
        Ok(request) => request,
        Err(error) => return reject(format!("invalid control request: {error}")),
    };
    if request.schema_version != CONTROL_SCHEMA_VERSION
        || request.session_key != session_key
        || request.launch_id != launch_id
    {
        return reject("control request identity does not match the Ready owner".to_string());
    }
    triggers.send(request.trigger);

    ControlReply {
        accepted: true,
        pid: std::process::id(),
        launch_id: launch_id.to_string(),
        error: None,
    }
}

fn write_metadata(path: &Path, metadata: &RuntimeMetadata) -> Result<(), SessionRuntimeError> {
    let temp_path = path.with_extension(format!("{}.tmp", metadata.launch_id));
    let contents = serde_json::to_vec_pretty(metadata)?;
    let mut temp = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    temp.set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(&contents)?;
    temp.sync_all()?;
    drop(temp);
    fs::rename(temp_path, path)?;
    Ok(())
}

fn new_launch_id() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_LAUNCH_ID.fetch_add(1, Ordering::Relaxed);
    format!("{}-{elapsed:x}-{sequence:x}", std::process::id())
}

struct RuntimeArtifacts {
    metadata_path: PathBuf,
    control_socket_path: PathBuf,
    launch_id: String,
}

impl Drop for RuntimeArtifacts {
    fn drop(&mut self) {
        let owns_metadata = fs::read(&self.metadata_path)
            .ok()
            .and_then(|contents| serde_json::from_slice::<RuntimeMetadata>(&contents).ok())
            .is_some_and(|metadata| metadata.launch_id == self.launch_id);
        if owns_metadata {
            let _ = fs::remove_file(&self.metadata_path);
        }
        let _ = fs::remove_file(&self.control_socket_path);
    }
}

#[derive(Debug)]
pub enum SessionRuntimeError {
    CurrentExe(io::Error),
    Io(std::io::Error),
    Json(serde_json::Error),
    StatePath(crate::paths::StatePathError),
    Startup(crate::startup::StartupError),
    Daemon(DaemonError),
    StartupGateBusy(PathBuf),
    AlreadyOwned(String),
    SpawnRuntime(io::Error),
    ChildWait(io::Error),
    ChildExitedBeforeReady(ExitStatus),
    HerdrStatusIo(io::Error),
    HerdrStatusFailed { status: ExitStatus, stderr: String },
    HerdrContract(String),
    Readiness(String),
    Control(String),
}

impl fmt::Display for SessionRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentExe(error) => {
                write!(formatter, "failed to locate the Tabby executable: {error}")
            }
            Self::Io(error) => write!(formatter, "session runtime I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "session runtime JSON failed: {error}"),
            Self::StatePath(error) => write!(formatter, "session runtime state failed: {error}"),
            Self::Startup(error) => write!(formatter, "session runtime startup failed: {error}"),
            Self::Daemon(error) => write!(formatter, "session runtime evaluation failed: {error}"),
            Self::StartupGateBusy(path) => write!(
                formatter,
                "Session Runtime startup gate `{}` is busy",
                path.display()
            ),
            Self::AlreadyOwned(session_key) => write!(
                formatter,
                "Herdr Session `{session_key}` already has a lease owner"
            ),
            Self::SpawnRuntime(error) => {
                write!(formatter, "failed to spawn the Session Runtime: {error}")
            }
            Self::ChildWait(error) => {
                write!(
                    formatter,
                    "failed to inspect the starting Session Runtime: {error}"
                )
            }
            Self::ChildExitedBeforeReady(status) => write!(
                formatter,
                "Session Runtime exited with {status} before readiness"
            ),
            Self::HerdrStatusIo(error) => {
                write!(formatter, "failed to inspect the Herdr runtime: {error}")
            }
            Self::HerdrStatusFailed { status, stderr } => {
                write!(
                    formatter,
                    "`herdr status --json` failed with {status}: {stderr}"
                )
            }
            Self::HerdrContract(message) => {
                write!(formatter, "Herdr runtime contract mismatch: {message}")
            }
            Self::Readiness(message) => {
                write!(formatter, "Session Runtime readiness failed: {message}")
            }
            Self::Control(message) => {
                write!(formatter, "Session Runtime control failed: {message}")
            }
        }
    }
}

impl std::error::Error for SessionRuntimeError {}

impl From<io::Error> for SessionRuntimeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for SessionRuntimeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<crate::paths::StatePathError> for SessionRuntimeError {
    fn from(error: crate::paths::StatePathError) -> Self {
        Self::StatePath(error)
    }
}

impl From<crate::startup::StartupError> for SessionRuntimeError {
    fn from(error: crate::startup::StartupError) -> Self {
        Self::Startup(error)
    }
}

impl From<DaemonError> for SessionRuntimeError {
    fn from(error: DaemonError) -> Self {
        Self::Daemon(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[derive(Default)]
    struct FakeAdapter {
        acquired_paths: Vec<PathBuf>,
        signaled: Vec<RefreshTrigger>,
        spawned: usize,
        ready_owner: Option<ReadySessionRuntime>,
    }

    struct FakeGuard;

    impl StartupGateGuard for FakeGuard {}

    impl SessionRuntimeAdapter for FakeAdapter {
        fn acquire_startup_gate(
            &mut self,
            path: &Path,
        ) -> Result<Box<dyn StartupGateGuard>, SessionRuntimeError> {
            self.acquired_paths.push(path.to_path_buf());
            Ok(Box::new(FakeGuard))
        }

        fn signal_ready_owner(
            &mut self,
            _launch: &SessionRuntimeLaunch<'_>,
            trigger: RefreshTrigger,
        ) -> Result<Option<ReadySessionRuntime>, SessionRuntimeError> {
            self.signaled.push(trigger);
            Ok(self.ready_owner.clone())
        }

        fn start_owner_and_wait_ready(
            &mut self,
            _launch: &SessionRuntimeLaunch<'_>,
            _trigger: RefreshTrigger,
        ) -> Result<ReadySessionRuntime, SessionRuntimeError> {
            self.spawned += 1;
            Ok(ReadySessionRuntime {
                pid: 202,
                launch_id: "new-owner".to_string(),
            })
        }
    }

    #[test]
    fn startup_gate_signals_a_ready_owner_without_spawning() {
        let socket = SessionSocket::resolve("/tmp/herdr.sock").expect("socket");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: Path::new("/tmp/tabby-state"),
            binary_path: Path::new("/tmp/tabby"),
        };
        let owner = ReadySessionRuntime {
            pid: 101,
            launch_id: "ready-owner".to_string(),
        };
        let mut adapter = FakeAdapter {
            ready_owner: Some(owner.clone()),
            ..FakeAdapter::default()
        };

        let outcome = ensure_ready_owner_with(&launch, RefreshTrigger::Focus, &mut adapter)
            .expect("ready owner");

        assert_eq!(outcome, EnsureRuntimeOutcome::ReadyOwnerSignaled(owner));
        assert_eq!(adapter.signaled, vec![RefreshTrigger::Focus]);
        assert_eq!(adapter.spawned, 0);
        assert_eq!(adapter.acquired_paths.len(), 1);
    }

    #[test]
    fn startup_gate_waits_for_a_new_ready_owner_when_none_is_ready() {
        let socket = SessionSocket::resolve("/tmp/new-herdr.sock").expect("socket");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: Path::new("/tmp/tabby-state"),
            binary_path: Path::new("/tmp/tabby"),
        };
        let mut adapter = FakeAdapter::default();

        let outcome = ensure_ready_owner_with(&launch, RefreshTrigger::Startup, &mut adapter)
            .expect("new ready owner");

        assert_eq!(
            outcome,
            EnsureRuntimeOutcome::NewOwnerReady(ReadySessionRuntime {
                pid: 202,
                launch_id: "new-owner".to_string(),
            })
        );
        assert_eq!(adapter.signaled, vec![RefreshTrigger::Startup]);
        assert_eq!(adapter.spawned, 1);
    }

    #[test]
    fn control_endpoint_delivers_a_validated_trigger_to_the_ready_owner() {
        let unique = NEXT_LAUNCH_ID.fetch_add(1, Ordering::Relaxed);
        let state_base =
            PathBuf::from("/tmp").join(format!("tby-control-test-{}-{unique}", std::process::id()));
        fs::create_dir(&state_base).expect("state base");
        let socket = SessionSocket::resolve(state_base.join("herdr.sock")).expect("socket");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: &state_base,
            binary_path: Path::new("/tmp/tabby"),
        };
        let paths = RuntimePaths::for_launch(&launch);
        fs::create_dir_all(&paths.directory).expect("runtime directory");
        let listener = bind_private_listener(&paths.control_directory, &paths.control_socket)
            .expect("control listener");
        let (sender, receiver) = trigger_mailbox();
        spawn_control_acceptor(
            listener,
            socket.session_key.clone(),
            "ready-launch".to_string(),
            sender,
        );
        write_metadata(
            &paths.metadata,
            &RuntimeMetadata {
                schema_version: RUNTIME_METADATA_SCHEMA_VERSION,
                state: RuntimeMetadataState::Ready,
                pid: std::process::id(),
                session_key: socket.session_key.clone(),
                socket_path: socket.socket_path.to_string_lossy().into_owned(),
                socket_identity_hex: socket.identity_hex(),
                launch_id: "ready-launch".to_string(),
                tabby_version: env!("CARGO_PKG_VERSION").to_string(),
                binary_path: "/tmp/tabby".to_string(),
            },
        )
        .expect("ready metadata");

        let owner = signal_ready_owner(&launch, RefreshTrigger::Focus)
            .expect("control request")
            .expect("ready owner");

        assert_eq!(owner.pid, std::process::id());
        assert_eq!(owner.launch_id, "ready-launch");
        assert_eq!(
            receiver
                .recv_until(Some(Instant::now() + Duration::from_secs(1)))
                .expect("delivered trigger"),
            RefreshTrigger::Focus
        );

        let _ = fs::remove_file(&paths.control_socket);
        let _ = fs::remove_dir_all(&paths.control_directory);
        fs::remove_dir_all(&state_base).expect("remove state base");
    }

    #[test]
    fn readiness_requires_the_confirmed_herdr_0_8_protocol_contract() {
        let socket = SessionSocket::resolve("/tmp/herdr-contract.sock").expect("socket");
        let valid = serde_json::json!({
            "server": {
                "running": true,
                "version": "0.8.0",
                "protocol": 19,
                "socket": "/tmp/herdr-contract.sock"
            }
        });
        validate_herdr_status(&valid, &socket).expect("accepted Herdr contract");

        let wrong_protocol = serde_json::json!({
            "server": {
                "running": true,
                "version": "0.8.0",
                "protocol": 18,
                "socket": "/tmp/herdr-contract.sock"
            }
        });
        assert!(validate_herdr_status(&wrong_protocol, &socket).is_err());
    }

    #[test]
    fn trigger_mailbox_coalesces_to_the_latest_actionable_trigger() {
        let (sender, receiver) = trigger_mailbox();

        sender.send(RefreshTrigger::Focus);
        sender.send(RefreshTrigger::Creation);
        sender.send(RefreshTrigger::Manual);

        assert_eq!(
            receiver.recv_until(Some(Instant::now())),
            Some(RefreshTrigger::Manual)
        );
        assert_eq!(receiver.recv_until(Some(Instant::now())), None);
    }
}
