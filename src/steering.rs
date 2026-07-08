//! Steering — the pure decision engine for the agent driving loop.
//!
//! This module owns *all* policy decisions about whether to nudge, judge,
//! accept, or abort an agent turn.  It is intentionally free of async, I/O,
//! and any `Agent` reference so that every decision path can be unit-tested
//! with plain data.
//!
//! The async orchestration shell in [`agent_driver`](crate::agent_driver)
//! feeds round outcomes into [`SteeringState`] and maps the returned
//! [`SteeringDecision`] into agent/observer actions.

use std::time::Instant;

use crate::agent::TurnJudge;

// ── Policy constants ─────────────────────────────────────────────────────────

/// Safety limit on how many times we will explicitly nudge for continuation
/// before accepting a text-only response.
pub const MAX_TEXT_NUDGES: u32 = 3;

/// Default budget for nudges driven by the judge between user interactions.
pub const NUDGE_BUDGET: u32 = 3;

/// Safety limit on auto-continuing when the inner round budget is exhausted
/// but the model was still actively calling tools.
pub const MAX_AUTO_CONTINUES: u32 = 3;

/// Hard cap on tool rounds per budget window.
pub const MAX_TOOL_ROUNDS: u32 = 120;

/// The nudge message pushed when the model pauses to narrate mid-turn.
pub const CONTINUATION_NUDGE: &str =
    "[Continue working. You paused to describe your plan instead of calling the next tool. \
     Call the next tool now — do not narrate.]";

/// Nudge when an approved plan is executing but the model stopped with text only.
pub const PLAN_EXECUTION_NUDGE: &str =
    "[Plan execution in progress. You paused without calling tools. \
     Finish the current plan step (write/patch/exec as needed), then call complete_plan_step. \
     Do not narrate — call the next tool now.]";

/// Nudge when plan mode bundles multiple clarification questions in one turn.
pub const PLAN_ONE_QUESTION_NUDGE: &str =
    "[Plan mode: ask exactly ONE clarification question per turn. \
     Pick the single most important decision, offer numbered options for that decision only, \
     then wait for the user's answer before asking the next question.]";

/// The recovery message pushed when finish_reason is "length" (model hit max_tokens).
pub const LENGTH_RECOVERY_NUDGE: &str =
    "[system: Your previous response was truncated (output limit). \
     Please use your tools now to investigate and solve the task. \
     Start with `list` or `read` to explore the codebase.]";

/// Max transparent retries for a single streaming LLM call on transient errors.
pub const MAX_STREAM_RETRIES: u32 = 2;

// ── Nudge text builders ──────────────────────────────────────────────────────

const EMPTY_RESPONSE_BASE: &str =
    "[Your previous response was empty (no text, no tool calls). \
     Start working immediately: use list / read / grep (or read_summary) \
     to explore the code and make progress on the task.]";

const DEFINE_DONE_REMINDER: &str =
    "[You have not called define_done() yet to declare what 'done' looks like \
     for this task. Please call it now with a clear, precise definition so \
     progress can be judged.]";

const DEFAULT_JUDGE_SUGGESTION: &str =
    "focus on minimal source changes that directly address the root cause. \
     Now use tools to satisfy the criteria (read files and patch)";

// ── Decision types ───────────────────────────────────────────────────────────

/// The action the driver should take after a steering decision.
#[derive(Debug, Clone, PartialEq)]
pub enum SteeringDecision {
    /// Keep looping — no nudge or intervention needed.
    Continue,
    /// Push `message` as a user message and continue the loop.
    Nudge {
        message: String,
        /// If true, use `push_continuation_nudge()` instead of `push_message`.
        use_continuation_nudge: bool,
        /// For observer: current nudge count.
        nudge_count: u32,
        /// For observer: max nudge limit to display.
        nudge_max: u32,
        /// Harness event label (e.g. "nudge", "stuck").
        log_event: String,
        /// Harness event detail.
        log_detail: String,
    },
    /// The driver should invoke the judge and then call `apply_judge_verdict()`.
    JudgeNeeded {
        /// Context label for logging (e.g. "criteria active", "ambiguous stop").
        context: String,
    },
    /// Turn is complete — model fulfilled the task or stopped naturally.
    Accept {
        /// If true, clear `completion_criteria` from session.
        clear_criteria: bool,
    },
    /// Agent is stuck — inject guidance and end the turn.
    Stuck {
        reason: String,
        suggested_guidance: String,
        /// The message to inject into conversation.
        inject_message: String,
        /// Harness log detail.
        log_detail: String,
    },
    /// Wall-clock timeout hit.
    Timeout {
        seconds: u64,
    },
}

/// Context about the current round, extracted from the agent/session by the driver.
#[derive(Debug, Default)]
pub struct RoundContext {
    pub effective_text: String,
    pub round_text: String,
    pub finish_reason: Option<String>,
    pub is_llm_error: bool,
    pub has_tool_calls: bool,
    /// Completion criteria from session, if set.
    pub criteria: Option<String>,
    /// The original user request from session meta.
    pub last_user_request: Option<String>,
    /// Whether the session exists.
    pub has_session: bool,
    /// Last assistant content from session log (for malformed tool detection).
    pub log_tail: String,
    /// Current agent mode (e.g. "plan" to allow narration during planning).
    pub agent_mode: String,
    /// Approved plan is actively executing in work mode.
    pub plan_executing: bool,
    /// 0-indexed current step index from plan execution state.
    pub plan_current_step: usize,
    pub plan_total_steps: usize,
    /// Observe-tier step waiting for user input — accept text-only stop.
    pub plan_pending_observe: bool,
}

/// Summary of recent actions for safety checks.
#[derive(Debug, Default)]
pub struct RecentActionsSummary {
    pub has_exec: bool,
    pub has_write_or_patch: bool,
    pub count: usize,
}

// ── Steering state ───────────────────────────────────────────────────────────

/// Mutable steering state that tracks nudge budgets and round counts.
///
/// Created once per `drive_turn()` call.  All decision methods are `&mut self`
/// because they may increment counters.
pub struct SteeringState {
    pub text_nudges: u32,
    pub judge_nudges: u32,
    pub tools_used: usize,
    pub total_rounds: u32,
    pub max_rounds: u32,
    pub enable_judge: bool,
    deadline: Option<Instant>,
}

impl SteeringState {
    /// Create a new steering state from the agent's config.
    pub fn new(max_rounds: u32, enable_judge: bool, max_duration_secs: Option<u64>) -> Self {
        let deadline = max_duration_secs.map(|s| Instant::now() + std::time::Duration::from_secs(s));
        Self {
            text_nudges: 0,
            judge_nudges: 0,
            tools_used: 0,
            total_rounds: 0,
            max_rounds: max_rounds.clamp(1, MAX_TOOL_ROUNDS),
            enable_judge,
            deadline,
        }
    }

    /// Create a steering state for the Super Judge's mini-loop (tighter budgets).
    #[allow(dead_code)]
    pub fn for_super_judge(max_rounds: u32) -> Self {
        Self {
            text_nudges: 0,
            judge_nudges: 0,
            tools_used: 0,
            total_rounds: 0,
            max_rounds: max_rounds.clamp(1, 20),
            enable_judge: false,
            deadline: None,
        }
    }

    /// Check if the wall-clock deadline has been exceeded.
    pub fn past_deadline(&self) -> bool {
        self.deadline.is_some_and(|dl| Instant::now() >= dl)
    }

    /// Get the configured max_duration_secs (for timeout messages).
    pub fn deadline_seconds(&self) -> Option<u64> {
        // We can't recover the original value from Instant, so the driver
        // should pass it through from config.  This is a helper for the
        // Timeout decision variant.
        None // Filled by the driver when constructing Timeout
    }

    /// Has budget remaining for another LLM round?
    pub fn has_round_budget(&self) -> bool {
        self.total_rounds < self.max_rounds
    }

    // ── Primary decision points ──────────────────────────────────────────

    /// Decide what to do when the model produced no tool calls.
    ///
    /// This is the most complex decision point, handling:
    /// - LLM stream error recovery
    /// - finish_reason=length recovery
    /// - Criteria-based judge invocation
    /// - Define-done reminders
    /// - Empty response recovery (with judge escalation on 3rd empty)
    /// - Plan narration detection
    /// - Malformed tool syntax detection → ambiguous stop
    pub fn decide_no_tools(
        &mut self,
        ctx: &RoundContext,
        recent: &RecentActionsSummary,
    ) -> SteeringDecision {
        // 1. LLM stream error recovery
        if ctx.is_llm_error
            && self.has_round_budget() {
                return SteeringDecision::Nudge {
                    message: "[LLM stream error during request. Please continue using your tools to solve the task.]".into(),
                    use_continuation_nudge: false,
                    nudge_count: self.text_nudges,
                    nudge_max: MAX_TEXT_NUDGES,
                    log_event: "nudge".into(),
                    log_detail: "llm-stream-error-recovery".into(),
                };
            }
            // Exhausted rounds — fall through to accept

        // 2. finish_reason=length → recovery nudge
        if ctx.finish_reason.as_deref() == Some("length") && self.has_round_budget() {
            return SteeringDecision::Nudge {
                message: LENGTH_RECOVERY_NUDGE.into(),
                use_continuation_nudge: false,
                nudge_count: self.text_nudges,
                nudge_max: MAX_TEXT_NUDGES,
                log_event: "nudge".into(),
                log_detail: "length-recovery".into(),
            };
        }

        // 2b. Active plan execution: nudge before judge/define_done (criteria must not end the turn mid-step)
        if plan_execution_incomplete(ctx)
            && ctx.agent_mode == "work"
            && self.has_round_budget()
        {
            let step = ctx.plan_current_step + 1;
            let total = ctx.plan_total_steps;
            self.text_nudges += 1;
            return SteeringDecision::Nudge {
                message: format!(
                    "{PLAN_EXECUTION_NUDGE}\n\nCurrent plan step: {step} of {total}."
                ),
                use_continuation_nudge: true,
                nudge_count: self.text_nudges,
                nudge_max: MAX_TEXT_NUDGES,
                log_event: "nudge".into(),
                log_detail: format!("plan-execution nudge at step {step}/{total}"),
            };
        }

        // 3. Criteria-based judge
        // Skip in plan mode — we are still in clarification; judge/define_done criteria
        // should not force completion or nudges until after "proceed".
        if let Some(ref criteria) = ctx.criteria {
            if !criteria.trim().is_empty() && ctx.agent_mode != "plan" {
                return SteeringDecision::JudgeNeeded {
                    context: "criteria active".into(),
                };
            }
        }

        // 4. Define-done reminder (no criteria yet, judge enabled, past round 1)
        // Skip in plan mode and during plan execution.
        if ctx.criteria.is_none()
            && self.total_rounds > 1
            && ctx.has_session
            && self.enable_judge
            && self.judge_nudges < NUDGE_BUDGET
            && ctx.agent_mode != "plan"
            && !plan_execution_incomplete(ctx) {
                self.judge_nudges += 1;
                return SteeringDecision::Nudge {
                    message: DEFINE_DONE_REMINDER.into(),
                    use_continuation_nudge: false,
                    nudge_count: self.judge_nudges,
                    nudge_max: NUDGE_BUDGET,
                    log_event: "nudge".into(),
                    log_detail: format!(
                        "define-done-reminder nudge {}/{}",
                        self.judge_nudges, NUDGE_BUDGET
                    ),
                };
            }

        // 5. Empty response recovery
        if ctx.effective_text.trim().is_empty() && !ctx.is_llm_error {
            if let Some(decision) = self.decide_empty_response(ctx) {
                return decision;
            }
        }

        // 6. Plan mode: one question per turn
        if ctx.agent_mode == "plan"
            && detect_plan_mode_multiple_questions(&ctx.effective_text)
            && self.text_nudges < MAX_TEXT_NUDGES
        {
            self.text_nudges += 1;
            return SteeringDecision::Nudge {
                message: PLAN_ONE_QUESTION_NUDGE.into(),
                use_continuation_nudge: false,
                nudge_count: self.text_nudges,
                nudge_max: MAX_TEXT_NUDGES,
                log_event: "nudge".into(),
                log_detail: format!(
                    "plan-one-question nudge {}/{}",
                    self.text_nudges, MAX_TEXT_NUDGES
                ),
            };
        }

        // 7. Plan narration detection
        // Skip during plan mode — the agent is *supposed* to describe and clarify the plan.
        if detect_plan_narration(&ctx.effective_text)
            && !ctx.is_llm_error
            && self.text_nudges < MAX_TEXT_NUDGES
            && ctx.agent_mode != "plan"
        {
            self.text_nudges += 1;
            return SteeringDecision::Nudge {
                message: CONTINUATION_NUDGE.into(),
                use_continuation_nudge: true,
                nudge_count: self.text_nudges,
                nudge_max: MAX_TEXT_NUDGES,
                log_event: "nudge".into(),
                log_detail: format!(
                    "plan-narration nudge {}/{}",
                    self.text_nudges, MAX_TEXT_NUDGES
                ),
            };
        }

        // 8. Malformed tool syntax + ambiguous stop check
        let has_malformed = detect_malformed_tool_syntax(&ctx.round_text)
            || detect_malformed_tool_syntax(&ctx.log_tail);
        let model_stopped = ctx.finish_reason.as_deref() == Some("stop");

        if !model_stopped && (self.tools_used > 0 || has_malformed) {
            return self.decide_ambiguous_stop(ctx, recent);
        }

        // 9. Natural completion
        SteeringDecision::Accept {
            clear_criteria: false,
        }
    }

    /// Handle empty response recovery (sub-decision of `decide_no_tools`).
    fn decide_empty_response(&mut self, ctx: &RoundContext) -> Option<SteeringDecision> {
        if !self.has_round_budget() {
            return None;
        }

        if self.text_nudges < MAX_TEXT_NUDGES {
            self.text_nudges += 1;

            let reminder = ctx
                .last_user_request
                .as_deref()
                .map(|r| format!("\n\nRemember the original request:\n{}", r))
                .unwrap_or_default();

            let criteria_reminder = ctx
                .criteria
                .as_deref()
                .map(|c| {
                    format!(
                        "\n\nYou defined done as: {}. Read the files in the traceback \
                         (schema.py, fields.py etc.) and use patch to make it true.",
                        c
                    )
                })
                .unwrap_or_default();

            if self.text_nudges < MAX_TEXT_NUDGES {
                // Normal empty nudge (1st or 2nd)
                let nudge = format!("{}{}{}", EMPTY_RESPONSE_BASE, reminder, criteria_reminder);
                return Some(SteeringDecision::Nudge {
                    message: nudge.clone(),
                    use_continuation_nudge: false,
                    nudge_count: self.text_nudges,
                    nudge_max: MAX_TEXT_NUDGES,
                    log_event: "nudge".into(),
                    log_detail: format!(
                        "empty-recovery nudge {}/{}: {}",
                        self.text_nudges, MAX_TEXT_NUDGES, nudge
                    ),
                });
            } else {
                // 3rd empty nudge: escalate to judge
                return Some(SteeringDecision::JudgeNeeded {
                    context: "3rd empty nudge escalation".into(),
                });
            }
        }
        None
    }

    /// Decide when the model stopped but finish_reason != "stop" and
    /// we've either used tools or detected malformed tool syntax.
    fn decide_ambiguous_stop(
        &mut self,
        ctx: &RoundContext,
        recent: &RecentActionsSummary,
    ) -> SteeringDecision {
        let implies_run = ctx
            .last_user_request
            .as_deref()
            .map(request_implies_execution)
            .unwrap_or(false);

        // Hard safety: request implies run but no exec yet
        if implies_run && !recent.has_exec {
            if self.text_nudges < MAX_TEXT_NUDGES {
                self.text_nudges += 1;
                return SteeringDecision::Nudge {
                    message: CONTINUATION_NUDGE.into(),
                    use_continuation_nudge: true,
                    nudge_count: self.text_nudges,
                    nudge_max: MAX_TEXT_NUDGES,
                    log_event: "judge".into(),
                    log_detail:
                        "JUDGE/HARD-SAFETY: request implies run but no exec yet → forcing nudge"
                            .into(),
                };
            }
            return SteeringDecision::Accept {
                clear_criteria: false,
            };
        }

        // Write/patch done but still implies run + no exec
        if recent.has_write_or_patch && implies_run && !recent.has_exec
            && self.text_nudges < MAX_TEXT_NUDGES {
                self.text_nudges += 1;
                return SteeringDecision::Nudge {
                    message: CONTINUATION_NUDGE.into(),
                    use_continuation_nudge: true,
                    nudge_count: self.text_nudges,
                    nudge_max: MAX_TEXT_NUDGES,
                    log_event: "nudge".into(),
                    log_detail: "write-but-no-exec nudge".into(),
                };
            }

        // Consult the judge
        SteeringDecision::JudgeNeeded {
            context: "ambiguous stop".into(),
        }
    }

    /// After a judge verdict, decide the next action.
    ///
    /// Called by the driver after invoking the judge in response to
    /// `SteeringDecision::JudgeNeeded`.
    pub fn apply_judge_verdict(
        &mut self,
        verdict: &TurnJudge,
        criteria: Option<&str>,
        context: &str,
    ) -> SteeringDecision {
        match verdict {
            TurnJudge::Fulfilled { .. } => SteeringDecision::Accept {
                clear_criteria: true,
            },

            TurnJudge::Stuck {
                reason,
                suggested_guidance,
            } => {
                let inject = match context {
                    "3rd empty nudge escalation" => format!(
                        "[You appear to be looping without progress after empty responses: {}. \
                         Stop and explain the situation or ask for guidance: {}]",
                        reason, suggested_guidance
                    ),
                    _ => format!(
                        "[You appear to be looping without progress: {}. \
                         Stop calling tools. Clearly explain the current situation \
                         to the user and ask for specific guidance: {}]\n\
                         If the diagnosis is clear, immediately use `patch` or `write` \
                         on the relevant source file to apply the fix.",
                        reason, suggested_guidance
                    ),
                };

                // For ambiguous stop with text_nudge budget remaining, give one
                // more chance after injecting the stuck guidance.
                if context == "ambiguous stop" && self.text_nudges < MAX_TEXT_NUDGES {
                    self.text_nudges += 1;
                    return SteeringDecision::Stuck {
                        reason: reason.clone(),
                        suggested_guidance: suggested_guidance.clone(),
                        inject_message: inject.clone(),
                        log_detail: inject,
                    };
                }

                SteeringDecision::Stuck {
                    reason: reason.clone(),
                    suggested_guidance: suggested_guidance.clone(),
                    inject_message: inject.clone(),
                    log_detail: inject,
                }
            }

            TurnJudge::Continue { suggestion } => {
                let criteria_active = criteria.is_some_and(|c| !c.trim().is_empty());
                let criteria_str = criteria.unwrap_or("");

                // Unbounded judge-continue when --enable-judge + criteria active
                if self.enable_judge && criteria_active {
                    self.judge_nudges += 1;
                    let base_sugg = suggestion
                        .clone()
                        .unwrap_or_else(|| DEFAULT_JUDGE_SUGGESTION.to_string());
                    let nudge_text = if suggestion.is_some() {
                        format!("[You defined done as the criteria. {}]", base_sugg)
                    } else {
                        CONTINUATION_NUDGE.to_string()
                    };
                    return SteeringDecision::Nudge {
                        message: nudge_text,
                        use_continuation_nudge: suggestion.is_none(),
                        nudge_count: self.judge_nudges,
                        nudge_max: 999,
                        log_event: "nudge".into(),
                        log_detail: format!(
                            "judge-continue nudge {}/inf (criteria)",
                            self.judge_nudges
                        ),
                    };
                }

                // Budgeted judge-continue
                let use_budget = criteria_active || self.judge_nudges < NUDGE_BUDGET;
                if use_budget {
                    if criteria_active {
                        self.judge_nudges += 1;
                    } else {
                        self.text_nudges += 1;
                    }
                    let (count, max) = if criteria_active {
                        (self.judge_nudges, NUDGE_BUDGET)
                    } else {
                        (self.text_nudges, MAX_TEXT_NUDGES)
                    };

                    let base_sugg = suggestion
                        .clone()
                        .unwrap_or_else(|| DEFAULT_JUDGE_SUGGESTION.to_string());
                    let nudge_text = if criteria_active {
                        format!(
                            "[You defined done as: {}. Suggestion: {}]",
                            criteria_str, base_sugg
                        )
                    } else if suggestion.is_some() {
                        format!("[You defined done as the criteria. {}]", base_sugg)
                    } else {
                        CONTINUATION_NUDGE.to_string()
                    };

                    return SteeringDecision::Nudge {
                        message: nudge_text,
                        use_continuation_nudge: suggestion.is_none() && !criteria_active,
                        nudge_count: count,
                        nudge_max: max,
                        log_event: "nudge".into(),
                        log_detail: format!("judge-continue nudge {}/{}", count, max),
                    };
                }

                // 3rd empty nudge context: judge said Continue despite limit
                if context == "3rd empty nudge escalation" {
                    let criteria_reminder = criteria
                        .map(|c| {
                            format!(
                                " You defined done as: {}. Now read the key files and patch to satisfy it.",
                                c
                            )
                        })
                        .unwrap_or_default();
                    let nudge = format!(
                        "{} [JUDGE: Continue despite limit.{}]",
                        EMPTY_RESPONSE_BASE, criteria_reminder
                    );
                    return SteeringDecision::Nudge {
                        message: nudge.clone(),
                        use_continuation_nudge: false,
                        nudge_count: self.text_nudges,
                        nudge_max: MAX_TEXT_NUDGES,
                        log_event: "nudge".into(),
                        log_detail: format!("3rd-empty-continue nudge: {}", nudge),
                    };
                }

                // Budget exhausted — accept
                SteeringDecision::Accept {
                    clear_criteria: false,
                }
            }
        }
    }
}

// ── Pure detection functions ─────────────────────────────────────────────────

/// Detect "plan narration" responses where the model describes what it will do
/// but doesn't actually call a tool.
/// Plan mode should ask one branching decision per turn — detect bundled question lists.
pub fn detect_plan_mode_multiple_questions(text: &str) -> bool {
    let mut numbered_items = 0u32;
    for line in text.lines() {
        let t = line.trim();
        if t.len() < 3 {
            continue;
        }
        let starts_digit = t.chars().next().is_some_and(|c| c.is_ascii_digit());
        if starts_digit && (t.contains(". ") || t.contains(") ")) {
            numbered_items += 1;
        }
    }
    numbered_items >= 2
}

pub fn detect_plan_narration(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    lower.contains("let me ")
        || lower.contains("i will ")
        || lower.contains("i need to ")
        || lower.contains("now i need to ")
        || lower.contains("the fix is")
        || lower.contains("implement this")
        || lower.contains("re-read the code")
        || (lower.contains("implement") && lower.contains("fix"))
}

/// True when work-mode plan execution still has steps to finish (not waiting on observe).
pub fn plan_execution_incomplete(ctx: &RoundContext) -> bool {
    ctx.plan_executing
        && !ctx.plan_pending_observe
        && ctx.plan_total_steps > 0
        && ctx.plan_current_step < ctx.plan_total_steps
}

/// Detect malformed/partial tool call XML syntax that didn't parse as a real tool call.
pub fn detect_malformed_tool_syntax(text: &str) -> bool {
    crate::tool_xml::contains_tool_xml_syntax(text)
}

/// Check if a user request text implies that execution/output is expected.
pub fn request_implies_execution(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("run")
        || lower.contains("exec")
        || lower.contains("show")
        || lower.contains("output")
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn basic_state() -> SteeringState {
        SteeringState::new(120, false, None)
    }

    fn judge_enabled_state() -> SteeringState {
        SteeringState::new(120, true, None)
    }

    fn empty_round() -> RoundContext {
        RoundContext {
            effective_text: String::new(),
            round_text: String::new(),
            finish_reason: Some("stop".into()),
            is_llm_error: false,
            has_tool_calls: false,
            criteria: None,
            last_user_request: None,
            has_session: true,
            log_tail: String::new(),
            agent_mode: "work".into(),
            ..Default::default()
        }
    }

    fn text_round(text: &str) -> RoundContext {
        RoundContext {
            effective_text: text.into(),
            round_text: text.into(),
            finish_reason: Some("stop".into()),
            is_llm_error: false,
            has_tool_calls: false,
            criteria: None,
            last_user_request: None,
            has_session: true,
            log_tail: String::new(),
            agent_mode: "work".into(),
            ..Default::default()
        }
    }

    fn no_recent() -> RecentActionsSummary {
        RecentActionsSummary::default()
    }

    // ── Detection function tests ──

    #[test]
    fn plan_narration_detects_let_me() {
        assert!(detect_plan_narration("Let me implement the fix now"));
    }

    #[test]
    fn plan_narration_detects_i_will() {
        assert!(detect_plan_narration("I will patch the file next"));
    }

    #[test]
    fn plan_narration_detects_the_fix_is() {
        assert!(detect_plan_narration("The fix is to change line 42"));
    }

    #[test]
    fn plan_narration_detects_implement_fix() {
        assert!(detect_plan_narration("We need to implement a fix for this"));
    }

    #[test]
    fn plan_narration_false_for_normal_text() {
        assert!(!detect_plan_narration("Here is the result of running the test"));
    }

    #[test]
    fn plan_mode_multiple_questions_detected() {
        let text = "1. **Location**: separate dir?\n2. **Library**: SFML?\n3. **Env**: installed?";
        assert!(detect_plan_mode_multiple_questions(text));
    }

    #[test]
    fn plan_mode_one_question_not_flagged() {
        assert!(!detect_plan_mode_multiple_questions(
            "Do you want a separate directory or a subdirectory here? I recommend subdirectory."
        ));
    }

    #[test]
    fn plan_narration_false_for_empty() {
        assert!(!detect_plan_narration(""));
    }

    #[test]
    fn plan_narration_detects_i_need_to() {
        assert!(detect_plan_narration(
            "Now I need to rebuild to verify the compilation succeeds."
        ));
    }

    #[test]
    fn request_implies_run() {
        assert!(request_implies_execution("run the tests"));
        assert!(request_implies_execution("exec this script"));
        assert!(request_implies_execution("show me the output"));
        assert!(request_implies_execution("what is the output?"));
    }

    #[test]
    fn request_does_not_imply_run() {
        assert!(!request_implies_execution("fix this bug"));
        assert!(!request_implies_execution("write a function"));
    }

    // ── SteeringState decision tests ──

    #[test]
    fn natural_stop_accepts() {
        let mut s = basic_state();
        s.total_rounds = 1;
        let ctx = text_round("Here's the answer to your question.");
        let d = s.decide_no_tools(&ctx, &no_recent());
        assert!(matches!(d, SteeringDecision::Accept { clear_criteria: false }));
    }

    fn plan_exec_round(text: &str, step_index: usize, total: usize) -> RoundContext {
        RoundContext {
            effective_text: text.into(),
            round_text: text.into(),
            finish_reason: Some("stop".into()),
            is_llm_error: false,
            has_tool_calls: false,
            criteria: None,
            last_user_request: Some("continue".into()),
            has_session: true,
            log_tail: String::new(),
            agent_mode: "work".into(),
            plan_executing: true,
            plan_current_step: step_index,
            plan_total_steps: total,
            plan_pending_observe: false,
        }
    }

    #[test]
    fn plan_execution_text_only_nudges_not_accepts() {
        let mut s = basic_state();
        s.total_rounds = 5;
        let ctx = plan_exec_round(
            "Now I need to rebuild to verify the compilation succeeds.",
            5,
            8,
        );
        let d = s.decide_no_tools(&ctx, &no_recent());
        assert!(
            matches!(d, SteeringDecision::Nudge { .. }),
            "expected nudge during plan execution, got {d:?}"
        );
    }

    #[test]
    fn plan_execution_nudge_beats_criteria_judge() {
        let mut s = basic_state();
        s.total_rounds = 5;
        let mut ctx = plan_exec_round(
            "Verification passed. I should call complete_plan_step for step 5.",
            4,
            7,
        );
        ctx.criteria = Some("cmake --build build".into());
        let d = s.decide_no_tools(&ctx, &no_recent());
        assert!(
            matches!(d, SteeringDecision::Nudge { .. }),
            "plan execution nudge must run before criteria judge, got {d:?}"
        );
    }

    #[test]
    fn plan_execution_accepts_when_all_steps_done() {
        let mut s = basic_state();
        s.total_rounds = 5;
        let mut ctx = plan_exec_round("All steps complete.", 8, 8);
        ctx.plan_current_step = 8;
        let d = s.decide_no_tools(&ctx, &no_recent());
        assert!(matches!(d, SteeringDecision::Accept { clear_criteria: false }));
    }

    #[test]
    fn llm_error_nudges_if_budget() {
        let mut s = basic_state();
        s.total_rounds = 1;
        let ctx = RoundContext {
            is_llm_error: true,
            ..empty_round()
        };
        let d = s.decide_no_tools(&ctx, &no_recent());
        assert!(matches!(d, SteeringDecision::Nudge { .. }));
    }

    #[test]
    fn length_recovery_nudge() {
        let mut s = basic_state();
        s.total_rounds = 1;
        let ctx = RoundContext {
            effective_text: "partial text".into(),
            finish_reason: Some("length".into()),
            ..empty_round()
        };
        let d = s.decide_no_tools(&ctx, &no_recent());
        match d {
            SteeringDecision::Nudge { message, .. } => {
                assert!(message.contains("truncated"));
            }
            other => panic!("Expected Nudge, got {:?}", other),
        }
    }

    #[test]
    fn criteria_active_triggers_judge() {
        let mut s = basic_state();
        s.total_rounds = 1;
        let ctx = RoundContext {
            effective_text: "I think I'm done".into(),
            criteria: Some("tests pass".into()),
            ..empty_round()
        };
        let d = s.decide_no_tools(&ctx, &no_recent());
        assert!(matches!(d, SteeringDecision::JudgeNeeded { .. }));
    }

    #[test]
    fn define_done_reminder_when_judge_enabled() {
        let mut s = judge_enabled_state();
        s.total_rounds = 2;
        let ctx = RoundContext {
            effective_text: "I looked at the code".into(),
            criteria: None,
            ..empty_round()
        };
        let d = s.decide_no_tools(&ctx, &no_recent());
        match d {
            SteeringDecision::Nudge { message, .. } => {
                assert!(message.contains("define_done"));
            }
            other => panic!("Expected Nudge with define_done, got {:?}", other),
        }
    }

    #[test]
    fn empty_response_nudges() {
        let mut s = basic_state();
        s.total_rounds = 1;
        let d = s.decide_no_tools(&empty_round(), &no_recent());
        match d {
            SteeringDecision::Nudge { message, .. } => {
                assert!(message.contains("empty"));
            }
            other => panic!("Expected empty nudge, got {:?}", other),
        }
        assert_eq!(s.text_nudges, 1);
    }

    #[test]
    fn plan_narration_nudges() {
        let mut s = basic_state();
        s.total_rounds = 1;
        let ctx = text_round("Let me implement the fix for this bug");
        let d = s.decide_no_tools(&ctx, &no_recent());
        assert!(matches!(d, SteeringDecision::Nudge { use_continuation_nudge: true, .. }));
        assert_eq!(s.text_nudges, 1);
    }

    #[test]
    fn ambiguous_stop_with_tools_used_triggers_judge() {
        let mut s = basic_state();
        s.total_rounds = 1;
        s.tools_used = 3;
        let ctx = RoundContext {
            effective_text: "I've made the changes".into(),
            finish_reason: None, // not "stop"
            ..empty_round()
        };
        let d = s.decide_no_tools(&ctx, &no_recent());
        assert!(matches!(d, SteeringDecision::JudgeNeeded { .. }));
    }

    #[test]
    fn implies_run_no_exec_forces_nudge() {
        let mut s = basic_state();
        s.total_rounds = 1;
        s.tools_used = 2;
        let ctx = RoundContext {
            effective_text: "I wrote the fix".into(),
            finish_reason: None,
            last_user_request: Some("run the tests and show output".into()),
            ..empty_round()
        };
        let recent = RecentActionsSummary {
            has_exec: false,
            has_write_or_patch: true,
            count: 2,
        };
        let d = s.decide_no_tools(&ctx, &recent);
        assert!(matches!(d, SteeringDecision::Nudge { .. }));
    }

    // ── Judge verdict tests ──

    #[test]
    fn fulfilled_verdict_accepts_and_clears() {
        let mut s = basic_state();
        let verdict = TurnJudge::Fulfilled {
            note: "All tests pass".into(),
        };
        let d = s.apply_judge_verdict(&verdict, Some("tests pass"), "criteria active");
        assert!(matches!(d, SteeringDecision::Accept { clear_criteria: true }));
    }

    #[test]
    fn stuck_verdict_returns_stuck() {
        let mut s = basic_state();
        let verdict = TurnJudge::Stuck {
            reason: "Repeating same action".into(),
            suggested_guidance: "Try a different approach".into(),
        };
        let d = s.apply_judge_verdict(&verdict, Some("tests pass"), "criteria active");
        assert!(matches!(d, SteeringDecision::Stuck { .. }));
    }

    #[test]
    fn continue_verdict_with_judge_enabled_nudges_unbounded() {
        let mut s = judge_enabled_state();
        let verdict = TurnJudge::Continue {
            suggestion: Some("run the script".into()),
        };
        let d = s.apply_judge_verdict(&verdict, Some("tests pass"), "criteria active");
        match d {
            SteeringDecision::Nudge { nudge_max, .. } => {
                assert_eq!(nudge_max, 999);
            }
            other => panic!("Expected unbounded nudge, got {:?}", other),
        }
    }

    #[test]
    fn continue_verdict_without_judge_uses_budget() {
        let mut s = basic_state();
        let verdict = TurnJudge::Continue {
            suggestion: Some("keep going".into()),
        };
        let d = s.apply_judge_verdict(&verdict, Some("tests pass"), "criteria active");
        assert!(matches!(d, SteeringDecision::Nudge { .. }));
        assert_eq!(s.judge_nudges, 1);
    }

    #[test]
    fn continue_verdict_budget_exhausted_accepts() {
        let mut s = basic_state();
        s.judge_nudges = NUDGE_BUDGET;
        s.text_nudges = MAX_TEXT_NUDGES;
        let verdict = TurnJudge::Continue { suggestion: None };
        let d = s.apply_judge_verdict(&verdict, None, "ambiguous stop");
        assert!(matches!(d, SteeringDecision::Accept { .. }));
    }
}
