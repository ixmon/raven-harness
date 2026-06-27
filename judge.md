# Judge Module Refactoring Plan

## Current State

### `src/agent.rs` (lines 900-1030): `judge_turn` method

The `judge_turn` method implements the judge LLM call that decides whether the agent has completed its task. It:

- Builds a judge prompt from session metadata (goal, achievement tests, completion criteria)
- Calls LLM with the prompt
- Parses response into `TurnJudge` variants:
  - `FULFILLED` - task is complete
  - `CONTINUE` - task not complete, with suggestion for next action
  - `STUCK` - agent is looping, needs user guidance

### `src/agent_driver.rs`: Judge usage

The judge is called in `drive_turn` around lines 355, 461, and 557-600 to make decisions about:
- When completion criteria are defined (nudge-v2)
- On 3rd empty response recovery
- For malformed tool syntax detection

### `src/agent.rs` (lines 78-94): `TurnJudge` enum

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnJudge {
    Fulfilled { note: String },
    Continue { suggestion: Option<String> },
    Stuck {
        reason: String,
        suggested_guidance: String,
    },
}
```

## Proposed Judge Module Structure

### `src/judge.rs` - New module

```rust
// src/judge.rs
use anyhow::Result;
use crate::agent::{TurnJudge, ActionRecord};
use crate::chat_backend::ChatBackend;
use crate::session::SessionMeta;

pub struct Judge {
    client: ChatBackend,
}

impl Judge {
    pub fn new(client: ChatBackend) -> Self {
        Self { client }
    }
    
    /// Judge whether the agent's recent actions satisfy the task criteria.
    /// This is the core inference-based judge that calls an LLM.
    pub async fn judge_turn(
        &self,
        meta: &SessionMeta,
        last_assistant_text: &str,
        recent_actions: &[ActionRecord],
    ) -> TurnJudge {
        // Extracted from Agent::judge_turn
        // Uses meta instead of self.session.as_ref().and_then(|s| &s.meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_build_judge_prompt_basic() {
        // Test prompt construction with minimal inputs
    }
    
    #[test]
    fn test_parse_judge_fulfilled() {
        // Test parsing "FULFILLED\nNote" response
    }
    
    #[test]
    fn test_parse_judge_continue() {
        // Test parsing "CONTINUE\nReason\nSuggestion" response
    }
    
    #[test]
    fn test_parse_judge_stuck() {
        // Test parsing "STUCK\nReason\nSuggestion" response
    }
}
```

### Updated `src/agent.rs`

```rust
// In Agent struct
use crate::judge::Judge;

pub struct Agent {
    // ... existing fields ...
    judge: Option<Judge>,  // New field
}

impl Agent {
    // New method that delegates to Judge
    pub async fn judge_turn(
        &self,
        last_assistant_text: &str,
        recent_actions: &[ActionRecord],
    ) -> TurnJudge {
        if let Some(ref judge) = self.judge {
            if let Some(ref session) = self.session {
                judge.judge_turn(&session.meta, last_assistant_text, recent_actions).await
            } else {
                TurnJudge::Continue { suggestion: None }
            }
        } else {
            TurnJudge::Continue { suggestion: None }
        }
    }
    
    // Keep existing judge_turn as fallback or remove if always using Judge
}
```

### Benefits of this refactoring

1. **Separation of concerns**: Judge logic is isolated from Agent logic
2. **Testability**: Judge can be tested with mock LLM without full Agent
3. **Reusability**: Judge can be used from other places if needed
4. **Clear boundaries**: `TurnJudge` stays in agent (core types), `Judge` is the inference engine
5. **Easier to modify**: Changing judge prompt/behavior doesn't touch Agent

## Testing Strategy

### 1. Unit Tests for Judge Module

```rust
// src/judge.rs
#[cfg(test)]
mod judge_tests {
    use super::*;
    use crate::chat_backend::MockChatBackend;
    use crate::llm::{ChatResponse, Message};
    use serde_json::json;
    
    fn make_mock_judge(responses: Vec<ChatResponse>) -> (Judge, MockChatBackend) {
        let mock = MockChatBackend::new(responses);
        let judge = Judge::new(ChatBackend::Mock(mock.clone()));
        (judge, mock)
    }
    
    #[tokio::test]
    async fn test_judge_fulfilled() {
        let (judge, _) = make_mock_judge(vec![ChatResponse {
            content: Some("FULFILLED\nAll tests pass".to_string()),
            tool_calls: vec![],
            finish_reason: Some("stop".to_string()),
            usage: None,
        }]);
        
        let meta = make_test_meta();
        let result = judge.judge_turn(&meta, "test", &[]).await;
        assert!(matches!(result, TurnJudge::Fulfilled { .. }));
    }
    
    #[tokio::test]
    async fn test_judge_continue_with_suggestion() {
        let (judge, _) = make_mock_judge(vec![ChatResponse {
            content: Some("CONTINUE\nStill working\nRun the script to see output".to_string()),
            tool_calls: vec![],
            finish_reason: Some("stop".to_string()),
            usage: None,
        }]);
        
        let (judge, _) = make_mock_judge(vec![ChatResponse {
            content: Some("FULFILLED\nAll tests pass".to_string()),
            tool_calls: vec![],
            finish_reason: Some("stop".to_string()),
            usage: None,
        }]);
        
        let meta = make_test_meta();
        let result = judge.judge_turn(&meta, "test", &[]).await;
        assert!(matches!(result, TurnJudge::Continue { .. }));
    }
    
    #[tokio::test]
    async fn test_judge_stuck() {
        let (judge, _) = make_mock_judge(vec![ChatResponse {
            content: Some("STUCK\nRepeating same action\nAsk user for guidance".to_string()),
            tool_calls: vec![],
            finish_reason: Some("stop".to_string()),
            usage: None,
        }]);
        
        let meta = make_test_meta();
        let result = judge.judge_turn(&meta, "test", &[]).await;
        assert!(matches!(result, TurnJudge::Stuck { .. }));
    }
    
    #[tokio::test]
    async fn test_judge_no_session() {
        let (judge, _) = make_mock_judge(vec![]);
        let result = judge.judge_turn(&SessionMeta::default(), "test", &[]).await;
        assert!(matches!(result, TurnJudge::Continue { .. }));
    }
}
```

### 2. Integration Tests with Mock LLM

```rust
// tests/agent_smoke.rs (add new scenarios)
#[tokio::test]
async fn test_judge_on_criteria_defined() {
    // Scenario where completion_criteria is set
    // Judge should be consulted and make FULFILLED/CONTINUE decision
    let scenario = load_smoke_scenario("mock_tool_loop").unwrap();
    let backend = mock_chat_backend_for(&scenario);
    let result = run_scenario(&scenario, backend).await.unwrap();
    
    assert_smoke_result(&scenario, &result).unwrap();
}

#[tokio::test]
async fn test_judge_detects_looping() {
    // Scenario where judge should detect looping and return STUCK
    // Use mock LLM to return "STUCK" response
    let mut scenario = load_smoke_scenario("mock_tool_loop").unwrap();
    scenario.llm_turns = vec![
        MockLlmTurn {
            content: "I'll fix this".to_string(),
            tool_calls: vec![],
        },
        // ... more turns that loop
    ];
    // Add judge mock to return STUCK
}
```

### 3. Smoke Scenarios for Judge Behavior

Create JSON scenarios in `evals/scenarios/`:

```json
// evals/scenarios/judge_fulfilled.json
{
  "type": "smoke",
  "name": "judge_fulfilled",
  "prompt": "Write a hello world script",
  "mock_tools": {},
  "llm_turns": [
    {"content": "", "tool_calls": [{"name": "write", "arguments": "{\"path\":\"hello.py\",\"content\":\"print('hello')\"}"}]},
    {"content": "I wrote the file", "tool_calls": []}
  ],
  "expect": {
    "stdout_contains": ["FULFILLED"],
    "tools_used": ["write"]
  }
}
```

```json
// evals/scenarios/judge_continue.json
{
  "type": "smoke",
  "name": "judge_continue",
  "prompt": "Fix the bug in foo.py",
  "mock_tools": {},
  "llm_turns": [
    {"content": "", "tool_calls": [{"name": "read", "arguments": "{\"path\":\"foo.py\"}"}]},
    {"content": "I see the issue", "tool_calls": []}
  ],
  "expect": {
    "stdout_contains": ["CONTINUE"],
    "tools_used": ["read"]
  }
}
```

```json
// evals/scenarios/judge_stuck.json
{
  "type": "smoke",
  "name": "judge_stuck",
  "prompt": "Fix the bug",
  "mock_tools": {},
  "llm_turns": [
    {"content": "", "tool_calls": [{"name": "read", "arguments": "{\"path\":\"foo.py\"}"}]},
    {"content": "Still trying", "tool_calls": []}
  ],
  "expect": {
    "stdout_contains": ["STUCK"],
    "tools_used": ["read"]
  }
}
```

### 4. Property-Based Tests for Prompt Construction

```rust
#[cfg(test)]
mod prompt_tests {
    use super::*;
    use proptest::prelude::*;
    
    proptest! {
        #[test]
        fn test_judge_prompt_contains_goal(goal in "[a-z]+") {
            let meta = SessionMeta {
                current_goal: goal.clone(),
                ..SessionMeta::default()
            };
            let prompt = build_judge_prompt(&meta, "test", &[]);
            assert!(prompt.contains(&goal));
        }
        
        #[test]
        fn test_judge_prompt_contains_recent_actions(actions in prop::collection::vec("[a-z]+", 1..5)) {
            let meta = SessionMeta::default();
            let recent: Vec<ActionRecord> = actions.iter()
                .map(|s| ActionRecord {
                    tool: s.clone(),
                    args: "".to_string(),
                    summary: "".to_string(),
                    output_to_model: "".to_string(),
                    raw_bytes: 0,
                    truncated: false,
                    estimated_tokens: 0,
                })
                .collect();
            let prompt = build_judge_prompt(&meta, "test", &recent);
            for action in &actions {
                assert!(prompt.contains(action));
            }
        }
    }
}
```

### 5. Benchmark Tests for Judge Performance

```rust
#[cfg(test)]
mod benchmarks {
    use super::*;
    use test::Bencher;
    
    #[bench]
    fn bench_judge_turn(b: &mut Bencher) {
        let meta = SessionMeta::default();
        let actions = vec![
            ActionRecord {
                tool: "read".to_string(),
                args: "".to_string(),
                summary: "summary".to_string(),
                output_to_model: "output".to_string(),
                raw_bytes: 100,
                truncated: false,
                estimated_tokens: 10,
            };
            8
        ];
        
        b.iter(|| {
            // This would need a real LLM client or mock
            // judge.judge_turn(&meta, "test", &actions).await
        });
    }
}
```

## Migration Strategy

### Phase 1: Create Judge module with extracted logic

1. Create `src/judge.rs` with `Judge` struct
2. Extract `judge_turn` method from `agent.rs`
3. Keep `Agent::judge_turn` as wrapper that delegates to `Judge`
4. Run `cargo test` to verify no regressions

### Phase 2: Add unit tests for Judge

1. Create unit tests for prompt construction
2. Create tests for response parsing
3. Test edge cases (no session, empty inputs, etc.)

### Phase 3: Update integration tests

1. Add judge-specific smoke scenarios
2. Update existing tests to verify judge behavior
3. Test with mock LLM that returns different judge responses

### Phase 4: Update agent_driver.rs usage

1. Verify `agent.judge_turn()` still works
2. Add tests for judge integration in driver loop
3. Test all three places where judge is called (lines 355, 461, 557-600)

### Phase 5: Add benchmarks and documentation

1. Benchmark judge performance
2. Add comments explaining judge behavior
3. Document when to use judge vs other decision logic

## Verification Checklist

After each phase, verify:
- [ ] All existing `cargo test` pass
- [ ] All `RAVEN_EVAL=1` smoke tests pass
- [ ] Judge-specific unit tests pass
- [ ] Manual test with `cargo run -- --prompt` works
- [ ] Judge is consulted when completion_criteria is set
- [ ] Judge returns correct variants (FULFILLED/CONTINUE/STUCK)

## Rollback Plan

If a refactoring breaks things:
1. Git revert the problematic commit
2. Review the diff
3. Extract smaller changes next time
4. Test more incrementally

## Future Enhancements

1. **Judge cache**: Cache judge results for same inputs
2. **Judge timeout**: Add timeout for judge LLM calls
3. **Judge fallback**: Fallback to simple heuristics if judge fails
4. **Judge metrics**: Track judge accuracy and response times
5. **Judge configuration**: Allow different judge prompts for different scenarios

## Avoiding Logic Loops

**Problem**: When the fix is clear but you keep explaining it without making the edit.

**Symptoms**:
- You find yourself repeating the same analysis 2+ times
- You say "the fix is X" multiple times without making the change
- You're waiting for permission to edit when you should just edit

**Rules to break the loop**:
1. **"Show me the edit" rule**: If the fix is clear, make the edit immediately. Don't ask permission - just do it.
2. **Three-repetition rule**: If you've explained the same fix 3 times, stop and ask: "Should I make this edit now?"
3. **Git safety net**: Remember you can always `git revert` if the edit is wrong. It's safer to make the edit and revert than to keep analyzing.
4. **Small units of work**: Break work into the smallest possible units. Make one small edit, confirm it compiles/works, then move to the next. Don't try to do a whole refactoring in one go.

**This project's specific lesson**:
The bin's `main.rs` had `mod agent;` and `mod judge;` as sibling modules, but these were already defined in the library. This caused type conflicts. The fix was to remove those `mod` declarations and use `use raven_tui::{agent, judge};` instead. I identified this multiple times but never made the edit. The next time this happens, just make the edit immediately.
