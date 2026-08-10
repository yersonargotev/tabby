//! Persistent Manually Locked Tab state.
//!
//! Locks are plugin-owned state, not user-editable configuration. The v1 store is
//! keyed by Herdr's `tab_id`, but those IDs can be reused after tab or workspace
//! churn. A label that exactly matches Herdr's reported tab number marks a fresh
//! lifecycle and discards stale state for that ID. Otherwise locks remain until an
//! explicit unlock operation removes them.

use crate::herdr_client::{HerdrApi, HerdrError};
use crate::labeler::LabelCandidate;
use crate::paths::{StatePathError, session_tab_state_path};
use crate::startup::SessionSocket;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SESSION_STATE_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManualLockDecision {
    AutoManaged,
    Lock { label: String },
}

pub fn is_default_tab_label(current_label: &str, tab_number: Option<u64>) -> bool {
    tab_number
        .map(|number| number.to_string())
        .is_some_and(|number| number == current_label)
}

pub fn detect_manual_lock(
    current_label: &str,
    last_plugin_label: Option<&str>,
    stable_label_candidate: Option<&LabelCandidate>,
) -> ManualLockDecision {
    if last_plugin_label.is_none() {
        return ManualLockDecision::AutoManaged;
    }

    if last_plugin_label == Some(current_label) {
        return ManualLockDecision::AutoManaged;
    }

    if stable_label_candidate.is_some_and(|candidate| candidate.label() == current_label) {
        return ManualLockDecision::AutoManaged;
    }

    ManualLockDecision::Lock {
        label: current_label.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualLock {
    tab_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

impl ManualLock {
    pub fn new(tab_id: impl Into<String>, label: Option<String>) -> Self {
        Self {
            tab_id: tab_id.into(),
            label,
        }
    }

    pub fn tab_id(&self) -> &str {
        &self.tab_id
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockStore {
    version: u8,
    locks: BTreeMap<String, ManualLock>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    last_plugin_labels: BTreeMap<String, String>,
}

impl Default for LockStore {
    fn default() -> Self {
        Self {
            version: 1,
            locks: BTreeMap::new(),
            last_plugin_labels: BTreeMap::new(),
        }
    }
}

impl LockStore {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, LockStoreError> {
        match fs::read_to_string(path.as_ref()) {
            Ok(contents) => {
                let store: Self = serde_json::from_str(&contents)?;
                if store.version == 1 {
                    Ok(store)
                } else {
                    Err(LockStoreError::UnsupportedVersion(store.version))
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), LockStoreError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }

        let contents = serde_json::to_string_pretty(self)?;
        let temp_path = temp_path_for(path);
        fs::write(&temp_path, contents)?;
        fs::rename(&temp_path, path)?;
        Ok(())
    }

    pub fn lock_tab(&mut self, tab_id: impl Into<String>, label: Option<String>) {
        let tab_id = tab_id.into();
        let lock = ManualLock::new(tab_id.clone(), label);
        self.locks.insert(tab_id, lock);
    }

    pub fn is_locked(&self, tab_id: &str) -> bool {
        self.locks.contains_key(tab_id)
    }

    pub fn last_plugin_label(&self, tab_id: &str) -> Option<&str> {
        self.last_plugin_labels.get(tab_id).map(String::as_str)
    }

    pub fn record_plugin_label(
        &mut self,
        tab_id: impl Into<String>,
        label: impl Into<String>,
    ) -> bool {
        let tab_id = tab_id.into();
        let label = label.into();
        if self.last_plugin_labels.get(&tab_id) == Some(&label) {
            return false;
        }
        self.last_plugin_labels.insert(tab_id, label);
        true
    }

    pub fn discard_tab_state_for_default_label(
        &mut self,
        tab_id: &str,
        current_label: &str,
        tab_number: Option<u64>,
    ) -> bool {
        if !is_default_tab_label(current_label, tab_number) {
            return false;
        }

        self.discard_tab_state(tab_id)
    }

    pub(crate) fn discard_tab_state(&mut self, tab_id: &str) -> bool {
        let removed_lock = self.remove_locked_tab_state(tab_id);
        let removed_baseline = self.last_plugin_labels.remove(tab_id).is_some();
        removed_lock || removed_baseline
    }

    pub fn unlock_tab(&mut self, tab_id: &str) -> bool {
        self.remove_locked_tab_state(tab_id)
    }

    fn remove_locked_tab_state(&mut self, tab_id: &str) -> bool {
        let removed = self.locks.remove(tab_id).is_some();
        if removed {
            self.last_plugin_labels.remove(tab_id);
        }
        removed
    }

    pub fn unlock_all(&mut self) {
        let locked_tab_ids = self.locks.keys().cloned().collect::<Vec<_>>();
        for tab_id in locked_tab_ids {
            self.remove_locked_tab_state(&tab_id);
        }
    }

    pub fn locks(&self) -> impl Iterator<Item = &ManualLock> {
        self.locks.values()
    }

    pub fn len(&self) -> usize {
        self.locks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.locks.is_empty()
    }

    pub fn baseline_count(&self) -> usize {
        self.last_plugin_labels.len()
    }
}

/// A crash-safe description of an automatic rename that may have reached Herdr
/// even if the runtime exited before it could persist its new baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomaticRenameIntent {
    tab_id: String,
    previous_label: String,
    intended_baseline: String,
}

impl AutomaticRenameIntent {
    pub fn new(
        tab_id: impl Into<String>,
        previous_label: impl Into<String>,
        intended_baseline: impl Into<String>,
    ) -> Self {
        Self {
            tab_id: tab_id.into(),
            previous_label: previous_label.into(),
            intended_baseline: intended_baseline.into(),
        }
    }

    pub fn tab_id(&self) -> &str {
        &self.tab_id
    }

    pub fn previous_label(&self) -> &str {
        &self.previous_label
    }

    pub fn intended_baseline(&self) -> &str {
        &self.intended_baseline
    }
}

/// The only persistent state that a Session Runtime may use for one Herdr
/// Session. The original, lossless identity is embedded beside its derived
/// storage key so a matching filename cannot authorize another session's data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTabState {
    schema_version: u8,
    session_key: String,
    socket_identity_hex: String,
    locks: LockStore,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    rename_intents: BTreeMap<String, AutomaticRenameIntent>,
}

impl SessionTabState {
    const SCHEMA_VERSION: u8 = 1;

    fn empty_for(session: &SessionSocket) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            session_key: session.session_key.clone(),
            socket_identity_hex: session.identity_hex(),
            locks: LockStore::default(),
            rename_intents: BTreeMap::new(),
        }
    }

    fn validate_identity(&self, session: &SessionSocket) -> Result<(), SessionTabStateError> {
        let requested_identity_hex = session.identity_hex();
        if self.session_key != session.session_key
            || self.socket_identity_hex != requested_identity_hex
        {
            return Err(SessionTabStateError::IdentityMismatch {
                requested_session_key: session.session_key.clone(),
                persisted_session_key: self.session_key.clone(),
            });
        }
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(SessionTabStateError::UnsupportedVersion(
                self.schema_version,
            ));
        }
        Ok(())
    }

    pub fn is_locked(&self, tab_id: &str) -> bool {
        self.locks.is_locked(tab_id)
    }

    pub fn last_plugin_label(&self, tab_id: &str) -> Option<&str> {
        self.locks.last_plugin_label(tab_id)
    }

    pub fn lock_tab(&mut self, tab_id: impl Into<String>, label: Option<String>) {
        self.locks.lock_tab(tab_id, label);
    }

    pub fn unlock_tab(&mut self, tab_id: &str) -> bool {
        self.locks.unlock_tab(tab_id)
    }

    pub fn unlock_all(&mut self) {
        self.locks.unlock_all();
    }

    pub fn record_plugin_label(
        &mut self,
        tab_id: impl Into<String>,
        label: impl Into<String>,
    ) -> bool {
        self.locks.record_plugin_label(tab_id, label)
    }

    pub fn discard_tab_state_for_default_label(
        &mut self,
        tab_id: &str,
        current_label: &str,
        tab_number: Option<u64>,
    ) -> bool {
        if !is_default_tab_label(current_label, tab_number) {
            return false;
        }

        let removed_intent = self.rename_intents.remove(tab_id).is_some();
        self.locks.discard_tab_state(tab_id) || removed_intent
    }

    pub fn lock_count(&self) -> usize {
        self.locks.len()
    }

    pub fn locks(&self) -> impl Iterator<Item = &ManualLock> {
        self.locks.locks()
    }

    pub fn baseline_count(&self) -> usize {
        self.locks.baseline_count()
    }

    pub fn unresolved_rename_intent_count(&self) -> usize {
        self.rename_intents.len()
    }

    pub fn automatic_rename_intent(&self, tab_id: &str) -> Option<&AutomaticRenameIntent> {
        self.rename_intents.get(tab_id)
    }

    fn record_automatic_rename_intent(
        &mut self,
        intent: AutomaticRenameIntent,
    ) -> Result<(), SessionTabStateError> {
        let tab_id = intent.tab_id.clone();
        if self.rename_intents.contains_key(&tab_id) {
            return Err(SessionTabStateError::UnresolvedRenameIntent { tab_id });
        }
        self.rename_intents.insert(tab_id, intent);
        Ok(())
    }

    fn reconcile_automatic_rename_intent(
        &mut self,
        tab_id: &str,
        visible_label: &str,
        tab_number: Option<u64>,
    ) -> RenameIntentReconciliation {
        let Some(intent) = self.rename_intents.get(tab_id).cloned() else {
            return RenameIntentReconciliation::NoIntent;
        };

        if visible_label == intent.intended_baseline {
            self.locks
                .record_plugin_label(tab_id.to_string(), intent.intended_baseline.clone());
            self.rename_intents.remove(tab_id);
            return RenameIntentReconciliation::Confirmed {
                intended_baseline: intent.intended_baseline,
            };
        }

        if visible_label == intent.previous_label {
            self.rename_intents.remove(tab_id);
            return RenameIntentReconciliation::SourceUnchanged;
        }

        if is_default_tab_label(visible_label, tab_number) {
            self.rename_intents.remove(tab_id);
            self.locks.discard_tab_state(tab_id);
            return RenameIntentReconciliation::ReusedTab;
        }

        self.rename_intents.remove(tab_id);
        self.locks
            .lock_tab(tab_id.to_string(), Some(visible_label.to_string()));
        RenameIntentReconciliation::PreservedManualIntent {
            label: visible_label.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameIntentReconciliation {
    NoIntent,
    Confirmed { intended_baseline: String },
    SourceUnchanged,
    ReusedTab,
    PreservedManualIntent { label: String },
}

/// A session-identity-bound persistence seam for the Session Runtime.
///
/// `record_automatic_rename_intent` must succeed before its caller invokes
/// `HerdrApi::rename_tab`. Later observations call
/// `reconcile_automatic_rename_intent` before making a new rename decision.
#[derive(Debug, Clone)]
pub struct SessionTabStateStore {
    path: PathBuf,
    session: SessionSocket,
}

impl SessionTabStateStore {
    pub fn open(
        state_base: impl AsRef<Path>,
        session: &SessionSocket,
    ) -> Result<Self, SessionTabStateError> {
        let path = session_tab_state_path(state_base, &session.session_key)?;
        ensure_private_state_directory(
            path.parent()
                .ok_or_else(|| SessionTabStateError::RelativePath(path.clone()))?,
        )?;
        Self::at_path(path, session)
    }

    /// Uses an explicit absolute path, primarily for dependency-injected test
    /// and repair tooling. Production callers should use `open`.
    pub fn at_path(
        path: impl AsRef<Path>,
        session: &SessionSocket,
    ) -> Result<Self, SessionTabStateError> {
        let path = path.as_ref().to_path_buf();
        if !path.is_absolute() {
            return Err(SessionTabStateError::RelativePath(path));
        }
        Ok(Self {
            path,
            session: session.clone(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read<R>(
        &self,
        inspect: impl FnOnce(&SessionTabState) -> R,
    ) -> Result<R, SessionTabStateError> {
        let _lock = SessionStateFileLock::acquire(&self.path, FileLockMode::Shared)?;
        let state = self.load_locked()?;
        Ok(inspect(&state))
    }

    /// Atomically loads, mutates, and persists one session's state while
    /// holding an advisory file lock shared by all Tabby processes.
    pub fn mutate<R>(
        &self,
        mutate: impl FnOnce(&mut SessionTabState) -> R,
    ) -> Result<R, SessionTabStateError> {
        let _lock = SessionStateFileLock::acquire(&self.path, FileLockMode::Exclusive)?;
        let mut state = self.load_locked()?;
        let result = mutate(&mut state);
        write_session_state_atomically(&self.path, &state)?;
        Ok(result)
    }

    pub fn record_automatic_rename_intent(
        &self,
        tab_id: impl Into<String>,
        previous_label: impl Into<String>,
        intended_baseline: impl Into<String>,
    ) -> Result<(), SessionTabStateError> {
        let intent = AutomaticRenameIntent::new(tab_id, previous_label, intended_baseline);
        self.mutate(|state| state.record_automatic_rename_intent(intent))?
    }

    pub fn reconcile_automatic_rename_intent(
        &self,
        tab_id: &str,
        visible_label: &str,
        tab_number: Option<u64>,
    ) -> Result<RenameIntentReconciliation, SessionTabStateError> {
        self.mutate(|state| {
            state.reconcile_automatic_rename_intent(tab_id, visible_label, tab_number)
        })
    }

    /// Deletes retained state only after an explicit Forget Session Action.
    ///
    /// The caller supplies proof that it established the Session Runtime is
    /// stopped. This store validates the embedded Session Identity first, so
    /// it cannot delete a record that belongs to another session.
    pub fn forget_session(
        &self,
        _runtime_stopped: RuntimeStoppedConfirmation,
    ) -> Result<(), SessionTabStateError> {
        let _lock = SessionStateFileLock::acquire(&self.path, FileLockMode::Exclusive)?;
        let _ = self.load_locked()?;
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Explicitly replaces invalid state after preserving its original bytes
    /// beside the state file for inspection. A caller cannot accidentally
    /// invoke this by passing a boolean; it must construct the confirmation.
    pub fn repair_discard(
        &self,
        _confirmation: RepairConfirmation,
    ) -> Result<RepairDiscardOutcome, SessionTabStateError> {
        let _lock = SessionStateFileLock::acquire(&self.path, FileLockMode::Exclusive)?;
        let contents = match read_session_state_bytes(&self.path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(RepairDiscardOutcome::NothingToRepair);
            }
            Err(error) => return Err(error.into()),
        };

        if let Ok(state) = serde_json::from_slice::<SessionTabState>(&contents) {
            match state.validate_identity(&self.session) {
                Ok(()) => return Ok(RepairDiscardOutcome::NothingToRepair),
                Err(error @ SessionTabStateError::IdentityMismatch { .. }) => return Err(error),
                Err(SessionTabStateError::UnsupportedVersion(_)) => {}
                Err(error) => return Err(error),
            }
        }

        let archived_evidence_path = archive_session_state_evidence(&self.path)?;
        write_session_state_atomically(&self.path, &SessionTabState::empty_for(&self.session))?;
        Ok(RepairDiscardOutcome::Repaired {
            archived_evidence_path,
        })
    }

    fn load_locked(&self) -> Result<SessionTabState, SessionTabStateError> {
        match read_session_state_bytes(&self.path) {
            Ok(contents) => {
                let state: SessionTabState = serde_json::from_slice(&contents)?;
                state.validate_identity(&self.session)?;
                Ok(state)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(SessionTabState::empty_for(&self.session))
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// Proof that a user explicitly authorized discarding invalid persisted state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairConfirmation(());

impl RepairConfirmation {
    pub fn confirmed() -> Self {
        Self(())
    }
}

/// Proof from the runtime-control boundary that no owner currently holds the
/// Session Runtime Lease. Construction belongs at that boundary, not in a
/// user-facing CLI parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeStoppedConfirmation(());

impl RuntimeStoppedConfirmation {
    pub fn confirmed() -> Self {
        Self(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairDiscardOutcome {
    NothingToRepair,
    Repaired { archived_evidence_path: PathBuf },
}

#[derive(Debug)]
pub enum SessionTabStateError {
    Io(io::Error),
    Json(serde_json::Error),
    Path(StatePathError),
    RelativePath(PathBuf),
    UnsupportedVersion(u8),
    IdentityMismatch {
        requested_session_key: String,
        persisted_session_key: String,
    },
    UnresolvedRenameIntent {
        tab_id: String,
    },
}

impl fmt::Display for SessionTabStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "session tab state I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "session tab state is invalid JSON: {error}"),
            Self::Path(error) => write!(formatter, "invalid session tab state path: {error}"),
            Self::RelativePath(path) => write!(
                formatter,
                "session tab state path `{}` must be absolute",
                path.display()
            ),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported session tab state version `{version}`"
                )
            }
            Self::IdentityMismatch {
                requested_session_key,
                persisted_session_key,
            } => write!(
                formatter,
                "session tab state for `{persisted_session_key}` does not belong to requested Session Identity `{requested_session_key}`"
            ),
            Self::UnresolvedRenameIntent { tab_id } => write!(
                formatter,
                "tab `{tab_id}` has an unresolved Automatic Rename Intent"
            ),
        }
    }
}

impl std::error::Error for SessionTabStateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Path(error) => Some(error),
            Self::RelativePath(_)
            | Self::UnsupportedVersion(_)
            | Self::IdentityMismatch { .. }
            | Self::UnresolvedRenameIntent { .. } => None,
        }
    }
}

impl From<io::Error> for SessionTabStateError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for SessionTabStateError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<StatePathError> for SessionTabStateError {
    fn from(error: StatePathError) -> Self {
        Self::Path(error)
    }
}

enum FileLockMode {
    Shared,
    Exclusive,
}

struct SessionStateFileLock {
    file: File,
}

impl SessionStateFileLock {
    fn acquire(path: &Path, mode: FileLockMode) -> Result<Self, SessionTabStateError> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        if let Some(parent) = parent {
            fs::create_dir_all(parent)?;
        }
        let lock_path = lock_path_for(path);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(lock_path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let operation = match mode {
            FileLockMode::Shared => libc::LOCK_SH,
            FileLockMode::Exclusive => libc::LOCK_EX,
        };
        // SAFETY: `file` stays open for this guard's lifetime, and `flock`
        // only operates on its valid Unix file descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), operation) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self { file })
    }
}

impl Drop for SessionStateFileLock {
    fn drop(&mut self) {
        // SAFETY: `self.file` owns a valid file descriptor until after Drop.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn lock_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    path.with_file_name(format!(".{file_name}.lock"))
}

fn write_session_state_atomically(
    path: &Path,
    state: &SessionTabState,
) -> Result<(), SessionTabStateError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_vec_pretty(state)?;
    let temp_path = unique_session_state_temp_path(path);
    let mut temp = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp_path)?;
    temp.set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(&contents)?;
    temp.sync_all()?;
    drop(temp);
    fs::rename(&temp_path, path)?;
    if let Some(parent) = parent {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn ensure_private_state_directory(path: &Path) -> Result<(), SessionTabStateError> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::other(format!(
            "session state directory `{}` is not a real directory",
            path.display()
        ))
        .into());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn read_session_state_bytes(path: &Path) -> Result<Vec<u8>, io::Error> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)?;
    Ok(contents)
}

fn unique_session_state_temp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    let sequence = NEXT_SESSION_STATE_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(
        ".{file_name}.{}-{sequence}.tmp",
        std::process::id()
    ))
}

fn archive_session_state_evidence(path: &Path) -> Result<PathBuf, SessionTabStateError> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.json");
    let sequence = NEXT_SESSION_STATE_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let archive_path = path.with_file_name(format!(
        ".{file_name}.{}-{sequence}.invalid",
        std::process::id()
    ));
    fs::rename(path, &archive_path)?;
    Ok(archive_path)
}

pub fn lock_tab_at_path(
    path: impl AsRef<Path>,
    tab_id: &str,
    label: Option<String>,
) -> Result<(), LockStoreError> {
    mutate_store_at_path(path, |store| store.lock_tab(tab_id.to_string(), label))
}

pub fn unlock_tab_at_path(path: impl AsRef<Path>, tab_id: &str) -> Result<bool, LockStoreError> {
    mutate_store_at_path(path, |store| store.unlock_tab(tab_id))
}

pub fn unlock_all_at_path(path: impl AsRef<Path>) -> Result<(), LockStoreError> {
    mutate_store_at_path(path, LockStore::unlock_all)
}

fn mutate_store_at_path<R>(
    path: impl AsRef<Path>,
    mutate: impl FnOnce(&mut LockStore) -> R,
) -> Result<R, LockStoreError> {
    let path = path.as_ref();
    let mut store = LockStore::load(path)?;
    let result = mutate(&mut store);
    store.save(path)?;
    Ok(result)
}

pub fn unlock_focused_tab_at_path<C>(
    path: impl AsRef<Path>,
    herdr: &mut C,
) -> Result<UnlockFocusedOutcome, UnlockFocusedError>
where
    C: HerdrApi,
{
    let focused_tab_id = herdr
        .observe_focused_tab()?
        .map(|observation| observation.tab.tab_id);

    let Some(tab_id) = focused_tab_id else {
        return Ok(UnlockFocusedOutcome::NoFocusedTab);
    };

    if unlock_tab_at_path(path, &tab_id)? {
        Ok(UnlockFocusedOutcome::Unlocked { tab_id })
    } else {
        Ok(UnlockFocusedOutcome::NotLocked { tab_id })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockFocusedOutcome {
    NoFocusedTab,
    Unlocked { tab_id: String },
    NotLocked { tab_id: String },
}

#[derive(Debug)]
pub enum LockStoreError {
    Io(io::Error),
    Json(serde_json::Error),
    UnsupportedVersion(u8),
}

impl fmt::Display for LockStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "lock store I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "lock store JSON parsing failed: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported lock store version `{version}`")
            }
        }
    }
}

impl std::error::Error for LockStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::UnsupportedVersion(_) => None,
        }
    }
}

impl From<io::Error> for LockStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for LockStoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug)]
pub enum UnlockFocusedError {
    Herdr(HerdrError),
    LockStore(LockStoreError),
}

impl fmt::Display for UnlockFocusedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Herdr(error) => write!(formatter, "failed to find focused Herdr tab: {error}"),
            Self::LockStore(error) => write!(formatter, "failed to unlock focused tab: {error}"),
        }
    }
}

impl std::error::Error for UnlockFocusedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Herdr(error) => Some(error),
            Self::LockStore(error) => Some(error),
        }
    }
}

impl From<HerdrError> for UnlockFocusedError {
    fn from(error: HerdrError) -> Self {
        Self::Herdr(error)
    }
}

impl From<LockStoreError> for UnlockFocusedError {
    fn from(error: LockStoreError) -> Self {
        Self::LockStore(error)
    }
}

fn temp_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("locks.json");
    path.with_file_name(format!(".{file_name}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr_client::{PaneInfo, PaneProcessInfo, RenameTabResult, TabInfo};
    use crate::labeler::LabelCandidate;
    use crate::startup::SessionSocket;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn detects_manual_lock_when_label_differs_from_plugin_and_candidate() {
        let decision = detect_manual_lock(
            "my custom label",
            Some("nvim"),
            Some(&LabelCandidate::significant_command("codex")),
        );

        assert_eq!(
            decision,
            ManualLockDecision::Lock {
                label: "my custom label".to_string()
            }
        );
    }

    #[test]
    fn does_not_lock_when_current_label_matches_last_applied_or_seen_label() {
        let decision = detect_manual_lock(
            "nvim",
            Some("nvim"),
            Some(&LabelCandidate::significant_command("codex")),
        );

        assert_eq!(decision, ManualLockDecision::AutoManaged);
    }

    #[test]
    fn does_not_lock_when_current_label_matches_stable_label_candidate() {
        let decision = detect_manual_lock(
            "codex",
            Some("nvim"),
            Some(&LabelCandidate::significant_command("codex")),
        );

        assert_eq!(decision, ManualLockDecision::AutoManaged);
    }

    #[test]
    fn does_not_lock_without_a_prior_plugin_baseline() {
        let decision = detect_manual_lock(
            "preexisting custom label",
            None,
            Some(&LabelCandidate::working_directory_basename("tabby")),
        );

        assert_eq!(decision, ManualLockDecision::AutoManaged);
    }

    #[test]
    fn default_numeric_label_discards_state_for_reused_tab_id() {
        let mut store = LockStore::default();
        store.record_plugin_label("w2:t1", "nvim");
        store.lock_tab("w2:t1", Some("custom".to_string()));

        let changed = store.discard_tab_state_for_default_label("w2:t1", "1", Some(1));

        assert!(changed);
        assert!(!store.is_locked("w2:t1"));
        assert_eq!(store.last_plugin_label("w2:t1"), None);
    }

    #[test]
    fn non_default_numeric_label_preserves_manual_state() {
        let mut store = LockStore::default();
        store.record_plugin_label("w2:t2", "nvim");
        store.lock_tab("w2:t2", Some("1".to_string()));

        let changed = store.discard_tab_state_for_default_label("w2:t2", "1", Some(2));

        assert!(!changed);
        assert!(store.is_locked("w2:t2"));
        assert_eq!(store.last_plugin_label("w2:t2"), Some("nvim"));
    }

    #[test]
    fn lock_survives_store_reload() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("state").join("locks.json");

        let mut store = LockStore::default();
        store.lock_tab("w1:t1", Some("custom".to_string()));
        store.save(&path).expect("save lock store");

        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert!(reloaded.is_locked("w1:t1"));
        assert_eq!(reloaded.len(), 1);
        assert_eq!(
            reloaded.locks().next().and_then(ManualLock::label),
            Some("custom")
        );
    }

    #[test]
    fn unlock_tab_removes_only_that_lock() {
        let mut store = LockStore::default();
        store.record_plugin_label("w1:t1", "editor");
        store.record_plugin_label("w1:t2", "server");
        store.lock_tab("w1:t1", Some("custom one".to_string()));
        store.lock_tab("w1:t2", Some("custom two".to_string()));

        assert!(store.unlock_tab("w1:t1"));

        assert!(!store.is_locked("w1:t1"));
        assert_eq!(store.last_plugin_label("w1:t1"), None);
        assert!(store.is_locked("w1:t2"));
        assert_eq!(store.last_plugin_label("w1:t2"), Some("server"));
    }

    #[test]
    fn unlock_focused_tab_removes_only_focused_lock_from_path() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("locks.json");
        let mut store = LockStore::default();
        store.lock_tab("w1:t1", Some("editor".to_string()));
        store.lock_tab("w1:t2", Some("server".to_string()));
        store.save(&path).expect("save lock store");
        let mut herdr = FakeHerdr {
            tabs: vec![tab("w1:t1", false), tab("w1:t2", true)],
        };

        let outcome = unlock_focused_tab_at_path(&path, &mut herdr).expect("unlock focused tab");
        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert_eq!(
            outcome,
            UnlockFocusedOutcome::Unlocked {
                tab_id: "w1:t2".to_string()
            }
        );
        assert!(reloaded.is_locked("w1:t1"));
        assert!(!reloaded.is_locked("w1:t2"));
    }

    #[test]
    fn unlock_tab_at_path_removes_only_that_lock() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("locks.json");
        lock_tab_at_path(&path, "w1:t1", Some("editor".to_string())).expect("lock tab one");
        lock_tab_at_path(&path, "w1:t2", Some("server".to_string())).expect("lock tab two");

        assert!(unlock_tab_at_path(&path, "w1:t1").expect("unlock tab"));
        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert!(!reloaded.is_locked("w1:t1"));
        assert!(reloaded.is_locked("w1:t2"));
    }

    #[test]
    fn unlock_focused_reports_when_focused_tab_was_not_locked() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("locks.json");
        lock_tab_at_path(&path, "w1:t1", Some("editor".to_string())).expect("lock tab one");
        let mut herdr = FakeHerdr {
            tabs: vec![tab("w1:t2", true)],
        };

        let outcome = unlock_focused_tab_at_path(&path, &mut herdr).expect("unlock focused tab");
        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert_eq!(
            outcome,
            UnlockFocusedOutcome::NotLocked {
                tab_id: "w1:t2".to_string()
            }
        );
        assert!(reloaded.is_locked("w1:t1"));
    }

    #[test]
    fn unlock_focused_reports_when_no_tab_is_focused() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("locks.json");
        lock_tab_at_path(&path, "w1:t1", Some("editor".to_string())).expect("lock tab one");
        let mut herdr = FakeHerdr {
            tabs: vec![tab("w1:t1", false)],
        };

        let outcome = unlock_focused_tab_at_path(&path, &mut herdr).expect("unlock focused tab");
        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert_eq!(outcome, UnlockFocusedOutcome::NoFocusedTab);
        assert!(reloaded.is_locked("w1:t1"));
    }

    #[test]
    fn unlock_all_clears_all_locks() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("locks.json");
        let mut store = LockStore::default();
        store.record_plugin_label("w1:t1", "editor");
        store.record_plugin_label("w1:t2", "server");
        store.record_plugin_label("w1:t3", "codex");
        store.lock_tab("w1:t1", Some("custom one".to_string()));
        store.lock_tab("w1:t2", Some("custom two".to_string()));
        store.save(&path).expect("save lock store");

        unlock_all_at_path(&path).expect("unlock all");
        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert!(reloaded.is_empty());
        assert_eq!(reloaded.last_plugin_label("w1:t1"), None);
        assert_eq!(reloaded.last_plugin_label("w1:t2"), None);
        assert_eq!(reloaded.last_plugin_label("w1:t3"), Some("codex"));
    }

    #[test]
    fn missing_store_loads_empty_and_saves_only_to_injected_temp_path() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("nested").join("locks.json");

        let store = LockStore::load(&path).expect("missing store loads empty");
        assert!(store.is_empty());

        unlock_all_at_path(&path).expect("save empty store to injected path");
        assert!(path.exists());
        assert!(path.starts_with(temp_dir.path()));
    }

    #[test]
    fn session_state_isolated_by_lossless_session_identity() {
        let temp_dir = TestTempDir::new();
        let first = SessionSocket::resolve("/tmp/tabby-state-first.sock").expect("first session");
        let second =
            SessionSocket::resolve("/tmp/tabby-state-second.sock").expect("second session");
        let first_store =
            SessionTabStateStore::open(temp_dir.path(), &first).expect("open first state");
        let second_store =
            SessionTabStateStore::open(temp_dir.path(), &second).expect("open second state");

        first_store
            .mutate(|state| state.lock_tab("w1:t1", Some("custom".to_string())))
            .expect("persist first lock");

        assert_ne!(first_store.path(), second_store.path());
        assert!(
            first_store
                .read(|state| state.is_locked("w1:t1"))
                .expect("read first state")
        );
        assert!(
            !second_store
                .read(|state| state.is_locked("w1:t1"))
                .expect("read second state")
        );
    }

    #[test]
    fn session_state_rejects_a_record_with_another_embedded_identity() {
        let temp_dir = TestTempDir::new();
        let first = SessionSocket::resolve("/tmp/tabby-state-first.sock").expect("first session");
        let second =
            SessionSocket::resolve("/tmp/tabby-state-second.sock").expect("second session");
        let path = temp_dir.path().join("injected-state.json");
        let first_store = SessionTabStateStore::at_path(&path, &first).expect("open first state");
        first_store
            .mutate(|state| state.lock_tab("w1:t1", Some("custom".to_string())))
            .expect("persist first state");
        let second_store =
            SessionTabStateStore::at_path(&path, &second).expect("open second state");

        let error = second_store
            .read(|state| state.is_locked("w1:t1"))
            .expect_err("identity mismatch must fail closed");

        assert!(matches!(
            error,
            SessionTabStateError::IdentityMismatch { .. }
        ));
    }

    #[test]
    fn automatic_rename_intent_survives_before_rename_and_confirms_the_baseline() {
        let temp_dir = TestTempDir::new();
        let session = SessionSocket::resolve("/tmp/tabby-state.sock").expect("session");
        let store = SessionTabStateStore::open(temp_dir.path(), &session).expect("open state");

        store
            .record_automatic_rename_intent("w1:t1", "shell", "nvim")
            .expect("persist intent before rename");
        assert_eq!(
            store
                .read(SessionTabState::unresolved_rename_intent_count)
                .expect("read persisted intent"),
            1
        );

        let outcome = store
            .reconcile_automatic_rename_intent("w1:t1", "nvim", Some(1))
            .expect("reconcile confirmed rename");

        assert_eq!(
            outcome,
            RenameIntentReconciliation::Confirmed {
                intended_baseline: "nvim".to_string()
            }
        );
        assert_eq!(
            store
                .read(|state| state.last_plugin_label("w1:t1").map(str::to_string))
                .expect("read confirmed baseline"),
            Some("nvim".to_string())
        );
        assert_eq!(
            store
                .read(SessionTabState::unresolved_rename_intent_count)
                .expect("intent cleared"),
            0
        );
    }

    #[test]
    fn reconciliation_preserves_an_ambiguous_visible_label_as_manual_intent() {
        let temp_dir = TestTempDir::new();
        let session = SessionSocket::resolve("/tmp/tabby-state.sock").expect("session");
        let store = SessionTabStateStore::open(temp_dir.path(), &session).expect("open state");
        store
            .record_automatic_rename_intent("w1:t1", "shell", "nvim")
            .expect("persist intent");

        let outcome = store
            .reconcile_automatic_rename_intent("w1:t1", "a user label", Some(1))
            .expect("reconcile manual intent");

        assert_eq!(
            outcome,
            RenameIntentReconciliation::PreservedManualIntent {
                label: "a user label".to_string()
            }
        );
        assert!(
            store
                .read(|state| state.is_locked("w1:t1"))
                .expect("manual label locks tab")
        );
    }

    #[test]
    fn reconciliation_discards_unchanged_source_without_creating_a_baseline() {
        let temp_dir = TestTempDir::new();
        let session = SessionSocket::resolve("/tmp/tabby-state.sock").expect("session");
        let store = SessionTabStateStore::open(temp_dir.path(), &session).expect("open state");
        store
            .record_automatic_rename_intent("w1:t1", "shell", "nvim")
            .expect("persist intent");

        let outcome = store
            .reconcile_automatic_rename_intent("w1:t1", "shell", Some(1))
            .expect("reconcile unchanged source");

        assert_eq!(outcome, RenameIntentReconciliation::SourceUnchanged);
        assert_eq!(
            store
                .read(|state| state.last_plugin_label("w1:t1").map(str::to_string))
                .expect("read no baseline"),
            None
        );
    }

    #[test]
    fn reconciliation_discards_reused_default_tab_state_and_intent() {
        let temp_dir = TestTempDir::new();
        let session = SessionSocket::resolve("/tmp/tabby-state.sock").expect("session");
        let store = SessionTabStateStore::open(temp_dir.path(), &session).expect("open state");
        store
            .mutate(|state| {
                state.record_plugin_label("w1:t1", "old");
                state.lock_tab("w1:t1", Some("custom".to_string()));
            })
            .expect("persist prior state");
        store
            .record_automatic_rename_intent("w1:t1", "shell", "nvim")
            .expect("persist intent");

        let outcome = store
            .reconcile_automatic_rename_intent("w1:t1", "1", Some(1))
            .expect("reconcile reused tab");

        assert_eq!(outcome, RenameIntentReconciliation::ReusedTab);
        assert!(
            store
                .read(|state| !state.is_locked("w1:t1")
                    && state.last_plugin_label("w1:t1").is_none()
                    && state.unresolved_rename_intent_count() == 0)
                .expect("read discarded state")
        );
    }

    #[test]
    fn repair_discard_requires_confirmation_and_archives_invalid_evidence() {
        let temp_dir = TestTempDir::new();
        let session = SessionSocket::resolve("/tmp/tabby-state.sock").expect("session");
        let store = SessionTabStateStore::open(temp_dir.path(), &session).expect("open state");
        fs::create_dir_all(store.path().parent().expect("state parent")).expect("create parent");
        fs::write(store.path(), b"not JSON").expect("write corrupt state");

        let outcome = store
            .repair_discard(RepairConfirmation::confirmed())
            .expect("explicit repair");

        let RepairDiscardOutcome::Repaired {
            archived_evidence_path,
        } = outcome
        else {
            panic!("invalid state should require replacement");
        };
        assert_eq!(
            fs::read(archived_evidence_path).expect("preserved evidence"),
            b"not JSON"
        );
        assert_eq!(
            store
                .read(SessionTabState::lock_count)
                .expect("read repaired empty state"),
            0
        );
    }

    #[test]
    fn forget_session_refuses_to_delete_another_session_identity() {
        let temp_dir = TestTempDir::new();
        let first = SessionSocket::resolve("/tmp/tabby-state-first.sock").expect("first session");
        let second =
            SessionSocket::resolve("/tmp/tabby-state-second.sock").expect("second session");
        let path = temp_dir.path().join("injected-state.json");
        let first_store = SessionTabStateStore::at_path(&path, &first).expect("open first state");
        first_store
            .mutate(|state| state.lock_tab("w1:t1", Some("custom".to_string())))
            .expect("persist first state");
        let second_store =
            SessionTabStateStore::at_path(&path, &second).expect("open second state");

        let error = second_store
            .forget_session(RuntimeStoppedConfirmation::confirmed())
            .expect_err("cannot delete another session's state");

        assert!(matches!(
            error,
            SessionTabStateError::IdentityMismatch { .. }
        ));
        assert!(path.exists());
    }

    #[test]
    fn concurrent_session_state_mutations_do_not_lose_either_update() {
        let temp_dir = TestTempDir::new();
        let session = SessionSocket::resolve("/tmp/tabby-state.sock").expect("session");
        let store =
            Arc::new(SessionTabStateStore::open(temp_dir.path(), &session).expect("open state"));
        let barrier = Arc::new(Barrier::new(3));
        let first_store = Arc::clone(&store);
        let first_barrier = Arc::clone(&barrier);
        let first = thread::spawn(move || {
            first_barrier.wait();
            first_store
                .mutate(|state| state.lock_tab("w1:t1", Some("first".to_string())))
                .expect("persist first update");
        });
        let second_store = Arc::clone(&store);
        let second_barrier = Arc::clone(&barrier);
        let second = thread::spawn(move || {
            second_barrier.wait();
            second_store
                .mutate(|state| state.lock_tab("w1:t2", Some("second".to_string())))
                .expect("persist second update");
        });

        barrier.wait();
        first.join().expect("first thread");
        second.join().expect("second thread");

        assert!(
            store
                .read(|state| state.is_locked("w1:t1") && state.is_locked("w1:t2"))
                .expect("read both updates")
        );
    }

    #[test]
    fn stale_lock_is_retained_until_explicit_unlock() {
        let temp_dir = TestTempDir::new();
        let path = temp_dir.path().join("locks.json");
        lock_tab_at_path(&path, "stale-tab-id", Some("old custom".to_string()))
            .expect("lock stale tab id");
        let mut herdr = FakeHerdr {
            tabs: vec![tab("current-tab-id", true)],
        };

        let outcome = unlock_focused_tab_at_path(&path, &mut herdr).expect("unlock focused tab");
        let reloaded = LockStore::load(&path).expect("reload lock store");

        assert_eq!(
            outcome,
            UnlockFocusedOutcome::NotLocked {
                tab_id: "current-tab-id".to_string()
            }
        );
        assert!(reloaded.is_locked("stale-tab-id"));
        assert!(!reloaded.is_locked("current-tab-id"));
    }

    struct FakeHerdr {
        tabs: Vec<TabInfo>,
    }

    impl HerdrApi for FakeHerdr {
        fn list_tabs(&mut self) -> Result<Vec<TabInfo>, HerdrError> {
            Ok(self.tabs.clone())
        }

        fn list_panes(&mut self) -> Result<Vec<PaneInfo>, HerdrError> {
            unreachable!("unlock-focused only needs tab.list")
        }

        fn observe_focused_tab(
            &mut self,
        ) -> Result<Option<crate::herdr_client::FocusedTabObservation>, HerdrError> {
            let Some(tab) = self.tabs.iter().find(|tab| tab.focused).cloned() else {
                return Ok(None);
            };
            let pane = PaneInfo {
                pane_id: format!("{}:pane", tab.tab_id),
                terminal_id: None,
                workspace_id: tab.workspace_id.clone(),
                tab_id: tab.tab_id.clone(),
                focused: true,
                label: None,
                title: None,
                cwd: None,
                foreground_cwd: None,
                agent: None,
                display_agent: None,
                custom_status: None,
                agent_status: None,
                revision: None,
            };
            Ok(Some(crate::herdr_client::FocusedTabObservation {
                working_directory: None,
                tab,
                pane,
            }))
        }

        fn pane_process_info(&mut self, _pane_id: &str) -> Result<PaneProcessInfo, HerdrError> {
            unreachable!("unlock-focused only needs tab.list")
        }

        fn rename_tab(
            &mut self,
            _tab_id: &str,
            _label: &str,
        ) -> Result<RenameTabResult, HerdrError> {
            unreachable!("unlock-focused only needs tab.list")
        }
    }

    fn tab(tab_id: &str, focused: bool) -> TabInfo {
        TabInfo {
            tab_id: tab_id.to_string(),
            workspace_id: "w1".to_string(),
            number: None,
            label: "label".to_string(),
            focused,
            pane_count: None,
            agent_status: None,
        }
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
                "tabby-locks-test-{}-{unique}-{id}",
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
