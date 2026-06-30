//! Super Judge — post-turn adversarial reviewer for "work" mode.
//!
//! When `agent_mode == "work"`, the Super Judge activates after the main
//! agent turn completes.  It reviews the agent's work from an external
//! reviewer perspective, optionally runs verification tools, and either:
//! - Declares the work complete
//! - Injects feedback to nudge the agent to continue
//! - Detects death spirals and injects anti-spiral guidance
//!
//! The Super Judge uses the same `drive_turn()` machinery with a
//! [`SuperJudgeObserver`] that auto-approves read/exec tools but
//! prevents further writes.

use crate::agent::{ActionRecord, Agent};
use crate::agent_driver::{self, TurnObserver};
use crate::llm::ToolCall;
use async_trait::async_trait;

/// Result of a Super Judge review cycle.
#[derive(Debug, Clone)]
pub enum SuperJudgeVerdict {
    /// Work appears complete; no further action needed.
    Complete { note: String },
    /// Agent should continue working; feedback injected as user message.
    Continue { feedback: String },
    /// Death spiral detected; anti-spiral guidance injected.
    DeathSpiral { feedback: String },
    /// Super Judge encountered an error; skip gracefully.
    Skipped { reason: String },
}

/// Build the Super Judge review prompt that gets injected as a user message.
///
/// This prompt adopts an adversarial external reviewer persona — the Super
/// Judge should verify claims, not trust the agent's self-assessment.
pub fn build_review_prompt(
    agent: &Agent,
    last_assistant_text: &str,
    recent_actions: &[ActionRecord],
) -> String {
    let goal = agent
        .session
        .as_ref()
        .map(|s| s.meta.current_goal.as_str())
        .unwrap_or("(no goal set)");

    let criteria = agent
        .session
        .as_ref()
        .and_then(|s| s.meta.completion_criteria.as_deref())
        .unwrap_or("(no completion criteria defined)");

    let original_request = agent
        .session
        .as_ref()
        .and_then(|s| s.meta.last_user_request.as_deref())
        .unwrap_or("(unknown)");

    // Build compact recent activity summary
    let activity: String = recent_actions
        .iter()
        .rev()
        .take(12)
        .map(|a| {
            if a.tool == "exec" {
                let lines: Vec<&str> = a.output_to_model.lines().take(20).collect();
                let preview = if lines.is_empty() {
                    "(no output)".to_string()
                } else {
                    lines.join("\n")
                };
                format!("• {} →\n{}", a.tool, preview)
            } else {
                format!("• {} → {}", a.tool, truncate(&a.summary, 200))
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Detect potential death spiral indicators
    let spiral_indicators = detect_spiral_indicators(recent_actions);

    let mut prompt = String::with_capacity(4096);
    prompt.push_str("🔍 SUPER JUDGE REVIEW\n\n");
    prompt.push_str("You are an external code reviewer, NOT the agent that did this work. ");
    prompt.push_str("Someone wrote this code — you don't know if it's good. ");
    prompt.push_str("Be critical. Verify claims with real tool use (read files, exec tests). ");
    prompt.push_str("Stop on the first real error you find.\n\n");

    prompt.push_str(&format!("## Original request\n{}\n\n", original_request));
    prompt.push_str(&format!("## Goal\n{}\n\n", goal));
    prompt.push_str(&format!("## Completion criteria\n{}\n\n", criteria));

    if !activity.is_empty() {
        prompt.push_str(&format!(
            "## Recent agent actions (most recent last)\n{}\n\n",
            activity
        ));
    }

    if !last_assistant_text.trim().is_empty() {
        prompt.push_str(&format!(
            "## Agent's final message\n{}\n\n",
            truncate(last_assistant_text, 500)
        ));
    }

    if !spiral_indicators.is_empty() {
        prompt.push_str("## ⚠ Potential issues detected\n");
        for indicator in &spiral_indicators {
            prompt.push_str(&format!("- {}\n", indicator));
        }
        prompt.push_str("\n");
    }

    prompt.push_str("## Your task\n");
    prompt.push_str("1. Read the key files that were modified to verify the changes are correct.\n");
    prompt.push_str("2. If the request involves running something, use `exec` to verify.\n");
    prompt.push_str("3. Stop on the FIRST real error.\n\n");
    prompt.push_str("## Your decision (final message must contain EXACTLY ONE of these):\n");
    prompt.push_str("- **WORK_COMPLETE**: The work genuinely satisfies the request. Say exactly: \"WORK_COMPLETE: <brief reason>\"\n");
    prompt.push_str("- **NEEDS_WORK**: Something is wrong or missing. Say exactly: \"NEEDS_WORK: <what's wrong and what to do next>\"\n");
    prompt.push_str("- **DEATH_SPIRAL**: The agent is repeating the same failing pattern. Say exactly: \"DEATH_SPIRAL: <pattern detected> | <suggested different approach>\"\n");

    prompt
}

/// Detect patterns that suggest a death spiral.
pub fn detect_spiral_indicators(recent_actions: &[ActionRecord]) -> Vec<String> {
    let mut indicators = vec![];

    // Check for repeated identical tool calls
    let last_8: Vec<_> = recent_actions.iter().rev().take(8).collect();
    let tool_sequence: Vec<&str> = last_8.iter().map(|a| a.tool.as_str()).collect();

    // Same tool called 4+ times in a row
    if tool_sequence.len() >= 4 {
        let first = tool_sequence[0];
        if tool_sequence[..4].iter().all(|t| *t == first) {
            indicators.push(format!(
                "Same tool '{}' called {} times consecutively",
                first,
                tool_sequence.iter().take_while(|t| **t == first).count()
            ));
        }
    }

    // Check for repeated patch/write to same file
    let write_paths: Vec<String> = last_8
        .iter()
        .filter(|a| a.tool == "write" || a.tool == "patch")
        .filter_map(|a| {
            serde_json::from_str::<serde_json::Value>(&a.args)
                .ok()
                .and_then(|v| v.get("path")?.as_str().map(|s| s.to_string()))
        })
        .collect();

    if write_paths.len() >= 3 {
        let first_target = &write_paths[0];
        let same_count = write_paths.iter().filter(|t| t == &first_target).count();
        if same_count >= 3 {
            indicators.push(format!(
                "Same file '{}' written/patched {} times in recent actions",
                first_target, same_count
            ));
        }
    }

    // Check for exec failures followed by identical retry
    let exec_outputs: Vec<&str> = last_8
        .iter()
        .filter(|a| a.tool == "exec")
        .map(|a| a.output_to_model.as_str())
        .collect();
    if exec_outputs.len() >= 2
        && exec_outputs[0] == exec_outputs[1]
        && exec_outputs[0].to_lowercase().contains("error")
    {
        indicators.push("Repeated exec producing identical error output".to_string());
    }

    indicators
}

/// Run the Super Judge review cycle using a headless observer.
///
/// This is the simplest integration: builds the review prompt, calls
/// `drive_turn` with a `SuperJudgeObserver`, and parses the verdict.
///
/// For TUI integration, the caller should construct a custom observer
/// that routes events to the UI update channel.
pub async fn run_super_judge(
    agent: &mut Agent,
    last_text: &str,
    recent_actions: &[ActionRecord],
) -> SuperJudgeVerdict {
    // Build the review prompt
    let review_prompt = build_review_prompt(agent, last_text, recent_actions);

    // Log the event
    agent.log_harness_event("super_judge", "Super Judge review started");

    // Create a Super Judge observer
    let mut observer = SuperJudgeObserver::new();

    // Run the Super Judge's own mini-turn
    let result = agent_driver::drive_turn(agent, &review_prompt, &mut observer).await;

    match result {
        Ok(turn_result) => {
            let response = turn_result.final_text.trim().to_string();
            agent.log_harness_event(
                "super_judge",
                &format!("Super Judge verdict: {}", truncate(&response, 200)),
            );
            parse_verdict(&response)
        }
        Err(e) => {
            let reason = format!("Super Judge inference error: {}", e);
            agent.log_harness_event("super_judge_error", &reason);
            SuperJudgeVerdict::Skipped { reason }
        }
    }
}

/// Run the Super Judge with a custom TurnObserver (for TUI integration).
///
/// The caller provides an observer that routes events to the UI.
pub async fn run_super_judge_with_observer(
    agent: &mut Agent,
    last_text: &str,
    recent_actions: &[ActionRecord],
    observer: &mut dyn TurnObserver,
) -> SuperJudgeVerdict {
    let review_prompt = build_review_prompt(agent, last_text, recent_actions);
    agent.log_harness_event("super_judge", "Super Judge review started (TUI)");

    let result = agent_driver::drive_turn(agent, &review_prompt, observer).await;

    match result {
        Ok(turn_result) => {
            let response = turn_result.final_text.trim().to_string();
            agent.log_harness_event(
                "super_judge",
                &format!("Super Judge verdict: {}", truncate(&response, 200)),
            );
            parse_verdict(&response)
        }
        Err(e) => {
            let reason = format!("Super Judge inference error: {}", e);
            agent.log_harness_event("super_judge_error", &reason);
            SuperJudgeVerdict::Skipped { reason }
        }
    }
}

/// Parse the Super Judge's response into a structured verdict.
pub fn parse_verdict(response: &str) -> SuperJudgeVerdict {
    let upper = response.to_uppercase();

    if upper.contains("WORK_COMPLETE") {
        let note = response
            .split("WORK_COMPLETE")
            .nth(1)
            .map(|s| s.trim_start_matches(':').trim().to_string())
            .unwrap_or_else(|| "Work appears complete".to_string());
        SuperJudgeVerdict::Complete { note }
    } else if upper.contains("DEATH_SPIRAL") {
        let feedback = response
            .split("DEATH_SPIRAL")
            .nth(1)
            .map(|s| s.trim_start_matches(':').trim().to_string())
            .unwrap_or_else(|| {
                "Agent appears to be in a death spiral. Try a completely different approach."
                    .to_string()
            });
        SuperJudgeVerdict::DeathSpiral { feedback }
    } else if upper.contains("NEEDS_WORK") {
        let feedback = response
            .split("NEEDS_WORK")
            .nth(1)
            .map(|s| s.trim_start_matches(':').trim().to_string())
            .unwrap_or_else(|| "Continue working on the task.".to_string());
        SuperJudgeVerdict::Continue { feedback }
    } else {
        // Default: treat ambiguous response as "continue"
        SuperJudgeVerdict::Continue {
            feedback: format!(
                "[Super Judge review (ambiguous verdict)]: {}",
                truncate(response, 300)
            ),
        }
    }
}

// ── Super Judge Observer ─────────────────────────────────────────────────────

/// Headless observer for the Super Judge's mini-turn.
///
/// - Auto-approves read tools (list, read, read_summary, grep)
/// - Auto-approves exec (for verification)
/// - Denies write/patch (Super Judge shouldn't modify code)
/// - Logs to stderr with 🔍 prefix
pub struct SuperJudgeObserver;

impl SuperJudgeObserver {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl TurnObserver for SuperJudgeObserver {
    fn on_token(&mut self, _t: &str) {
        // Silent in headless mode (TUI variant uses custom observer)
    }

    fn on_thinking(&mut self, t: &str) {
        if !t.is_empty() {
            eprint!("🔍 {}", t);
        }
    }

    fn on_tool_start(&mut self, name: &str, _args: &str) {
        eprint!("  🔍 {} → ", name);
    }

    fn on_tool_result(&mut self, record: &ActionRecord) {
        eprintln!("🔍 {}", record.summary.lines().next().unwrap_or(""));
    }

    async fn approve_tool(&mut self, tc: &ToolCall) -> bool {
        let name = &tc.function.name;
        // Allow wiki writes (private scratchpad)
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) {
            if v.get("wiki").and_then(|w| w.as_bool()).unwrap_or(false) {
                return true;
            }
        }
        // Allow read-only tools + exec (for verification)
        // Deny workspace write/patch (Super Judge is a reviewer, not an editor)
        match name.as_str() {
            "read" | "read_summary" | "list" | "grep" | "exec" => true,
            "write" | "patch" => {
                eprintln!("  🔍 DENIED: Super Judge cannot write/patch workspace files");
                false
            }
            _ => true,
        }
    }

    fn on_nudge(&mut self, count: u32, max: u32) {
        eprintln!("  🔍 [nudge {}/{}]", count, max);
    }

    fn on_stuck(&mut self, reason: &str, _suggested: &str) {
        eprintln!("  🔍 [stuck: {}]", reason);
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_work_complete() {
        let response = "After reviewing the code changes and running tests:\nWORK_COMPLETE: All tests pass and the bug is fixed correctly.";
        match parse_verdict(response) {
            SuperJudgeVerdict::Complete { note } => {
                assert!(note.contains("All tests pass"));
            }
            other => panic!("Expected Complete, got {:?}", other),
        }
    }

    #[test]
    fn parse_needs_work() {
        let response = "The fix addresses the wrong function.\nNEEDS_WORK: The patch modifies foo() but the bug is in bar(). Read bar.py and patch that instead.";
        match parse_verdict(response) {
            SuperJudgeVerdict::Continue { feedback } => {
                assert!(feedback.contains("bar()"));
            }
            other => panic!("Expected Continue, got {:?}", other),
        }
    }

    #[test]
    fn parse_death_spiral() {
        let response = "DEATH_SPIRAL: Agent has patched schema.py 4 times with the same incorrect fix | Try reading the test file first to understand what's expected";
        match parse_verdict(response) {
            SuperJudgeVerdict::DeathSpiral { feedback } => {
                assert!(feedback.contains("schema.py"));
            }
            other => panic!("Expected DeathSpiral, got {:?}", other),
        }
    }

    #[test]
    fn parse_ambiguous_defaults_to_continue() {
        let response = "I checked the code and it looks mostly fine but I'm not sure about edge cases.";
        assert!(matches!(
            parse_verdict(response),
            SuperJudgeVerdict::Continue { .. }
        ));
    }

    #[test]
    fn spiral_detection_repeated_tool() {
        let actions: Vec<ActionRecord> = (0..5)
            .map(|i| ActionRecord {
                tool: "exec".into(),
                args: "{}".into(),
                summary: format!("exec {}", i),
                output_to_model: "error: test failed".into(),
                raw_bytes: 0,
                truncated: false,
                estimated_tokens: 0,
            })
            .collect();
        let indicators = detect_spiral_indicators(&actions);
        assert!(
            !indicators.is_empty(),
            "Should detect repeated exec pattern"
        );
    }

    #[test]
    fn spiral_detection_no_spiral() {
        let actions = vec![
            ActionRecord {
                tool: "read".into(),
                args: "{}".into(),
                summary: "read file".into(),
                output_to_model: "content".into(),
                raw_bytes: 0,
                truncated: false,
                estimated_tokens: 0,
            },
            ActionRecord {
                tool: "patch".into(),
                args: "{}".into(),
                summary: "patched file".into(),
                output_to_model: "ok".into(),
                raw_bytes: 0,
                truncated: false,
                estimated_tokens: 0,
            },
            ActionRecord {
                tool: "exec".into(),
                args: "{}".into(),
                summary: "ran tests".into(),
                output_to_model: "all pass".into(),
                raw_bytes: 0,
                truncated: false,
                estimated_tokens: 0,
            },
        ];
        let indicators = detect_spiral_indicators(&actions);
        assert!(indicators.is_empty(), "Should not detect spiral for normal workflow");
    }

    #[test]
    fn spiral_detection_repeated_writes() {
        let actions: Vec<ActionRecord> = (0..4)
            .map(|i| ActionRecord {
                tool: "patch".into(),
                args: r#"{"path": "src/main.rs"}"#.into(),
                summary: format!("patched main.rs attempt {}", i),
                output_to_model: "ok".into(),
                raw_bytes: 0,
                truncated: false,
                estimated_tokens: 0,
            })
            .collect();
        let indicators = detect_spiral_indicators(&actions);
        assert!(
            indicators.iter().any(|i| i.contains("src/main.rs")),
            "Should detect repeated writes to same file: {:?}",
            indicators
        );
    }
}
