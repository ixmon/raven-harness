use raven_tui::agent::Agent;
use raven_tui::agent_driver;
use raven_tui::chat_backend::ChatBackend;
use raven_tui::config::Config;
use raven_tui::config::ContextBudget;
use raven_tui::eval_smoke::{self, MockLlmToolCall, MockLlmTurn, SmokeExpect, SmokeScenario};
use raven_tui::tools::backend::MockToolBackend;
use raven_tui::tools::ToolBackend;
use serde_json::json;

fn eval_config(
    scenario: &SmokeScenario,
    tool_backend: MockToolBackend,
) -> (Config, std::path::PathBuf) {
    let ctx_tokens = scenario.context_tokens.unwrap_or(8192);
    let max_rounds = scenario.max_rounds.unwrap_or(10);
    // Create a unique workspace per test invocation so we get a fresh session
    let workspace = std::env::temp_dir().join(format!(
        "raven_smoke_{}_{}",
        scenario.name,
        std::process::id()
    ));
    std::fs::create_dir_all(&workspace).ok();

    // Create session
    let session = raven_tui::session::Session::init(&workspace).unwrap();

    let config = Config {
        base_url: "http://127.0.0.1:0/v1".into(),
        model: "mock".into(),
        api_key: Some("dummy-key".into()),
        workspace: workspace.clone(),
        temperature: 0.0,
        max_tokens: 1024,
        max_rounds,
        prebuilt_session: Some(session),
        context_budget: ContextBudget::from_context_tokens(ctx_tokens, max_rounds),
        tool_backend: ToolBackend::Mock(tool_backend),
        tools_enabled: !scenario.disable_tools,
        enable_judge: !scenario.disable_judge,
        flags: raven_tui::runtime::RuntimeFlags::default(),
        harness: raven_tui::runtime::EvalHarness::default(),
        openrouter_reasoning: raven_tui::config::OpenRouterReasoningMode::Auto,
    };
    (config, workspace)
}

#[tokio::test]
async fn test_hello_world() {
    let scenario = eval_smoke::SmokeScenario {
        name: "hello_world".to_string(),
        description: "Test basic hello world scenario".to_string(),
        prompt: "Say hello!".to_string(),
        mock_tools: json!([]),
        llm_turns: vec![MockLlmTurn {
            content: "Hello! How can I help you today?".to_string(),
            tool_calls: vec![],
        }],
        context_tokens: Some(8192),
        max_rounds: Some(5),
        disable_tools: true,
        disable_judge: true,
        max_duration_secs: None,
        expect: SmokeExpect {
            stdout_contains: vec!["Hello".to_string()],
            ..Default::default()
        },
        completion_criteria: None,
    };

    let tool_backend = eval_smoke::mock_backend_for(&scenario);
    let (config, workspace) = eval_config(&scenario, tool_backend);

    let chat_backend = ChatBackend::Mock(eval_smoke::mock_chat_backend_for(&scenario));
    let mut app = Agent::new(config, chat_backend);
    app.reset();

    let mut observer = agent_driver::HeadlessObserver;
    let result = agent_driver::drive_turn(&mut app, &scenario.prompt, &mut observer)
        .await
        .unwrap();

    eval_smoke::assert_smoke_result(&scenario, &result, &workspace).unwrap();
}

#[tokio::test]
async fn test_file_edit() {
    let scenario = eval_smoke::SmokeScenario {
        name: "file_edit".to_string(),
        description: "Test file creation scenario".to_string(),
        prompt: "Create a file called test.txt with content 'hello'".to_string(),
        mock_tools: json!({
            "write": {
                "test.txt": "File created successfully"
            }
        }),
        llm_turns: vec![
            MockLlmTurn {
                content: "".to_string(),
                tool_calls: vec![MockLlmToolCall {
                    name: "write".to_string(),
                    arguments: json!({
                        "path": "test.txt",
                        "content": "hello"
                    }),
                }],
            },
            MockLlmTurn {
                content: "I have created the file test.txt with the content 'hello'.".to_string(),
                tool_calls: vec![],
            },
        ],
        context_tokens: Some(8192),
        max_rounds: Some(5),
        disable_tools: false,
        disable_judge: true,
        max_duration_secs: None,
        expect: SmokeExpect {
            stdout_contains: vec!["created".to_string()],
            ..Default::default()
        },
        completion_criteria: None,
    };

    let tool_backend = eval_smoke::mock_backend_for(&scenario);
    let (config, workspace) = eval_config(&scenario, tool_backend);

    let chat_backend = ChatBackend::Mock(eval_smoke::mock_chat_backend_for(&scenario));
    let mut app = Agent::new(config, chat_backend);
    app.reset();

    let mut observer = agent_driver::HeadlessObserver;
    let result = agent_driver::drive_turn(&mut app, &scenario.prompt, &mut observer)
        .await
        .unwrap();

    eval_smoke::assert_smoke_result(&scenario, &result, &workspace).unwrap();
}
