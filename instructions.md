# Raven TUI Code Review

Comprehensive review of the agent coding harness in [tui/](file:///home/darkstar/cursor/raven-hotel/tui).

---

## Architecture Overview

The project is well-structured for its purpose: a local-first agentic coding assistant that talks to llama.cpp-compatible endpoints. The module decomposition is clean:

| Module | LOC | Purpose |
|--------|-----|---------|
| [main.rs](file:///home/darkstar/cursor/raven-hotel/tui/src/main.rs) | 122 | CLI parsing, session bootstrap, dispatch |
| [agent.rs](file:///home/darkstar/cursor/raven-hotel/tui/src/agent.rs) | 607 | Core agent loop, conversation management, history pruning |
| [session.rs](file:///home/darkstar/cursor/raven-hotel/tui/src/session.rs) | 655 | Persistent session (goal tracking, repo cache, SQLite summary cache) |
| [llm.rs](file:///home/darkstar/cursor/raven-hotel/tui/src/llm.rs) | 382 | OpenAI-compatible HTTP client (streaming + non-streaming) |
| [tui_app.rs](file:///home/darkstar/cursor/raven-hotel/tui/src/tui_app.rs) | 598 | Ratatui dual-pane TUI (conversation + trace) |
| [tools/*](file:///home/darkstar/cursor/raven-hotel/tui/src/tools) | ~580 | exec, fs, web tools |
| [config.rs](file:///home/darkstar/cursor/raven-hotel/tui/src/config.rs) | 20 | Config struct |

The session-injection architecture (goal tracking, repo cache, mtime-matched file summary cache) is genuinely clever — it's exactly the kind of "external working memory" that makes small local models viable for multi-step coding tasks. Good design call.

---

## 🔴 Critical Issues (3)

### 1. Agent state is destroyed every turn in the TUI

> [!CAUTION]
> This is the single biggest bug in the codebase. Every interactive turn creates a **brand new `Agent`**, losing all conversation history.

In [tui_app.rs:441-555](file:///home/darkstar/cursor/raven-hotel/tui/src/tui_app.rs#L441-L555):

```rust
let mut agent_handle = agent; // move into task
// ...
tokio::spawn(async move { /* uses agent_handle */ });
// ...
agent = Agent::new(config.clone()); // Line 555: CREATES A FRESH AGENT
```

The agent is *moved* into the spawned task (necessary for `Send` bounds), and a **new agent** is constructed afterward. This means:
- The model has no memory of prior turns (the `conversation: Vec<Message>` is empty each time)
- Session tools like `update_goal` write to disk but the in-memory session is re-initialized
- The `prune_history` / `summarize_messages` logic never fires because the conversation never grows beyond one turn

**Fix**: Use `Arc<Mutex<Agent>>` (or better, an `mpsc` command channel to own the agent in a dedicated task) so the agent survives across turns. This is the most architecturally impactful change.

```rust
// Sketch of the channel approach:
let (cmd_tx, mut cmd_rx) = mpsc::channel::<AgentCommand>(4);
tokio::spawn(async move {
    let mut agent = Agent::new(config);
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            AgentCommand::Turn { prompt, ui_tx } => {
                // run streaming turn, send UiUpdates via ui_tx
            }
        }
    }
});
```

### 2. Potential panic on non-UTF8 char boundaries in truncation helpers

Multiple places slice strings by byte offset without ensuring they land on a UTF-8 character boundary:

- [agent.rs:604](file:///home/darkstar/cursor/raven-hotel/tui/src/agent.rs#L604): `&s[..LIMIT]`
- [agent.rs:85](file:///home/darkstar/cursor/raven-hotel/tui/src/agent.rs#L85): `&sess.meta.current_goal[..60]`
- [agent.rs:318](file:///home/darkstar/cursor/raven-hotel/tui/src/agent.rs#L318): `combined[..1800].to_string()`
- [agent.rs:531](file:///home/darkstar/cursor/raven-hotel/tui/src/agent.rs#L531): `merged[..1600].to_string()`
- [agent.rs:380](file:///home/darkstar/cursor/raven-hotel/tui/src/agent.rs#L380): `&user_input[..160]`
- [tui_app.rs:595](file:///home/darkstar/cursor/raven-hotel/tui/src/tui_app.rs#L595): `&s[..max]`
- [web.rs:226](file:///home/darkstar/cursor/raven-hotel/tui/src/tools/web.rs#L226): `&text[..12000]`
- [tools/mod.rs:281](file:///home/darkstar/cursor/raven-hotel/tui/src/tools/mod.rs#L281): `&goal[..80]`

Any of these will **panic at runtime** if the byte offset falls inside a multi-byte codepoint (emoji, CJK, accented characters, model-generated Unicode). Since the model's output is not controlled, this *will* happen eventually.

**Fix**: Add a utility function and use it everywhere:

```rust
fn truncate_at_char(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
```

### 3. Spider mode double-fetches every page

In [web.rs:142-162](file:///home/darkstar/cursor/raven-hotel/tui/src/tools/web.rs#L142-L162), `browse()` with `depth > 0` calls `fetch_and_format()` (which does a GET), then **immediately does another GET** to extract links:

```rust
let page = fetch_and_format(&client, &current, extract).await;  // GET #1
// ...
if let Ok(resp) = client.get(&current).send().await {            // GET #2 (same URL!)
```

This wastes bandwidth/time and may return different content if the page is dynamic. The HTML should be fetched once and reused for both extraction and link discovery.

---

## 🟡 Significant Issues (7)

### 4. `exec` tool has no working-directory escape prevention

[exec.rs](file:///home/darkstar/cursor/raven-hotel/tui/src/tools/exec.rs) sets `current_dir` to the workspace but doesn't prevent `cd /` or absolute-path operations. The blocklist is also trivially bypassed (e.g. `rm -r -f /`, spaces in `r m`, etc.). For a local tool this is acceptable but worth documenting as a conscious choice. Consider at minimum also blocking `sudo`, `su`, `chmod -R 777`, etc.

### 5. `MAX_TOOL_ROUNDS` vs `config.max_rounds` double-cap is confusing

[agent.rs:82](file:///home/darkstar/cursor/raven-hotel/tui/src/agent.rs#L82):
```rust
for round in 0..self.config.max_rounds.max(1).min(MAX_TOOL_ROUNDS)
```
The hardcoded `MAX_TOOL_ROUNDS = 12` silently overrides `--max-rounds 20`. Either remove the hardcoded cap or document/warn about it.

### 6. Streaming continuation only goes one level deep

[tui_app.rs:505-536](file:///home/darkstar/cursor/raven-hotel/tui/src/tui_app.rs#L505-L536): After executing tool calls from the first streaming response, the code calls `continue_turn_streaming()` **once**. If the model issues *another* round of tool calls in that continuation, they are silently ignored:

```rust
StreamChunk::Done { content, tool_calls: _ } => {
    // Nested tools from continuation are not handled here
}
```

A real agentic loop needs to keep continuing until the model stops calling tools (respecting `max_rounds`). This should be a `loop` with a round counter.

### 7. `persist_turn` triggers summarization mid-tool-execution

[agent.rs:522-536](file:///home/darkstar/cursor/raven-hotel/tui/src/agent.rs#L522-L536): Every 4th message, `persist_turn` fires an LLM summarization call. This happens *during* tool execution rounds, meaning the agent is doing an extra inference call (at `temperature=0.3`) to summarize the last 4 messages — slowing down the actual work. This should only run between turns, not within a tool-use loop.

### 8. `resolve()` in `fs.rs` doesn't actually prevent workspace escape

[fs.rs:7-20](file:///home/darkstar/cursor/raven-hotel/tui/src/tools/fs.rs#L7-L20):
```rust
if p.is_absolute() {
    if let Ok(stripped) = p.strip_prefix(workspace) {
        workspace.join(stripped)
    } else {
        p.to_path_buf()  // ← allows reading ANY absolute path!
    }
}
```
The comment says "basic containment" but the code allows any absolute path through. Relative paths with `../../../etc/passwd` also work because `workspace.join("../../x")` doesn't canonicalize. The function should either enforce containment strictly or not pretend to.

### 9. `session_id` hash might collide on similar paths

[session.rs:420](file:///home/darkstar/cursor/raven-hotel/tui/src/session.rs#L420): The FNV hash is truncated to 8 hex chars (32 bits), giving only ~4 billion possible IDs. With `canonicalize()` failing (the `unwrap_or_else` path), two different non-existent paths could collide. This is unlikely but worth noting.

### 10. `main.rs` creates a **second** session that's never connected to the TUI's agent

[main.rs:65](file:///home/darkstar/cursor/raven-hotel/tui/src/main.rs#L65): `Session::init()` is called, the trust prompt runs, and the repo cache is built — but the resulting session is never passed to the TUI. Then [agent.rs:60](file:///home/darkstar/cursor/raven-hotel/tui/src/agent.rs#L60) creates a *second* session inside `Agent::new()`. The first session's trust decision and repo cache are wasted if the workspace doesn't already have a persisted `meta.json`. The main session should be threaded through to the Agent.

---

## 🟢 Improvement Suggestions (12)

### 11. Add `exit_code` to exec output

The model can't distinguish "command succeeded with no output" from "command failed silently". Return the exit code:

```rust
format!("[exit {}] {}", o.status.code().unwrap_or(-1), result)
```

### 12. Show token usage / timing in the trace pane

The `Usage` struct in `ChatResponse` is parsed but never displayed. Show prompt/completion token counts and wall-clock time per inference in the trace pane — invaluable for debugging context-length issues with local models.

### 13. The non-streaming `run_turn` is unused

[agent.rs:75-186](file:///home/darkstar/cursor/raven-hotel/tui/src/agent.rs#L75-L186): `run_turn()` has a complete multi-round tool loop, but the TUI exclusively uses `run_turn_streaming()` + `continue_turn_streaming()`. The `--prompt` non-interactive mode calls `run_turn()`, but it doesn't benefit from streaming. Consider either:
- Making `--prompt` also use the streaming path (for consistency and proper multi-round tool use), or
- Removing `run_turn()` and consolidating into one code path.

Having two parallel agent loops is a maintenance hazard — bugs fixed in one won't be fixed in the other.

### 14. Tool schema is cloned every round

[agent.rs:80](file:///home/darkstar/cursor/raven-hotel/tui/src/agent.rs#L80): `tools::all_tools()` allocates a fresh `Vec<ToolDef>` with all the JSON schemas on every loop iteration. This is cheap enough to not matter, but `lazy_static` / `OnceCell` would be cleaner.

### 15. Add `Ctrl-D` (EOF) as a quit shortcut

Standard terminal UX. Currently only `Ctrl-C` works.

### 16. Input should support multiline (Shift+Enter or paste)

Currently, Enter always sends. For coding prompts that contain code snippets, multi-line input is essential. Consider a toggle or Shift+Enter for newlines.

### 17. Scrollbar code is duplicated

[tui_app.rs:182-203](file:///home/darkstar/cursor/raven-hotel/tui/src/tui_app.rs#L182-L203) and [tui_app.rs:240-261](file:///home/darkstar/cursor/raven-hotel/tui/src/tui_app.rs#L240-L261) are identical scrollbar rendering logic. Extract into a helper:

```rust
fn render_scrollbar(buf: &mut Buffer, area: Rect, scroll: u16, total_lines: u16) { ... }
```

### 18. DuckDuckGo URL extraction misses the actual href

[web.rs:66-70](file:///home/darkstar/cursor/raven-hotel/tui/src/tools/web.rs#L66-L70): DuckDuckGo's `.result__url` is a `<span>` with display text, not an `<a>` with an `href` attribute. The actual link is on `.result__a` (which you already parse for titles). So `url` will always be empty. You should extract the href from `title_sel` instead:

```rust
let url = result
    .select(&title_sel)
    .next()
    .and_then(|e| e.value().attr("href"))
    .map(|h| resolve_ddg_redirect(h))  // DDG wraps in //duckduckgo.com/l/?uddg=...
    .unwrap_or_default();
```

### 19. Consider `tokio::process::Command` instead of `spawn_blocking` + `std::process::Command`

[exec.rs:23-45](file:///home/darkstar/cursor/raven-hotel/tui/src/tools/exec.rs#L23-L45): Tokio has its own async `Command` which avoids tying up a blocking thread. You're already using tokio's timeout correctly around it, but the native async version is cleaner and supports streaming stdout/stderr for long-running commands (which would let you show live `cargo build` output).

### 20. `Regex::new` is called on every `grep` and every `extract_text`

- [fs.rs:185](file:///home/darkstar/cursor/raven-hotel/tui/src/tools/fs.rs#L185): Pattern compilation on every grep call
- [web.rs:214](file:///home/darkstar/cursor/raven-hotel/tui/src/tools/web.rs#L214): `\s+` regex compiled on every page extraction

Not a performance problem now, but low-hanging fruit for `once_cell::sync::Lazy`.

### 21. Add `--session-dir` flag

Currently hardcoded to `~/.raven-hotel/`. For testing or running multiple isolated harness instances, an override would be useful.

### 22. Consider structured logging to a file

The TUI captures the alternate screen, so `eprintln!` output is invisible during interactive use. A `tracing` subscriber writing to `~/.raven-hotel/sessions/{id}/debug.log` would make debugging much easier without interfering with the TUI.

---

## Summary: Priority Actions

| Priority | Issue | Effort |
|----------|-------|--------|
| 🔴 P0 | **#1**: Fix agent-per-turn state loss (Arc+Mutex or channel) | Medium |
| 🔴 P0 | **#2**: Fix char-boundary panics in all truncation sites | Small |
| 🔴 P0 | **#3**: Fix spider double-fetch | Small |
| 🟡 P1 | **#6**: Multi-round tool loop in streaming path | Medium |
| 🟡 P1 | **#10**: Thread session from main→Agent | Small |
| 🟡 P1 | **#18**: Fix DuckDuckGo URL extraction | Small |
| 🟢 P2 | **#11**: Add exit code to exec output | Trivial |
| 🟢 P2 | **#13**: Consolidate agent loop code paths | Medium |
| 🟢 P2 | **#17**: Extract scrollbar helper | Trivial |
| 🟢 P2 | **#12**: Show token usage in trace | Small |

Overall the codebase is solid for a first working version. The session/injection architecture is genuinely good — once the agent-state-per-turn bug is fixed, it'll be a capable harness. The code is readable and well-commented in the tricky areas. Nice work.
