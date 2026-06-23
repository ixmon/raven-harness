//! In-process integration tests: run `Agent::run_turn()` with mock backends.
//!
//! These exercise the full agent loop (prompt assembly → LLM → tool dispatch → result)
//! without spawning a subprocess, giving rich assertions on internal state.

use raven_tui::agent::Agent;
use raven_tui::chat_backend::ChatBackend;
use raven_tui::config::{Config, ContextBudget};
use raven_tui::eval_smoke::{load_smoke_scenario, mock_backend_for, mock_chat_backend_for, assert_smoke_result};
use raven_tui::tools::ToolBackend;
use std::path::PathBuf;

/// Build a minimal Config suitable for mock eval scenarios.
/// Uses a unique temp directory to prevent session state from accumulating
/// across test runs (Session::init persists to ~/.raven/sessions/<hash>).
fn eval_config(scenario: &raven_tui::eval_smoke::SmokeScenario, tool_backend: ToolBackend) -> Config {
    let ctx_tokens = scenario.context_tokens.unwrap_or(8192);
    let max_rounds = scenario.max_rounds.unwrap_or(10);
    // Create a unique workspace per test invocation so we get a fresh session
    let workspace = std::env::temp_dir()
        .join(format!("raven_smoke_{}_{}", scenario.name, std::process::id()));
    std::fs::create_dir_all(&workspace).ok();

    Config {
        base_url: "http://127.0.0.1:0/v1".into(),
        model: "mock".into(),
        api_key: None,
        workspace,
        temperature: 0.0,
        max_tokens: 1024,
        max_rounds,
        prebuilt_session: None,
        context_budget: ContextBudget::from_context_tokens(ctx_tokens, max_rounds),
        tool_backend,
        tools_enabled: !scenario.disable_tools,
    }
}

#[tokio::test]
async fn mock_tool_loop_runs_in_process() {
    let scenario = load_smoke_scenario("mock_tool_loop").expect("load scenario");
    let tools = ToolBackend::Mock(mock_backend_for(&scenario));
    let chat = ChatBackend::Mock(mock_chat_backend_for(&scenario));
    let config = eval_config(&scenario, tools);

    let mut agent = Agent::new(config, chat);
    let result = agent.run_turn(&scenario.prompt).await.expect("run_turn");

    // Assert on TurnResult (same as the external smoke assertions)
    assert_smoke_result(&scenario, &result).expect("smoke assertions");

    // Assert on Agent internals — the new power of in-process testing
    assert!(agent.conversation_len() > 0, "conversation should have messages");
    assert!(result.metrics.llm_rounds <= 3, "should finish in ≤3 LLM rounds");
}

#[tokio::test]
async fn churn_then_answer_exercises_retry_path() {
    let scenario = load_smoke_scenario("mock_churn_then_answer").expect("load scenario");
    let tools = ToolBackend::Mock(mock_backend_for(&scenario));
    let chat = ChatBackend::Mock(mock_chat_backend_for(&scenario));
    let config = eval_config(&scenario, tools);

    let mut agent = Agent::new(config, chat);
    let result = agent.run_turn(&scenario.prompt).await.expect("run_turn");

    assert_smoke_result(&scenario, &result).expect("smoke assertions");

    // Churn scenario: model retries wrong file 3 times, then finds the right one
    assert!(result.actions.len() >= 4, "should have ≥4 tool calls (3 wrong + 1 right)");
    assert!(result.final_text.contains("8080"), "final text should contain the port number");

    // Context should be reasonable
    let tokens = agent.estimated_context_tokens();
    assert!(tokens < 10000, "context should be well under budget, got {tokens}");
}

#[tokio::test]
async fn huge_grep_truncates_output() {
    let scenario = load_smoke_scenario("mock_huge_grep").expect("load scenario");
    let tools = ToolBackend::Mock(mock_backend_for(&scenario));
    let chat = ChatBackend::Mock(mock_chat_backend_for(&scenario));
    let config = eval_config(&scenario, tools);

    let mut agent = Agent::new(config, chat);
    let result = agent.run_turn(&scenario.prompt).await.expect("run_turn");

    assert_smoke_result(&scenario, &result).expect("smoke assertions");

    // The grep output is 300 lines but context_tokens is 2048 — must truncate
    assert!(result.actions[0].truncated, "grep output should be truncated");
    assert!(
        result.actions[0].output_to_model.len() < result.actions[0].raw_bytes,
        "output to model should be smaller than raw output"
    );
}
