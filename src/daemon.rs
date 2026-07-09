//! Herdr tab auto-renaming orchestration.
//!
//! The normal runtime path is a short One-Shot Refresh: wait before entering this
//! module, inspect only the currently focused tab, optionally rename it once, and
//! exit. Runtime state is intentionally injectable. The persisted lock store
//! remains plugin-owned state keyed by Herdr `tab_id`; whether those IDs survive
//! Herdr restarts is still an open design decision documented in
//! `docs/design/open-decisions.md`.

use crate::herdr_client::{
    HerdrApi, HerdrClient, HerdrError, PaneInfo, TabInfo, UnixSocketTransport,
};
use crate::labeler::{LabelCandidate, LabelPolicy};
use crate::locks::{
    LockStore, LockStoreError, ManualLockDecision, detect_manual_lock, unlock_all_at_path,
    unlock_focused_tab_at_path,
};
use crate::paths::{StatePathError, lock_store_path_from_runtime};
use crate::stability::{StabilityDecision, StabilityPolicy, StabilityState};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_REFRESH_STABILIZATION_DELAY: Duration = Duration::from_millis(400);

#[derive(Debug)]
pub struct DaemonState {
    tabs: BTreeMap<String, TabRuntimeState>,
    locks: LockStore,
    label_policy: LabelPolicy,
    stability_policy: StabilityPolicy,
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new(LockStore::default())
    }
}

impl DaemonState {
    pub fn new(locks: LockStore) -> Self {
        Self {
            tabs: BTreeMap::new(),
            locks,
            label_policy: LabelPolicy::default(),
            stability_policy: StabilityPolicy::default(),
        }
    }

    pub fn load(lock_store_path: impl AsRef<Path>) -> Result<Self, DaemonError> {
        Ok(Self::new(LockStore::load(lock_store_path)?))
    }

    pub fn locks(&self) -> &LockStore {
        &self.locks
    }

    pub fn locks_mut(&mut self) -> &mut LockStore {
        &mut self.locks
    }
}

#[derive(Debug, Clone)]
struct TabRuntimeState {
    stability: StabilityState,
    last_plugin_label: Option<String>,
}

impl TabRuntimeState {
    fn new(stability_policy: StabilityPolicy) -> Self {
        Self {
            stability: StabilityState::new(stability_policy),
            last_plugin_label: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub tabs: Vec<TabTickReport>,
}

impl TickReport {
    fn has_new_lock(&self) -> bool {
        self.tabs
            .iter()
            .any(|tab| matches!(tab.action, TabTickAction::SkippedManualLockCreated { .. }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabTickReport {
    pub tab_id: String,
    pub current_label: String,
    pub selected_pane_id: Option<String>,
    pub raw_candidate_label: Option<String>,
    pub stable_candidate_label: Option<String>,
    pub process_info_error: Option<String>,
    pub action: TabTickAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabTickAction {
    SkippedLocked,
    SkippedInactive,
    SkippedNoPane,
    SkippedNoCandidate,
    DeferredUnstable { candidate_label: String },
    SkippedManualLockCreated { locked_label: String },
    SkippedAlreadyCurrent { label: String },
    Renamed { from: String, to: String },
}

fn skipped_tab_report(tab: TabInfo, action: TabTickAction) -> TabTickReport {
    TabTickReport {
        tab_id: tab.tab_id,
        current_label: tab.label,
        selected_pane_id: None,
        raw_candidate_label: None,
        stable_candidate_label: None,
        process_info_error: None,
        action,
    }
}

pub fn tick<C>(
    herdr: &mut C,
    state: &mut DaemonState,
    observed_at: Instant,
) -> Result<TickReport, DaemonError>
where
    C: HerdrApi,
{
    let tabs = herdr.list_tabs()?;
    let panes = herdr.list_panes()?;
    let mut reports = Vec::with_capacity(tabs.len());

    for tab in tabs {
        if state.locks.is_locked(&tab.tab_id) {
            reports.push(skipped_tab_report(tab, TabTickAction::SkippedLocked));
            continue;
        }

        if !tab.focused {
            reports.push(skipped_tab_report(tab, TabTickAction::SkippedInactive));
            continue;
        }

        let Some(selection) = select_pane_for_tab(&panes, &tab.tab_id) else {
            reports.push(skipped_tab_report(tab, TabTickAction::SkippedNoPane));
            continue;
        };
        let pane = selection.pane;

        let (process_info, process_info_error) = if selection.inspect_process {
            match herdr.pane_process_info(&pane.pane_id) {
                Ok(process_info) => (Some(process_info), None),
                Err(error) => (None, Some(error.to_string())),
            }
        } else {
            (None, None)
        };

        let Some(candidate) = state
            .label_policy
            .candidate_for_pane(pane, process_info.as_ref())
        else {
            reports.push(TabTickReport {
                tab_id: tab.tab_id,
                current_label: tab.label,
                selected_pane_id: Some(pane.pane_id.clone()),
                raw_candidate_label: None,
                stable_candidate_label: None,
                process_info_error,
                action: TabTickAction::SkippedNoCandidate,
            });
            continue;
        };

        let raw_candidate_label = candidate.label().to_string();
        let tab_id = tab.tab_id;
        let current_label = tab.label;
        let runtime = state
            .tabs
            .entry(tab_id.clone())
            .or_insert_with(|| TabRuntimeState::new(state.stability_policy));
        let stability_decision = runtime.stability.observe(candidate, observed_at);
        let stable_label = stable_label_from_decision(&stability_decision).map(str::to_string);
        let stable_candidate = stable_label
            .as_ref()
            .map(|label| LabelCandidate::working_directory_basename(label.clone()));

        if let ManualLockDecision::Lock { label } = detect_manual_lock(
            &current_label,
            runtime.last_plugin_label.as_deref(),
            stable_candidate.as_ref(),
        ) {
            state.locks.lock_tab(tab_id.clone(), Some(label.clone()));
            reports.push(TabTickReport {
                tab_id,
                current_label,
                selected_pane_id: Some(pane.pane_id.clone()),
                raw_candidate_label: Some(raw_candidate_label),
                stable_candidate_label: stable_label,
                process_info_error,
                action: TabTickAction::SkippedManualLockCreated {
                    locked_label: label,
                },
            });
            continue;
        }

        let action = match stability_decision {
            StabilityDecision::Pending => TabTickAction::DeferredUnstable {
                candidate_label: raw_candidate_label.clone(),
            },
            StabilityDecision::Rename { label } => {
                if label == current_label {
                    runtime.last_plugin_label = Some(label.clone());
                    TabTickAction::SkippedAlreadyCurrent { label }
                } else {
                    herdr.rename_tab(&tab_id, &label)?;
                    let from = current_label.clone();
                    runtime.last_plugin_label = Some(label.clone());
                    TabTickAction::Renamed { from, to: label }
                }
            }
            StabilityDecision::NoOp { label } => {
                if label == current_label {
                    runtime.last_plugin_label = Some(label.clone());
                }
                TabTickAction::SkippedAlreadyCurrent { label }
            }
        };

        reports.push(TabTickReport {
            tab_id,
            current_label,
            selected_pane_id: Some(pane.pane_id.clone()),
            raw_candidate_label: Some(raw_candidate_label),
            stable_candidate_label: stable_label,
            process_info_error,
            action,
        });
    }

    Ok(TickReport { tabs: reports })
}

pub fn tick_and_save_locks<C>(
    herdr: &mut C,
    state: &mut DaemonState,
    lock_store_path: impl AsRef<Path>,
    observed_at: Instant,
) -> Result<TickReport, DaemonError>
where
    C: HerdrApi,
{
    let report = tick(herdr, state, observed_at)?;
    if report.has_new_lock() {
        state.locks.save(lock_store_path)?;
    }
    Ok(report)
}

pub fn refresh_once<C>(
    herdr: &mut C,
    lock_store_path: impl AsRef<Path>,
) -> Result<TickReport, DaemonError>
where
    C: HerdrApi,
{
    let lock_store_path = lock_store_path.as_ref();
    let mut locks = LockStore::load(lock_store_path)?;
    let (report, store_changed) = refresh_focused_tab(herdr, &mut locks)?;
    if store_changed {
        locks.save(lock_store_path)?;
    }
    Ok(report)
}

fn refresh_focused_tab<C>(
    herdr: &mut C,
    locks: &mut LockStore,
) -> Result<(TickReport, bool), DaemonError>
where
    C: HerdrApi,
{
    let Some(tab) = herdr.list_tabs()?.into_iter().find(|tab| tab.focused) else {
        return Ok((TickReport { tabs: Vec::new() }, false));
    };

    if locks.is_locked(&tab.tab_id) {
        return Ok((
            TickReport {
                tabs: vec![skipped_tab_report(tab, TabTickAction::SkippedLocked)],
            },
            false,
        ));
    }

    let panes = herdr.list_panes()?;
    let Some(selection) = select_pane_for_tab(&panes, &tab.tab_id) else {
        return Ok((
            TickReport {
                tabs: vec![skipped_tab_report(tab, TabTickAction::SkippedNoPane)],
            },
            false,
        ));
    };
    let pane = selection.pane;

    let (process_info, process_info_error) = if selection.inspect_process {
        match herdr.pane_process_info(&pane.pane_id) {
            Ok(process_info) => (Some(process_info), None),
            Err(error) => (None, Some(error.to_string())),
        }
    } else {
        (None, None)
    };

    let Some(candidate) = LabelPolicy::default().candidate_for_pane(pane, process_info.as_ref())
    else {
        return Ok((
            TickReport {
                tabs: vec![TabTickReport {
                    tab_id: tab.tab_id,
                    current_label: tab.label,
                    selected_pane_id: Some(pane.pane_id.clone()),
                    raw_candidate_label: None,
                    stable_candidate_label: None,
                    process_info_error,
                    action: TabTickAction::SkippedNoCandidate,
                }],
            },
            false,
        ));
    };

    let candidate_label = candidate.label().to_string();
    let tab_id = tab.tab_id;
    let current_label = tab.label;
    let mut store_changed = false;
    let action = if let ManualLockDecision::Lock { label } = detect_manual_lock(
        &current_label,
        locks.last_plugin_label(&tab_id),
        Some(&candidate),
    ) {
        locks.lock_tab(tab_id.clone(), Some(label.clone()));
        store_changed = true;
        TabTickAction::SkippedManualLockCreated {
            locked_label: label,
        }
    } else if candidate_label == current_label {
        store_changed |= locks.record_plugin_label(tab_id.clone(), candidate_label.clone());
        TabTickAction::SkippedAlreadyCurrent {
            label: candidate_label.clone(),
        }
    } else {
        herdr.rename_tab(&tab_id, &candidate_label)?;
        store_changed |= locks.record_plugin_label(tab_id.clone(), candidate_label.clone());
        TabTickAction::Renamed {
            from: current_label.clone(),
            to: candidate_label.clone(),
        }
    };

    Ok((
        TickReport {
            tabs: vec![TabTickReport {
                tab_id,
                current_label,
                selected_pane_id: Some(pane.pane_id.clone()),
                raw_candidate_label: Some(candidate_label.clone()),
                stable_candidate_label: Some(candidate_label),
                process_info_error,
                action,
            }],
        },
        store_changed,
    ))
}

pub fn run_one_shot_refresh_from_env() -> Result<String, RuntimeError> {
    thread::sleep(DEFAULT_REFRESH_STABILIZATION_DELAY);
    let lock_store_path = lock_store_path_from_runtime()?;
    let transport = UnixSocketTransport::from_env()?;
    let mut client = HerdrClient::new(transport);
    let report = refresh_once(&mut client, lock_store_path)?;
    Ok(format!("tabby refresh: {report:?}"))
}

pub fn unlock_focused_from_env() -> Result<String, RuntimeError> {
    let lock_store_path = lock_store_path_from_runtime()?;
    let transport = UnixSocketTransport::from_env()?;
    let mut client = HerdrClient::new(transport);
    let outcome = unlock_focused_tab_at_path(lock_store_path, &mut client)?;
    Ok(format!("tabby unlock-focused: {outcome:?}"))
}

pub fn unlock_all_from_env() -> Result<String, RuntimeError> {
    let lock_store_path = lock_store_path_from_runtime()?;
    unlock_all_at_path(lock_store_path)?;
    Ok("tabby unlock-all: cleared persisted manual locks".to_string())
}

#[derive(Debug, Clone, Copy)]
struct PaneSelection<'a> {
    pane: &'a PaneInfo,
    inspect_process: bool,
}

fn select_pane_for_tab<'a>(panes: &'a [PaneInfo], tab_id: &str) -> Option<PaneSelection<'a>> {
    let mut tab_panes = panes.iter().filter(|pane| pane.tab_id == tab_id);
    let first = tab_panes.next()?;

    if first.focused {
        return Some(PaneSelection {
            pane: first,
            inspect_process: true,
        });
    }

    let mut pane_count = 1;
    for pane in tab_panes {
        pane_count += 1;
        if pane.focused {
            return Some(PaneSelection {
                pane,
                inspect_process: true,
            });
        }
    }

    Some(PaneSelection {
        pane: first,
        inspect_process: pane_count == 1,
    })
}

fn stable_label_from_decision(decision: &StabilityDecision) -> Option<&str> {
    match decision {
        StabilityDecision::Pending => None,
        StabilityDecision::Rename { label } | StabilityDecision::NoOp { label } => Some(label),
    }
}

#[derive(Debug)]
pub enum DaemonError {
    Herdr(HerdrError),
    LockStore(LockStoreError),
}

impl fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Herdr(error) => write!(formatter, "daemon Herdr operation failed: {error}"),
            Self::LockStore(error) => {
                write!(formatter, "daemon lock store operation failed: {error}")
            }
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Herdr(error) => Some(error),
            Self::LockStore(error) => Some(error),
        }
    }
}

impl From<HerdrError> for DaemonError {
    fn from(error: HerdrError) -> Self {
        Self::Herdr(error)
    }
}

impl From<LockStoreError> for DaemonError {
    fn from(error: LockStoreError) -> Self {
        Self::LockStore(error)
    }
}

#[derive(Debug)]
pub enum RuntimeError {
    StatePath(StatePathError),
    Herdr(HerdrError),
    LockStore(LockStoreError),
    UnlockFocused(crate::locks::UnlockFocusedError),
    Daemon(DaemonError),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StatePath(error) => write!(
                formatter,
                "failed to resolve Tabby lock store path: {error}"
            ),
            Self::Herdr(error) => write!(formatter, "Herdr runtime setup failed: {error}"),
            Self::LockStore(error) => {
                write!(formatter, "lock store runtime operation failed: {error}")
            }
            Self::UnlockFocused(error) => write!(formatter, "unlock-focused failed: {error}"),
            Self::Daemon(error) => write!(formatter, "daemon failed: {error}"),
        }
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StatePath(error) => Some(error),
            Self::Herdr(error) => Some(error),
            Self::LockStore(error) => Some(error),
            Self::UnlockFocused(error) => Some(error),
            Self::Daemon(error) => Some(error),
        }
    }
}

impl From<StatePathError> for RuntimeError {
    fn from(error: StatePathError) -> Self {
        Self::StatePath(error)
    }
}

impl From<HerdrError> for RuntimeError {
    fn from(error: HerdrError) -> Self {
        Self::Herdr(error)
    }
}

impl From<LockStoreError> for RuntimeError {
    fn from(error: LockStoreError) -> Self {
        Self::LockStore(error)
    }
}

impl From<crate::locks::UnlockFocusedError> for RuntimeError {
    fn from(error: crate::locks::UnlockFocusedError) -> Self {
        Self::UnlockFocused(error)
    }
}

impl From<DaemonError> for RuntimeError {
    fn from(error: DaemonError) -> Self {
        Self::Daemon(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr_client::{PaneProcess, PaneProcessInfo, RenameTabResult, TabInfo};
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn unlocked_tab_renames_when_stable_candidate_differs_from_current_label() {
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "old", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = DaemonState::default();

        let first = tick(&mut herdr, &mut state, start).expect("first tick");
        let second =
            tick(&mut herdr, &mut state, start + Duration::from_millis(500)).expect("second tick");

        assert_eq!(
            herdr.renames,
            vec![("w1:t1".to_string(), "nvim".to_string())]
        );
        assert_eq!(
            first.tabs[0].action,
            TabTickAction::DeferredUnstable {
                candidate_label: "nvim".to_string()
            }
        );
        assert_eq!(
            second.tabs[0].action,
            TabTickAction::Renamed {
                from: "old".to_string(),
                to: "nvim".to_string()
            }
        );
    }

    #[test]
    fn no_op_when_stable_candidate_matches_current_label() {
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "nvim", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = DaemonState::default();

        tick(&mut herdr, &mut state, start).expect("first tick");
        let report =
            tick(&mut herdr, &mut state, start + Duration::from_millis(500)).expect("second tick");

        assert!(herdr.renames.is_empty());
        assert_eq!(
            report.tabs[0].action,
            TabTickAction::SkippedAlreadyCurrent {
                label: "nvim".to_string()
            }
        );
    }

    #[test]
    fn manually_locked_tabs_are_skipped_without_renaming() {
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "custom", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = DaemonState::default();
        state
            .locks_mut()
            .lock_tab("w1:t1", Some("custom".to_string()));

        let report = tick(&mut herdr, &mut state, start).expect("tick");

        assert!(herdr.process_info_calls.is_empty());
        assert!(herdr.renames.is_empty());
        assert_eq!(report.tabs[0].action, TabTickAction::SkippedLocked);
    }

    #[test]
    fn manual_label_change_creates_persistent_lock() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "old", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = DaemonState::load(&lock_path).expect("load daemon state");

        tick(&mut herdr, &mut state, start).expect("first tick");
        tick(&mut herdr, &mut state, start + Duration::from_millis(500)).expect("rename tick");
        herdr.set_tab_label("w1:t1", "my custom label");

        let report = tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(1000),
        )
        .expect("manual lock tick");
        let persisted = LockStore::load(&lock_path).expect("reload lock store");

        assert_eq!(
            report.tabs[0].action,
            TabTickAction::SkippedManualLockCreated {
                locked_label: "my custom label".to_string()
            }
        );
        assert!(persisted.is_locked("w1:t1"));
        assert_eq!(
            herdr.renames,
            vec![("w1:t1".to_string(), "nvim".to_string())]
        );
    }

    #[test]
    fn process_info_error_falls_back_to_cwd_basename() {
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "old", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_error("w1:p1");
        let mut state = DaemonState::default();

        tick(&mut herdr, &mut state, start).expect("first tick");
        let report =
            tick(&mut herdr, &mut state, start + Duration::from_millis(500)).expect("second tick");

        assert_eq!(
            herdr.renames,
            vec![("w1:t1".to_string(), "tabby".to_string())]
        );
        assert!(report.tabs[0].process_info_error.is_some());
        assert_eq!(report.tabs[0].raw_candidate_label.as_deref(), Some("tabby"));
        assert_eq!(
            report.tabs[0].action,
            TabTickAction::Renamed {
                from: "old".to_string(),
                to: "tabby".to_string()
            }
        );
    }

    #[test]
    fn unstable_candidates_are_deferred_without_rename() {
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "old", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = DaemonState::default();

        let report = tick(&mut herdr, &mut state, start).expect("tick");

        assert!(herdr.renames.is_empty());
        assert_eq!(
            report.tabs[0].action,
            TabTickAction::DeferredUnstable {
                candidate_label: "nvim".to_string()
            }
        );
    }

    #[test]
    fn persisted_locks_are_respected_when_daemon_state_is_recreated() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let mut store = LockStore::default();
        store.lock_tab("w1:t1", Some("custom".to_string()));
        store.save(&lock_path).expect("save lock store");
        let mut state = DaemonState::load(&lock_path).expect("load daemon state");
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "custom", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));

        let report = tick(&mut herdr, &mut state, Instant::now()).expect("tick");

        assert!(state.locks().is_locked("w1:t1"));
        assert!(herdr.renames.is_empty());
        assert_eq!(report.tabs[0].action, TabTickAction::SkippedLocked);
    }

    #[test]
    fn focused_pane_is_selected_with_first_pane_as_conservative_fallback() {
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "old", true)],
            vec![
                pane("w1:p1", "w1:t1", false, "fallback"),
                pane("w1:p2", "w1:t1", true, "focused"),
            ],
        )
        .with_process_info(process("w1:p2", "nvim", &["nvim"]));
        let mut state = DaemonState::default();

        let report = tick(&mut herdr, &mut state, start).expect("tick");

        assert_eq!(herdr.process_info_calls, vec!["w1:p2".to_string()]);
        assert_eq!(report.tabs[0].selected_pane_id.as_deref(), Some("w1:p2"));
    }

    #[test]
    fn fallback_pane_uses_cwd_without_process_inspection() {
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "old", true)],
            vec![
                pane("w1:p1", "w1:t1", false, "fallback"),
                pane("w1:p2", "w1:t1", false, "other"),
            ],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = DaemonState::default();

        tick(&mut herdr, &mut state, start).expect("first tick");
        let report =
            tick(&mut herdr, &mut state, start + Duration::from_millis(500)).expect("second tick");

        assert!(herdr.process_info_calls.is_empty());
        assert_eq!(
            herdr.renames,
            vec![("w1:t1".to_string(), "fallback".to_string())]
        );
        assert_eq!(report.tabs[0].selected_pane_id.as_deref(), Some("w1:p1"));
        assert_eq!(
            report.tabs[0].action,
            TabTickAction::Renamed {
                from: "old".to_string(),
                to: "fallback".to_string()
            }
        );
    }

    #[test]
    fn inactive_tabs_are_not_renamed_or_inspected_while_user_navigates() {
        let start = Instant::now();
        let mut herdr = focused_codex_and_inactive_nvim_tabs();
        let mut state = DaemonState::default();

        tick(&mut herdr, &mut state, start).expect("first tick");
        tick(&mut herdr, &mut state, start + Duration::from_millis(500)).expect("second tick");

        assert_eq!(
            herdr.process_info_calls,
            vec!["w1:p1".to_string(), "w1:p1".to_string()]
        );
        assert_eq!(
            herdr.renames,
            vec![("w1:t1".to_string(), "codex".to_string())]
        );
        assert_eq!(herdr.tab_label("w1:t1"), Some("codex"));
        assert_eq!(herdr.tab_label("w1:t2"), Some("old"));
    }

    #[test]
    fn one_shot_refresh_renames_only_the_focused_tab_once() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let mut herdr = focused_codex_and_inactive_nvim_tabs();

        let report = refresh_once(&mut herdr, &lock_path).expect("refresh once");

        assert_eq!(report.tabs.len(), 1);
        assert_eq!(report.tabs[0].tab_id, "w1:t1");
        assert_eq!(
            herdr.process_info_calls,
            vec!["w1:p1".to_string()],
            "one-shot refresh must inspect only the focused tab pane"
        );
        assert_eq!(
            herdr.renames,
            vec![("w1:t1".to_string(), "codex".to_string())]
        );
        assert_eq!(herdr.tab_label("w1:t1"), Some("codex"));
        assert_eq!(herdr.tab_label("w1:t2"), Some("old"));
    }

    #[test]
    fn one_shot_refresh_observes_focus_at_refresh_time() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let mut herdr = focused_codex_and_inactive_nvim_tabs();
        herdr.set_focus("w1:t2", "w1:p2");

        let report = refresh_once(&mut herdr, &lock_path).expect("refresh once");

        assert_eq!(report.tabs.len(), 1);
        assert_eq!(report.tabs[0].tab_id, "w1:t2");
        assert_eq!(herdr.process_info_calls, vec!["w1:p2".to_string()]);
        assert_eq!(
            herdr.renames,
            vec![("w1:t2".to_string(), "nvim".to_string())]
        );
        assert_eq!(herdr.tab_label("w1:t1"), Some("old"));
        assert_eq!(herdr.tab_label("w1:t2"), Some("nvim"));
    }

    #[test]
    fn one_shot_refresh_manual_label_change_creates_persistent_lock() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let mut herdr = focused_codex_and_inactive_nvim_tabs();

        refresh_once(&mut herdr, &lock_path).expect("initial refresh");
        herdr.set_tab_label("w1:t1", "my custom label");

        let report = refresh_once(&mut herdr, &lock_path).expect("manual lock refresh");
        let persisted = LockStore::load(&lock_path).expect("reload lock store");

        assert_eq!(
            report.tabs[0].action,
            TabTickAction::SkippedManualLockCreated {
                locked_label: "my custom label".to_string()
            }
        );
        assert!(persisted.is_locked("w1:t1"));
        assert_eq!(
            herdr.renames,
            vec![("w1:t1".to_string(), "codex".to_string())]
        );
    }

    #[test]
    fn one_shot_refresh_respects_persisted_manual_locks() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let mut store = LockStore::default();
        store.lock_tab("w1:t1", Some("custom".to_string()));
        store.save(&lock_path).expect("save lock store");
        let mut herdr = focused_codex_and_inactive_nvim_tabs();

        let report = refresh_once(&mut herdr, &lock_path).expect("refresh once");

        assert!(herdr.process_info_calls.is_empty());
        assert!(herdr.renames.is_empty());
        assert_eq!(report.tabs[0].action, TabTickAction::SkippedLocked);
    }

    #[test]
    fn inactive_single_pane_tabs_keep_significant_commands_across_focus_flip() {
        let start = Instant::now();
        let mut herdr = focused_codex_and_inactive_nvim_tabs();
        let mut state = DaemonState::default();

        tick(&mut herdr, &mut state, start).expect("initial codex-focused tick");
        tick(&mut herdr, &mut state, start + Duration::from_millis(500))
            .expect("initial codex-focused stable tick");

        herdr.set_focus("w1:t2", "w1:p2");
        tick(&mut herdr, &mut state, start + Duration::from_millis(1000))
            .expect("nvim-focused tick");
        tick(&mut herdr, &mut state, start + Duration::from_millis(1500))
            .expect("nvim-focused stable tick");

        herdr.set_focus("w1:t1", "w1:p1");
        herdr.process_info_calls.clear();
        tick(&mut herdr, &mut state, start + Duration::from_millis(4000))
            .expect("codex-refocused tick after grace");
        tick(&mut herdr, &mut state, start + Duration::from_millis(4500))
            .expect("codex-refocused stable tick after grace");

        assert_eq!(
            herdr.process_info_calls,
            vec!["w1:p1".to_string(), "w1:p1".to_string()]
        );
        assert_eq!(herdr.tab_label("w1:t1"), Some("codex"));
        assert_eq!(herdr.tab_label("w1:t2"), Some("nvim"));
        assert!(
            !herdr
                .renames
                .iter()
                .any(|(tab_id, label)| tab_id == "w1:t2" && label == "tabby"),
            "inactive nvim tab unexpectedly degraded to cwd: {:?}",
            herdr.renames
        );
    }

    struct FakeHerdr {
        tabs: Vec<TabInfo>,
        panes: Vec<PaneInfo>,
        process_infos: BTreeMap<String, PaneProcessInfo>,
        process_errors: BTreeSet<String>,
        process_info_calls: Vec<String>,
        renames: Vec<(String, String)>,
    }

    impl FakeHerdr {
        fn new(tabs: Vec<TabInfo>, panes: Vec<PaneInfo>) -> Self {
            Self {
                tabs,
                panes,
                process_infos: BTreeMap::new(),
                process_errors: BTreeSet::new(),
                process_info_calls: Vec::new(),
                renames: Vec::new(),
            }
        }

        fn with_process_info(mut self, process_info: PaneProcessInfo) -> Self {
            self.process_infos
                .insert(process_info.pane_id.clone(), process_info);
            self
        }

        fn with_process_error(mut self, pane_id: &str) -> Self {
            self.process_errors.insert(pane_id.to_string());
            self
        }

        fn set_tab_label(&mut self, tab_id: &str, label: &str) {
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.tab_id == tab_id) {
                tab.label = label.to_string();
            }
        }

        fn set_focus(&mut self, tab_id: &str, pane_id: &str) {
            for tab in &mut self.tabs {
                tab.focused = tab.tab_id == tab_id;
            }

            for pane in &mut self.panes {
                pane.focused = pane.pane_id == pane_id;
            }
        }

        fn tab_label(&self, tab_id: &str) -> Option<&str> {
            self.tabs
                .iter()
                .find(|tab| tab.tab_id == tab_id)
                .map(|tab| tab.label.as_str())
        }
    }

    impl HerdrApi for FakeHerdr {
        fn list_tabs(&mut self) -> Result<Vec<TabInfo>, HerdrError> {
            Ok(self.tabs.clone())
        }

        fn list_panes(&mut self) -> Result<Vec<PaneInfo>, HerdrError> {
            Ok(self.panes.clone())
        }

        fn pane_process_info(&mut self, pane_id: &str) -> Result<PaneProcessInfo, HerdrError> {
            self.process_info_calls.push(pane_id.to_string());
            if self.process_errors.contains(pane_id) {
                return Err(HerdrError::Protocol("process info unavailable".to_string()));
            }

            self.process_infos.get(pane_id).cloned().ok_or_else(|| {
                HerdrError::Protocol(format!("missing fake process info for {pane_id}"))
            })
        }

        fn rename_tab(&mut self, tab_id: &str, label: &str) -> Result<RenameTabResult, HerdrError> {
            self.renames.push((tab_id.to_string(), label.to_string()));
            self.set_tab_label(tab_id, label);
            Ok(RenameTabResult::Ok)
        }
    }

    fn tab(tab_id: &str, label: &str, focused: bool) -> TabInfo {
        TabInfo {
            tab_id: tab_id.to_string(),
            workspace_id: "w1".to_string(),
            number: None,
            label: label.to_string(),
            focused,
            pane_count: None,
            agent_status: None,
        }
    }

    fn pane(pane_id: &str, tab_id: &str, focused: bool, cwd_basename: &str) -> PaneInfo {
        PaneInfo {
            pane_id: pane_id.to_string(),
            terminal_id: Some("terminal".to_string()),
            workspace_id: "w1".to_string(),
            tab_id: tab_id.to_string(),
            focused,
            label: None,
            title: None,
            cwd: Some(format!("/Users/me/dev/{cwd_basename}")),
            foreground_cwd: None,
            agent: None,
            display_agent: None,
            custom_status: None,
            agent_status: None,
            revision: None,
        }
    }

    fn process(pane_id: &str, name: &str, argv: &[&str]) -> PaneProcessInfo {
        PaneProcessInfo {
            pane_id: pane_id.to_string(),
            shell_pid: Some(100),
            foreground_process_group_id: Some(200),
            foreground_processes: vec![PaneProcess {
                pid: 201,
                name: name.to_string(),
                argv: Some(argv.iter().map(|arg| (*arg).to_string()).collect()),
                argv0: argv.first().map(|arg| (*arg).to_string()),
                cmdline: Some(argv.join(" ")),
                cwd: Some("/Users/me/dev/tabby".to_string()),
            }],
            tty: Some("/dev/ttys001".to_string()),
        }
    }

    fn focused_codex_and_inactive_nvim_tabs() -> FakeHerdr {
        FakeHerdr::new(
            vec![tab("w1:t1", "old", true), tab("w1:t2", "old", false)],
            vec![
                pane("w1:p1", "w1:t1", true, "tabby"),
                pane("w1:p2", "w1:t2", false, "tabby"),
            ],
        )
        .with_process_info(process("w1:p1", "codex", &["codex"]))
        .with_process_info(process("w1:p2", "nvim", &["nvim", "."]))
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
                "tabby-daemon-test-{}-{unique}-{id}",
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
