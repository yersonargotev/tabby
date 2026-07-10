//! Herdr tab auto-renaming orchestration.
//!
//! The normal runtime path is the Hybrid Session Refresher: one long-running
//! process per Herdr Session that observes focus/create events, waits through a
//! Focus Quiet Window, and then inspects only the focused tab. `tabby refresh`
//! remains a bounded one-shot compatibility path. Runtime state is intentionally
//! injectable. The persisted lock store remains plugin-owned state keyed by
//! Herdr `tab_id`; whether those IDs survive Herdr restarts is still an open
//! design decision documented in `docs/design/open-decisions.md`.

use crate::herdr_client::{
    HYBRID_REFRESHER_SUBSCRIPTIONS, HerdrApi, HerdrClient, HerdrError, HerdrEventStream, PaneInfo,
    TabInfo, UnixSocketTransport,
};
use crate::labeler::{LabelCandidate, LabelPolicy};
use crate::locks::{
    LockStore, LockStoreError, ManualLockDecision, detect_manual_lock, is_default_tab_label,
    unlock_all_at_path, unlock_focused_tab_at_path,
};
use crate::paths::{StatePathError, lock_store_path_from_runtime};
use crate::stability::{StabilityDecision, StabilityPolicy, StabilityState};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_REFRESH_STABILIZATION_DELAY: Duration = Duration::from_millis(400);
pub const DEFAULT_FOCUS_QUIET_WINDOW: Duration = Duration::from_millis(1000);
pub const DEFAULT_HYBRID_IDLE_POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct DaemonState {
    tabs: BTreeMap<String, TabRuntimeState>,
    locks: LockStore,
    label_policy: LabelPolicy,
    stability_policy: StabilityPolicy,
    default_labeled_tabs: BTreeSet<String>,
    locks_dirty: bool,
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
            default_labeled_tabs: BTreeSet::new(),
            locks_dirty: false,
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

    pub fn poll_interval(&self) -> Duration {
        self.stability_policy.poll_interval()
    }

    fn mark_locks_dirty(&mut self) {
        self.locks_dirty = true;
    }

    fn take_locks_dirty(&mut self) -> bool {
        let dirty = self.locks_dirty;
        self.locks_dirty = false;
        dirty
    }

    fn replace_persisted_locks(&mut self, locks: LockStore) {
        for (tab_id, runtime) in &mut self.tabs {
            runtime.last_plugin_label = locks.last_plugin_label(tab_id).map(str::to_string);
        }
        self.locks = locks;
    }

    fn note_effective_automatic_label(
        &mut self,
        tab_id: &str,
        tab_number: Option<u64>,
        action: &TabTickAction,
    ) {
        if action
            .effective_automatic_label()
            .is_some_and(|label| !is_default_tab_label(label, tab_number))
        {
            self.default_labeled_tabs.remove(tab_id);
        }
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
struct PendingRename {
    tab_id: String,
    label: String,
    focus_generation: u64,
}

#[derive(Debug)]
pub struct HybridRefresherState {
    runtime: DaemonState,
    quiet_until: Option<Instant>,
    focus_generation: u64,
    pending_rename: Option<PendingRename>,
    last_observed_lock_store: Option<LockStore>,
}

impl HybridRefresherState {
    pub fn new(runtime: DaemonState) -> Self {
        Self {
            runtime,
            quiet_until: None,
            focus_generation: 0,
            pending_rename: None,
            last_observed_lock_store: None,
        }
    }

    pub fn load(lock_store_path: impl AsRef<Path>) -> Result<Self, DaemonError> {
        let locks = LockStore::load(lock_store_path)?;
        let mut state = Self::new(DaemonState::new(locks.clone()));
        state.last_observed_lock_store = Some(locks);
        Ok(state)
    }

    pub fn note_focus_or_create_event(&mut self, observed_at: Instant) {
        self.focus_generation = self.focus_generation.saturating_add(1);
        self.quiet_until = Some(observed_at + DEFAULT_FOCUS_QUIET_WINDOW);
        self.pending_rename = None;
    }

    fn is_quiet(&self, observed_at: Instant) -> bool {
        self.quiet_until
            .is_some_and(|quiet_until| observed_at < quiet_until)
    }

    pub fn poll_interval(&self) -> Duration {
        DEFAULT_HYBRID_IDLE_POLL_INTERVAL
    }

    fn next_tick_after_quiet(&self, observed_at: Instant) -> Instant {
        self.quiet_until
            .filter(|quiet_until| observed_at < *quiet_until)
            .unwrap_or(observed_at)
    }

    fn sync_external_lock_store(&mut self, path: &Path) -> Result<(), LockStoreError> {
        let persisted = LockStore::load(path)?;
        if self
            .last_observed_lock_store
            .as_ref()
            .is_some_and(|last_observed| last_observed != &persisted)
        {
            self.runtime.replace_persisted_locks(persisted.clone());
        }
        self.last_observed_lock_store = Some(persisted);
        Ok(())
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
    SkippedFocusQuiet,
    SkippedPendingCancelled,
    SkippedNoPane,
    SkippedNoCandidate,
    DeferredUnstable { candidate_label: String },
    SkippedManualLockCreated { locked_label: String },
    SkippedAlreadyCurrent { label: String },
    Renamed { from: String, to: String },
}

impl TabTickAction {
    fn effective_automatic_label(&self) -> Option<&str> {
        match self {
            Self::Renamed { to, .. } | Self::SkippedAlreadyCurrent { label: to } => Some(to),
            _ => None,
        }
    }
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
    retain_runtime_state_for_present_tabs(state, &tabs);
    let panes = herdr.list_panes()?;
    let mut reports = Vec::with_capacity(tabs.len());

    for tab in tabs {
        reconcile_reused_tab_id(state, &tab);

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
        let tab_number = tab.number;
        let current_label = tab.label;
        let persisted_plugin_label = state.locks.last_plugin_label(&tab_id).map(str::to_string);
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
            runtime
                .last_plugin_label
                .as_deref()
                .or(persisted_plugin_label.as_deref()),
            stable_candidate.as_ref(),
        ) {
            state.locks.lock_tab(tab_id.clone(), Some(label.clone()));
            state.mark_locks_dirty();
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
                let record_label = label.clone();
                if label == current_label {
                    runtime.last_plugin_label = Some(label.clone());
                    if state
                        .locks
                        .record_plugin_label(tab_id.clone(), record_label)
                    {
                        state.mark_locks_dirty();
                    }
                    TabTickAction::SkippedAlreadyCurrent { label }
                } else {
                    herdr.rename_tab(&tab_id, &label)?;
                    let from = current_label.clone();
                    runtime.last_plugin_label = Some(label.clone());
                    if state
                        .locks
                        .record_plugin_label(tab_id.clone(), record_label)
                    {
                        state.mark_locks_dirty();
                    }
                    TabTickAction::Renamed { from, to: label }
                }
            }
            StabilityDecision::NoOp { label } => {
                let record_label = label.clone();
                runtime.last_plugin_label = Some(label.clone());
                if state
                    .locks
                    .record_plugin_label(tab_id.clone(), record_label)
                {
                    state.mark_locks_dirty();
                }
                if label == current_label {
                    TabTickAction::SkippedAlreadyCurrent { label }
                } else {
                    herdr.rename_tab(&tab_id, &label)?;
                    let from = current_label.clone();
                    TabTickAction::Renamed { from, to: label }
                }
            }
        };

        state.note_effective_automatic_label(&tab_id, tab_number, &action);

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
    if report.has_new_lock() || state.take_locks_dirty() {
        state.locks.save(lock_store_path)?;
    }
    Ok(report)
}

pub fn hybrid_tick_and_save_locks<C>(
    herdr: &mut C,
    state: &mut HybridRefresherState,
    lock_store_path: impl AsRef<Path>,
    observed_at: Instant,
) -> Result<TickReport, DaemonError>
where
    C: HerdrApi,
{
    let lock_store_path = lock_store_path.as_ref();

    if state.is_quiet(observed_at) {
        return Ok(quiet_window_tick());
    }

    state.sync_external_lock_store(lock_store_path)?;

    let report = if state.pending_rename.is_some()
        && let Some(report) = revalidate_pending_rename(herdr, state, lock_store_path, observed_at)?
    {
        report
    } else {
        tick_and_save_locks(herdr, &mut state.runtime, lock_store_path, observed_at)?
    };
    state.sync_external_lock_store(lock_store_path)?;
    Ok(report)
}

fn quiet_window_tick() -> TickReport {
    TickReport { tabs: Vec::new() }
}

fn revalidate_pending_rename<C>(
    herdr: &mut C,
    state: &mut HybridRefresherState,
    lock_store_path: &Path,
    observed_at: Instant,
) -> Result<Option<TickReport>, DaemonError>
where
    C: HerdrApi,
{
    let Some(pending) = state.pending_rename.clone() else {
        return Ok(None);
    };

    if pending.focus_generation != state.focus_generation {
        state.pending_rename = None;
        return Ok(None);
    }

    let tabs = herdr.list_tabs()?;
    retain_runtime_state_for_present_tabs(&mut state.runtime, &tabs);
    let Some(tab) = tabs.into_iter().find(|tab| tab.focused) else {
        state.pending_rename = None;
        return Ok(Some(TickReport { tabs: Vec::new() }));
    };

    if reconcile_reused_tab_id(&mut state.runtime, &tab) {
        state.pending_rename = None;
        return Ok(None);
    }

    if tab.tab_id != pending.tab_id
        || tab.label == pending.label
        || state.runtime.locks.is_locked(&tab.tab_id)
    {
        state.pending_rename = None;
        return Ok(Some(TickReport {
            tabs: vec![skipped_tab_report(
                tab,
                TabTickAction::SkippedPendingCancelled,
            )],
        }));
    }

    let panes = herdr.list_panes()?;
    let Some(selection) = select_pane_for_tab(&panes, &tab.tab_id) else {
        state.pending_rename = None;
        return Ok(Some(TickReport {
            tabs: vec![skipped_tab_report(tab, TabTickAction::SkippedNoPane)],
        }));
    };
    let pane = selection.pane;
    let process_info = if selection.inspect_process {
        herdr.pane_process_info(&pane.pane_id).ok()
    } else {
        None
    };
    let Some(candidate) = state
        .runtime
        .label_policy
        .candidate_for_pane(pane, process_info.as_ref())
    else {
        state.pending_rename = None;
        return Ok(Some(TickReport {
            tabs: vec![skipped_tab_report(
                tab,
                TabTickAction::SkippedPendingCancelled,
            )],
        }));
    };
    if candidate.label() != pending.label {
        state.pending_rename = None;
        return Ok(Some(TickReport {
            tabs: vec![skipped_tab_report(
                tab,
                TabTickAction::SkippedPendingCancelled,
            )],
        }));
    }

    state.pending_rename = None;
    let raw_candidate_label = candidate.label().to_string();
    let tab_id = tab.tab_id;
    let tab_number = tab.number;
    let current_label = tab.label;
    let persisted_plugin_label = state
        .runtime
        .locks
        .last_plugin_label(&tab_id)
        .map(str::to_string);
    let runtime = state
        .runtime
        .tabs
        .entry(tab_id.clone())
        .or_insert_with(|| TabRuntimeState::new(state.runtime.stability_policy));
    let stability_decision = runtime.stability.observe(candidate, observed_at);
    let stable_label = stable_label_from_decision(&stability_decision).map(str::to_string);
    if stable_label.as_deref() != Some(pending.label.as_str()) {
        return Ok(Some(TickReport {
            tabs: vec![TabTickReport {
                tab_id,
                current_label,
                selected_pane_id: Some(pane.pane_id.clone()),
                raw_candidate_label: Some(raw_candidate_label.clone()),
                stable_candidate_label: stable_label,
                process_info_error: None,
                action: TabTickAction::DeferredUnstable {
                    candidate_label: raw_candidate_label,
                },
            }],
        }));
    }

    let stable_candidate = LabelCandidate::working_directory_basename(pending.label.clone());
    let mut store_changed = false;
    let action = if let ManualLockDecision::Lock { label } = detect_manual_lock(
        &current_label,
        runtime
            .last_plugin_label
            .as_deref()
            .or(persisted_plugin_label.as_deref()),
        Some(&stable_candidate),
    ) {
        state
            .runtime
            .locks
            .lock_tab(tab_id.clone(), Some(label.clone()));
        store_changed = true;
        TabTickAction::SkippedManualLockCreated {
            locked_label: label,
        }
    } else if pending.label == current_label {
        runtime.last_plugin_label = Some(pending.label.clone());
        store_changed |= state
            .runtime
            .locks
            .record_plugin_label(tab_id.clone(), pending.label.clone());
        TabTickAction::SkippedAlreadyCurrent {
            label: pending.label.clone(),
        }
    } else {
        herdr.rename_tab(&tab_id, &pending.label)?;
        runtime.last_plugin_label = Some(pending.label.clone());
        store_changed |= state
            .runtime
            .locks
            .record_plugin_label(tab_id.clone(), pending.label.clone());
        TabTickAction::Renamed {
            from: current_label.clone(),
            to: pending.label.clone(),
        }
    };

    state
        .runtime
        .note_effective_automatic_label(&tab_id, tab_number, &action);

    if store_changed {
        state.runtime.locks.save(lock_store_path)?;
    }

    Ok(Some(TickReport {
        tabs: vec![TabTickReport {
            tab_id,
            current_label,
            selected_pane_id: Some(pane.pane_id.clone()),
            raw_candidate_label: Some(raw_candidate_label),
            stable_candidate_label: stable_label,
            process_info_error: None,
            action,
        }],
    }))
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

    let mut store_changed =
        locks.discard_tab_state_for_default_label(&tab.tab_id, &tab.label, tab.number);

    if locks.is_locked(&tab.tab_id) {
        return Ok((
            TickReport {
                tabs: vec![skipped_tab_report(tab, TabTickAction::SkippedLocked)],
            },
            store_changed,
        ));
    }

    let panes = herdr.list_panes()?;
    let Some(selection) = select_pane_for_tab(&panes, &tab.tab_id) else {
        return Ok((
            TickReport {
                tabs: vec![skipped_tab_report(tab, TabTickAction::SkippedNoPane)],
            },
            store_changed,
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
            store_changed,
        ));
    };

    let candidate_label = candidate.label().to_string();
    let tab_id = tab.tab_id;
    let current_label = tab.label;
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

fn reconcile_reused_tab_id(state: &mut DaemonState, tab: &TabInfo) -> bool {
    if !is_default_tab_label(&tab.label, tab.number) {
        state.default_labeled_tabs.remove(&tab.tab_id);
        return false;
    }

    if !state.default_labeled_tabs.insert(tab.tab_id.clone()) {
        return false;
    }

    let discarded_persisted_state = state.locks.discard_tab_state(&tab.tab_id);
    state.tabs.remove(&tab.tab_id);
    if discarded_persisted_state {
        state.mark_locks_dirty();
    }
    true
}

fn retain_runtime_state_for_present_tabs(state: &mut DaemonState, tabs: &[TabInfo]) {
    let present_tab_ids = tabs
        .iter()
        .map(|tab| tab.tab_id.as_str())
        .collect::<BTreeSet<_>>();
    state
        .tabs
        .retain(|tab_id, _| present_tab_ids.contains(tab_id.as_str()));
    state
        .default_labeled_tabs
        .retain(|tab_id| present_tab_ids.contains(tab_id.as_str()));
}

pub fn run_one_shot_refresh_from_env() -> Result<String, RuntimeError> {
    thread::sleep(DEFAULT_REFRESH_STABILIZATION_DELAY);
    let lock_store_path = lock_store_path_from_runtime()?;
    let transport = UnixSocketTransport::from_env()?;
    let mut client = HerdrClient::new(transport);
    let report = refresh_once(&mut client, lock_store_path)?;
    Ok(format!("tabby refresh: {report:?}"))
}

pub fn run_hybrid_refresher_from_env() -> Result<(), RuntimeError> {
    let lock_store_path = lock_store_path_from_runtime()?;
    let transport = UnixSocketTransport::from_env()?;
    let socket_path = transport.socket_path().to_path_buf();
    let mut client = HerdrClient::new(transport);
    let mut events = HerdrEventStream::subscribe(&socket_path, HYBRID_REFRESHER_SUBSCRIPTIONS)?;
    run_hybrid_refresher_loop(&mut client, &mut events, lock_store_path)?;
    Ok(())
}

pub trait RefresherEvents {
    fn next_event_timeout(&mut self, timeout: Duration) -> Result<Option<String>, DaemonError>;
}

impl RefresherEvents for HerdrEventStream {
    fn next_event_timeout(&mut self, timeout: Duration) -> Result<Option<String>, DaemonError> {
        HerdrEventStream::next_event_timeout(self, timeout)
            .map(|event| event.map(|event| event.event))
            .map_err(DaemonError::from)
    }
}

pub fn run_hybrid_refresher_loop<C, E>(
    herdr: &mut C,
    events: &mut E,
    lock_store_path: impl AsRef<Path>,
) -> Result<(), DaemonError>
where
    C: HerdrApi,
    E: RefresherEvents,
{
    let lock_store_path = lock_store_path.as_ref();
    let mut state = HybridRefresherState::load(lock_store_path)?;
    let now = Instant::now();
    state.note_focus_or_create_event(now);
    let mut next_tick_at = state.next_tick_after_quiet(now);

    loop {
        let now = Instant::now();
        if now >= next_tick_at {
            let _ = hybrid_tick_and_save_locks(herdr, &mut state, lock_store_path, now)?;
            next_tick_at = now + state.poll_interval();
        }

        let timeout = next_tick_at.saturating_duration_since(Instant::now());
        if let Some(event) = events.next_event_timeout(timeout)?
            && is_refresher_quiet_event(&event)
        {
            let now = Instant::now();
            state.note_focus_or_create_event(now);
            next_tick_at = state.next_tick_after_quiet(now);
        }
    }
}

fn is_refresher_quiet_event(event: &str) -> bool {
    matches!(
        event,
        "tab_focused"
            | "workspace_focused"
            | "tab_created"
            | "workspace_created"
            | "pane_focused"
            | "tab.focused"
            | "workspace.focused"
            | "tab.created"
            | "workspace.created"
            | "pane.focused"
    )
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
            Self::Herdr(error) => write!(formatter, "refresher Herdr operation failed: {error}"),
            Self::LockStore(error) => {
                write!(formatter, "refresher lock store operation failed: {error}")
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
            Self::Daemon(error) => write!(formatter, "refresher failed: {error}"),
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
    fn hybrid_quiet_window_does_not_call_any_herdr_api() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let start = Instant::now();
        let mut herdr = focused_codex_and_inactive_nvim_tabs();
        let mut state = HybridRefresherState::new(DaemonState::default());
        state.note_focus_or_create_event(start);

        let report = hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(500),
        )
        .expect("quiet tick");

        assert!(report.tabs.is_empty());
        assert!(herdr.list_tab_calls.is_empty());
        assert!(herdr.list_pane_calls.is_empty());
        assert!(herdr.process_info_calls.is_empty());
        assert!(herdr.renames.is_empty());
    }

    #[test]
    fn hybrid_focus_event_resets_quiet_window_and_cancels_pending_rename() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "old", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = HybridRefresherState::new(DaemonState::default());

        hybrid_tick_and_save_locks(&mut herdr, &mut state, &lock_path, start)
            .expect("first observation");
        hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(500),
        )
        .expect("stable observation");
        herdr.renames.clear();
        herdr.process_info_calls.clear();
        herdr.set_tab_label("w1:t1", "old");

        state.note_focus_or_create_event(start + Duration::from_millis(600));
        hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(700),
        )
        .expect("quiet tick skips all Herdr API calls");
        assert!(state.pending_rename.is_none());

        herdr.set_process_info(process("w1:p1", "codex", &["codex"]));
        state.note_focus_or_create_event(start + Duration::from_millis(800));
        hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(1801),
        )
        .expect("post quiet tick");

        assert!(
            !herdr.renames.iter().any(|(_, label)| label == "nvim"),
            "stale pending rename should be cancelled after focus reset: {:?}",
            herdr.renames
        );
    }

    #[test]
    fn hybrid_reobserves_after_quiet_before_renaming() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "nvim", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = HybridRefresherState::new(DaemonState::default());

        hybrid_tick_and_save_locks(&mut herdr, &mut state, &lock_path, start)
            .expect("first observation");
        hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(500),
        )
        .expect("stable observation");
        state.runtime.locks = LockStore::default();
        state
            .runtime
            .tabs
            .get_mut("w1:t1")
            .expect("runtime tab")
            .last_plugin_label = None;
        herdr.set_tab_label("w1:t1", "old");
        herdr.renames.clear();

        state.note_focus_or_create_event(start + Duration::from_millis(600));
        hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(700),
        )
        .expect("quiet tick skips all Herdr API calls");
        assert!(state.pending_rename.is_none());

        let first_after_quiet = hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(1601),
        )
        .expect("first post-quiet tick");

        assert_eq!(
            first_after_quiet.tabs[0].action,
            TabTickAction::Renamed {
                from: "old".to_string(),
                to: "nvim".to_string()
            }
        );
        assert_eq!(
            herdr.renames,
            vec![("w1:t1".to_string(), "nvim".to_string())]
        );
    }

    #[test]
    fn hybrid_persisted_plugin_baseline_detects_manual_lock_after_restart() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "old", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut first_state = DaemonState::load(&lock_path).expect("load first state");

        tick_and_save_locks(&mut herdr, &mut first_state, &lock_path, start)
            .expect("first observation");
        tick_and_save_locks(
            &mut herdr,
            &mut first_state,
            &lock_path,
            start + Duration::from_millis(500),
        )
        .expect("rename and persist baseline");
        herdr.set_tab_label("w1:t1", "custom");

        let mut restarted_state = DaemonState::load(&lock_path).expect("load restarted state");
        let report = tick_and_save_locks(
            &mut herdr,
            &mut restarted_state,
            &lock_path,
            start + Duration::from_millis(1000),
        )
        .expect("restarted first observation");
        let persisted = LockStore::load(&lock_path).expect("reload lock store");

        assert_eq!(
            report.tabs[0].action,
            TabTickAction::SkippedManualLockCreated {
                locked_label: "custom".to_string()
            }
        );
        assert!(persisted.is_locked("w1:t1"));
        assert_eq!(herdr.tab_label("w1:t1"), Some("custom"));
    }

    #[test]
    fn hybrid_reused_tab_id_with_default_label_discards_stale_lock_state() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let start = Instant::now();
        let mut store = LockStore::default();
        store.record_plugin_label("w1:t1", "nvim");
        store.lock_tab("w1:t1", Some("custom".to_string()));
        store.save(&lock_path).expect("save stale lock state");
        let mut reused_tab = tab("w1:t1", "1", true);
        reused_tab.number = Some(1);
        let mut herdr = FakeHerdr::new(
            vec![reused_tab],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "codex", &["codex"]));
        let mut state = HybridRefresherState::load(&lock_path).expect("load stale lock state");

        let first = hybrid_tick_and_save_locks(&mut herdr, &mut state, &lock_path, start)
            .expect("first observation of reused tab id");
        let second = hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(500),
        )
        .expect("stable observation of reused tab id");
        let persisted = LockStore::load(&lock_path).expect("reload reconciled lock store");

        assert_eq!(
            first.tabs[0].action,
            TabTickAction::DeferredUnstable {
                candidate_label: "codex".to_string()
            }
        );
        assert_eq!(
            second.tabs[0].action,
            TabTickAction::Renamed {
                from: "1".to_string(),
                to: "codex".to_string()
            }
        );
        assert!(!persisted.is_locked("w1:t1"));
        assert_eq!(persisted.last_plugin_label("w1:t1"), Some("codex"));
    }

    #[test]
    fn hybrid_reused_tab_id_discards_in_memory_plugin_baseline() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "old", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = HybridRefresherState::new(DaemonState::default());

        hybrid_tick_and_save_locks(&mut herdr, &mut state, &lock_path, start)
            .expect("first observation of original tab");
        hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(500),
        )
        .expect("rename original tab");

        herdr.set_tab_label("w1:t1", "1");
        herdr.tabs[0].number = Some(1);
        herdr.set_process_info(process("w1:p1", "codex", &["codex"]));
        herdr.renames.clear();

        let first_reused = hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(1000),
        )
        .expect("first observation of reused tab id");
        let second_reused = hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(1500),
        )
        .expect("stable observation of reused tab id");

        assert_eq!(
            first_reused.tabs[0].action,
            TabTickAction::DeferredUnstable {
                candidate_label: "codex".to_string()
            }
        );
        assert_eq!(
            second_reused.tabs[0].action,
            TabTickAction::Renamed {
                from: "1".to_string(),
                to: "codex".to_string()
            }
        );
        assert!(!state.runtime.locks().is_locked("w1:t1"));
    }

    #[test]
    fn hybrid_reused_tab_id_resets_pending_stability_observation() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "old", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "codex", &["codex"]));
        let mut state = HybridRefresherState::new(DaemonState::default());

        hybrid_tick_and_save_locks(&mut herdr, &mut state, &lock_path, start)
            .expect("pending observation for original tab");
        herdr.set_tab_label("w1:t1", "1");
        herdr.tabs[0].number = Some(1);

        let first_reused = hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(500),
        )
        .expect("first observation of reused tab id");
        let second_reused = hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(1000),
        )
        .expect("stable observation of reused tab id");

        assert_eq!(
            first_reused.tabs[0].action,
            TabTickAction::DeferredUnstable {
                candidate_label: "codex".to_string()
            }
        );
        assert_eq!(
            second_reused.tabs[0].action,
            TabTickAction::Renamed {
                from: "1".to_string(),
                to: "codex".to_string()
            }
        );
    }

    #[test]
    fn hybrid_prunes_lifecycle_marker_before_default_labeled_id_is_reused() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let start = Instant::now();
        let mut original_tab = tab("w1:t1", "1", true);
        original_tab.number = Some(1);
        let mut herdr = FakeHerdr::new(
            vec![original_tab],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "codex", &["codex"]));
        let mut state = HybridRefresherState::new(DaemonState::default());

        hybrid_tick_and_save_locks(&mut herdr, &mut state, &lock_path, start)
            .expect("pending observation for original default-labeled tab");
        herdr.tabs.clear();
        herdr.panes.clear();
        hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(500),
        )
        .expect("observe original tab disappearance");

        let mut reused_tab = tab("w1:t1", "1", true);
        reused_tab.number = Some(1);
        herdr.tabs.push(reused_tab);
        herdr.panes.push(pane("w1:p1", "w1:t1", true, "tabby"));

        let first_reused = hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(1000),
        )
        .expect("first observation of reused tab id");
        let second_reused = hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(1500),
        )
        .expect("stable observation of reused tab id");

        assert_eq!(
            first_reused.tabs[0].action,
            TabTickAction::DeferredUnstable {
                candidate_label: "codex".to_string()
            }
        );
        assert_eq!(
            second_reused.tabs[0].action,
            TabTickAction::Renamed {
                from: "1".to_string(),
                to: "codex".to_string()
            }
        );
    }

    #[test]
    fn hybrid_inactive_tabs_are_not_inspected_or_renamed() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let start = Instant::now();
        let mut herdr = focused_codex_and_inactive_nvim_tabs();
        let mut state = HybridRefresherState::new(DaemonState::default());

        hybrid_tick_and_save_locks(&mut herdr, &mut state, &lock_path, start)
            .expect("first observation");
        hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(500),
        )
        .expect("stable observation");

        assert_eq!(
            herdr.process_info_calls,
            vec!["w1:p1".to_string(), "w1:p1".to_string()]
        );
        assert_eq!(
            herdr.renames,
            vec![("w1:t1".to_string(), "codex".to_string())]
        );
        assert_eq!(herdr.tab_label("w1:t2"), Some("old"));
    }

    #[test]
    fn hybrid_stability_requires_two_consecutive_observations() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "old", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = HybridRefresherState::new(DaemonState::default());

        hybrid_tick_and_save_locks(&mut herdr, &mut state, &lock_path, start)
            .expect("first observation");
        assert!(herdr.renames.is_empty());

        hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(500),
        )
        .expect("second observation");

        assert_eq!(
            herdr.renames,
            vec![("w1:t1".to_string(), "nvim".to_string())]
        );
    }

    #[test]
    fn hybrid_manual_locks_are_respected_without_inspection_or_rename() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "custom", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = HybridRefresherState::new(DaemonState::default());
        state
            .runtime
            .locks_mut()
            .lock_tab("w1:t1", Some("custom".to_string()));

        let report = hybrid_tick_and_save_locks(&mut herdr, &mut state, &lock_path, start)
            .expect("hybrid tick");

        assert_eq!(report.tabs[0].action, TabTickAction::SkippedLocked);
        assert!(herdr.process_info_calls.is_empty());
        assert!(herdr.renames.is_empty());
    }

    #[test]
    fn hybrid_refresher_observes_unlock_all_from_another_process() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let start = Instant::now();
        let mut store = LockStore::default();
        store.record_plugin_label("w1:t1", "codex");
        store.lock_tab("w1:t1", Some("my custom label".to_string()));
        store.save(&lock_path).expect("save manually locked tab");
        let mut state = HybridRefresherState::load(&lock_path).expect("load refresher state");
        unlock_all_at_path(&lock_path).expect("unlock all from action process");
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "my custom label", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "codex", &["codex"]));

        let first = hybrid_tick_and_save_locks(&mut herdr, &mut state, &lock_path, start)
            .expect("first refresh after unlock all");
        let second = hybrid_tick_and_save_locks(
            &mut herdr,
            &mut state,
            &lock_path,
            start + Duration::from_millis(500),
        )
        .expect("stable refresh after unlock all");

        assert_eq!(
            first.tabs[0].action,
            TabTickAction::DeferredUnstable {
                candidate_label: "codex".to_string()
            }
        );
        assert_eq!(
            second.tabs[0].action,
            TabTickAction::Renamed {
                from: "my custom label".to_string(),
                to: "codex".to_string()
            }
        );
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
        let mut state = DaemonState::load(&lock_path).expect("load refresher state");

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
    fn persisted_locks_are_respected_when_refresher_state_is_recreated() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let mut store = LockStore::default();
        store.lock_tab("w1:t1", Some("custom".to_string()));
        store.save(&lock_path).expect("save lock store");
        let mut state = DaemonState::load(&lock_path).expect("load refresher state");
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
    fn one_shot_refresh_resumes_automatic_naming_after_unlock_all() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let mut store = LockStore::default();
        store.record_plugin_label("w1:t1", "codex");
        store.lock_tab("w1:t1", Some("my custom label".to_string()));
        store.save(&lock_path).expect("save manually locked tab");
        unlock_all_at_path(&lock_path).expect("unlock all tabs");
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "my custom label", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "codex", &["codex"]));

        let report = refresh_once(&mut herdr, &lock_path).expect("refresh after unlock all");
        let persisted = LockStore::load(&lock_path).expect("reload lock store");

        assert_eq!(
            report.tabs[0].action,
            TabTickAction::Renamed {
                from: "my custom label".to_string(),
                to: "codex".to_string()
            }
        );
        assert!(!persisted.is_locked("w1:t1"));
        assert_eq!(persisted.last_plugin_label("w1:t1"), Some("codex"));
    }

    #[test]
    fn one_shot_default_label_does_not_relock_after_unlock_all() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let mut store = LockStore::default();
        store.record_plugin_label("w1:t2", "nvim");
        store.lock_tab("w1:t2", Some("2".to_string()));
        store.save(&lock_path).expect("save manually locked tab");
        unlock_all_at_path(&lock_path).expect("unlock all tabs");
        let mut focused_tab = tab("w1:t2", "2", true);
        focused_tab.number = Some(2);
        let mut herdr = FakeHerdr::new(
            vec![focused_tab],
            vec![pane("w1:p2", "w1:t2", true, "tabby")],
        )
        .with_process_info(process("w1:p2", "codex", &["codex"]));

        let report = refresh_once(&mut herdr, &lock_path).expect("refresh after unlock all");
        let persisted = LockStore::load(&lock_path).expect("reload lock store");

        assert_eq!(
            report.tabs[0].action,
            TabTickAction::Renamed {
                from: "2".to_string(),
                to: "codex".to_string()
            }
        );
        assert!(!persisted.is_locked("w1:t2"));
        assert_eq!(persisted.last_plugin_label("w1:t2"), Some("codex"));
    }

    #[test]
    fn one_shot_refresh_resumes_automatic_naming_after_unlock_focused() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let mut store = LockStore::default();
        store.record_plugin_label("w1:t1", "codex");
        store.lock_tab("w1:t1", Some("my custom label".to_string()));
        store.save(&lock_path).expect("save manually locked tab");
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "my custom label", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "codex", &["codex"]));
        unlock_focused_tab_at_path(&lock_path, &mut herdr).expect("unlock focused tab");

        let report = refresh_once(&mut herdr, &lock_path).expect("refresh after focused unlock");
        let persisted = LockStore::load(&lock_path).expect("reload lock store");

        assert_eq!(
            report.tabs[0].action,
            TabTickAction::Renamed {
                from: "my custom label".to_string(),
                to: "codex".to_string()
            }
        );
        assert!(!persisted.is_locked("w1:t1"));
        assert_eq!(persisted.last_plugin_label("w1:t1"), Some("codex"));
    }

    #[test]
    fn one_shot_reused_tab_id_with_default_label_discards_stale_baseline() {
        let temp_dir = TestTempDir::new();
        let lock_path = temp_dir.path().join("locks.json");
        let mut store = LockStore::default();
        store.record_plugin_label("w1:t1", "nvim");
        store.save(&lock_path).expect("save stale plugin baseline");
        let mut reused_tab = tab("w1:t1", "1", true);
        reused_tab.number = Some(1);
        let mut herdr = FakeHerdr::new(
            vec![reused_tab],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "codex", &["codex"]));

        let report = refresh_once(&mut herdr, &lock_path).expect("refresh reused tab id");
        let persisted = LockStore::load(&lock_path).expect("reload reconciled lock store");

        assert_eq!(
            report.tabs[0].action,
            TabTickAction::Renamed {
                from: "1".to_string(),
                to: "codex".to_string()
            }
        );
        assert!(!persisted.is_locked("w1:t1"));
        assert_eq!(persisted.last_plugin_label("w1:t1"), Some("codex"));
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
        list_tab_calls: Vec<()>,
        list_pane_calls: Vec<()>,
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
                list_tab_calls: Vec::new(),
                list_pane_calls: Vec::new(),
                process_info_calls: Vec::new(),
                renames: Vec::new(),
            }
        }

        fn with_process_info(mut self, process_info: PaneProcessInfo) -> Self {
            self.process_infos
                .insert(process_info.pane_id.clone(), process_info);
            self
        }

        fn set_process_info(&mut self, process_info: PaneProcessInfo) {
            self.process_infos
                .insert(process_info.pane_id.clone(), process_info);
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
            self.list_tab_calls.push(());
            Ok(self.tabs.clone())
        }

        fn list_panes(&mut self) -> Result<Vec<PaneInfo>, HerdrError> {
            self.list_pane_calls.push(());
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
                "tabby-refresher-test-{}-{unique}-{id}",
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
