//! Plan mode types: steps, loop phase, and progress state.

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum PlanStepStatus {
    #[default]
    Pending,
    InProgress,
    Done,
    #[allow(dead_code)]
    Failed,
}

/// Verification tier for a planned step (see docs/plan-mode-improvements.md).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum PlanStepTier {
    #[default]
    Exec,
    Check,
    Attested,
    Observe,
}

impl PlanStepTier {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "exec" => Some(Self::Exec),
            "check" => Some(Self::Check),
            "attested" => Some(Self::Attested),
            "observe" => Some(Self::Observe),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::Check => "check",
            Self::Attested => "attested",
            Self::Observe => "observe",
        }
    }

    /// Short label for the plan pane step list.
    pub fn pane_label(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::Check => "check",
            Self::Attested => "attested",
            Self::Observe => "observe",
        }
    }
}

#[derive(Clone, Default)]
pub struct PlanStep {
    pub description: String,
    pub verification: Option<String>,
    pub tier: Option<PlanStepTier>,
    pub note: Option<String>,
    pub observe_prompt: Option<String>,
    pub status: PlanStepStatus,
}

/// Harness-driven JSON plan loop phase.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlanLoopPhase {
    #[default]
    Idle,
    /// Waiting for clarify JSON from the model.
    FetchingQuestion,
    /// Showing a structured question; waiting for user answer.
    AwaitingUserAnswer,
    /// Waiting for proposal JSON from the model.
    FetchingProposal,
    /// Showing final proposal; waiting for proceed consent.
    AwaitingProceedConsent,
}

#[derive(Default)]
pub struct PlanState {
    pub active: bool,
    pub goal: String,
    pub success_criteria: String,
    pub verification_steps: Vec<String>,
    pub rollback: String,
    pub constraints: String,
    /// User-chosen or inferred deliverable subdirectory (e.g. `galaga`).
    pub project_workdir: Option<String>,
    pub steps: Vec<PlanStep>,
    pub current_step: usize,
    pub spinner_tick: usize,
    /// Observe-tier: prompt shown to user before agent continues.
    pub pending_observe_prompt: Option<String>,
    pub pending_observe_step: Option<usize>,
    /// True after the agent invites "proceed?" — user assent counts only then.
    pub recap_offered: bool,
    /// JSON-driven plan loop (clarify → propose → consent).
    pub loop_phase: PlanLoopPhase,
    pub initial_request: String,
    pub qa_history: Vec<raven_tui::plan_protocol::PlanQaEntry>,
    pub pending_question: Option<raven_tui::plan_protocol::PlanQuestion>,
    pub pending_proposal: Option<raven_tui::plan_protocol::PlanProposal>,
    /// Plan execution finished; hide the pane on the user's next message.
    pub dismiss_pane_on_next_input: bool,
}

impl PlanState {
    /// True when every approved step has been completed (`current_step` past the last step).
    pub fn is_execution_complete(&self) -> bool {
        !self.steps.is_empty() && self.current_step >= self.steps.len()
    }

    /// Mark the current step Done and advance the pointer (if possible).
    #[allow(dead_code)]
    pub fn advance_one_step(&mut self) {
        if !self.steps.is_empty() && self.current_step < self.steps.len() {
            self.steps[self.current_step].status = PlanStepStatus::Done;
            self.current_step += 1;
        }
    }

    /// Mark every step Done and move current_step to the end.
    pub fn complete(&mut self) {
        for s in &mut self.steps {
            s.status = PlanStepStatus::Done;
        }
        if !self.steps.is_empty() {
            self.current_step = self.steps.len();
        }
    }

    /// Heuristic: does this text represent strong task completion for plan progress purposes?
    #[allow(dead_code)]
    pub fn is_strong_completion_signal(text: &str) -> bool {
        let t = text.to_lowercase();
        t.contains("work_complete")
            || t.contains("fulfilled")
            || t.contains("**done")
            || t.contains("done!")
            || t.contains("task is complete")
            || (t.contains("successfully") && t.contains("criteria"))
    }

    /// Whole-plan completion from judge WORK_COMPLETE only (not per-turn heuristics).
    pub fn complete_on_work_complete_signal(&mut self, summary: &str) {
        if !self.active || self.steps.is_empty() {
            return;
        }
        if summary.contains("WORK_COMPLETE") {
            self.complete();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_steps(n: usize) -> Vec<PlanStep> {
        (0..n)
            .map(|i| PlanStep {
                description: format!("step {}", i + 1),
                verification: None,
                tier: None,
                note: None,
                observe_prompt: None,
                status: PlanStepStatus::Pending,
            })
            .collect()
    }

    #[test]
    fn advance_one_step_marks_and_increments() {
        let mut p = PlanState {
            active: true,
            steps: make_steps(3),
            current_step: 0,
            ..Default::default()
        };
        p.advance_one_step();
        assert_eq!(p.current_step, 1);
        assert!(matches!(p.steps[0].status, PlanStepStatus::Done));
        assert!(matches!(p.steps[1].status, PlanStepStatus::Pending));
    }

    #[test]
    fn is_execution_complete_when_pointer_past_last_step() {
        let mut p = PlanState {
            active: true,
            steps: make_steps(3),
            current_step: 3,
            ..Default::default()
        };
        assert!(p.is_execution_complete());
        p.current_step = 2;
        assert!(!p.is_execution_complete());
    }

    #[test]
    fn complete_marks_all_and_goes_to_end() {
        let mut p = PlanState {
            active: true,
            steps: make_steps(3),
            current_step: 1,
            ..Default::default()
        };
        p.complete();
        assert_eq!(p.current_step, 3);
        assert!(p.steps.iter().all(|s| matches!(s.status, PlanStepStatus::Done)));
    }

    #[test]
    fn strong_completion_signals() {
        assert!(PlanState::is_strong_completion_signal("**Done!** the task succeeded"));
        assert!(PlanState::is_strong_completion_signal("WORK_COMPLETE: all good"));
        assert!(PlanState::is_strong_completion_signal(
            "The script runs successfully and meets all criteria."
        ));
        assert!(PlanState::is_strong_completion_signal("FULFILLED"));
        assert!(!PlanState::is_strong_completion_signal("still working on it"));
    }

    #[test]
    fn work_complete_signal_completes_plan() {
        let mut p = PlanState {
            active: true,
            steps: make_steps(3),
            current_step: 1,
            ..Default::default()
        };
        p.complete_on_work_complete_signal("still working");
        assert_eq!(p.current_step, 1);

        p.complete_on_work_complete_signal("⭐⭐ JUDGE: WORK_COMPLETE: criteria satisfied");
        assert_eq!(p.current_step, 3);
        assert!(p.steps.iter().all(|s| matches!(s.status, PlanStepStatus::Done)));
    }
}