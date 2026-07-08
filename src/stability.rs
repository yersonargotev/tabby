use crate::labeler::{LabelCandidate, LabelCandidateSource};
use std::time::{Duration, Instant};

pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);
pub const DEFAULT_REQUIRED_OBSERVATIONS: u8 = 2;
pub const DEFAULT_SIGNIFICANT_TO_CWD_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StabilityPolicy {
    poll_interval: Duration,
    required_consecutive_observations: u8,
    significant_to_cwd_grace: Duration,
}

impl Default for StabilityPolicy {
    fn default() -> Self {
        Self {
            poll_interval: DEFAULT_POLL_INTERVAL,
            required_consecutive_observations: DEFAULT_REQUIRED_OBSERVATIONS,
            significant_to_cwd_grace: DEFAULT_SIGNIFICANT_TO_CWD_GRACE,
        }
    }
}

impl StabilityPolicy {
    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    pub fn required_consecutive_observations(&self) -> u8 {
        self.required_consecutive_observations
    }

    pub fn significant_to_cwd_grace(&self) -> Duration {
        self.significant_to_cwd_grace
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StabilityDecision {
    Pending,
    Rename { label: String },
    NoOp { label: String },
}

#[derive(Debug, Clone)]
pub struct StabilityState {
    policy: StabilityPolicy,
    pending: Option<PendingCandidate>,
    last_stable: Option<LabelCandidate>,
    last_stable_significant: Option<TimedCandidate>,
}

impl Default for StabilityState {
    fn default() -> Self {
        Self::new(StabilityPolicy::default())
    }
}

impl StabilityState {
    pub fn new(policy: StabilityPolicy) -> Self {
        Self {
            policy,
            pending: None,
            last_stable: None,
            last_stable_significant: None,
        }
    }

    pub fn policy(&self) -> StabilityPolicy {
        self.policy
    }

    pub fn observe(
        &mut self,
        candidate: LabelCandidate,
        observed_at: Instant,
    ) -> StabilityDecision {
        let raw_source = candidate.source();
        let effective_candidate = self.effective_candidate(candidate, observed_at);
        let consecutive_count = self.record_observation(&effective_candidate);

        let decision = if let Some(last_stable) = &self.last_stable
            && last_stable == &effective_candidate
        {
            StabilityDecision::NoOp {
                label: effective_candidate.label().to_string(),
            }
        } else if consecutive_count >= self.policy.required_consecutive_observations {
            self.last_stable = Some(effective_candidate.clone());
            StabilityDecision::Rename {
                label: effective_candidate.label().to_string(),
            }
        } else {
            StabilityDecision::Pending
        };

        if raw_source == LabelCandidateSource::SignificantCommand
            && matches!(
                decision,
                StabilityDecision::Rename { .. } | StabilityDecision::NoOp { .. }
            )
        {
            self.last_stable_significant = Some(TimedCandidate {
                candidate: effective_candidate,
                observed_at,
            });
        }

        decision
    }

    fn effective_candidate(
        &self,
        candidate: LabelCandidate,
        observed_at: Instant,
    ) -> LabelCandidate {
        match candidate.source() {
            LabelCandidateSource::SignificantCommand => candidate,
            LabelCandidateSource::WorkingDirectoryBasename => self
                .last_stable_significant
                .as_ref()
                .filter(|last_stable_significant| {
                    observed_at.duration_since(last_stable_significant.observed_at)
                        < self.policy.significant_to_cwd_grace
                })
                .map(|last_stable_significant| last_stable_significant.candidate.clone())
                .unwrap_or(candidate),
        }
    }

    fn record_observation(&mut self, candidate: &LabelCandidate) -> u8 {
        match &mut self.pending {
            Some(pending) if pending.candidate == *candidate => {
                pending.consecutive_observations =
                    pending.consecutive_observations.saturating_add(1);
                pending.consecutive_observations
            }
            _ => {
                self.pending = Some(PendingCandidate {
                    candidate: candidate.clone(),
                    consecutive_observations: 1,
                });
                1
            }
        }
    }
}

#[derive(Debug, Clone)]
struct PendingCandidate {
    candidate: LabelCandidate,
    consecutive_observations: u8,
}

#[derive(Debug, Clone)]
struct TimedCandidate {
    candidate: LabelCandidate,
    observed_at: Instant,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_matches_design_timings() {
        let policy = StabilityPolicy::default();

        assert_eq!(policy.poll_interval(), Duration::from_millis(500));
        assert_eq!(policy.required_consecutive_observations(), 2);
        assert_eq!(policy.significant_to_cwd_grace(), Duration::from_secs(2));
    }

    #[test]
    fn candidate_is_stable_after_two_consecutive_observations() {
        let start = Instant::now();
        let mut state = StabilityState::default();

        assert_eq!(
            state.observe(significant("nvim"), start),
            StabilityDecision::Pending
        );
        assert_eq!(
            state.observe(significant("nvim"), start + Duration::from_millis(500)),
            StabilityDecision::Rename {
                label: "nvim".to_string()
            }
        );
    }

    #[test]
    fn candidate_is_not_stable_with_one_observation() {
        let mut state = StabilityState::default();

        assert_eq!(
            state.observe(significant("nvim"), Instant::now()),
            StabilityDecision::Pending
        );
    }

    #[test]
    fn transient_candidate_changes_do_not_trigger_rename() {
        let start = Instant::now();
        let mut state = StabilityState::default();

        assert_eq!(
            state.observe(significant("nvim"), start),
            StabilityDecision::Pending
        );
        assert_eq!(
            state.observe(significant("codex"), start + Duration::from_millis(500)),
            StabilityDecision::Pending
        );
        assert_eq!(
            state.observe(significant("nvim"), start + Duration::from_millis(1000)),
            StabilityDecision::Pending
        );
        assert_eq!(
            state.observe(cwd("tabby"), start + Duration::from_millis(1500)),
            StabilityDecision::Pending
        );
    }

    #[test]
    fn no_op_when_candidate_matches_last_stable_label() {
        let start = Instant::now();
        let mut state = StabilityState::default();

        assert_eq!(
            state.observe(significant("nvim"), start),
            StabilityDecision::Pending
        );
        assert_eq!(
            state.observe(significant("nvim"), start + Duration::from_millis(500)),
            StabilityDecision::Rename {
                label: "nvim".to_string()
            }
        );
        assert_eq!(
            state.observe(significant("nvim"), start + Duration::from_millis(1000)),
            StabilityDecision::NoOp {
                label: "nvim".to_string()
            }
        );
    }

    #[test]
    fn grace_period_keeps_last_significant_command_before_cwd_fallback() {
        let start = Instant::now();
        let mut state = StabilityState::default();

        assert_eq!(
            state.observe(significant("nvim"), start),
            StabilityDecision::Pending
        );
        assert_eq!(
            state.observe(significant("nvim"), start + Duration::from_millis(500)),
            StabilityDecision::Rename {
                label: "nvim".to_string()
            }
        );
        assert_eq!(
            state.observe(
                cwd("tabby"),
                start + Duration::from_millis(500) + DEFAULT_SIGNIFICANT_TO_CWD_GRACE
                    - Duration::from_millis(1)
            ),
            StabilityDecision::NoOp {
                label: "nvim".to_string()
            }
        );
    }

    #[test]
    fn falls_back_to_cwd_after_grace_period_expires() {
        let start = Instant::now();
        let mut state = StabilityState::default();

        assert_eq!(
            state.observe(significant("nvim"), start),
            StabilityDecision::Pending
        );
        assert_eq!(
            state.observe(significant("nvim"), start + Duration::from_millis(500)),
            StabilityDecision::Rename {
                label: "nvim".to_string()
            }
        );
        assert_eq!(
            state.observe(
                cwd("tabby"),
                start + Duration::from_millis(500) + DEFAULT_SIGNIFICANT_TO_CWD_GRACE
            ),
            StabilityDecision::Pending
        );
        assert_eq!(
            state.observe(
                cwd("tabby"),
                start + Duration::from_millis(1000) + DEFAULT_SIGNIFICANT_TO_CWD_GRACE
            ),
            StabilityDecision::Rename {
                label: "tabby".to_string()
            }
        );
    }

    fn significant(label: &str) -> LabelCandidate {
        LabelCandidate::significant_command(label)
    }

    fn cwd(label: &str) -> LabelCandidate {
        LabelCandidate::working_directory_basename(label)
    }
}
