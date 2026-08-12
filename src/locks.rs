//! Persistent Manually Locked Tab state.
//!
//! Locks are plugin-owned state, not user-editable configuration. The v1 store is
//! keyed by Herdr's `tab_id`, but those IDs can be reused after tab or workspace
//! churn. A label that exactly matches Herdr's reported tab number marks a fresh
//! lifecycle and discards stale state for that ID. Otherwise locks remain until an
//! explicit unlock operation removes them.

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
struct SessionTabStateData {
    version: u8,
    locks: BTreeMap<String, ManualLock>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    last_plugin_labels: BTreeMap<String, String>,
}

impl Default for SessionTabStateData {
    fn default() -> Self {
        Self {
            version: 1,
            locks: BTreeMap::new(),
            last_plugin_labels: BTreeMap::new(),
        }
    }
}

impl SessionTabStateData {
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
    lifecycle: TabLifecycleEvidence,
}

/// Snapshot evidence that makes a `tab_id` meaningful for one tab lifecycle.
///
/// Herdr may reuse a tab id after workspace churn. An unresolved rename intent
/// therefore cannot be reconciled against an observation unless this evidence
/// still matches the tab and pane from which the intent was recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabLifecycleEvidence {
    workspace_id: String,
    tab_number: Option<u64>,
    pane_id: String,
    pane_revision: Option<u64>,
}

impl TabLifecycleEvidence {
    pub fn new(
        workspace_id: impl Into<String>,
        tab_number: Option<u64>,
        pane_id: impl Into<String>,
        pane_revision: Option<u64>,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            tab_number,
            pane_id: pane_id.into(),
            pane_revision,
        }
    }
}

impl AutomaticRenameIntent {
    pub fn new(
        tab_id: impl Into<String>,
        previous_label: impl Into<String>,
        intended_baseline: impl Into<String>,
        lifecycle: TabLifecycleEvidence,
    ) -> Self {
        Self {
            tab_id: tab_id.into(),
            previous_label: previous_label.into(),
            intended_baseline: intended_baseline.into(),
            lifecycle,
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

    pub fn lifecycle(&self) -> &TabLifecycleEvidence {
        &self.lifecycle
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
    locks: SessionTabStateData,
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
            locks: SessionTabStateData::default(),
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
        lifecycle: &TabLifecycleEvidence,
    ) -> RenameIntentReconciliation {
        let Some(intent) = self.rename_intents.get(tab_id).cloned() else {
            return RenameIntentReconciliation::NoIntent;
        };

        if intent.lifecycle != *lifecycle
            || is_default_tab_label(visible_label, lifecycle.tab_number)
        {
            self.rename_intents.remove(tab_id);
            self.locks.discard_tab_state(tab_id);
            return RenameIntentReconciliation::ReusedTab;
        }

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
    legacy_evidence_path: Option<PathBuf>,
}

/// A non-mutating diagnostic projection of one session's retained state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTabStateInspection {
    Missing,
    Valid {
        manual_locks: usize,
        baselines: usize,
        unresolved_rename_intents: usize,
    },
    Fault {
        diagnostic: String,
    },
}

impl SessionTabStateStore {
    /// Inspects state without opening a lock, creating a directory, or changing
    /// permissions. Runtime Status uses this seam to remain strictly read-only.
    pub fn inspect_read_only(
        state_base: impl AsRef<Path>,
        session: &SessionSocket,
    ) -> SessionTabStateInspection {
        let state_base = state_base.as_ref();
        let path = match session_tab_state_path(state_base, &session.session_key) {
            Ok(path) => path,
            Err(error) => {
                return SessionTabStateInspection::Fault {
                    diagnostic: error.to_string(),
                };
            }
        };
        let legacy_path = state_base.join("locks.json");
        match fs::symlink_metadata(&legacy_path) {
            Ok(_) => {
                return SessionTabStateInspection::Fault {
                    diagnostic: format!(
                        "obsolete identity-less lock evidence at `{}` requires `tabby repair-state --discard`",
                        legacy_path.display()
                    ),
                };
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return SessionTabStateInspection::Fault {
                    diagnostic: format!("cannot inspect `{}`: {error}", legacy_path.display()),
                };
            }
        }
        let contents = match read_session_state_bytes(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return SessionTabStateInspection::Missing;
            }
            Err(error) => {
                return SessionTabStateInspection::Fault {
                    diagnostic: format!("cannot read `{}`: {error}", path.display()),
                };
            }
        };
        let state = match serde_json::from_slice::<SessionTabState>(&contents) {
            Ok(state) => state,
            Err(error) => {
                return SessionTabStateInspection::Fault {
                    diagnostic: format!("invalid JSON in `{}`: {error}", path.display()),
                };
            }
        };
        if let Err(error) = state.validate_identity(session) {
            return SessionTabStateInspection::Fault {
                diagnostic: error.to_string(),
            };
        }
        SessionTabStateInspection::Valid {
            manual_locks: state.lock_count(),
            baselines: state.baseline_count(),
            unresolved_rename_intents: state.unresolved_rename_intent_count(),
        }
    }

    pub fn open(
        state_base: impl AsRef<Path>,
        session: &SessionSocket,
    ) -> Result<Self, SessionTabStateError> {
        let path = session_tab_state_path(state_base, &session.session_key)?;
        let legacy_evidence_path = legacy_lock_store_path(&path)?;
        ensure_no_legacy_global_evidence(&legacy_evidence_path)?;
        ensure_private_state_directory(
            path.parent()
                .ok_or_else(|| SessionTabStateError::RelativePath(path.clone()))?,
        )?;
        Self::at_path_with_legacy_evidence(path, session, Some(legacy_evidence_path))
    }

    /// Opens the selected state only for an explicit repair action. Runtime
    /// evaluation must use `open`, which rejects identity-less legacy evidence.
    pub fn open_for_repair(
        state_base: impl AsRef<Path>,
        session: &SessionSocket,
    ) -> Result<Self, SessionTabStateError> {
        let path = session_tab_state_path(state_base, &session.session_key)?;
        let legacy_evidence_path = legacy_lock_store_path(&path)?;
        ensure_private_state_directory(
            path.parent()
                .ok_or_else(|| SessionTabStateError::RelativePath(path.clone()))?,
        )?;
        Self::at_path_with_legacy_evidence(path, session, Some(legacy_evidence_path))
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
        Self::at_path_with_legacy_evidence(path, session, None)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the validated Session Identity this state store belongs to.
    pub fn session_identity(&self) -> &SessionSocket {
        &self.session
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
        lifecycle: TabLifecycleEvidence,
    ) -> Result<(), SessionTabStateError> {
        let intent =
            AutomaticRenameIntent::new(tab_id, previous_label, intended_baseline, lifecycle);
        self.mutate(|state| state.record_automatic_rename_intent(intent))?
    }

    pub fn reconcile_automatic_rename_intent(
        &self,
        tab_id: &str,
        visible_label: &str,
        lifecycle: &TabLifecycleEvidence,
    ) -> Result<RenameIntentReconciliation, SessionTabStateError> {
        self.mutate(|state| {
            state.reconcile_automatic_rename_intent(tab_id, visible_label, lifecycle)
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
        let legacy_path = legacy_lock_store_path(&self.path)?;
        let legacy_evidence_exists = match fs::symlink_metadata(&legacy_path) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        let contents = match read_session_state_bytes(&self.path) {
            Ok(contents) => Some(contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let state_is_valid = contents.as_ref().is_some_and(|contents| {
            serde_json::from_slice::<SessionTabState>(contents)
                .is_ok_and(|state| state.validate_identity(&self.session).is_ok())
        });
        if (contents.is_none() || state_is_valid) && !legacy_evidence_exists {
            return Ok(RepairDiscardOutcome::NothingToRepair);
        }

        let archived_evidence_path = if contents.is_some() {
            archive_session_state_evidence(&self.path)?
        } else {
            archive_session_state_evidence(&legacy_path)?
        };
        if legacy_evidence_exists && contents.is_some() {
            archive_session_state_evidence(&legacy_path)?;
        }
        write_session_state_atomically(&self.path, &SessionTabState::empty_for(&self.session))?;
        Ok(RepairDiscardOutcome::Repaired {
            archived_evidence_path,
        })
    }

    fn load_locked(&self) -> Result<SessionTabState, SessionTabStateError> {
        if let Some(legacy_evidence_path) = &self.legacy_evidence_path {
            ensure_no_legacy_global_evidence(legacy_evidence_path)?;
        }
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

    fn at_path_with_legacy_evidence(
        path: PathBuf,
        session: &SessionSocket,
        legacy_evidence_path: Option<PathBuf>,
    ) -> Result<Self, SessionTabStateError> {
        if !path.is_absolute() {
            return Err(SessionTabStateError::RelativePath(path));
        }
        Ok(Self {
            path,
            session: session.clone(),
            legacy_evidence_path,
        })
    }
}

fn legacy_lock_store_path(state_path: &Path) -> Result<PathBuf, SessionTabStateError> {
    let state_base = state_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| SessionTabStateError::RelativePath(state_path.to_path_buf()))?;
    Ok(state_base.join("locks.json"))
}

fn ensure_no_legacy_global_evidence(path: &Path) -> Result<(), SessionTabStateError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(SessionTabStateError::LegacyGlobalEvidence {
            path: path.to_path_buf(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
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
    LegacyGlobalEvidence {
        path: PathBuf,
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
            Self::LegacyGlobalEvidence { path } => write!(
                formatter,
                "identity-less legacy lock evidence at `{}` requires `tabby repair-state --discard`",
                path.display()
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
            | Self::LegacyGlobalEvidence { .. }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labeler::LabelCandidate;
    use crate::startup::SessionSocket;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

    fn test_lifecycle() -> TabLifecycleEvidence {
        TabLifecycleEvidence::new("w1", Some(1), "w1:p1", Some(7))
    }

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
            Some(&LabelCandidate::working_directory_suffix("tabby")),
        );

        assert_eq!(decision, ManualLockDecision::AutoManaged);
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
        let path = session_tab_state_path(temp_dir.path(), &second.session_key)
            .expect("second session path");
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
            .record_automatic_rename_intent("w1:t1", "shell", "nvim", test_lifecycle())
            .expect("persist intent before rename");
        assert_eq!(
            store
                .read(SessionTabState::unresolved_rename_intent_count)
                .expect("read persisted intent"),
            1
        );

        let outcome = store
            .reconcile_automatic_rename_intent("w1:t1", "nvim", &test_lifecycle())
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
    fn reconciliation_discards_an_intent_from_a_reused_tab_lifecycle() {
        let temp_dir = TestTempDir::new();
        let session = SessionSocket::resolve("/tmp/tabby-state.sock").expect("session");
        let store = SessionTabStateStore::open(temp_dir.path(), &session).expect("open state");
        store
            .record_automatic_rename_intent("w1:t1", "shell", "nvim", test_lifecycle())
            .expect("persist intent");

        let outcome = store
            .reconcile_automatic_rename_intent(
                "w1:t1",
                "shell",
                &TabLifecycleEvidence::new("w2", Some(1), "w2:p1", Some(8)),
            )
            .expect("reconcile reused lifecycle");

        assert_eq!(outcome, RenameIntentReconciliation::ReusedTab);
        assert_eq!(
            store
                .read(SessionTabState::unresolved_rename_intent_count)
                .expect("read state"),
            0
        );
    }

    #[test]
    fn read_only_inspection_does_not_create_the_missing_state_directory() {
        let temp_dir = TestTempDir::new();
        let session = SessionSocket::resolve("/tmp/tabby-state.sock").expect("session");
        let state_directory = temp_dir.path().join("session-tab-state");

        let inspection = SessionTabStateStore::inspect_read_only(temp_dir.path(), &session);

        assert_eq!(inspection, SessionTabStateInspection::Missing);
        assert!(
            !state_directory.exists(),
            "status inspection must not create state directories or lock files"
        );
    }

    #[test]
    fn read_only_inspection_reports_identity_fault_without_repairing_it() {
        let temp_dir = TestTempDir::new();
        let first = SessionSocket::resolve("/tmp/tabby-state-first.sock").expect("first");
        let second = SessionSocket::resolve("/tmp/tabby-state-second.sock").expect("second");
        let path = session_tab_state_path(temp_dir.path(), &second.session_key)
            .expect("second session path");
        let first_store = SessionTabStateStore::at_path(&path, &first).expect("first store");
        first_store
            .mutate(|state| state.lock_tab("w1:t1", Some("custom".to_string())))
            .expect("persist state");

        let inspection = SessionTabStateStore::inspect_read_only(temp_dir.path(), &second);

        assert!(matches!(
            inspection,
            SessionTabStateInspection::Fault { diagnostic }
                if diagnostic.contains("does not belong")
        ));
        assert!(
            path.exists(),
            "inspection does not rewrite injected evidence"
        );
    }

    #[test]
    fn reconciliation_preserves_an_ambiguous_visible_label_as_manual_intent() {
        let temp_dir = TestTempDir::new();
        let session = SessionSocket::resolve("/tmp/tabby-state.sock").expect("session");
        let store = SessionTabStateStore::open(temp_dir.path(), &session).expect("open state");
        store
            .record_automatic_rename_intent("w1:t1", "shell", "nvim", test_lifecycle())
            .expect("persist intent");

        let outcome = store
            .reconcile_automatic_rename_intent("w1:t1", "a user label", &test_lifecycle())
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
            .record_automatic_rename_intent("w1:t1", "shell", "nvim", test_lifecycle())
            .expect("persist intent");

        let outcome = store
            .reconcile_automatic_rename_intent("w1:t1", "shell", &test_lifecycle())
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
            .record_automatic_rename_intent("w1:t1", "shell", "nvim", test_lifecycle())
            .expect("persist intent");

        let outcome = store
            .reconcile_automatic_rename_intent("w1:t1", "1", &test_lifecycle())
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
    fn repair_discard_leaves_missing_state_missing() {
        let temp_dir = TestTempDir::new();
        let session = SessionSocket::resolve("/tmp/tabby-state.sock").expect("session");
        let store = SessionTabStateStore::open(temp_dir.path(), &session).expect("open state");

        let outcome = store
            .repair_discard(RepairConfirmation::confirmed())
            .expect("repair inspection");

        assert_eq!(outcome, RepairDiscardOutcome::NothingToRepair);
        assert!(!store.path().exists());
    }

    #[test]
    fn legacy_global_lock_evidence_fails_closed_until_explicit_repair() {
        let temp_dir = TestTempDir::new();
        let session = SessionSocket::resolve("/tmp/tabby-state.sock").expect("session");
        let legacy_path = temp_dir.path().join("locks.json");
        fs::write(&legacy_path, b"identity-less evidence").expect("write legacy evidence");

        let inspection = SessionTabStateStore::inspect_read_only(temp_dir.path(), &session);
        assert!(matches!(
            inspection,
            SessionTabStateInspection::Fault { diagnostic }
                if diagnostic.contains("identity-less")
        ));

        let error = SessionTabStateStore::open(temp_dir.path(), &session)
            .expect_err("runtime state must fail closed before mutation");
        assert!(matches!(
            error,
            SessionTabStateError::LegacyGlobalEvidence { .. }
        ));

        let repair_store = SessionTabStateStore::open_for_repair(temp_dir.path(), &session)
            .expect("repair access remains available");
        let outcome = repair_store
            .repair_discard(RepairConfirmation::confirmed())
            .expect("archive legacy evidence");
        assert!(matches!(outcome, RepairDiscardOutcome::Repaired { .. }));
        assert!(!legacy_path.exists());
        let store = SessionTabStateStore::open(temp_dir.path(), &session)
            .expect("open succeeds after explicit repair");
        assert_eq!(
            store
                .read(SessionTabState::lock_count)
                .expect("read repaired state"),
            0
        );
        assert_eq!(
            SessionTabStateStore::inspect_read_only(temp_dir.path(), &session),
            SessionTabStateInspection::Valid {
                manual_locks: 0,
                baselines: 0,
                unresolved_rename_intents: 0,
            }
        );
    }

    #[test]
    fn repair_discard_archives_identity_mismatched_evidence_before_replacing_it() {
        let temp_dir = TestTempDir::new();
        let first = SessionSocket::resolve("/tmp/tabby-state-first.sock").expect("first");
        let second = SessionSocket::resolve("/tmp/tabby-state-second.sock").expect("second");
        let path = session_tab_state_path(temp_dir.path(), &second.session_key)
            .expect("second session path");
        let first_store = SessionTabStateStore::at_path(&path, &first).expect("first store");
        first_store
            .mutate(|state| state.lock_tab("w1:t1", Some("custom".to_string())))
            .expect("persist mismatched state");
        let second_store = SessionTabStateStore::at_path(&path, &second).expect("second store");

        let outcome = second_store
            .repair_discard(RepairConfirmation::confirmed())
            .expect("explicit repair archives mismatched evidence");

        let RepairDiscardOutcome::Repaired {
            archived_evidence_path,
        } = outcome
        else {
            panic!("identity mismatch requires explicit replacement");
        };
        assert!(archived_evidence_path.exists());
        assert_eq!(
            second_store
                .read(SessionTabState::lock_count)
                .expect("replacement belongs to requested session"),
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
