# Refactoring Ideas for Raven TUI

## Overview
This document captures refactoring opportunities identified during code review of the raven-tui codebase.

---

## 1. Session/Discovery Logic (`src/session.rs` - 824 lines)

### Current State
The `Session` struct handles both persistence AND repository discovery/indexing in a single file.

### Issues
- Mixed concerns: session management + file system discovery
- Harder to test discovery logic in isolation
- Large file with multiple responsibilities

### Suggestion
Extract the discovery/indexing logic into a separate `RepositoryAnalyzer` module:

```rust
// New: src/repository_analyzer.rs
pub struct RepositoryAnalyzer {
    limits: DiscoveryLimits,
}

impl RepositoryAnalyzer {
    pub fn analyze(&self, workspace: &Path) -> Result<RepoCache> { ... }
}
```

### Benefits
- Cleaner separation of concerns
- Easier to test discovery logic independently
- More maintainable code structure

---

## 2. Agent State Management (`src/agent.rs` - 1234 lines)

### Current State
The `Agent` struct handles:
- Conversation management
- Tool execution
- Session management
- Logging

### Issues
- Too many responsibilities in a single struct
- Mixed concerns make testing difficult
- Hard to mock dependencies for unit tests

### Suggestion
Break out concerns into dedicated components:

```rust
// New: src/tool_executor.rs
pub struct ToolExecutor {
    config: Config,
    workspace: PathBuf,
}

impl ToolExecutor {
    pub async fn execute(&self, tool_name: &str, args: &str) -> Result<String> { ... }
}

// New: src/conversation_history.rs
pub struct ConversationHistory {
    messages: Vec<Message>,
    max_entries: usize,
}

impl ConversationHistory {
    pub fn push(&mut self, message: Message) { ... }
    pub fn truncate(&mut self) { ... }
}
```

### Benefits
- More testable units
- Easier to mock dependencies
- Clearer responsibility boundaries

---

## 3. TUI Application (`src/tui_app.rs` - 2031 lines!)

### Current State
Massive single file with 25+ state fields mixed together in the `App` struct.

### Issues
- Extremely large file (2000+ lines)
- All state in one struct
- Difficult to understand state relationships
- Hard to maintain and modify

### Suggestion
Group related state into sub-structs:

```rust
struct App {
    // Conversation / output
    left_committed: Vec<String>,
    current_response: String,
    
    // Right pane
    trace_lines: Vec<String>,
    current_thinking: String,
    
    // Input (grouped)
    input: InputState,
    
    // Navigation / scroll (grouped)
    navigation: NavigationState,
    
    // Processing state (grouped)
    processing: ProcessingState,
    
    // Approval (grouped)
    approval: ApprovalState,
    
    // Mode menu (grouped)
    mode_menu: ModeMenuState,
    
    // Slash menu (grouped)
    slash_commands: SlashCommandState,
    
    // Display state (grouped)
    display: DisplayState,
    
    // Settings modal (grouped)
    settings: SettingsModal,
    
    // Last draw layout (grouped)
    layout: LayoutState,
    
    // Conversation / trace search (grouped)
    search: SearchState,
    
    // Multi-desktop (grouped)
    desktop: DesktopState,
}
```

### Benefits
- More maintainable
- Clearer state boundaries
- Easier to understand relationships
- Better code organization

---

## 4. Eval Operator Module (`src/eval_operator/`)

### Current State
Multiple submodules (runner, registry, state) with overlapping concerns.

### Issues
- Some overlapping functionality between modules
- Less explicit interfaces between components
- Mixed orchestration and data processing concerns

### Suggestion
Consider:
1. Unifying related functionality where appropriate
2. Making interfaces more explicit between modules
3. Separating orchestration from data processing (e.g., `EvalOrchestrator` vs `EvalMetricsAggregator`)

### Benefits
- Clearer eval execution pipeline
- Better separation of concerns
- Easier to test individual components

---

## 5. Tool System (`src/tools/`)

### Current State
Well-organized module with clear separation (exec, fs, web), but file operations share common patterns.

### Issues
- File operations (read, write, patch, grep) have repetitive error handling
- Some duplicated patterns across tools

### Suggestion
Consider a common trait or helper for file operation error handling:

```rust
// New: src/tools/file_utils.rs
pub trait FileOperation {
    fn with_workspace(&self, workspace: &Path) -> Result<String>;
    fn handle_not_found(&self, path: &Path) -> String;
    // ... common helpers
}
```

### Benefits
- Reduced boilerplate in tool implementations
- Consistent error handling
- Easier to add new file operations

---

## Quick Wins (Low Effort, High Value)

### 1. Extract Injection Block Construction
**Location**: `src/session.rs`
**Effort**: ~30 lines extracted
**Benefit**: Testable in isolation, clearer responsibilities

### 2. Add More Unit Tests for Driving Loop
**Location**: `src/agent_driver.rs`
**Effort**: Medium
**Benefit**: Critical path logic covered, safer refactoring

### 3. Typed Config Builder
**Location**: `src/config.rs`
**Effort**: Medium
**Benefit**: Catch config errors at build time, better IDE support

---

## Priority Recommendations

### High Priority (Impact vs Effort)
1. **TUI App State Grouping** - biggest maintainability win
2. **Injection Block Extraction** - quick testability improvement
3. **Agent State Breakout** - enables better testing

### Medium Priority
4. **Eval Operator Refactoring** - improves eval system clarity
5. **Tool System Helpers** - reduces future boilerplate

### Low Priority (Nice to Have)
6. **Repository Analyzer Extraction** - cleaner separation
7. **Unit Tests for Driving Loop** - safety net for refactoring

---

## Notes
- The `agent_driver.rs` driving loop is well-designed with clear observer patterns
- The eval system has good separation between orchestration and execution
- Session persistence under `~/.raven-hotel/` is well-structured
- The tool system follows good OpenAI function schema conventions
