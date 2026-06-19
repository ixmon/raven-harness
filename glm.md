# Improvement Ideas for Raven Hotel

A comprehensive code review of the Raven Hotel TUI codebase. No changes have been made yet — this is a catalog of opportunities organized by category.

---

## 1. Architecture & Code Organization

**`tui_app.rs` is a 2097-line monster.** The entire UI state machine, rendering, input handling, agent orchestration, approval flow, settings modal, slash commands, and endpoint switching all live in one function (`run_app`) with deeply nested closures. This is the single biggest improvement opportunity:

- **Extract a `App` struct** holding all the mutable state (`left_committed`, `trace_lines`, scroll positions, focus, approval state, settings state, etc.) instead of ~60 local variables in `run_app`.
- **Split rendering into functions** — `draw_status_bar`, `draw_left_pane`, `draw_right_pane`, `draw_input_bar`, `draw_approval_popup`, `draw_slash_menu`, `draw_mode_menu`, `draw_settings_modal`. Each is currently an inline closure block.
- **Split input handling into a match-dispatch** — the key handling is buried inside `select!` branches with lots of duplication.
- **Extract the settings modal** into its own module — it has its own state machine (browsing, adding, field editing) that's complex enough to warrant isolation.

---

## 2. Security

**`fs::resolve` is a containment check that doesn't actually enforce containment.** Look at the logic: if canonicalization shows the path escapes the workspace, it *falls back to returning the escaped path anyway* (line 21-23: "return candidate"). The `..` check on line 38-40 also has a comment "still return it." This is a real security gap — the "containment" is advisory, not enforced. In Babysitter mode this is mitigated by the approval dialog, but in Spring Break / Thunderdome the agent could write outside the workspace.

**`exec.rs` dangerous-command blocklist is trivially bypassable.** The patterns `["rm -rf /", "mkfs", ":(){ :|:& };:", "dd if=/dev"]` can be evaded with whitespace (`rm  -rf  /`), variable expansion (`rm -rf $ROOT`), or base64 encoding. The blocklist is also tiny. The real protection is the approval system, but the blocklist gives a false sense of safety.

**`browse` accepts invalid certs** (`danger_accept_invalid_certs(true)`). Fine for internal dev sites, but worth making configurable rather than always-on.

---

## 3. Correctness & Robustness

**`exec` has a 60s timeout but doesn't kill the child process.** `tokio::time::timeout` drops the future, but `spawn_blocking`'s `Command::output()` may keep the subprocess alive. Long-running commands (like `cargo build` on a big project) get killed mid-run with no cleanup. Consider using `tokio::process::Command` with `kill_on_drop(true)`.

**`persist_turn` only logs the last message** (line 602), but a single turn can push 3+ messages (assistant + tool calls + tool results). The full log is incomplete for resume purposes.

**`prune_history` summarization can loop.** When `summarize_messages` is called during pruning, it makes an LLM call. If that call fails, it returns `[summarization failed: ...]` which gets stored as the summary — useless. There's no retry or fallback to a simpler truncation.

**`estimated_context_tokens` uses a flat 3.5 bytes/token heuristic** that doesn't account for the system prompt + session injection block, which can be large (the repo cache + recent summary). The status bar's context gauge may undercount significantly.

**`record_tool_denial` pushes the tool call as an "assistant" message** (line 306-311), but `execute_and_record_tool_calls` also pushes assistant messages (line 286-290). This means denied tools get double-counted in some flows — once as the denial, once if the loop re-encounters them.

---

## 4. Error Handling

**Lots of `unwrap_or` / `unwrap_or_default` silently swallowing errors** in critical paths:
- `Session::init` line 147: `serde_json::from_str(&data).unwrap_or_default()` — corrupt meta.json silently resets to empty, losing goal/pitfalls/discoveries.
- `load_recent_conversation`: parse failures are silently skipped with no logging.
- The keystore load in `main.rs` line 126-130: if loading fails, it *immediately tries again* (`unwrap()` on the second call) which can panic.

**`agent.try_lock()` in the draw loop** returns placeholder `"…"` strings when locked, but this means the status bar can flicker between real and placeholder values during processing. A small latched cache of the last-known values would be smoother.

---

## 5. Performance

**The draw loop runs every iteration** even when nothing changed. There's no dirty-flag / damage tracking. With a 10ms input poll, the terminal redraws constantly. A `needs_redraw` flag (set on input or `UiUpdate`) would let idle frames skip the draw.

**`build_repo_cache` walks the entire tree on every trust grant** with no incremental update. For large repos this is slow. Could cache the walk results and only re-rank on subsequent calls.

**`grep_files` reads every file fully into memory** (`fs::read_to_string`) then regex-matches line by line. For large workspaces this is slow and memory-hungry. Streaming reads + `is_match` on buffered lines would be better. Also no `.gitignore` awareness — it greps `target/` contents even though `build_repo_cache` skips them.

---

## 6. UX & Features

**No clipboard support.** `arboard` is in `Cargo.toml` as an optional dependency but never used. Copying agent output or pasting long prompts is painful in a terminal.

**No search within the conversation or trace pane.** When the history gets long, finding a specific earlier message requires scrolling. A `/search` command or Ctrl-F would help.

**No way to edit a submitted prompt.** If you typo a prompt, you have to retype it entirely. Up-arrow history recall (like a shell) would be valuable.

**The trace pane mixes thinking + tool calls + results** with no filtering. A way to toggle "only show tool calls" or "only show thinking" would reduce noise.

**No partial-line rendering during streaming.** The left pane only shows completed lines from `current_response` — mid-stream partial lines aren't visible until a `\n` arrives. This makes streaming feel less live than it could.

**Settings modal has no edit flow for existing endpoints** — you can add and delete, but not edit a URL or model in place. You'd have to delete and re-add.

**No `/help` content beyond the slash command list.** A dedicated help screen showing keybindings (Tab to switch focus, scroll keys, approval keys) would aid discoverability.

---

## 7. Testing

**Almost no tests.** Only one: `session_id_is_stable_for_path` in `session.rs`. The entire tool system, patch logic, line-range parser, context budget math, keystore encryption/decryption, and SSE stream parser are untested. High-value test targets:
- `parse_line_range` — parsing edge cases (`"-40"`, `"10-"`, `"42"`, garbage)
- `patch_file` — single match, multiple match + `near_line`, zero match
- `safe_truncate` — UTF-8 boundary safety
- `ContextBudget::from_context_tokens` — floor/cap behavior
- `Keystore` — encrypt → decrypt round-trip, wrong password rejection
- SSE parsing in `llm::do_stream` — tool call accumulation across chunks

---

## 8. Code Quality / Cleanup

**7 compiler warnings** — unused fields (`args`, `rounds_used`, `stream`, `finish_reason`, `usage`) and unused `settings_editing` variable. Either use them or `#[allow(dead_code)]` intentionally.

**`safe_truncate` is duplicated** in `agent.rs`, `tools/mod.rs`, and inline in `web.rs`. Should be a shared utility.

**`MAX_TOOL_ROUNDS` (12) in `agent.rs` vs `config.max_rounds` (default 10)** — the `run_turn` loop uses `self.config.max_rounds.max(1).min(MAX_TOOL_ROUNDS)`, so the CLI arg is capped at 12 silently. The relationship is unclear.

**Magic numbers everywhere** — `8000` byte exec truncation, `60` grep match limit, `12000` browse char limit, `2 * 1024 * 1024` file read limit, `1800` / `1600` summary char limits, `48` / `6` conversation prune thresholds. These should be named constants.

**`system_message` is a 50-line raw string** embedded in `agent.rs`. It's the system prompt — arguably the most important string in the whole project. Moving it to a separate file (or constant) with a test that asserts it stays under a token budget would be useful.

---

## 9. Documentation

**`docs/headroom-ideas.md` exists** and overlaps with several suggestions here (CCR/reversible compression, `raven learn` mining, smarter history pruning, cache alignment, output steering). Worth cross-referencing when prioritizing.

**No CONTRIBUTING or architecture doc** beyond the README. The session/context-injection system is non-obvious and would benefit from a dedicated write-up.

---

## Top 5 Priorities (suggested order)

1. **Extract `App` struct + split `tui_app.rs`** — biggest maintainability win
2. **Fix `fs::resolve` to actually enforce containment** — real security issue
3. **Add tests for `patch_file`, `parse_line_range`, `ContextBudget`, `Keystore`** — high-value, low-risk
4. **Add a `needs_redraw` flag to skip idle redraws** — easy perf win
5. **Kill child processes on exec timeout** — correctness fix

---

## Cross-reference: Headroom Ideas (`docs/headroom-ideas.md`)

These forward-looking ideas complement the review above and are tracked separately in `docs/headroom-ideas.md`:

- **CCR — Compress-Cache-Retrieve**: reversible compression for tool results with a `retrieve(hash)` tool. Generalizes the existing file summary cache.
- **`raven learn`**: offline mining of `full_log.jsonl` to auto-generate project-specific AGENTS.md / learned patterns.
- **CacheAligner**: prefix stabilization for better KV cache hits on local llama.cpp.
- **Output token reduction**: verbosity steering + effort routing to trim what the model writes back.
- **Specialized compressors**: SmartCrusher (JSON arrays), CodeCompressor (AST-aware), content-type routing.
- **Smarter history pruning**: drop tool call/result pairs atomically, multi-factor importance scoring, CCR-aware markers.
