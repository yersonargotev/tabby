//! Pure, bounded Refresh Decision policy.
//!
//! This module owns no Herdr client, filesystem path, session state store, or
//! rename side effect. The runtime adapter supplies observations and executes
//! the returned decision.

use crate::labeler::LabelCandidate;
use crate::locks::{ManualLockDecision, detect_manual_lock};
use crate::stability::{StabilityDecision, StabilityPolicy, StabilityState};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const FOCUS_QUIET_WINDOW: Duration = Duration::from_millis(1000);
pub const EVALUATION_DEADLINE: Duration = Duration::from_millis(2500);
pub const MAX_EVALUATION_SAMPLES: u8 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshObservation {
    /// Lossless Session Identity encoded by the runtime for comparison and
    /// diagnostics; it is not a tab identifier.
    pub session_identity: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub tab_number: Option<u64>,
    pub pane_id: String,
    pub pane_revision: Option<u64>,
    pub visible_label: String,
    pub working_directory: Option<String>,
    pub significant_command: Option<String>,
    pub candidate: Option<LabelCandidate>,
    pub manually_locked: bool,
    pub automatic_label_baseline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshDecision {
    WaitUntil(Instant),
    Observe,
    Stop,
    Fault { diagnostic: String },
    SkipLocked,
    SkipNoCandidate,
    CreateManualLock { label: String },
    Defer { candidate_label: String },
    RecordBaseline { label: String },
    Rename { label: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveEvaluation {
    generation: u64,
    deadline: Instant,
    samples_taken: u8,
    last_sample_at: Option<Instant>,
}

/// In-memory state for one bounded evaluation. It does not survive a runtime
/// restart and contains no persistence or I/O adapter.
#[derive(Debug)]
pub struct RefreshDecisionState {
    policy: StabilityPolicy,
    stability: BTreeMap<String, StabilityState>,
    quiet_until: Option<Instant>,
    trigger_deadline: Option<Instant>,
    trigger_generation: u64,
    evaluation: Option<ActiveEvaluation>,
}

impl RefreshDecisionState {
    pub fn new(policy: StabilityPolicy) -> Self {
        Self {
            policy,
            stability: BTreeMap::new(),
            quiet_until: None,
            trigger_deadline: None,
            trigger_generation: 0,
            evaluation: None,
        }
    }

    pub fn note_trigger(&mut self, observed_at: Instant) {
        self.trigger_generation = self.trigger_generation.saturating_add(1);
        self.quiet_until = Some(observed_at + FOCUS_QUIET_WINDOW);
        self.trigger_deadline = Some(observed_at + EVALUATION_DEADLINE);
        self.evaluation = None;
        self.stability.clear();
    }

    pub fn next(&mut self, observed_at: Instant) -> RefreshDecision {
        if let Some(quiet_until) = self.quiet_until {
            if observed_at < quiet_until {
                return RefreshDecision::WaitUntil(quiet_until);
            }
            self.quiet_until = None;
        }
        if self.evaluation.is_none() {
            self.evaluation = Some(ActiveEvaluation {
                generation: self.trigger_generation,
                deadline: self
                    .trigger_deadline
                    .take()
                    .unwrap_or(observed_at + EVALUATION_DEADLINE),
                samples_taken: 0,
                last_sample_at: None,
            });
            self.stability.clear();
        }
        let evaluation = self.evaluation.expect("evaluation is initialized above");
        if observed_at >= evaluation.deadline || evaluation.generation != self.trigger_generation {
            self.complete();
            return RefreshDecision::Stop;
        }
        RefreshDecision::Observe
    }

    pub fn decide_observation(
        &mut self,
        observation: RefreshObservation,
        observed_at: Instant,
    ) -> RefreshDecision {
        let decision = if observation.session_identity.is_empty()
            || observation.workspace_id.is_empty()
            || observation.tab_id.is_empty()
            || observation.pane_id.is_empty()
        {
            RefreshDecision::Fault {
                diagnostic:
                    "Refresh Observation is missing Session Identity or focused lifecycle identity"
                        .to_string(),
            }
        } else if observation.manually_locked {
            RefreshDecision::SkipLocked
        } else {
            match observation.candidate {
                None => RefreshDecision::SkipNoCandidate,
                Some(candidate) => {
                    let candidate_label = candidate.label().to_string();
                    let stability = self
                        .stability
                        .entry(observation.tab_id.clone())
                        .or_insert_with(|| StabilityState::new(self.policy));
                    let stability_decision = stability.observe(candidate.clone(), observed_at);
                    let stable_candidate = match &stability_decision {
                        StabilityDecision::Pending => None,
                        StabilityDecision::Rename { label } | StabilityDecision::NoOp { label } => {
                            Some(LabelCandidate::working_directory_basename(label.clone()))
                        }
                    };
                    match detect_manual_lock(
                        &observation.visible_label,
                        observation.automatic_label_baseline.as_deref(),
                        stable_candidate.as_ref(),
                    ) {
                        ManualLockDecision::Lock { label } => {
                            RefreshDecision::CreateManualLock { label }
                        }
                        ManualLockDecision::AutoManaged => match stability_decision {
                            StabilityDecision::Pending => {
                                RefreshDecision::Defer { candidate_label }
                            }
                            StabilityDecision::Rename { label }
                            | StabilityDecision::NoOp { label } => {
                                if label == observation.visible_label {
                                    RefreshDecision::RecordBaseline { label }
                                } else {
                                    RefreshDecision::Rename { label }
                                }
                            }
                        },
                    }
                }
            }
        };
        self.record_sample(
            observed_at,
            !matches!(decision, RefreshDecision::Defer { .. }),
        );
        decision
    }

    pub fn next_sample_at(&self) -> Option<Instant> {
        self.evaluation.and_then(|evaluation| {
            (evaluation.samples_taken < MAX_EVALUATION_SAMPLES)
                .then(|| {
                    evaluation
                        .last_sample_at
                        .map(|at| at + self.policy.poll_interval())
                })
                .flatten()
        })
    }

    /// Ends the current evaluation when the executor cannot produce a usable
    /// focused observation (for example, the focused tab disappeared).
    pub fn stop(&mut self) {
        self.complete();
    }

    /// Drops in-memory candidate history for a tab whose lifecycle was proven
    /// fresh by the persistence adapter.
    pub fn reset_tab(&mut self, tab_id: &str) {
        self.stability.remove(tab_id);
    }

    fn record_sample(&mut self, observed_at: Instant, complete: bool) {
        let exhausted = self.evaluation.as_mut().is_some_and(|evaluation| {
            evaluation.samples_taken = evaluation.samples_taken.saturating_add(1);
            evaluation.last_sample_at = Some(observed_at);
            evaluation.samples_taken >= MAX_EVALUATION_SAMPLES
        });
        if complete || exhausted {
            self.complete();
        }
    }

    fn complete(&mut self) {
        self.evaluation = None;
        self.trigger_deadline = None;
        self.stability.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_two_equal_candidates_before_rename() {
        let start = Instant::now();
        let mut state = RefreshDecisionState::new(StabilityPolicy::default());
        assert_eq!(state.next(start), RefreshDecision::Observe);
        assert_eq!(
            state.decide_observation(observation("old", Some("nvim")), start),
            RefreshDecision::Defer {
                candidate_label: "nvim".to_string()
            }
        );
        assert_eq!(
            state.decide_observation(
                observation("old", Some("nvim")),
                start + Duration::from_millis(500),
            ),
            RefreshDecision::Rename {
                label: "nvim".to_string()
            }
        );
    }

    #[test]
    fn preserves_manual_intent_before_a_rename_decision() {
        let start = Instant::now();
        let mut state = RefreshDecisionState::new(StabilityPolicy::default());
        let mut input = observation("custom", Some("nvim"));
        input.automatic_label_baseline = Some("nvim".to_string());
        assert_eq!(state.next(start), RefreshDecision::Observe);
        assert_eq!(
            state.decide_observation(input, start),
            RefreshDecision::CreateManualLock {
                label: "custom".to_string()
            }
        );
    }

    #[test]
    fn trigger_waits_then_stops_at_its_absolute_deadline() {
        let start = Instant::now();
        let mut state = RefreshDecisionState::new(StabilityPolicy::default());
        state.note_trigger(start);
        assert_eq!(
            state.next(start + Duration::from_millis(999)),
            RefreshDecision::WaitUntil(start + FOCUS_QUIET_WINDOW)
        );
        assert_eq!(
            state.next(start + FOCUS_QUIET_WINDOW),
            RefreshDecision::Observe
        );
        assert_eq!(
            state.next(start + EVALUATION_DEADLINE),
            RefreshDecision::Stop
        );
    }

    #[test]
    fn rejects_an_observation_without_session_or_lifecycle_identity() {
        let start = Instant::now();
        let mut state = RefreshDecisionState::new(StabilityPolicy::default());
        let mut input = observation("old", Some("nvim"));
        input.session_identity.clear();

        assert_eq!(state.next(start), RefreshDecision::Observe);
        assert!(matches!(
            state.decide_observation(input, start),
            RefreshDecision::Fault { .. }
        ));
    }

    fn observation(visible_label: &str, candidate: Option<&str>) -> RefreshObservation {
        RefreshObservation {
            session_identity: "00ff".to_string(),
            workspace_id: "w1".to_string(),
            tab_id: "w1:t1".to_string(),
            tab_number: Some(1),
            pane_id: "w1:p1".to_string(),
            pane_revision: Some(7),
            visible_label: visible_label.to_string(),
            working_directory: Some("/Users/me/dev/tabby".to_string()),
            significant_command: candidate.map(str::to_string),
            candidate: candidate.map(LabelCandidate::significant_command),
            manually_locked: false,
            automatic_label_baseline: None,
        }
    }
}
