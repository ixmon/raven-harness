# Raven Hotel - Agent Harness

Raven Harness exists because I wanted a simple, capable way for a model to do real agentic coding work locally — without being forced to use a commercial product whose internals and data practices I don't control.

At the same time, I wanted the same harness to work smoothly with frontier cloud models when a task calls for maximum capability. This gives genuine flexibility: use fast, private, low-cost local inference (via llama.cpp or similar) for everyday work, and reach for the most powerful models only when you actually need them — all through one consistent interface and toolset.

Privacy and security are first-class concerns. Your code never has to leave your machine unless you explicitly decide to use a remote endpoint. The TUI includes a full execution approval system (`/mode`) with four sandbox levels (Babysitter always-ask through Thunderdome yolo) so you stay in control of writes and shell commands. Flexibility is equally important: the system is designed to be pointed at any OpenAI-compatible server, local or remote.

A significant part of making this practical (especially when using paid APIs) is aggressive, intelligent context management. Features like persistent sessions with goal tracking, repo-aware discovery with importance ranking, mtime-matched file summaries, and dynamic context budgeting are not just nice-to-haves — they let the agent stay coherent over long tasks while dramatically reducing token spend.

Finally, the harness itself is deliberately simple and open. It should be something that can be improved — including by AI — rather than a fixed, opaque commercial artifact. If the coding agent can help evolve the very tools it uses, we all benefit.

A focused ratatui-based terminal UI for agentic coding against local (or remote) OpenAI-compatible servers (llama.cpp, etc.).

## Quick start

```bash
cd tui

# Interactive (recommended during development)
cargo run -q --

# Point at your own endpoint + workspace
cargo run -q -- \
  --base-url http://localhost:8080/v1 \
  --model qwen2.5-coder \
  --workspace ~/my-project

# One-shot / non-interactive
cargo run -q -- --prompt "Refactor the error handling in src/main.rs and run cargo check"
```

**Always use `cargo run -q`** for interactive sessions. Plain `cargo run` dumps compiler output before the TUI can take over the alternate screen.

## New & Notable Features

### Context Budget Probing & Automatic Adaptation
At startup the harness probes the model's actual context window (via `/v1/models`) or accepts an explicit `--context-size` / `LLM_CONTEXT_SIZE`.

It then derives smart per-tool limits:
- Maximum bytes per tool result
- Default line limit for `read` operations
- Rough round budget

These limits are printed at startup and used throughout the agent to keep context under control even when the underlying model has a very large (or very small) context.

### Persistent Sessions + Rich Context Injection (`~/.raven-hotel/`)

Each workspace gets a persistent session:

```
~/.raven-hotel/sessions/<session-id>/
├── meta.json        # goal, tests, pitfalls, discoveries, repo_cache, recent_turns_summary, exec_approval_mode
├── full_log.jsonl   # append-only history of turns
└── context.db       # SQLite cache of mtime-matched file summaries
```

On first interactive run you are asked the Cursor-style question:

> Do you trust the code in /path/to/workspace ? [y/N]

When trusted, a **safe, deterministic repo discovery** is performed:
- Walks the tree with hard limits (depth, max entries per dir, file size)
- Skips heavy directories (`.git`, `target`, `node_modules`, ...)
- Ranks files by recency + strong signals (READMEs, manifests, source files)
- Produces a compact tree + "most important files" list + language hint + short summary

This information (plus current goal, known pitfalls, recent discoveries, and a rolling summary of the last ~10 turns) is injected as a compact **"SESSION CONTEXT"** block at the top of every model prompt.

### Execution Approvals & Sandbox Modes (`/mode`)

The TUI gates side-effecting tool calls behind user approval (separate from the initial "trust this workspace?" indexing prompt).

Press `/mode` at any time to bring up a 4-option menu (↑/↓ or j/k to navigate, Enter to select, Esc to cancel). The current mode is pre-selected and the choice is persisted in `meta.json`.

| Mode          | When it asks                                                                 | Persisted? |
|---------------|------------------------------------------------------------------------------|------------|
| **Babysitter**    | Always, for any `write`, `patch`, or `exec` (recommended default)            | No (resets to this on new sessions) |
| **Spring Break**  | Never (yolo for the rest of this run)                                        | No |
| **Vegas**         | Only for `exec` commands that look like they escape the workspace sandbox    | Yes |
| **Thunderdome**   | Never                                                                        | Yes |

**Sandbox detection** (for Vegas) is a simple conservative heuristic on the command string: anything containing `cd /`, `/etc`, `/root`, `curl `, `wget `, `nc `, etc. is treated as "outside".

- Approval shows a compact yellow overlay dialog above the input:
  - `write path/to/file.rs (1234 bytes)`
  - `patch src/main.rs`
  - `exec: cargo test --quiet`
  - (Full file contents or giant blobs are **never** shown in the prompt.)
- Answer with **Y** / **N** / **Esc** (deny). Keys work even while the agent is "processing".
- Denied actions are reported back to the model as tool results so the turn stays coherent; they are never executed.
- Only the actually-approved subset of tool calls proceeds to `execute_and_record_tool_calls`.
- Current mode is logged on turn start and visible via `/status`.

This gives you progressive trust: full guardrails when the agent is exploring or editing unfamiliar code, relaxed operation once you're comfortable inside a project.

The same approval channel and dialog infrastructure also protects `update_goal` in Babysitter mode.

### File Summary Cache (`read_summary` / `store_summary`)

To avoid repeatedly dumping large source files into context, the agent has two dedicated tools:

- `read_summary(path)` — returns a cached summary if the file's mtime has not changed. If the summary is stale or missing, it returns the current mtime + a capped raw view and instructs the agent to analyze it.
- `store_summary(path, mtime, summary)` — lets the agent persist a concise, factual summary for that exact mtime.

The cache lives in the per-session `context.db`. The system prompt strongly encourages the agent to prefer `read_summary` over raw `read` for source exploration.

This is one of the most effective ways small local models stay coherent across long coding sessions.

### Dual-Pane Interface with Autoscroll + Focus

- **Conversation** (left) – committed history + current turn output. Autoscrolls by default.
- **Trace (thinking + tools)** (right) – model reasoning, tool calls, and results. Now also **autoscrolls** by default when new thinking or tool output arrives.

You can manually scroll either pane. When the right pane has focus (or you hold Shift), arrow keys and PageUp/PageDown control the Trace pane. There's visual feedback (colored borders) and a small "flash" effect when you hit the top or bottom while scrolling.

A dedicated background thread keeps keyboard input responsive even while the model is streaming long responses.

Approval dialogs (Babysitter mode etc.) appear as a yellow "Action Approval" overlay above the input line; Y/N/Esc are handled with priority even during streaming.

### Token Usage Visibility

When the backend provides usage information, it is printed into the Trace pane as:

```
📊 tokens: prompt=1234 completion=567
```

Very useful when working close to context limits with local models.

## Tools the agent has

- `exec` – run shell commands (workspace as cwd, 60s timeout)
- `read` / `write` / `patch` (with `near_line`) / `grep` / `list`
- `web_search`
- `browse` (single page or shallow spider)
- **Context tools**:
  - `update_goal(goal, tests?, pitfalls?)`
  - `record_discovery(text)`
  - `read_summary(path)`
  - `store_summary(path, mtime, summary)`

**All mutating tools** (`write`, `patch`, `exec`) are subject to the current `/mode` approval policy (see Execution Approvals above).

The agent is instructed to follow Think → Act (minimal tools) → Report actual results, and strongly prefers `read` then `patch` for edits.

## Flags & Environment Variables

- `--base-url` / `LLM_BASE_URL`
- `--model` / `LLM_MODEL`
- `--workspace` / `WORKSPACE_DIR`
- `--api-key` / `LLM_API_KEY` (also falls back to `OPENROUTER_API_KEY`)
- `--max-rounds`, `--temperature`, `--max-tokens`
- `--context-size` / `LLM_CONTEXT_SIZE` – override the probed context window
- `--prompt "..."` – non-interactive one-shot mode

## Architecture Notes

- The agent instance (conversation history + session) now survives across turns.
- Streaming tool-use continuation is a real loop (handles multiple rounds of tool calls).
- All truncation is UTF-8 safe.
- The heavy context/session logic lives in `session.rs` and is injected fresh on every prompt so the model always sees an up-to-date view.

The harness is intentionally kept as a relatively thin, local-first TUI so the interesting agent behavior and context management can be reused or ported elsewhere.

Enjoy! 🏛️
