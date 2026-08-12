//! One Ready Session Runtime per running Herdr Session.

mod unix;

use crate::herdr_client::{HerdrClient, UnixSocketTransport};
use crate::paths::HERDR_PLUGIN_STATE_DIR_ENV;
use crate::refresh_executor::{self, OneShotRefreshState, RefreshExecutionError};
use crate::startup::SessionSocket;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use self::unix::{
    LifetimeLease, bind_private_listener, ensure_private_directory, peer_executable_identity,
    peer_uid,
};

const HERDR_SOCKET_PATH_ENV: &str = "HERDR_SOCKET_PATH";
const RUNTIMES_DIR_NAME: &str = "session-runtimes";
const CONTROL_SOCKET_NAME: &str = "control.sock";
const RUNTIME_METADATA_NAME: &str = "runtime.json";
const STARTUP_GATE_NAME: &str = "startup.lock";
const RUNTIME_LEASE_NAME: &str = "runtime.lease";
const CONTROL_SCHEMA_VERSION: u8 = 1;
const RUNTIME_METADATA_SCHEMA_VERSION: u8 = 1;
const READINESS_TIMEOUT: Duration = Duration::from_secs(2);
const RUNTIME_LEASE_WAIT: Duration = Duration::from_secs(1);
const CONTROL_IO_TIMEOUT: Duration = Duration::from_secs(1);
const CONTROL_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_MAX_LINE_BYTES: u64 = 64 * 1024;
const CONTROL_WORKER_COUNT: usize = 4;
const CONTROL_WORKER_QUEUE: usize = 16;
const CONTROL_RECENT_REQUEST_IDS: usize = 128;
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);

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
    Busy,
    TimedOut,
    Faulted { diagnostic: String },
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_evaluation_unix_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_periodic_unix_ms: Option<u128>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeMetadataState {
    Ready,
    Faulted,
}

/// Read-only lifecycle information for one selected Herdr Session.
///
/// This inspection never signals, starts, repairs, or otherwise mutates a runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeInspection {
    Absent,
    Starting {
        lease_held: bool,
    },
    Ready {
        pid: u32,
        launch_id: String,
        version: String,
        binary_path: PathBuf,
        lease_held: bool,
        last_evaluation_unix_ms: Option<u128>,
        last_failure: Option<String>,
        next_periodic_unix_ms: Option<u128>,
    },
    Faulted {
        diagnostic: String,
        lease_held: bool,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct ControlRequest {
    schema_version: u8,
    session_key: String,
    socket_identity_hex: String,
    launch_id: String,
    request_id: String,
    operation: RuntimeControlOperation,
}

#[derive(Debug, Serialize, Deserialize)]
struct ControlReply {
    accepted: bool,
    pid: u32,
    launch_id: String,
    request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip)]
    handoff_reply_written: Option<mpsc::SyncSender<()>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeControlOperation {
    Signal { trigger: RefreshTrigger },
    UnlockFocused,
    UnlockAll,
    RepairStateDiscard,
    PrepareHandoff { replacement_binary_identity: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeMutation {
    UnlockFocused,
    UnlockAll,
    RepairStateDiscard,
    PrepareHandoff { replacement_binary_identity: String },
}

struct RuntimeCommand {
    mutation: RuntimeMutation,
    completion: mpsc::SyncSender<Result<(), String>>,
    handoff_reply_written: Option<mpsc::Receiver<()>>,
}

struct QueuedControlCommand {
    completion: mpsc::Receiver<Result<(), String>>,
    handoff_reply_written: Option<mpsc::SyncSender<()>>,
}

enum RuntimeControlEvent {
    Trigger(RefreshTrigger),
    Command(RuntimeCommand),
}

#[derive(Default)]
struct TriggerMailboxState {
    pending_trigger: Option<RefreshTrigger>,
    commands: VecDeque<RuntimeCommand>,
    handoff_requested: bool,
}

#[derive(Default)]
struct RecentControlRequests {
    ids: VecDeque<String>,
}

impl RecentControlRequests {
    fn remember(&mut self, request_id: &str) -> bool {
        if self.ids.iter().any(|known| known == request_id) {
            return false;
        }
        self.ids.push_back(request_id.to_string());
        if self.ids.len() > CONTROL_RECENT_REQUEST_IDS {
            self.ids.pop_front();
        }
        true
    }
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
    fn send_trigger(&self, trigger: RefreshTrigger) -> Result<(), String> {
        let (lock, ready) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.handoff_requested {
            return Err("Session Runtime is preparing a cooperative handoff".to_string());
        }
        match (state.pending_trigger, trigger) {
            (Some(existing), RefreshTrigger::Creation) if existing != RefreshTrigger::Creation => {}
            (_, trigger) => state.pending_trigger = Some(trigger),
        }
        ready.notify_one();
        Ok(())
    }

    fn enqueue_mutation(&self, mutation: RuntimeMutation) -> Result<QueuedControlCommand, String> {
        let (lock, ready) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.handoff_requested {
            return Err("Session Runtime is preparing a cooperative handoff".to_string());
        }
        if matches!(mutation, RuntimeMutation::PrepareHandoff { .. }) {
            state.handoff_requested = true;
        }
        let (completion, result) = mpsc::sync_channel(1);
        let (handoff_reply_written, owner_wait) =
            if matches!(mutation, RuntimeMutation::PrepareHandoff { .. }) {
                let (reply_written, owner_wait) = mpsc::sync_channel(0);
                (Some(reply_written), Some(owner_wait))
            } else {
                (None, None)
            };
        state.commands.push_back(RuntimeCommand {
            mutation,
            completion,
            handoff_reply_written: owner_wait,
        });
        ready.notify_one();
        Ok(QueuedControlCommand {
            completion: result,
            handoff_reply_written,
        })
    }
}

impl TriggerReceiver {
    fn recv_until(&self, deadline: Option<Instant>) -> Option<RuntimeControlEvent> {
        let (lock, ready) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some(command) = state.commands.pop_front() {
                return Some(RuntimeControlEvent::Command(command));
            }
            if let Some(trigger) = state.pending_trigger.take() {
                return Some(RuntimeControlEvent::Trigger(trigger));
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
                    if timeout.timed_out()
                        && state.pending_trigger.is_none()
                        && state.commands.is_empty()
                    {
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
    let _gate = match adapter.acquire_startup_gate(&paths.startup_gate) {
        Ok(gate) => gate,
        Err(error) => return Ok(error.into_gate_outcome()),
    };

    match adapter.signal_ready_owner(launch, trigger) {
        Ok(Some(owner)) => return Ok(EnsureRuntimeOutcome::ReadyOwnerSignaled(owner)),
        Ok(None) => {}
        Err(error) => return Ok(error.into_gate_outcome()),
    }

    match adapter.start_owner_and_wait_ready(launch, trigger) {
        Ok(owner) => Ok(EnsureRuntimeOutcome::NewOwnerReady(owner)),
        Err(error) => Ok(error.into_gate_outcome()),
    }
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
        EnsureRuntimeOutcome::Busy => {
            "Tabby Session Runtime startup gate is busy; another hook is ensuring readiness"
                .to_string()
        }
        EnsureRuntimeOutcome::TimedOut => {
            return Err(SessionRuntimeError::Readiness(
                "timed out while waiting for the Session Runtime to become ready".to_string(),
            ));
        }
        EnsureRuntimeOutcome::Faulted { diagnostic } => {
            return Err(SessionRuntimeError::Control(diagnostic));
        }
    })
}

/// Delivers a manual Refresh Trigger through the Startup Gate and control endpoint.
pub fn signal_manual_refresh_from_env() -> Result<String, SessionRuntimeError> {
    ensure_ready_owner_from_env(RefreshTrigger::Manual)
}

/// Ensures the invoking binary owns the selected Herdr Session Runtime.
///
/// A Ready owner receives an authenticated handoff request and exits on its own; this path
/// never treats a recorded PID as authority to terminate a process.
pub fn activate_current_runtime_from_env() -> Result<String, SessionRuntimeError> {
    let socket = crate::startup::resolve_socket_from_env()?;
    let state_base = crate::startup::state_base_from_runtime()?;
    let binary_path = std::env::current_exe().map_err(SessionRuntimeError::CurrentExe)?;
    let launch = SessionRuntimeLaunch {
        socket: &socket,
        state_base: &state_base,
        binary_path: &binary_path,
    };
    let mut adapter = SystemSessionRuntimeAdapter::default();
    activate_current_runtime_with(&launch, &mut adapter)
}

trait ActivationAdapter {
    fn acquire_activation_gate(
        &mut self,
        path: &Path,
    ) -> Result<Box<dyn StartupGateGuard>, SessionRuntimeError>;
    fn inspect_runtime(
        &mut self,
        launch: &SessionRuntimeLaunch<'_>,
    ) -> Result<RuntimeInspection, SessionRuntimeError>;
    fn request_ready_owner(
        &mut self,
        launch: &SessionRuntimeLaunch<'_>,
        operation: RuntimeControlOperation,
    ) -> Result<Option<ReadySessionRuntime>, SessionRuntimeError>;
    fn wait_for_runtime_release(&mut self, paths: &RuntimePaths)
    -> Result<(), SessionRuntimeError>;
    fn start_activation_owner(
        &mut self,
        launch: &SessionRuntimeLaunch<'_>,
        trigger: RefreshTrigger,
    ) -> Result<ReadySessionRuntime, SessionRuntimeError>;
}

impl ActivationAdapter for SystemSessionRuntimeAdapter {
    fn acquire_activation_gate(
        &mut self,
        path: &Path,
    ) -> Result<Box<dyn StartupGateGuard>, SessionRuntimeError> {
        SessionRuntimeAdapter::acquire_startup_gate(self, path)
    }

    fn inspect_runtime(
        &mut self,
        launch: &SessionRuntimeLaunch<'_>,
    ) -> Result<RuntimeInspection, SessionRuntimeError> {
        inspect_runtime(launch)
    }

    fn request_ready_owner(
        &mut self,
        launch: &SessionRuntimeLaunch<'_>,
        operation: RuntimeControlOperation,
    ) -> Result<Option<ReadySessionRuntime>, SessionRuntimeError> {
        request_ready_owner(launch, operation)
    }

    fn wait_for_runtime_release(
        &mut self,
        paths: &RuntimePaths,
    ) -> Result<(), SessionRuntimeError> {
        wait_for_runtime_release(paths)
    }

    fn start_activation_owner(
        &mut self,
        launch: &SessionRuntimeLaunch<'_>,
        trigger: RefreshTrigger,
    ) -> Result<ReadySessionRuntime, SessionRuntimeError> {
        SessionRuntimeAdapter::start_owner_and_wait_ready(self, launch, trigger)
    }
}

fn activate_current_runtime_with(
    launch: &SessionRuntimeLaunch<'_>,
    adapter: &mut impl ActivationAdapter,
) -> Result<String, SessionRuntimeError> {
    let paths = RuntimePaths::for_launch(launch);
    let _gate = adapter.acquire_activation_gate(&paths.startup_gate)?;

    match adapter.inspect_runtime(launch)? {
        RuntimeInspection::Ready { binary_path, .. }
            if binary_path == crate::startup::binary_identity(launch.binary_path) =>
        {
            let owner = adapter
                .request_ready_owner(
                    launch,
                    RuntimeControlOperation::Signal {
                        trigger: RefreshTrigger::Startup,
                    },
                )?
                .ok_or_else(|| {
                    SessionRuntimeError::Control(
                        "Ready owner disappeared during activation verification".to_string(),
                    )
                })?;
            return Ok(format!(
                "current Tabby Session Runtime already ready with pid {}",
                owner.pid
            ));
        }
        RuntimeInspection::Ready { .. } => {
            let replacement_binary_identity = crate::startup::binary_identity(launch.binary_path)
                .to_string_lossy()
                .into_owned();
            adapter
                .request_ready_owner(
                    launch,
                    RuntimeControlOperation::PrepareHandoff {
                        replacement_binary_identity,
                    },
                )?
                .ok_or_else(|| {
                    SessionRuntimeError::Control(
                        "Ready owner did not accept cooperative handoff".to_string(),
                    )
                })?;
            adapter.wait_for_runtime_release(&paths)?;
        }
        RuntimeInspection::Absent => {}
        RuntimeInspection::Starting { .. } => {
            return Err(SessionRuntimeError::Control(
                "a Session Runtime is still starting during activation".to_string(),
            ));
        }
        RuntimeInspection::Faulted { diagnostic, .. } => {
            return Err(SessionRuntimeError::Control(diagnostic));
        }
    }

    let owner = adapter.start_activation_owner(launch, RefreshTrigger::Startup)?;
    Ok(format!(
        "started current Tabby Session Runtime with pid {} after cooperative handoff",
        owner.pid
    ))
}

fn wait_for_runtime_release(paths: &RuntimePaths) -> Result<(), SessionRuntimeError> {
    let deadline = Instant::now() + HANDOFF_TIMEOUT;
    while LifetimeLease::is_held(&paths.lifetime_lease)? {
        if Instant::now() >= deadline {
            return Err(SessionRuntimeError::Readiness(
                "timed out waiting for the previous Session Runtime to release its lease"
                    .to_string(),
            ));
        }
        thread::park_timeout(Duration::from_millis(25));
    }
    Ok(())
}

/// Repairs invalid Session-Scoped Tab State only after the explicit discard command.
pub fn repair_session_state_from_env() -> Result<String, SessionRuntimeError> {
    let socket = crate::startup::resolve_socket_from_env()?;
    let state_base = crate::startup::state_base_from_runtime()?;
    let binary_path = std::env::current_exe().map_err(SessionRuntimeError::CurrentExe)?;
    let launch = SessionRuntimeLaunch {
        socket: &socket,
        state_base: &state_base,
        binary_path: &binary_path,
    };
    let paths = RuntimePaths::for_launch(&launch);
    let mut adapter = SystemSessionRuntimeAdapter::default();
    let _gate = adapter.acquire_startup_gate(&paths.startup_gate)?;

    match inspect_runtime(&launch)? {
        RuntimeInspection::Ready { .. } => {
            request_ready_owner(&launch, RuntimeControlOperation::RepairStateDiscard)?.ok_or_else(
                || {
                    SessionRuntimeError::Control(
                        "Ready owner disappeared before repairing Session-Scoped Tab State"
                            .to_string(),
                    )
                },
            )?;
        }
        RuntimeInspection::Absent => {
            crate::locks::SessionTabStateStore::open_for_repair(&state_base, &socket)?
                .repair_discard(crate::locks::RepairConfirmation::confirmed())?;
        }
        RuntimeInspection::Faulted {
            lease_held: false, ..
        } => {
            crate::locks::SessionTabStateStore::open_for_repair(&state_base, &socket)?
                .repair_discard(crate::locks::RepairConfirmation::confirmed())?;
        }
        RuntimeInspection::Starting { .. }
        | RuntimeInspection::Faulted {
            lease_held: true, ..
        } => {
            return Err(SessionRuntimeError::Control(
                "cannot repair Session-Scoped Tab State while its Session Runtime holds a lease"
                    .to_string(),
            ));
        }
    }
    Ok("tabby repair-state: discarded invalid Session-Scoped Tab State".to_string())
}

/// Requests that the Ready Session Runtime unlock the currently focused tab.
pub fn request_unlock_focused_from_env() -> Result<String, SessionRuntimeError> {
    request_runtime_operation_from_env(RuntimeControlOperation::UnlockFocused)
}

/// Requests that the Ready Session Runtime clear all manually locked tabs.
pub fn request_unlock_all_from_env() -> Result<String, SessionRuntimeError> {
    request_runtime_operation_from_env(RuntimeControlOperation::UnlockAll)
}

fn request_runtime_operation_from_env(
    operation: RuntimeControlOperation,
) -> Result<String, SessionRuntimeError> {
    let socket = crate::startup::resolve_socket_from_env()?;
    let state_base = crate::startup::state_base_from_runtime()?;
    let binary_path = std::env::current_exe().map_err(SessionRuntimeError::CurrentExe)?;
    let launch = SessionRuntimeLaunch {
        socket: &socket,
        state_base: &state_base,
        binary_path: &binary_path,
    };
    let mut adapter = SystemSessionRuntimeAdapter::default();
    match ensure_ready_owner_with(&launch, RefreshTrigger::Manual, &mut adapter)? {
        EnsureRuntimeOutcome::ReadyOwnerSignaled(_) | EnsureRuntimeOutcome::NewOwnerReady(_) => {}
        EnsureRuntimeOutcome::Busy => {
            return Err(SessionRuntimeError::StartupGateBusy(
                RuntimePaths::for_launch(&launch).startup_gate,
            ));
        }
        EnsureRuntimeOutcome::TimedOut => {
            return Err(SessionRuntimeError::Readiness(
                "timed out while preparing the Session Runtime control operation".to_string(),
            ));
        }
        EnsureRuntimeOutcome::Faulted { diagnostic } => {
            return Err(SessionRuntimeError::Control(diagnostic));
        }
    }
    request_ready_owner(&launch, operation)?.ok_or_else(|| {
        SessionRuntimeError::Control(
            "Ready owner disappeared before accepting the control operation".to_string(),
        )
    })?;
    Ok("tabby control operation accepted by the Ready Session Runtime".to_string())
}

/// Forgets retained session state only after verifying that the explicitly
/// selected Herdr session and its Tabby runtime are both stopped.
pub fn forget_session_from_env() -> Result<String, SessionRuntimeError> {
    let socket = crate::startup::resolve_stopped_socket_from_env()?;
    let state_base = crate::startup::state_base_from_runtime()?;
    let binary_path = std::env::current_exe().map_err(SessionRuntimeError::CurrentExe)?;
    let launch = SessionRuntimeLaunch {
        socket: &socket,
        state_base: &state_base,
        binary_path: &binary_path,
    };
    forget_stopped_session(&launch)?;
    Ok("tabby forget-session: removed retained Session-Scoped Tab State".to_string())
}

fn forget_stopped_session(launch: &SessionRuntimeLaunch<'_>) -> Result<(), SessionRuntimeError> {
    let paths = RuntimePaths::for_launch(launch);
    if LifetimeLease::is_held(&paths.lifetime_lease)? {
        return Err(SessionRuntimeError::Control(
            "refusing to forget Session-Scoped Tab State while its Session Runtime holds the lifetime lease"
                .to_string(),
        ));
    }
    match UnixStream::connect(&launch.socket.socket_path) {
        Ok(_) => {
            return Err(SessionRuntimeError::Control(
                "refusing to forget Session-Scoped Tab State while the selected Herdr Session is running"
                    .to_string(),
            ));
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) => {}
        Err(error) => return Err(error.into()),
    }
    crate::locks::SessionTabStateStore::open(launch.state_base, launch.socket)?
        .forget_session(crate::locks::RuntimeStoppedConfirmation::confirmed())?;
    Ok(())
}

/// Inspects the selected Session Runtime without starting or signalling it.
pub fn inspect_runtime_from_env() -> Result<RuntimeInspection, SessionRuntimeError> {
    let socket = crate::startup::resolve_socket_from_env()?;
    let state_base = crate::startup::state_base_from_runtime()?;
    let binary_path = std::env::current_exe().map_err(SessionRuntimeError::CurrentExe)?;
    let launch = SessionRuntimeLaunch {
        socket: &socket,
        state_base: &state_base,
        binary_path: &binary_path,
    };
    inspect_runtime(&launch)
}

/// Inspects one Session Runtime at the injectable launch seam.
pub fn inspect_runtime(
    launch: &SessionRuntimeLaunch<'_>,
) -> Result<RuntimeInspection, SessionRuntimeError> {
    let paths = RuntimePaths::for_launch(launch);
    let lease_held = LifetimeLease::is_held(&paths.lifetime_lease)?;
    let metadata = match fs::read(&paths.metadata) {
        Ok(bytes) => match serde_json::from_slice::<RuntimeMetadata>(&bytes) {
            Ok(metadata) => metadata,
            Err(error) => {
                return Ok(RuntimeInspection::Faulted {
                    diagnostic: format!("runtime metadata cannot be decoded: {error}"),
                    lease_held,
                });
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(if lease_held {
                RuntimeInspection::Starting { lease_held }
            } else {
                RuntimeInspection::Absent
            });
        }
        Err(error) => return Err(error.into()),
    };

    if let Err(diagnostic) = validate_runtime_identity(&metadata, launch) {
        return Ok(RuntimeInspection::Faulted {
            diagnostic,
            lease_held,
        });
    }
    if metadata.state == RuntimeMetadataState::Faulted {
        return Ok(RuntimeInspection::Faulted {
            diagnostic: metadata
                .last_failure
                .unwrap_or_else(|| "Session Runtime entered Faulted state".to_string()),
            lease_held,
        });
    }

    let control_is_socket = fs::symlink_metadata(&paths.control_socket)
        .map(|metadata| metadata.file_type().is_socket())
        .unwrap_or(false);
    if !lease_held && !control_is_socket {
        return Ok(RuntimeInspection::Absent);
    }
    if !lease_held || !control_is_socket {
        return Ok(RuntimeInspection::Faulted {
            diagnostic: "runtime metadata is Ready but its lease or control endpoint is absent"
                .to_string(),
            lease_held,
        });
    }

    Ok(RuntimeInspection::Ready {
        pid: metadata.pid,
        launch_id: metadata.launch_id,
        version: metadata.tabby_version,
        binary_path: PathBuf::from(metadata.binary_path),
        lease_held,
        last_evaluation_unix_ms: metadata.last_evaluation_unix_ms,
        last_failure: metadata.last_failure,
        next_periodic_unix_ms: metadata.next_periodic_unix_ms,
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

        LifetimeLease::try_acquire(path)?.map_or_else(
            || Err(SessionRuntimeError::StartupGateBusy(path.to_path_buf())),
            |lease| Ok(Box::new(lease) as Box<dyn StartupGateGuard>),
        )
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
    request_ready_owner(launch, RuntimeControlOperation::Signal { trigger })
}

fn request_ready_owner(
    launch: &SessionRuntimeLaunch<'_>,
    operation: RuntimeControlOperation,
) -> Result<Option<ReadySessionRuntime>, SessionRuntimeError> {
    let paths = RuntimePaths::for_launch(launch);
    let metadata = match fs::read(&paths.metadata) {
        Ok(bytes) => serde_json::from_slice::<RuntimeMetadata>(&bytes)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };

    validate_runtime_metadata(&metadata, launch).map_err(SessionRuntimeError::Control)?;

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
    let response_timeout = match &operation {
        RuntimeControlOperation::Signal { .. } => CONTROL_IO_TIMEOUT,
        RuntimeControlOperation::UnlockFocused
        | RuntimeControlOperation::UnlockAll
        | RuntimeControlOperation::RepairStateDiscard
        | RuntimeControlOperation::PrepareHandoff { .. } => CONTROL_COMMAND_TIMEOUT,
    };
    stream.set_read_timeout(Some(response_timeout))?;
    stream.set_write_timeout(Some(CONTROL_IO_TIMEOUT))?;
    let request = ControlRequest {
        schema_version: CONTROL_SCHEMA_VERSION,
        session_key: launch.socket.session_key.clone(),
        socket_identity_hex: launch.socket.identity_hex(),
        launch_id: metadata.launch_id.clone(),
        request_id: new_launch_id(),
        operation,
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
    if reply.request_id != request.request_id {
        return Err(SessionRuntimeError::Control(
            "control reply does not match the request".to_string(),
        ));
    }
    Ok(Some(ReadySessionRuntime {
        pid: reply.pid,
        launch_id: reply.launch_id,
    }))
}

fn validate_runtime_metadata(
    metadata: &RuntimeMetadata,
    launch: &SessionRuntimeLaunch<'_>,
) -> Result<(), String> {
    validate_runtime_identity(metadata, launch)?;
    if metadata.state != RuntimeMetadataState::Ready {
        return Err("Session Runtime is Faulted and cannot accept control requests".to_string());
    }
    Ok(())
}

fn validate_runtime_identity(
    metadata: &RuntimeMetadata,
    launch: &SessionRuntimeLaunch<'_>,
) -> Result<(), String> {
    if metadata.schema_version != RUNTIME_METADATA_SCHEMA_VERSION
        || metadata.session_key != launch.socket.session_key
        || metadata.socket_path != launch.socket.socket_path.to_string_lossy()
        || metadata.socket_identity_hex != launch.socket.identity_hex()
    {
        return Err("runtime metadata contradicts the requested Session Identity".to_string());
    }
    Ok(())
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

    let tab_state = crate::locks::SessionTabStateStore::open(launch.state_base, launch.socket)?;
    let mut refresh_state =
        OneShotRefreshState::new(refresh_executor::RefreshExecutorState::default());
    let listener = bind_private_listener(&paths.control_directory, &paths.control_socket)?;
    let (trigger_tx, trigger_rx) = trigger_mailbox();
    let binary_identity = crate::startup::binary_identity(launch.binary_path)
        .to_string_lossy()
        .into_owned();
    spawn_control_acceptor(
        listener,
        launch.socket.session_key.clone(),
        launch.socket.identity_hex(),
        launch_id.to_string(),
        binary_identity.clone(),
        trigger_tx,
    );

    let mut metadata = RuntimeMetadata {
        schema_version: RUNTIME_METADATA_SCHEMA_VERSION,
        state: RuntimeMetadataState::Ready,
        pid: std::process::id(),
        session_key: launch.socket.session_key.clone(),
        socket_path: launch.socket.socket_path.to_string_lossy().into_owned(),
        socket_identity_hex: launch.socket.identity_hex(),
        launch_id: launch_id.to_string(),
        tabby_version: env!("CARGO_PKG_VERSION").to_string(),
        binary_path: binary_identity,
        last_evaluation_unix_ms: None,
        last_failure: None,
        next_periodic_unix_ms: Some(unix_time_after(
            refresh_executor::DEFAULT_SESSION_REFRESH_INTERVAL,
        )),
    };
    write_metadata(&paths.metadata, &metadata)?;
    let mut artifacts = RuntimeArtifacts {
        metadata_path: paths.metadata.clone(),
        control_socket_path: paths.control_socket.clone(),
        launch_id: launch_id.to_string(),
        retain_metadata: false,
    };

    let transport = UnixSocketTransport::new(&launch.socket.socket_path);
    let mut client = HerdrClient::new(transport);
    let result = run_runtime_loop(
        &mut client,
        &mut refresh_state,
        &tab_state,
        trigger_rx,
        &paths.metadata,
        &mut metadata,
    );
    if let Err(error) = &result {
        metadata.state = RuntimeMetadataState::Faulted;
        metadata.last_failure = Some(error.to_string());
        metadata.next_periodic_unix_ms = None;
        write_metadata(&paths.metadata, &metadata)?;
        artifacts.retain_metadata = true;
    }
    result
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
    state: &mut OneShotRefreshState,
    tab_state: &crate::locks::SessionTabStateStore,
    triggers: TriggerReceiver,
    metadata_path: &Path,
    metadata: &mut RuntimeMetadata,
) -> Result<(), SessionRuntimeError> {
    // A newly spawned owner always begins with an initial quiet evaluation. This is what makes
    // a creation hook that recovered a missing owner distinct from a creation signal delivered
    // to an already Ready owner, which remains recovery-only below.
    let initial_now = Instant::now();
    state.note_refresh_trigger(initial_now);
    let mut next_tick_at = Some(initial_now + refresh_executor::DEFAULT_FOCUS_QUIET_WINDOW);

    loop {
        let trigger = triggers.recv_until(next_tick_at);

        if let Some(event) = trigger {
            match event {
                RuntimeControlEvent::Command(command) => {
                    let is_handoff =
                        matches!(&command.mutation, RuntimeMutation::PrepareHandoff { .. });
                    let result: Result<(), SessionRuntimeError> = (|| match command.mutation {
                        RuntimeMutation::PrepareHandoff { .. } => Ok(()),
                        RuntimeMutation::UnlockFocused => {
                            if let Some(observation) = herdr
                                .observe_focused_tab()
                                .map_err(RefreshExecutionError::Herdr)?
                            {
                                tab_state
                                    .mutate(|state| state.unlock_tab(&observation.tab.tab_id))?;
                            }
                            let now = Instant::now();
                            state.note_refresh_trigger(now);
                            next_tick_at = Some(now + refresh_executor::DEFAULT_FOCUS_QUIET_WINDOW);
                            Ok(())
                        }
                        RuntimeMutation::UnlockAll => {
                            tab_state.mutate(crate::locks::SessionTabState::unlock_all)?;
                            let now = Instant::now();
                            state.note_refresh_trigger(now);
                            next_tick_at = Some(now + refresh_executor::DEFAULT_FOCUS_QUIET_WINDOW);
                            Ok(())
                        }
                        RuntimeMutation::RepairStateDiscard => {
                            tab_state
                                .repair_discard(crate::locks::RepairConfirmation::confirmed())?;
                            let now = Instant::now();
                            state.note_refresh_trigger(now);
                            next_tick_at = Some(now + refresh_executor::DEFAULT_FOCUS_QUIET_WINDOW);
                            Ok(())
                        }
                    })();
                    let _ = command
                        .completion
                        .send(result.map_err(|error: SessionRuntimeError| error.to_string()));
                    if is_handoff {
                        if let Some(reply_written) = command.handoff_reply_written {
                            let _ = reply_written.recv_timeout(CONTROL_IO_TIMEOUT);
                        }
                        return Ok(());
                    }
                    continue;
                }
                RuntimeControlEvent::Trigger(RefreshTrigger::Creation) => continue,
                RuntimeControlEvent::Trigger(_) => {
                    let now = Instant::now();
                    state.note_refresh_trigger(now);
                    next_tick_at = Some(now + refresh_executor::DEFAULT_FOCUS_QUIET_WINDOW);
                    continue;
                }
            }
        }

        let now = Instant::now();
        match refresh_executor::execute_one_shot(herdr, state, tab_state, now) {
            Ok(_) => {
                metadata.last_evaluation_unix_ms = Some(unix_time_after(Duration::ZERO));
                metadata.last_failure = None;
            }
            Err(error) if error.proves_session_stop() => return Ok(()),
            Err(error @ RefreshExecutionError::Herdr(_)) => {
                // Ambiguous transport/application failures end this evaluation only.
                metadata.last_failure = Some(error.to_string());
            }
            Err(error) => return Err(error.into()),
        }
        next_tick_at = state.next_sample_at().or(Some(
            now + refresh_executor::DEFAULT_SESSION_REFRESH_INTERVAL,
        ));
        metadata.next_periodic_unix_ms = next_tick_at
            .map(|next| unix_time_after(next.saturating_duration_since(Instant::now())));
        write_metadata(metadata_path, metadata)?;
    }
}

fn spawn_control_acceptor(
    listener: UnixListener,
    session_key: String,
    socket_identity_hex: String,
    launch_id: String,
    owner_binary_identity: String,
    triggers: TriggerSender,
) {
    thread::spawn(move || {
        let (work_sender, work_receiver) = mpsc::sync_channel(CONTROL_WORKER_QUEUE);
        let work_receiver = Arc::new(Mutex::new(work_receiver));
        let recent_requests = Arc::new(Mutex::new(RecentControlRequests::default()));
        for _ in 0..CONTROL_WORKER_COUNT {
            let receiver = Arc::clone(&work_receiver);
            let session_key = session_key.clone();
            let socket_identity_hex = socket_identity_hex.clone();
            let launch_id = launch_id.clone();
            let owner_binary_identity = owner_binary_identity.clone();
            let triggers = triggers.clone();
            let recent_requests = Arc::clone(&recent_requests);
            thread::spawn(move || {
                loop {
                    let stream = {
                        let receiver = receiver
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        receiver.recv()
                    };
                    let Ok(mut stream) = stream else {
                        return;
                    };
                    let mut reply = handle_control_request(
                        &mut stream,
                        &session_key,
                        &socket_identity_hex,
                        &launch_id,
                        &owner_binary_identity,
                        &triggers,
                        &recent_requests,
                    );
                    let handoff_reply_written = reply.handoff_reply_written.take();
                    if !write_control_reply(&mut stream, &reply) {
                        continue;
                    }
                    if let Some(handoff_reply_written) = handoff_reply_written {
                        let _ = handoff_reply_written.send(());
                    }
                }
            });
        }

        for incoming in listener.incoming() {
            let Ok(stream) = incoming else {
                break;
            };
            if let Err(error) = work_sender.try_send(stream) {
                let mut stream = match error {
                    mpsc::TrySendError::Full(stream) | mpsc::TrySendError::Disconnected(stream) => {
                        stream
                    }
                };
                let reply = ControlReply {
                    accepted: false,
                    pid: std::process::id(),
                    launch_id: launch_id.clone(),
                    request_id: String::new(),
                    error: Some("runtime control endpoint is busy".to_string()),
                    handoff_reply_written: None,
                };
                let _ = write_control_reply(&mut stream, &reply);
            }
        }
    });
}

fn write_control_reply(stream: &mut UnixStream, reply: &ControlReply) -> bool {
    stream.set_write_timeout(Some(CONTROL_IO_TIMEOUT)).is_ok()
        && serde_json::to_writer(&mut *stream, reply).is_ok()
        && stream.write_all(b"\n").is_ok()
        && stream.flush().is_ok()
}

fn handle_control_request(
    stream: &mut UnixStream,
    session_key: &str,
    socket_identity_hex: &str,
    launch_id: &str,
    owner_binary_identity: &str,
    triggers: &TriggerSender,
    recent_requests: &Mutex<RecentControlRequests>,
) -> ControlReply {
    let reject = |message: String, request_id: String| ControlReply {
        accepted: false,
        pid: std::process::id(),
        launch_id: launch_id.to_string(),
        request_id,
        error: Some(message),
        handoff_reply_written: None,
    };

    match peer_uid(stream) {
        Ok(uid) if uid == unsafe { libc::geteuid() } => {}
        Ok(_) => {
            return reject(
                "control peer is not the runtime owner".to_string(),
                String::new(),
            );
        }
        Err(error) => {
            return reject(
                format!("could not validate control peer: {error}"),
                String::new(),
            );
        }
    }
    if let Err(error) = stream.set_read_timeout(Some(CONTROL_IO_TIMEOUT)) {
        return reject(
            format!("could not bound control read: {error}"),
            String::new(),
        );
    }
    let mut line = String::new();
    if let Err(error) = BufReader::new(&mut *stream)
        .take(CONTROL_MAX_LINE_BYTES)
        .read_line(&mut line)
    {
        return reject(
            format!("could not read control request: {error}"),
            String::new(),
        );
    }
    let request: ControlRequest = match serde_json::from_str(&line) {
        Ok(request) => request,
        Err(error) => return reject(format!("invalid control request: {error}"), String::new()),
    };
    if request.schema_version != CONTROL_SCHEMA_VERSION
        || request.session_key != session_key
        || request.socket_identity_hex != socket_identity_hex
        || request.launch_id != launch_id
    {
        return reject(
            "control request identity does not match the Ready owner".to_string(),
            request.request_id,
        );
    }
    if !recent_requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remember(&request.request_id)
    {
        return reject(
            "duplicate control request was rejected".to_string(),
            request.request_id,
        );
    }
    let completion = match request.operation {
        RuntimeControlOperation::Signal { trigger } => {
            if let Err(error) = triggers.send_trigger(trigger) {
                return reject(error, request.request_id);
            }
            None
        }
        RuntimeControlOperation::UnlockFocused => {
            Some(triggers.enqueue_mutation(RuntimeMutation::UnlockFocused))
        }
        RuntimeControlOperation::UnlockAll => {
            Some(triggers.enqueue_mutation(RuntimeMutation::UnlockAll))
        }
        RuntimeControlOperation::RepairStateDiscard => {
            Some(triggers.enqueue_mutation(RuntimeMutation::RepairStateDiscard))
        }
        RuntimeControlOperation::PrepareHandoff {
            replacement_binary_identity,
        } => {
            if let Err(error) = validate_replacement_binary_identity(
                stream,
                &replacement_binary_identity,
                owner_binary_identity,
            ) {
                return reject(error, request.request_id);
            }
            Some(triggers.enqueue_mutation(RuntimeMutation::PrepareHandoff {
                replacement_binary_identity,
            }))
        }
    };
    let mut handoff_reply_written = None;
    if let Some(completion) = completion {
        let completion = match completion {
            Ok(completion) => completion,
            Err(error) => return reject(error, request.request_id),
        };
        match completion.completion.recv_timeout(CONTROL_COMMAND_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return reject(error, request.request_id),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return reject(
                    "Session Runtime did not complete the control operation in time".to_string(),
                    request.request_id,
                );
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return reject(
                    "Session Runtime stopped before completing the control operation".to_string(),
                    request.request_id,
                );
            }
        }
        handoff_reply_written = completion.handoff_reply_written;
    }

    ControlReply {
        accepted: true,
        pid: std::process::id(),
        launch_id: launch_id.to_string(),
        request_id: request.request_id,
        error: None,
        handoff_reply_written,
    }
}

fn validate_replacement_binary_identity(
    stream: &UnixStream,
    replacement_binary_identity: &str,
    owner_binary_identity: &str,
) -> Result<(), String> {
    validate_declared_replacement_binary_identity(
        replacement_binary_identity,
        owner_binary_identity,
    )?;
    let peer_binary_identity = crate::startup::binary_identity(
        &peer_executable_identity(stream)
            .map_err(|error| format!("could not validate replacement peer executable: {error}"))?,
    )
    .to_string_lossy()
    .into_owned();
    if peer_binary_identity != replacement_binary_identity {
        return Err(
            "replacement executable identity does not match the executing control peer".to_string(),
        );
    }
    Ok(())
}

fn validate_declared_replacement_binary_identity(
    replacement_binary_identity: &str,
    owner_binary_identity: &str,
) -> Result<(), String> {
    let replacement = Path::new(replacement_binary_identity);
    if !replacement.is_absolute() {
        return Err("replacement executable identity must be an absolute path".to_string());
    }
    let metadata = fs::metadata(replacement)
        .map_err(|error| format!("replacement executable identity cannot be validated: {error}"))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(
            "replacement executable identity is not an executable regular file".to_string(),
        );
    }
    let canonical = crate::startup::binary_identity(replacement);
    if canonical.to_string_lossy() != replacement_binary_identity {
        return Err("replacement executable identity is not canonical".to_string());
    }
    if replacement_binary_identity == owner_binary_identity {
        return Err(
            "cooperative handoff requires a different Tabby executable identity".to_string(),
        );
    }
    Ok(())
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

fn unix_time_after(delay: Duration) -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .saturating_add(delay)
        .as_millis()
}

struct RuntimeArtifacts {
    metadata_path: PathBuf,
    control_socket_path: PathBuf,
    launch_id: String,
    retain_metadata: bool,
}

impl Drop for RuntimeArtifacts {
    fn drop(&mut self) {
        let owns_metadata = fs::read(&self.metadata_path)
            .ok()
            .and_then(|contents| serde_json::from_slice::<RuntimeMetadata>(&contents).ok())
            .is_some_and(|metadata| metadata.launch_id == self.launch_id);
        if owns_metadata && !self.retain_metadata {
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
    SessionTabState(crate::locks::SessionTabStateError),
    Startup(crate::startup::StartupError),
    RefreshExecution(RefreshExecutionError),
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
            Self::SessionTabState(error) => {
                write!(
                    formatter,
                    "session runtime Session-Scoped Tab State failed: {error}"
                )
            }
            Self::Startup(error) => write!(formatter, "session runtime startup failed: {error}"),
            Self::RefreshExecution(error) => {
                write!(formatter, "session runtime evaluation failed: {error}")
            }
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

impl SessionRuntimeError {
    fn into_gate_outcome(self) -> EnsureRuntimeOutcome {
        match self {
            Self::StartupGateBusy(_) => EnsureRuntimeOutcome::Busy,
            Self::Readiness(_) => EnsureRuntimeOutcome::TimedOut,
            error => EnsureRuntimeOutcome::Faulted {
                diagnostic: error.to_string(),
            },
        }
    }
}

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

impl From<crate::locks::SessionTabStateError> for SessionRuntimeError {
    fn from(error: crate::locks::SessionTabStateError) -> Self {
        Self::SessionTabState(error)
    }
}

impl From<crate::startup::StartupError> for SessionRuntimeError {
    fn from(error: crate::startup::StartupError) -> Self {
        Self::Startup(error)
    }
}

impl From<RefreshExecutionError> for SessionRuntimeError {
    fn from(error: RefreshExecutionError) -> Self {
        Self::RefreshExecution(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    #[derive(Default)]
    struct FakeAdapter {
        acquired_paths: Vec<PathBuf>,
        signaled: Vec<RefreshTrigger>,
        spawned: usize,
        ready_owner: Option<ReadySessionRuntime>,
        gate_error: Option<SessionRuntimeError>,
        signal_error: Option<SessionRuntimeError>,
        start_error: Option<SessionRuntimeError>,
    }

    struct FakeGuard;

    impl StartupGateGuard for FakeGuard {}

    impl SessionRuntimeAdapter for FakeAdapter {
        fn acquire_startup_gate(
            &mut self,
            path: &Path,
        ) -> Result<Box<dyn StartupGateGuard>, SessionRuntimeError> {
            self.acquired_paths.push(path.to_path_buf());
            if let Some(error) = self.gate_error.take() {
                return Err(error);
            }
            Ok(Box::new(FakeGuard))
        }

        fn signal_ready_owner(
            &mut self,
            _launch: &SessionRuntimeLaunch<'_>,
            trigger: RefreshTrigger,
        ) -> Result<Option<ReadySessionRuntime>, SessionRuntimeError> {
            self.signaled.push(trigger);
            if let Some(error) = self.signal_error.take() {
                return Err(error);
            }
            Ok(self.ready_owner.clone())
        }

        fn start_owner_and_wait_ready(
            &mut self,
            _launch: &SessionRuntimeLaunch<'_>,
            _trigger: RefreshTrigger,
        ) -> Result<ReadySessionRuntime, SessionRuntimeError> {
            self.spawned += 1;
            if let Some(error) = self.start_error.take() {
                return Err(error);
            }
            Ok(ReadySessionRuntime {
                pid: 202,
                launch_id: "new-owner".to_string(),
            })
        }
    }

    #[derive(Default)]
    struct FakeActivationAdapter {
        inspection: Option<RuntimeInspection>,
        requested_operations: Vec<RuntimeControlOperation>,
        request_owner: Option<ReadySessionRuntime>,
        request_error: Option<SessionRuntimeError>,
        release_error: Option<SessionRuntimeError>,
        starts: usize,
    }

    impl ActivationAdapter for FakeActivationAdapter {
        fn acquire_activation_gate(
            &mut self,
            _path: &Path,
        ) -> Result<Box<dyn StartupGateGuard>, SessionRuntimeError> {
            Ok(Box::new(FakeGuard))
        }

        fn inspect_runtime(
            &mut self,
            _launch: &SessionRuntimeLaunch<'_>,
        ) -> Result<RuntimeInspection, SessionRuntimeError> {
            Ok(self.inspection.clone().unwrap_or(RuntimeInspection::Absent))
        }

        fn request_ready_owner(
            &mut self,
            _launch: &SessionRuntimeLaunch<'_>,
            operation: RuntimeControlOperation,
        ) -> Result<Option<ReadySessionRuntime>, SessionRuntimeError> {
            self.requested_operations.push(operation);
            if let Some(error) = self.request_error.take() {
                return Err(error);
            }
            Ok(self.request_owner.clone())
        }

        fn wait_for_runtime_release(
            &mut self,
            _paths: &RuntimePaths,
        ) -> Result<(), SessionRuntimeError> {
            match self.release_error.take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        fn start_activation_owner(
            &mut self,
            _launch: &SessionRuntimeLaunch<'_>,
            _trigger: RefreshTrigger,
        ) -> Result<ReadySessionRuntime, SessionRuntimeError> {
            self.starts += 1;
            Ok(ReadySessionRuntime {
                pid: 202,
                launch_id: "replacement-owner".to_string(),
            })
        }
    }

    #[test]
    fn activation_signals_a_matching_ready_binary_without_replacement() {
        let socket = SessionSocket::resolve("/tmp/activation-same.sock").expect("socket");
        let binary = std::env::current_exe().expect("current executable");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: Path::new("/tmp/tabby-state"),
            binary_path: &binary,
        };
        let mut adapter = FakeActivationAdapter {
            inspection: Some(ready_inspection(&binary)),
            request_owner: Some(ReadySessionRuntime {
                pid: 101,
                launch_id: "same-owner".to_string(),
            }),
            ..FakeActivationAdapter::default()
        };

        let message = activate_current_runtime_with(&launch, &mut adapter).expect("activation");

        assert!(message.contains("already ready with pid 101"));
        assert!(matches!(
            adapter.requested_operations.as_slice(),
            [RuntimeControlOperation::Signal {
                trigger: RefreshTrigger::Startup
            }]
        ));
        assert_eq!(adapter.starts, 0);
    }

    #[test]
    fn activation_fails_closed_for_starting_and_faulted_owners() {
        let socket = SessionSocket::resolve("/tmp/activation-fault.sock").expect("socket");
        let binary = std::env::current_exe().expect("current executable");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: Path::new("/tmp/tabby-state"),
            binary_path: &binary,
        };

        for inspection in [
            RuntimeInspection::Starting { lease_held: true },
            RuntimeInspection::Faulted {
                diagnostic: "invalid owner identity".to_string(),
                lease_held: true,
            },
        ] {
            let mut adapter = FakeActivationAdapter {
                inspection: Some(inspection),
                ..FakeActivationAdapter::default()
            };

            assert!(activate_current_runtime_with(&launch, &mut adapter).is_err());
            assert!(adapter.requested_operations.is_empty());
            assert_eq!(adapter.starts, 0);
        }
    }

    #[test]
    fn activation_preserves_the_owner_when_handoff_validation_or_release_fails() {
        let socket = SessionSocket::resolve("/tmp/activation-handoff.sock").expect("socket");
        let binary = std::env::current_exe().expect("current executable");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: Path::new("/tmp/tabby-state"),
            binary_path: &binary,
        };

        for (request_error, release_error) in [
            (
                Some(SessionRuntimeError::Control(
                    "invalid replacement peer".to_string(),
                )),
                None,
            ),
            (
                None,
                Some(SessionRuntimeError::Readiness(
                    "handoff timed out".to_string(),
                )),
            ),
        ] {
            let mut adapter = FakeActivationAdapter {
                inspection: Some(ready_inspection(Path::new("/opt/other/tabby"))),
                request_owner: Some(ReadySessionRuntime {
                    pid: 101,
                    launch_id: "old-owner".to_string(),
                }),
                request_error,
                release_error,
                ..FakeActivationAdapter::default()
            };

            assert!(activate_current_runtime_with(&launch, &mut adapter).is_err());
            assert_eq!(adapter.starts, 0);
        }
    }

    fn ready_inspection(binary_path: &Path) -> RuntimeInspection {
        RuntimeInspection::Ready {
            pid: 101,
            launch_id: "ready-owner".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            binary_path: crate::startup::binary_identity(binary_path),
            lease_held: true,
            last_evaluation_unix_ms: None,
            last_failure: None,
            next_periodic_unix_ms: None,
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
    fn startup_gate_reports_busy_without_spawning_another_owner() {
        let socket = SessionSocket::resolve("/tmp/busy-herdr.sock").expect("socket");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: Path::new("/tmp/tabby-state"),
            binary_path: Path::new("/tmp/tabby"),
        };
        let mut adapter = FakeAdapter {
            gate_error: Some(SessionRuntimeError::StartupGateBusy(PathBuf::from(
                "/tmp/gate",
            ))),
            ..FakeAdapter::default()
        };

        assert_eq!(
            ensure_ready_owner_with(&launch, RefreshTrigger::Focus, &mut adapter)
                .expect("classified busy gate"),
            EnsureRuntimeOutcome::Busy
        );
        assert!(adapter.signaled.is_empty());
        assert_eq!(adapter.spawned, 0);
    }

    #[test]
    fn startup_gate_reports_a_failed_spawn_as_faulted() {
        let socket = SessionSocket::resolve("/tmp/spawn-herdr.sock").expect("socket");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: Path::new("/tmp/tabby-state"),
            binary_path: Path::new("/tmp/tabby"),
        };
        let mut adapter = FakeAdapter {
            start_error: Some(SessionRuntimeError::SpawnRuntime(io::Error::other(
                "denied",
            ))),
            ..FakeAdapter::default()
        };

        let outcome = ensure_ready_owner_with(&launch, RefreshTrigger::Startup, &mut adapter)
            .expect("classified spawn failure");

        assert!(matches!(outcome, EnsureRuntimeOutcome::Faulted { .. }));
        assert_eq!(adapter.spawned, 1);
    }

    #[test]
    fn startup_gate_reports_a_child_exit_before_readiness_as_faulted() {
        let socket = SessionSocket::resolve("/tmp/child-exit-herdr.sock").expect("socket");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: Path::new("/tmp/tabby-state"),
            binary_path: Path::new("/tmp/tabby"),
        };
        let exit_status = Command::new("false").status().expect("run false");
        let mut adapter = FakeAdapter {
            start_error: Some(SessionRuntimeError::ChildExitedBeforeReady(exit_status)),
            ..FakeAdapter::default()
        };

        let outcome = ensure_ready_owner_with(&launch, RefreshTrigger::Startup, &mut adapter)
            .expect("classified child exit");

        assert!(matches!(outcome, EnsureRuntimeOutcome::Faulted { .. }));
    }

    #[test]
    fn startup_gate_reports_a_readiness_timeout_without_exposing_error_interpretation() {
        let socket = SessionSocket::resolve("/tmp/timeout-herdr.sock").expect("socket");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: Path::new("/tmp/tabby-state"),
            binary_path: Path::new("/tmp/tabby"),
        };
        let mut adapter = FakeAdapter {
            start_error: Some(SessionRuntimeError::Readiness("timed out".to_string())),
            ..FakeAdapter::default()
        };

        assert_eq!(
            ensure_ready_owner_with(&launch, RefreshTrigger::Startup, &mut adapter)
                .expect("classified timeout"),
            EnsureRuntimeOutcome::TimedOut
        );
    }

    #[test]
    fn startup_gate_reports_an_identity_contradiction_as_faulted() {
        let socket = SessionSocket::resolve("/tmp/identity-herdr.sock").expect("socket");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: Path::new("/tmp/tabby-state"),
            binary_path: Path::new("/tmp/tabby"),
        };
        let mut adapter = FakeAdapter {
            signal_error: Some(SessionRuntimeError::Control(
                "runtime metadata contradicts the requested Session Identity".to_string(),
            )),
            ..FakeAdapter::default()
        };

        let outcome = ensure_ready_owner_with(&launch, RefreshTrigger::Focus, &mut adapter)
            .expect("classified identity fault");

        assert!(matches!(outcome, EnsureRuntimeOutcome::Faulted { .. }));
        assert_eq!(adapter.spawned, 0);
    }

    #[test]
    fn forget_session_removes_only_an_explicitly_stopped_session_identity() {
        let unique = NEXT_LAUNCH_ID.fetch_add(1, Ordering::Relaxed);
        let state_base = PathBuf::from("/tmp").join(format!(
            "tby-forget-stopped-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&state_base).expect("state base");
        let socket = SessionSocket::resolve(state_base.join("stopped.sock")).expect("socket");
        let store = crate::locks::SessionTabStateStore::open(&state_base, &socket)
            .expect("session state store");
        store
            .mutate(|state| state.record_plugin_label("tab-1", "nvim"))
            .expect("persist selected session state");
        let state_path = store.path().to_path_buf();
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: &state_base,
            binary_path: Path::new("/tmp/tabby"),
        };

        forget_stopped_session(&launch).expect("forget stopped session");

        assert!(!state_path.exists());
        fs::remove_dir_all(&state_base).expect("remove state base");
    }

    #[test]
    fn forget_session_rejects_a_running_herdr_listener_without_mutating_state() {
        let unique = NEXT_LAUNCH_ID.fetch_add(1, Ordering::Relaxed);
        let state_base = PathBuf::from("/tmp").join(format!(
            "tby-forget-running-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&state_base).expect("state base");
        let socket_path = state_base.join("running.sock");
        let listener = UnixListener::bind(&socket_path).expect("running Herdr listener");
        let socket = SessionSocket::resolve(&socket_path).expect("socket");
        let store = crate::locks::SessionTabStateStore::open(&state_base, &socket)
            .expect("session state store");
        store
            .mutate(|state| state.record_plugin_label("tab-1", "nvim"))
            .expect("persist selected session state");
        let state_path = store.path().to_path_buf();
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: &state_base,
            binary_path: Path::new("/tmp/tabby"),
        };

        let error = forget_stopped_session(&launch).expect_err("running session must be rejected");

        assert!(
            error
                .to_string()
                .contains("selected Herdr Session is running")
        );
        assert!(state_path.exists());
        drop(listener);
        fs::remove_dir_all(&state_base).expect("remove state base");
    }

    #[test]
    fn stale_ready_metadata_without_a_control_endpoint_is_not_treated_as_an_owner() {
        let unique = NEXT_LAUNCH_ID.fetch_add(1, Ordering::Relaxed);
        let state_base = PathBuf::from("/tmp").join(format!(
            "tby-stale-runtime-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&state_base).expect("state base");
        let socket = SessionSocket::resolve(state_base.join("herdr.sock")).expect("socket");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: &state_base,
            binary_path: Path::new("/tmp/tabby"),
        };
        let paths = RuntimePaths::for_launch(&launch);
        fs::create_dir_all(&paths.directory).expect("runtime directory");
        write_metadata(
            &paths.metadata,
            &RuntimeMetadata {
                schema_version: RUNTIME_METADATA_SCHEMA_VERSION,
                state: RuntimeMetadataState::Ready,
                pid: 999_999,
                session_key: socket.session_key.clone(),
                socket_path: socket.socket_path.to_string_lossy().into_owned(),
                socket_identity_hex: socket.identity_hex(),
                launch_id: "stale-launch".to_string(),
                tabby_version: env!("CARGO_PKG_VERSION").to_string(),
                binary_path: "/tmp/tabby".to_string(),
                last_evaluation_unix_ms: None,
                last_failure: None,
                next_periodic_unix_ms: None,
            },
        )
        .expect("stale metadata");

        assert_eq!(
            signal_ready_owner(&launch, RefreshTrigger::Focus).expect("check stale endpoint"),
            None
        );
        assert_eq!(
            inspect_runtime(&launch).expect("inspect stale runtime"),
            RuntimeInspection::Absent
        );

        fs::remove_dir_all(&state_base).expect("remove state base");
    }

    #[test]
    fn wrong_identity_metadata_is_a_fault_and_is_never_signalled() {
        let unique = NEXT_LAUNCH_ID.fetch_add(1, Ordering::Relaxed);
        let state_base = PathBuf::from("/tmp").join(format!(
            "tby-identity-runtime-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&state_base).expect("state base");
        let socket = SessionSocket::resolve(state_base.join("herdr.sock")).expect("socket");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: &state_base,
            binary_path: Path::new("/tmp/tabby"),
        };
        let paths = RuntimePaths::for_launch(&launch);
        fs::create_dir_all(&paths.directory).expect("runtime directory");
        write_metadata(
            &paths.metadata,
            &RuntimeMetadata {
                schema_version: RUNTIME_METADATA_SCHEMA_VERSION,
                state: RuntimeMetadataState::Ready,
                pid: 999_999,
                session_key: socket.session_key.clone(),
                socket_path: socket.socket_path.to_string_lossy().into_owned(),
                socket_identity_hex: "wrong-identity".to_string(),
                launch_id: "wrong-identity".to_string(),
                tabby_version: env!("CARGO_PKG_VERSION").to_string(),
                binary_path: "/tmp/tabby".to_string(),
                last_evaluation_unix_ms: None,
                last_failure: None,
                next_periodic_unix_ms: None,
            },
        )
        .expect("wrong identity metadata");

        assert!(matches!(
            signal_ready_owner(&launch, RefreshTrigger::Focus),
            Err(SessionRuntimeError::Control(_))
        ));
        assert!(matches!(
            inspect_runtime(&launch).expect("inspect wrong identity"),
            RuntimeInspection::Faulted { .. }
        ));

        fs::remove_dir_all(&state_base).expect("remove state base");
    }

    struct ConcurrentGateAdapter {
        starts: Arc<AtomicUsize>,
    }

    impl SessionRuntimeAdapter for ConcurrentGateAdapter {
        fn acquire_startup_gate(
            &mut self,
            path: &Path,
        ) -> Result<Box<dyn StartupGateGuard>, SessionRuntimeError> {
            match LifetimeLease::try_acquire(path)? {
                Some(lease) => Ok(Box::new(lease)),
                None => Err(SessionRuntimeError::StartupGateBusy(path.to_path_buf())),
            }
        }

        fn signal_ready_owner(
            &mut self,
            _launch: &SessionRuntimeLaunch<'_>,
            _trigger: RefreshTrigger,
        ) -> Result<Option<ReadySessionRuntime>, SessionRuntimeError> {
            Ok(None)
        }

        fn start_owner_and_wait_ready(
            &mut self,
            _launch: &SessionRuntimeLaunch<'_>,
            _trigger: RefreshTrigger,
        ) -> Result<ReadySessionRuntime, SessionRuntimeError> {
            self.starts.fetch_add(1, AtomicOrdering::SeqCst);
            thread::sleep(Duration::from_millis(25));
            Ok(ReadySessionRuntime {
                pid: 303,
                launch_id: "concurrent-owner".to_string(),
            })
        }
    }

    #[test]
    fn concurrent_startup_gate_callers_allow_exactly_one_new_owner() {
        let unique = NEXT_LAUNCH_ID.fetch_add(1, Ordering::Relaxed);
        let state_base = PathBuf::from("/tmp").join(format!(
            "tby-concurrent-gate-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&state_base).expect("state base");
        let socket = SessionSocket::resolve(state_base.join("herdr.sock")).expect("socket");
        let paths = RuntimePaths::for_launch(&SessionRuntimeLaunch {
            socket: &socket,
            state_base: &state_base,
            binary_path: Path::new("/tmp/tabby"),
        });
        fs::create_dir_all(&paths.directory).expect("runtime directory");
        let starts = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let socket = socket.clone();
            let state_base = state_base.clone();
            let starts = Arc::clone(&starts);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                let launch = SessionRuntimeLaunch {
                    socket: &socket,
                    state_base: &state_base,
                    binary_path: Path::new("/tmp/tabby"),
                };
                let mut adapter = ConcurrentGateAdapter { starts };
                ensure_ready_owner_with(&launch, RefreshTrigger::Startup, &mut adapter)
                    .expect("classified concurrent outcome")
            }));
        }

        let first = handles.remove(0).join().expect("first caller");
        let second = handles.remove(0).join().expect("second caller");
        assert_eq!(starts.load(AtomicOrdering::SeqCst), 1);
        assert!(
            matches!(first, EnsureRuntimeOutcome::NewOwnerReady(_))
                || matches!(second, EnsureRuntimeOutcome::NewOwnerReady(_))
        );
        assert!(
            matches!(first, EnsureRuntimeOutcome::Busy)
                || matches!(second, EnsureRuntimeOutcome::Busy)
        );

        fs::remove_dir_all(&state_base).expect("remove state base");
    }

    #[test]
    fn a_busy_system_startup_gate_returns_to_the_hook_without_waiting() {
        let unique = NEXT_LAUNCH_ID.fetch_add(1, Ordering::Relaxed);
        let state_base = PathBuf::from("/tmp").join(format!(
            "tby-busy-system-gate-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&state_base).expect("state base");
        let socket = SessionSocket::resolve(state_base.join("herdr.sock")).expect("socket");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: &state_base,
            binary_path: Path::new("/tmp/tabby"),
        };
        let paths = RuntimePaths::for_launch(&launch);
        fs::create_dir_all(&paths.directory).expect("runtime directory");
        let _held_gate = LifetimeLease::try_acquire(&paths.startup_gate)
            .expect("acquire gate")
            .expect("gate available");
        let mut adapter = SystemSessionRuntimeAdapter::default();
        let started_at = Instant::now();

        let error = match adapter.acquire_startup_gate(&paths.startup_gate) {
            Ok(_) => panic!("second hook must see a busy gate"),
            Err(error) => error,
        };

        assert!(matches!(error, SessionRuntimeError::StartupGateBusy(_)));
        assert!(
            started_at.elapsed() < Duration::from_secs(1),
            "a concurrent hook must return Busy promptly"
        );
        fs::remove_dir_all(&state_base).expect("remove state base");
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
            socket.identity_hex(),
            "ready-launch".to_string(),
            "/tmp/tabby-owner".to_string(),
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
                last_evaluation_unix_ms: None,
                last_failure: None,
                next_periodic_unix_ms: None,
            },
        )
        .expect("ready metadata");

        let owner = signal_ready_owner(&launch, RefreshTrigger::Focus)
            .expect("control request")
            .expect("ready owner");

        assert_eq!(owner.pid, std::process::id());
        assert_eq!(owner.launch_id, "ready-launch");
        assert!(matches!(
            receiver
                .recv_until(Some(Instant::now() + Duration::from_secs(1)))
                .expect("delivered trigger"),
            RuntimeControlEvent::Trigger(RefreshTrigger::Focus)
        ));

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

        sender
            .send_trigger(RefreshTrigger::Focus)
            .expect("focus trigger");
        sender
            .send_trigger(RefreshTrigger::Creation)
            .expect("creation trigger");
        sender
            .send_trigger(RefreshTrigger::Manual)
            .expect("manual trigger");

        assert!(matches!(
            receiver.recv_until(Some(Instant::now())),
            Some(RuntimeControlEvent::Trigger(RefreshTrigger::Manual))
        ));
        assert!(receiver.recv_until(Some(Instant::now())).is_none());
    }

    #[test]
    fn mutation_commands_are_completed_in_order_without_being_coalesced() {
        let (sender, receiver) = trigger_mailbox();
        let unlock = sender
            .enqueue_mutation(RuntimeMutation::UnlockAll)
            .expect("queue unlock");
        let repair = sender
            .enqueue_mutation(RuntimeMutation::RepairStateDiscard)
            .expect("queue repair");

        let RuntimeControlEvent::Command(first) = receiver
            .recv_until(Some(Instant::now()))
            .expect("first mutation")
        else {
            panic!("first event is a mutation command");
        };
        assert_eq!(first.mutation, RuntimeMutation::UnlockAll);
        first.completion.send(Ok(())).expect("complete unlock");

        let RuntimeControlEvent::Command(second) = receiver
            .recv_until(Some(Instant::now()))
            .expect("second mutation")
        else {
            panic!("second event is a mutation command");
        };
        assert_eq!(second.mutation, RuntimeMutation::RepairStateDiscard);
        second.completion.send(Ok(())).expect("complete repair");

        assert_eq!(unlock.completion.recv().expect("unlock result"), Ok(()));
        assert_eq!(repair.completion.recv().expect("repair result"), Ok(()));
    }

    #[test]
    fn queued_handoff_rejects_duplicate_or_stale_follow_up_commands() {
        let (sender, receiver) = trigger_mailbox();
        let handoff = sender
            .enqueue_mutation(RuntimeMutation::PrepareHandoff {
                replacement_binary_identity: "/replacement/tabby".to_string(),
            })
            .expect("queue handoff");

        assert!(
            sender
                .enqueue_mutation(RuntimeMutation::UnlockFocused)
                .is_err()
        );
        assert!(sender.send_trigger(RefreshTrigger::Focus).is_err());

        let RuntimeControlEvent::Command(command) = receiver
            .recv_until(Some(Instant::now()))
            .expect("handoff command")
        else {
            panic!("handoff remains a command");
        };
        assert!(matches!(
            command.mutation,
            RuntimeMutation::PrepareHandoff { .. }
        ));
        command.completion.send(Ok(())).expect("complete handoff");
        assert_eq!(handoff.completion.recv().expect("handoff result"), Ok(()));
    }

    #[test]
    fn handoff_rejects_the_current_or_unvalidated_replacement_identity() {
        let current = std::env::current_exe()
            .expect("current executable")
            .canonicalize()
            .expect("canonical current executable");
        let current = current.to_string_lossy().into_owned();

        assert!(validate_declared_replacement_binary_identity(&current, &current).is_err());
        assert!(
            validate_declared_replacement_binary_identity("relative/tabby", "/owner/tabby")
                .is_err()
        );
        assert!(
            validate_declared_replacement_binary_identity("/not/a/tabby", "/owner/tabby").is_err()
        );
    }

    #[test]
    fn control_endpoint_rejects_stale_duplicate_and_same_binary_handoff_requests() {
        let unique = NEXT_LAUNCH_ID.fetch_add(1, Ordering::Relaxed);
        let state_base = PathBuf::from("/tmp").join(format!(
            "tby-control-authentication-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&state_base).expect("state base");
        let socket = SessionSocket::resolve(state_base.join("herdr.sock")).expect("socket");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: &state_base,
            binary_path: Path::new("/tmp/tabby"),
        };
        let paths = RuntimePaths::for_launch(&launch);
        let listener = bind_private_listener(&paths.control_directory, &paths.control_socket)
            .expect("control listener");
        let (sender, receiver) = trigger_mailbox();
        let current = std::env::current_exe()
            .expect("current executable")
            .canonicalize()
            .expect("canonical current executable")
            .to_string_lossy()
            .into_owned();
        spawn_control_acceptor(
            listener,
            socket.session_key.clone(),
            socket.identity_hex(),
            "ready-launch".to_string(),
            current.clone(),
            sender,
        );

        let base_request = |request_id: &str| ControlRequest {
            schema_version: CONTROL_SCHEMA_VERSION,
            session_key: socket.session_key.clone(),
            socket_identity_hex: socket.identity_hex(),
            launch_id: "ready-launch".to_string(),
            request_id: request_id.to_string(),
            operation: RuntimeControlOperation::Signal {
                trigger: RefreshTrigger::Focus,
            },
        };
        let first = send_raw_control_request(&paths.control_socket, base_request("duplicate-id"));
        assert!(first.accepted);
        assert!(matches!(
            receiver.recv_until(Some(Instant::now() + Duration::from_secs(1))),
            Some(RuntimeControlEvent::Trigger(RefreshTrigger::Focus))
        ));
        let duplicate =
            send_raw_control_request(&paths.control_socket, base_request("duplicate-id"));
        assert!(!duplicate.accepted);
        assert!(
            duplicate
                .error
                .expect("duplicate error")
                .contains("duplicate")
        );

        let mut stale = base_request("stale-id");
        stale.launch_id = "stale-launch".to_string();
        let stale_reply = send_raw_control_request(&paths.control_socket, stale);
        assert!(!stale_reply.accepted);
        assert!(stale_reply.error.expect("stale error").contains("identity"));

        let same_binary = send_raw_control_request(
            &paths.control_socket,
            ControlRequest {
                schema_version: CONTROL_SCHEMA_VERSION,
                session_key: socket.session_key.clone(),
                socket_identity_hex: socket.identity_hex(),
                launch_id: "ready-launch".to_string(),
                request_id: "same-binary-handoff".to_string(),
                operation: RuntimeControlOperation::PrepareHandoff {
                    replacement_binary_identity: current,
                },
            },
        );
        assert!(!same_binary.accepted);
        assert!(
            same_binary
                .error
                .expect("same binary error")
                .contains("different Tabby executable")
        );

        let _ = fs::remove_file(&paths.control_socket);
        let _ = fs::remove_dir_all(&paths.control_directory);
        fs::remove_dir_all(&state_base).expect("remove state base");
    }

    #[test]
    fn control_endpoint_rejects_a_replacement_identity_not_executed_by_its_peer() {
        let unique = NEXT_LAUNCH_ID.fetch_add(1, Ordering::Relaxed);
        let state_base = PathBuf::from("/tmp").join(format!(
            "tby-control-peer-executable-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&state_base).expect("state base");
        let socket = SessionSocket::resolve(state_base.join("herdr.sock")).expect("socket");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: &state_base,
            binary_path: Path::new("/tmp/tabby"),
        };
        let paths = RuntimePaths::for_launch(&launch);
        let state_store = crate::locks::SessionTabStateStore::open(&state_base, &socket)
            .expect("session state store");
        state_store
            .mutate(|state| {
                state.record_plugin_label("tab-1", "nvim");
                state.lock_tab("tab-1", Some("manual".to_string()));
            })
            .expect("persist owner state");
        let listener = bind_private_listener(&paths.control_directory, &paths.control_socket)
            .expect("control listener");
        let (sender, receiver) = trigger_mailbox();
        let owner_identity = std::env::current_exe()
            .expect("current executable")
            .canonicalize()
            .expect("canonical current executable")
            .to_string_lossy()
            .into_owned();
        spawn_control_acceptor(
            listener,
            socket.session_key.clone(),
            socket.identity_hex(),
            "ready-launch".to_string(),
            owner_identity.clone(),
            sender,
        );
        let declared_identity = PathBuf::from("/bin/sh")
            .canonicalize()
            .expect("canonical shell")
            .to_string_lossy()
            .into_owned();
        let test_binary = std::env::current_exe().expect("test binary");
        let status = Command::new(test_binary)
            .args([
                "--exact",
                "session_runtime::tests::wrong_replacement_peer_helper",
                "--nocapture",
            ])
            .env("TABBY_TEST_CONTROL_SOCKET", &paths.control_socket)
            .env("TABBY_TEST_CONTROL_SESSION_KEY", &socket.session_key)
            .env("TABBY_TEST_CONTROL_SOCKET_IDENTITY", socket.identity_hex())
            .env("TABBY_TEST_CONTROL_REPLACEMENT", declared_identity)
            .status()
            .expect("run wrong replacement peer");
        assert!(
            status.success(),
            "subprocess rejected the wrong peer as expected"
        );
        assert!(receiver.recv_until(Some(Instant::now())).is_none());

        let still_ready = send_raw_control_request(
            &paths.control_socket,
            ControlRequest {
                schema_version: CONTROL_SCHEMA_VERSION,
                session_key: socket.session_key.clone(),
                socket_identity_hex: socket.identity_hex(),
                launch_id: "ready-launch".to_string(),
                request_id: "signal-after-rejected-handoff".to_string(),
                operation: RuntimeControlOperation::Signal {
                    trigger: RefreshTrigger::Focus,
                },
            },
        );
        assert!(still_ready.accepted, "proven owner remains available");
        assert!(matches!(
            receiver.recv_until(Some(Instant::now() + Duration::from_secs(1))),
            Some(RuntimeControlEvent::Trigger(RefreshTrigger::Focus))
        ));
        assert_eq!(
            crate::locks::SessionTabStateStore::inspect_read_only(&state_base, &socket),
            crate::locks::SessionTabStateInspection::Valid {
                manual_locks: 1,
                baselines: 1,
                unresolved_rename_intents: 0,
            }
        );

        let _ = fs::remove_file(&paths.control_socket);
        let _ = fs::remove_dir_all(&paths.control_directory);
        fs::remove_dir_all(&state_base).expect("remove state base");
    }

    #[test]
    fn wrong_replacement_peer_helper() {
        let Ok(control_socket) = std::env::var("TABBY_TEST_CONTROL_SOCKET") else {
            return;
        };
        let request = ControlRequest {
            schema_version: CONTROL_SCHEMA_VERSION,
            session_key: std::env::var("TABBY_TEST_CONTROL_SESSION_KEY")
                .expect("session key supplied"),
            socket_identity_hex: std::env::var("TABBY_TEST_CONTROL_SOCKET_IDENTITY")
                .expect("socket identity supplied"),
            launch_id: "ready-launch".to_string(),
            request_id: "wrong-peer-handoff".to_string(),
            operation: RuntimeControlOperation::PrepareHandoff {
                replacement_binary_identity: std::env::var("TABBY_TEST_CONTROL_REPLACEMENT")
                    .expect("replacement supplied"),
            },
        };
        let reply = send_raw_control_request(Path::new(&control_socket), request);
        assert!(!reply.accepted);
        assert!(
            reply
                .error
                .expect("peer identity error")
                .contains("does not match the executing control peer")
        );
    }

    #[test]
    fn cooperative_handoff_releases_the_old_process_lease_before_a_new_owner_can_acquire_it() {
        let unique = NEXT_LAUNCH_ID.fetch_add(1, Ordering::Relaxed);
        let state_base = PathBuf::from("/tmp").join(format!(
            "tby-cooperative-handoff-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&state_base).expect("state base");
        let socket = SessionSocket::resolve(state_base.join("herdr.sock")).expect("socket");
        let parent_binary = std::env::current_exe()
            .expect("parent test executable")
            .canonicalize()
            .expect("canonical parent test executable");
        let owner_binary = state_base.join("handoff-owner-test-binary");
        fs::copy(&parent_binary, &owner_binary).expect("copy independent owner executable");
        fs::set_permissions(&owner_binary, fs::Permissions::from_mode(0o700))
            .expect("make owner executable");
        let ready_path = state_base.join("owner-ready");
        let mut child = Command::new(&owner_binary)
            .args([
                "--exact",
                "session_runtime::tests::cooperative_handoff_owner_helper",
                "--nocapture",
            ])
            .env("TABBY_TEST_HANDOFF_SOCKET", &socket.socket_path)
            .env("TABBY_TEST_HANDOFF_STATE_BASE", &state_base)
            .env("TABBY_TEST_HANDOFF_READY", &ready_path)
            .spawn()
            .expect("start independent runtime owner");

        let ready_deadline = Instant::now() + Duration::from_secs(2);
        while !ready_path.exists() && Instant::now() < ready_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(ready_path.exists(), "old owner did not publish readiness");

        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: &state_base,
            binary_path: &parent_binary,
        };
        let paths = RuntimePaths::for_launch(&launch);
        assert!(LifetimeLease::is_held(&paths.lifetime_lease).expect("old lease held"));
        assert!(
            LifetimeLease::try_acquire(&paths.lifetime_lease)
                .expect("attempt concurrent owner")
                .is_none()
        );

        let owner = request_ready_owner(
            &launch,
            RuntimeControlOperation::PrepareHandoff {
                replacement_binary_identity: parent_binary.to_string_lossy().into_owned(),
            },
        )
        .expect("request cooperative handoff")
        .expect("old owner accepted cooperative handoff");
        assert_eq!(owner.launch_id, "handoff-owner");
        assert!(child.wait().expect("reap old owner").success());

        assert!(
            LifetimeLease::try_acquire(&paths.lifetime_lease)
                .expect("new owner acquires released lease")
                .is_some()
        );

        let _ = fs::remove_file(&paths.control_socket);
        let _ = fs::remove_dir_all(&paths.control_directory);
        fs::remove_dir_all(&state_base).expect("remove state base");
    }

    #[test]
    fn cooperative_handoff_owner_helper() {
        let Ok(socket_path) = std::env::var("TABBY_TEST_HANDOFF_SOCKET") else {
            return;
        };
        let state_base = PathBuf::from(
            std::env::var("TABBY_TEST_HANDOFF_STATE_BASE").expect("state base supplied"),
        );
        let ready_path =
            PathBuf::from(std::env::var("TABBY_TEST_HANDOFF_READY").expect("ready path supplied"));
        let socket = SessionSocket::resolve(socket_path).expect("resolve session socket");
        let binary_path = std::env::current_exe()
            .expect("owner executable")
            .canonicalize()
            .expect("canonical owner executable");
        let launch = SessionRuntimeLaunch {
            socket: &socket,
            state_base: &state_base,
            binary_path: &binary_path,
        };
        let paths = RuntimePaths::for_launch(&launch);
        fs::create_dir_all(&paths.directory).expect("runtime directory");
        let lease = LifetimeLease::try_acquire(&paths.lifetime_lease)
            .expect("acquire owner lease")
            .expect("owner lease available");
        let listener = bind_private_listener(&paths.control_directory, &paths.control_socket)
            .expect("bind owner control endpoint");
        let (sender, receiver) = trigger_mailbox();
        spawn_control_acceptor(
            listener,
            socket.session_key.clone(),
            socket.identity_hex(),
            "handoff-owner".to_string(),
            binary_path.to_string_lossy().into_owned(),
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
                launch_id: "handoff-owner".to_string(),
                tabby_version: env!("CARGO_PKG_VERSION").to_string(),
                binary_path: binary_path.to_string_lossy().into_owned(),
                last_evaluation_unix_ms: None,
                last_failure: None,
                next_periodic_unix_ms: None,
            },
        )
        .expect("write owner metadata");
        fs::write(ready_path, b"ready").expect("publish owner readiness");

        let RuntimeControlEvent::Command(command) =
            receiver.recv_until(None).expect("handoff command")
        else {
            panic!("runtime owner only exits for a handoff command");
        };
        assert!(matches!(
            command.mutation,
            RuntimeMutation::PrepareHandoff { .. }
        ));
        command.completion.send(Ok(())).expect("complete handoff");
        if let Some(reply_written) = command.handoff_reply_written {
            reply_written
                .recv_timeout(CONTROL_IO_TIMEOUT)
                .expect("handoff caller received its acknowledgement");
        }
        drop(lease);
    }

    fn send_raw_control_request(path: &Path, request: ControlRequest) -> ControlReply {
        let mut stream = UnixStream::connect(path).expect("connect control endpoint");
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("bound reply read");
        serde_json::to_writer(&mut stream, &request).expect("write request");
        stream.write_all(b"\n").expect("terminate request");
        stream.flush().expect("flush request");
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .expect("read reply");
        serde_json::from_str(&line).expect("decode reply")
    }
}
