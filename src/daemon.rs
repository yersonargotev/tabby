//! Herdr tab auto-renaming orchestration.
//!
//! The normal runtime path is the Session Runtime: one long-running
//! process per Herdr Session that receives Refresh Triggers, observes only the
//! focused tab, and drives bounded One-Shot Refresh decisions. The Session
//! Runtime injects the Herdr adapter and Session-Scoped Tab State; this module
//! neither resolves global state paths nor exposes a direct CLI refresh path.

#[cfg(test)]
use crate::herdr_client::PaneInfo;
use crate::herdr_client::{HerdrApi, HerdrError, TabInfo};
use crate::labeler::{LabelCandidate, LabelPolicy};
use crate::locks::{
    ManualLockDecision, RenameIntentReconciliation, SessionTabStateError, SessionTabStateStore,
    TabLifecycleEvidence, detect_manual_lock,
};
use crate::stability::{DEFAULT_POLL_INTERVAL, StabilityDecision, StabilityPolicy, StabilityState};
use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};

pub const DEFAULT_FOCUS_QUIET_WINDOW: Duration = Duration::from_millis(1000);
pub const DEFAULT_SESSION_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
pub struct DaemonState {
    tabs: BTreeMap<String, TabRuntimeState>,
    label_policy: LabelPolicy,
    stability_policy: StabilityPolicy,
}

/// Executes at most one bounded sample of the focused-tab decision flow.
///
/// Session Runtime owns scheduling: it calls this at the quiet-window end and
/// at `OneShotRefreshState::next_sample_at()` until the evaluation completes.
/// The only mutation performed by this path is preceded by a durable,
/// session-scoped Automatic Rename Intent.
pub fn evaluate_one_shot<C>(
    herdr: &mut C,
    state: &mut OneShotRefreshState,
    tab_state: &SessionTabStateStore,
    observed_at: Instant,
) -> Result<TickReport, DaemonError>
where
    C: HerdrApi,
{
    if state.is_quiet(observed_at) {
        return Ok(quiet_window_tick());
    }
    state.quiet_until = None;
    if state.evaluation.is_none() {
        state.begin_evaluation(observed_at);
    }

    let Some(evaluation) = state.evaluation else {
        return Ok(quiet_window_tick());
    };
    if observed_at >= evaluation.deadline || evaluation.generation != state.trigger_generation {
        state.complete_evaluation();
        return Ok(TickReport { tabs: Vec::new() });
    }

    let report = evaluate_focused_sample(herdr, &mut state.runtime, tab_state, observed_at)?;
    let completed = report.tabs.is_empty()
        || report
            .tabs
            .iter()
            .all(|tab| !matches!(tab.action, TabTickAction::DeferredUnstable { .. }));
    let exhausted = state.evaluation.as_mut().is_some_and(|evaluation| {
        evaluation.samples_taken = evaluation.samples_taken.saturating_add(1);
        evaluation.last_sample_at = Some(observed_at);
        evaluation.samples_taken >= DEFAULT_MAX_EVALUATION_SAMPLES
    });
    if completed || exhausted {
        state.complete_evaluation();
    }
    Ok(report)
}

fn evaluate_focused_sample<C>(
    herdr: &mut C,
    runtime: &mut DaemonState,
    tab_state: &SessionTabStateStore,
    observed_at: Instant,
) -> Result<TickReport, DaemonError>
where
    C: HerdrApi,
{
    let Some(observation) = herdr.observe_focused_tab()? else {
        return Ok(TickReport { tabs: Vec::new() });
    };
    let tab = observation.tab;
    let pane = observation.pane;
    let lifecycle = lifecycle_evidence(&tab, &pane);

    match tab_state.reconcile_automatic_rename_intent(&tab.tab_id, &tab.label, &lifecycle)? {
        RenameIntentReconciliation::ReusedTab => {
            runtime.tabs.remove(&tab.tab_id);
        }
        RenameIntentReconciliation::PreservedManualIntent { label } => {
            return Ok(TickReport {
                tabs: vec![skipped_tab_report(
                    tab,
                    TabTickAction::SkippedManualLockCreated {
                        locked_label: label,
                    },
                )],
            });
        }
        RenameIntentReconciliation::NoIntent
        | RenameIntentReconciliation::Confirmed { .. }
        | RenameIntentReconciliation::SourceUnchanged => {}
    }

    if tab_state.read(|state| state.is_locked(&tab.tab_id))? {
        return Ok(TickReport {
            tabs: vec![skipped_tab_report(tab, TabTickAction::SkippedLocked)],
        });
    }

    let (process_info, process_info_error) = if pane.focused {
        match herdr.pane_process_info(&pane.pane_id) {
            Ok(process_info) => (Some(process_info), None),
            Err(error) => (None, Some(error.to_string())),
        }
    } else {
        (None, None)
    };
    let Some(candidate) = runtime
        .label_policy
        .candidate_for_pane(&pane, process_info.as_ref())
    else {
        return Ok(TickReport {
            tabs: vec![TabTickReport {
                tab_id: tab.tab_id,
                current_label: tab.label,
                selected_pane_id: Some(pane.pane_id),
                raw_candidate_label: None,
                stable_candidate_label: None,
                process_info_error,
                action: TabTickAction::SkippedNoCandidate,
            }],
        });
    };

    let raw_candidate_label = candidate.label().to_string();
    let tab_id = tab.tab_id;
    let current_label = tab.label;
    let persisted_plugin_label =
        tab_state.read(|state| state.last_plugin_label(&tab_id).map(str::to_string))?;
    let tab_runtime = runtime
        .tabs
        .entry(tab_id.clone())
        .or_insert_with(|| TabRuntimeState::new(runtime.stability_policy));
    let stability_decision = tab_runtime.stability.observe(candidate, observed_at);
    let stable_label = stable_label_from_decision(&stability_decision).map(str::to_string);
    let stable_candidate = stable_label
        .as_ref()
        .map(|label| LabelCandidate::working_directory_basename(label.clone()));

    if let ManualLockDecision::Lock { label } = detect_manual_lock(
        &current_label,
        tab_runtime
            .last_plugin_label
            .as_deref()
            .or(persisted_plugin_label.as_deref()),
        stable_candidate.as_ref(),
    ) {
        tab_state.mutate(|state| state.lock_tab(tab_id.clone(), Some(label.clone())))?;
        return Ok(TickReport {
            tabs: vec![TabTickReport {
                tab_id,
                current_label,
                selected_pane_id: Some(pane.pane_id),
                raw_candidate_label: Some(raw_candidate_label),
                stable_candidate_label: stable_label,
                process_info_error,
                action: TabTickAction::SkippedManualLockCreated {
                    locked_label: label,
                },
            }],
        });
    }

    let action = match stability_decision {
        StabilityDecision::Pending => TabTickAction::DeferredUnstable {
            candidate_label: raw_candidate_label.clone(),
        },
        StabilityDecision::Rename { label } | StabilityDecision::NoOp { label } => {
            if label == current_label {
                tab_state
                    .mutate(|state| state.record_plugin_label(tab_id.clone(), label.clone()))?;
                tab_runtime.last_plugin_label = Some(label.clone());
                TabTickAction::SkippedAlreadyCurrent { label }
            } else if revalidate_focused_candidate(
                herdr,
                tab_state,
                &tab_id,
                &current_label,
                &label,
                &lifecycle,
            )? {
                tab_state.record_automatic_rename_intent(
                    tab_id.clone(),
                    current_label.clone(),
                    label.clone(),
                    lifecycle,
                )?;
                herdr.rename_tab(&tab_id, &label)?;
                tab_runtime.last_plugin_label = Some(label.clone());
                TabTickAction::Renamed {
                    from: current_label.clone(),
                    to: label,
                }
            } else {
                TabTickAction::SkippedFocusChanged
            }
        }
    };
    Ok(TickReport {
        tabs: vec![TabTickReport {
            tab_id,
            current_label,
            selected_pane_id: Some(pane.pane_id),
            raw_candidate_label: Some(raw_candidate_label),
            stable_candidate_label: stable_label,
            process_info_error,
            action,
        }],
    })
}

fn revalidate_focused_candidate<C>(
    herdr: &mut C,
    tab_state: &SessionTabStateStore,
    expected_tab_id: &str,
    expected_label: &str,
    expected_candidate: &str,
    expected_lifecycle: &TabLifecycleEvidence,
) -> Result<bool, DaemonError>
where
    C: HerdrApi,
{
    let Some(observation) = herdr.observe_focused_tab()? else {
        return Ok(false);
    };
    if observation.tab.tab_id != expected_tab_id
        || observation.tab.label != expected_label
        || lifecycle_evidence(&observation.tab, &observation.pane) != *expected_lifecycle
        || tab_state.read(|state| state.is_locked(expected_tab_id))?
    {
        return Ok(false);
    }
    let process_info = observation
        .pane
        .focused
        .then(|| herdr.pane_process_info(&observation.pane.pane_id).ok())
        .flatten();
    Ok(LabelPolicy::default()
        .candidate_for_pane(&observation.pane, process_info.as_ref())
        .is_some_and(|candidate| candidate.label() == expected_candidate))
}

fn lifecycle_evidence(tab: &TabInfo, pane: &crate::herdr_client::PaneInfo) -> TabLifecycleEvidence {
    TabLifecycleEvidence::new(
        tab.workspace_id.clone(),
        tab.number,
        pane.pane_id.clone(),
        pane.revision,
    )
}

impl DaemonState {
    fn reset_stability_for_next_evaluation(&mut self) {
        for runtime in self.tabs.values_mut() {
            runtime.stability = StabilityState::new(self.stability_policy);
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

pub const DEFAULT_MAX_EVALUATION_SAMPLES: u8 = 3;
pub const DEFAULT_EVALUATION_DEADLINE: Duration = Duration::from_millis(2500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OneShotEvaluation {
    generation: u64,
    deadline: Instant,
    samples_taken: u8,
    last_sample_at: Option<Instant>,
}

#[derive(Debug)]
pub struct OneShotRefreshState {
    runtime: DaemonState,
    quiet_until: Option<Instant>,
    trigger_deadline: Option<Instant>,
    trigger_generation: u64,
    evaluation: Option<OneShotEvaluation>,
}

impl OneShotRefreshState {
    pub fn new(runtime: DaemonState) -> Self {
        Self {
            runtime,
            quiet_until: None,
            trigger_deadline: None,
            trigger_generation: 0,
            evaluation: None,
        }
    }

    pub fn note_refresh_trigger(&mut self, observed_at: Instant) {
        self.trigger_generation = self.trigger_generation.saturating_add(1);
        self.quiet_until = Some(observed_at + DEFAULT_FOCUS_QUIET_WINDOW);
        self.trigger_deadline = Some(observed_at + DEFAULT_EVALUATION_DEADLINE);
        self.evaluation = None;
        self.runtime.reset_stability_for_next_evaluation();
    }

    fn is_quiet(&self, observed_at: Instant) -> bool {
        self.quiet_until
            .is_some_and(|quiet_until| observed_at < quiet_until)
    }

    pub fn poll_interval(&self) -> Duration {
        DEFAULT_SESSION_REFRESH_INTERVAL
    }

    /// Returns the earliest time when the current bounded evaluation needs
    /// another sample. The Session Runtime owns waiting and trigger delivery.
    pub fn next_sample_at(&self) -> Option<Instant> {
        if let Some(quiet_until) = self.quiet_until {
            return Some(quiet_until);
        }

        self.evaluation.and_then(|evaluation| {
            (evaluation.samples_taken < DEFAULT_MAX_EVALUATION_SAMPLES)
                .then(|| {
                    evaluation
                        .last_sample_at
                        .map(|at| at + DEFAULT_POLL_INTERVAL)
                })
                .flatten()
        })
    }

    fn begin_evaluation(&mut self, observed_at: Instant) {
        self.evaluation = Some(OneShotEvaluation {
            generation: self.trigger_generation,
            deadline: self
                .trigger_deadline
                .take()
                .unwrap_or(observed_at + DEFAULT_EVALUATION_DEADLINE),
            samples_taken: 0,
            last_sample_at: None,
        });
        self.runtime.reset_stability_for_next_evaluation();
    }

    fn complete_evaluation(&mut self) {
        self.evaluation = None;
        self.trigger_deadline = None;
        self.runtime.reset_stability_for_next_evaluation();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub tabs: Vec<TabTickReport>,
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
    SkippedFocusChanged,
    SkippedNoPane,
    SkippedNoCandidate,
    DeferredUnstable { candidate_label: String },
    SkippedManualLockCreated { locked_label: String },
    SkippedAlreadyCurrent { label: String },
    Renamed { from: String, to: String },
}

fn quiet_window_tick() -> TickReport {
    TickReport { tabs: Vec::new() }
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

fn stable_label_from_decision(decision: &StabilityDecision) -> Option<&str> {
    match decision {
        StabilityDecision::Pending => None,
        StabilityDecision::Rename { label } | StabilityDecision::NoOp { label } => Some(label),
    }
}

#[derive(Debug)]
pub enum DaemonError {
    Herdr(HerdrError),
    SessionTabState(SessionTabStateError),
}

impl fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Herdr(error) => write!(formatter, "refresher Herdr operation failed: {error}"),
            Self::SessionTabState(error) => {
                write!(formatter, "session tab state operation failed: {error}")
            }
        }
    }
}

impl DaemonError {
    pub fn proves_session_stop(&self) -> bool {
        matches!(
            self,
            Self::Herdr(HerdrError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                )
        )
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Herdr(error) => Some(error),
            Self::SessionTabState(error) => Some(error),
        }
    }
}

impl From<HerdrError> for DaemonError {
    fn from(error: HerdrError) -> Self {
        Self::Herdr(error)
    }
}

impl From<SessionTabStateError> for DaemonError {
    fn from(error: SessionTabStateError) -> Self {
        Self::SessionTabState(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr_client::{PaneProcess, PaneProcessInfo, RenameTabResult, TabInfo};
    use crate::locks::SessionTabState;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn focused_one_shot_requires_two_samples_then_records_intent_before_rename() {
        let temp_dir = TestTempDir::new();
        let socket = crate::startup::SessionSocket::resolve(temp_dir.path().join("session.sock"))
            .expect("session socket");
        let tab_state = SessionTabStateStore::open(temp_dir.path(), &socket).expect("tab state");
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "old", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = OneShotRefreshState::new(DaemonState::default());

        let first =
            evaluate_one_shot(&mut herdr, &mut state, &tab_state, start).expect("first sample");

        assert_eq!(
            first.tabs[0].action,
            TabTickAction::DeferredUnstable {
                candidate_label: "nvim".to_string()
            }
        );
        assert_eq!(
            state.next_sample_at(),
            Some(start + Duration::from_millis(500))
        );
        assert!(herdr.renames.is_empty());

        let second = evaluate_one_shot(
            &mut herdr,
            &mut state,
            &tab_state,
            start + Duration::from_millis(500),
        )
        .expect("second sample");

        assert_eq!(
            second.tabs[0].action,
            TabTickAction::Renamed {
                from: "old".to_string(),
                to: "nvim".to_string()
            }
        );
        assert_eq!(
            tab_state
                .read(|state| state.unresolved_rename_intent_count())
                .expect("read intent"),
            1,
            "the durable intent remains available for reconciliation after the rename"
        );
    }

    #[test]
    fn focused_one_shot_stops_after_three_unstable_samples() {
        let temp_dir = TestTempDir::new();
        let socket = crate::startup::SessionSocket::resolve(temp_dir.path().join("session.sock"))
            .expect("session socket");
        let tab_state = SessionTabStateStore::open(temp_dir.path(), &socket).expect("tab state");
        let start = Instant::now();
        let mut herdr = FakeHerdr::new(
            vec![tab("w1:t1", "old", true)],
            vec![pane("w1:p1", "w1:t1", true, "tabby")],
        )
        .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = OneShotRefreshState::new(DaemonState::default());

        evaluate_one_shot(&mut herdr, &mut state, &tab_state, start).expect("first sample");
        herdr.set_process_info(process("w1:p1", "codex", &["codex"]));
        evaluate_one_shot(
            &mut herdr,
            &mut state,
            &tab_state,
            start + Duration::from_millis(500),
        )
        .expect("second sample");
        herdr.set_process_info(process("w1:p1", "nvim", &["nvim"]));
        let third = evaluate_one_shot(
            &mut herdr,
            &mut state,
            &tab_state,
            start + Duration::from_millis(1000),
        )
        .expect("third sample");

        assert!(matches!(
            third.tabs[0].action,
            TabTickAction::DeferredUnstable { .. }
        ));
        assert!(state.next_sample_at().is_none());
        assert!(herdr.renames.is_empty());
    }

    #[test]
    fn one_shot_revalidation_rejects_a_reused_lifecycle_before_rename() {
        let temp_dir = TestTempDir::new();
        let socket = crate::startup::SessionSocket::resolve(temp_dir.path().join("session.sock"))
            .expect("session socket");
        let tab_state = SessionTabStateStore::open(temp_dir.path(), &socket).expect("tab state");
        let start = Instant::now();
        let original_tab = tab("w1:t1", "old", true);
        let original_pane = pane("w1:p1", "w1:t1", true, "tabby");
        let mut replacement_pane = original_pane.clone();
        replacement_pane.revision = Some(2);
        let mut herdr = FakeHerdr::new(vec![original_tab.clone()], vec![original_pane.clone()])
            .with_process_info(process("w1:p1", "nvim", &["nvim"]));
        let mut state = OneShotRefreshState::new(DaemonState::default());

        evaluate_one_shot(&mut herdr, &mut state, &tab_state, start).expect("first sample");
        herdr = herdr.with_observation_sequence(vec![
            crate::herdr_client::FocusedTabObservation {
                tab: original_tab.clone(),
                pane: original_pane,
                working_directory: Some("/Users/me/dev/tabby".to_string()),
            },
            crate::herdr_client::FocusedTabObservation {
                tab: original_tab,
                pane: replacement_pane,
                working_directory: Some("/Users/me/dev/tabby".to_string()),
            },
        ]);

        let report = evaluate_one_shot(
            &mut herdr,
            &mut state,
            &tab_state,
            start + Duration::from_millis(500),
        )
        .expect("stable sample");

        assert_eq!(report.tabs[0].action, TabTickAction::SkippedFocusChanged);
        assert!(herdr.renames.is_empty());
        assert_eq!(
            tab_state
                .read(SessionTabState::unresolved_rename_intent_count)
                .expect("read intent count"),
            0,
            "no mutation intent is persisted when lifecycle revalidation fails"
        );
    }

    struct FakeHerdr {
        tabs: Vec<TabInfo>,
        panes: Vec<PaneInfo>,
        process_infos: BTreeMap<String, PaneProcessInfo>,
        observation_sequence: Vec<crate::herdr_client::FocusedTabObservation>,
        process_info_calls: Vec<String>,
        renames: Vec<(String, String)>,
    }

    impl FakeHerdr {
        fn new(tabs: Vec<TabInfo>, panes: Vec<PaneInfo>) -> Self {
            Self {
                tabs,
                panes,
                process_infos: BTreeMap::new(),
                observation_sequence: Vec::new(),
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

        fn with_observation_sequence(
            mut self,
            observations: Vec<crate::herdr_client::FocusedTabObservation>,
        ) -> Self {
            self.observation_sequence = observations;
            self
        }

        fn set_tab_label(&mut self, tab_id: &str, label: &str) {
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.tab_id == tab_id) {
                tab.label = label.to_string();
            }
        }
    }

    impl HerdrApi for FakeHerdr {
        fn observe_focused_tab(
            &mut self,
        ) -> Result<Option<crate::herdr_client::FocusedTabObservation>, HerdrError> {
            if !self.observation_sequence.is_empty() {
                return Ok(Some(self.observation_sequence.remove(0)));
            }
            let Some(tab) = self.tabs.iter().find(|tab| tab.focused).cloned() else {
                return Ok(None);
            };
            let Some(pane) = self
                .panes
                .iter()
                .find(|pane| pane.tab_id == tab.tab_id && pane.focused)
                .or_else(|| self.panes.iter().find(|pane| pane.tab_id == tab.tab_id))
                .cloned()
            else {
                return Ok(None);
            };
            Ok(Some(crate::herdr_client::FocusedTabObservation {
                working_directory: pane.cwd.clone(),
                tab,
                pane,
            }))
        }

        fn pane_process_info(&mut self, pane_id: &str) -> Result<PaneProcessInfo, HerdrError> {
            self.process_info_calls.push(pane_id.to_string());
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
