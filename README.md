# Raven Hotel - Agent Harness

Raven Harness exists because I wanted a simple, capable way for a model to do real agentic coding work locally — without being forced to use a commercial product whose internals and data practices I don't control.

At the same time, I wanted the same harness to work smoothly with frontier cloud models when a task calls for maximum capability. This gives genuine flexibility: use fast, private, low-cost local inference (via llama.cpp or similar) for everyday work, and reach for the most powerful models only when you actually need them — all through one consistent interface and toolset.

Privacy and security are first-class concerns. Your code never has to leave your machine unless you explicitly decide to use a remote endpoint. API keys are encrypted at rest with AES-256-GCM (Argon2id key derivation). The TUI includes a full execution approval system (`/mode`) with four sandbox levels (Babysitter always-ask through Thunderdome yolo) so you stay in control of writes and shell commands. File operations enforce workspace containment — the agent cannot read or write outside the project directory.

A significant part of making this practical (especially when using paid APIs) is aggressive, intelligent context management. Features like persistent sessions with goal tracking, repo-aware discovery with importance ranking, mtime-matched file summaries, and dynamic context budgeting are not just nice-to-haves — they let the agent stay coherent over long tasks while dramatically reducing token spend.

Finally, the harness itself is deliberately simple and open. It should be something that can be improved — including by AI — rather than a fixed, opaque commercial artifact. If the coding agent can help evolve the very tools it uses, we all benefit.

A focused ratatui-based terminal UI for agentic coding against local (or remote) OpenAI-compatible servers (llama.cpp, OpenRouter, etc.).

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

# Skip interactive vault password prompt (for scripting/CI)
RAVEN_VAULT_PASSWORD=mypassword cargo run -q --
```

**Always use `cargo run -q`** for interactive sessions. Plain `cargo run` dumps compiler output before the TUI can take over the alternate screen.

## Features

### Multi-Endpoint Management (`/settings`)

Hot-swap between inference endpoints without restarting. The `/settings` command opens a modal overlay where you can:

- **Browse** saved endpoints with `Up/Down`
- **Switch** the active endpoint with `Enter` (context window is re-probed automatically)
- **Add** new endpoints with `A` (guided wizard for label, URL, model, API key)
- **Edit** existing endpoints with `E`
- **Delete** endpoints with `D`

Endpoints are persisted in `~/.raven-hotel/endpoints.json`. API keys are encrypted with AES-256-GCM using an Argon2id-derived key. On first use with an API key, you set a vault password; on subsequent launches you unlock the vault at startup.

**OpenRouter support** is built in — openrouter.ai URLs are auto-detected and the correct reasoning parameters and attribution headers are injected.

### Context Budget Probing & Automatic Adaptation

At startup the harness probes the model's actual context window (via `/v1/models`) or accepts an explicit `--context-size` / `LLM_CONTEXT_SIZE`.

It then derives smart per-tool limits:
- Maximum bytes per tool result
- Default line limit for `read` operations
- Rough round budget

These limits are printed at startup and used throughout the agent to keep context under control. The status bar shows live context usage (tokens used / total), and the estimate includes the full prompt (system message + session injection + conversation history).

### Persistent Sessions + Rich Context Injection (`~/.raven-hotel/`)

Each workspace gets a persistent session:

```
~/.raven-hotel/sessions/<session-id>/
├── meta.json        # goal, tests, pitfalls, discoveries, repo_cache, recent_turns_summary, exec_approval_mode
├── full_log.jsonl   # append-only history of all messages (assistant + tool calls + results)
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

Press `/mode` at any time to bring up a 4-option menu. The current mode is pre-selected and the choice is persisted in `meta.json`.

| Mode          | When it asks                                                                 | Persisted? |
|---------------|------------------------------------------------------------------------------|------------|
| **Babysitter**    | Always, for any `write`, `patch`, or `exec` (recommended default)            | No (resets to this on new sessions) |
| **Spring Break**  | Never (yolo for the rest of this run)                                        | No |
| **Vegas**         | Only for `exec` commands that look like they escape the workspace sandbox    | Yes |
| **Thunderdome**   | Never                                                                        | Yes |

Approval shows a compact yellow overlay dialog above the input with a summary of the action. Answer with **Y** / **N** / **Esc** (deny). Keys work even while the agent is streaming. Denied actions are reported back to the model so the turn stays coherent.

### Workspace Containment

File operations (`read`, `write`, `patch`, `grep`, `list`) enforce containment via path canonicalization. Attempts to escape the workspace (e.g., `../../etc/passwd`) are rejected with an error — the agent cannot access files outside the project directory. This is enforced regardless of the current `/mode` setting.

Shell commands (`exec`) run with the workspace as cwd and a 60-second timeout. Child processes are killed automatically when the timeout expires (`kill_on_drop`).

### File Summary Cache (`read_summary` / `store_summary`)

To avoid repeatedly dumping large source files into context, the agent has two dedicated tools:

- `read_summary(path)` — returns a cached summary if the file's mtime has not changed.
- `store_summary(path, mtime, summary)` — persists a concise summary for that exact mtime.

The cache lives in the per-session `context.db`. This is one of the most effective ways small local models stay coherent across long coding sessions.

### Dual-Pane Interface

- **Conversation** (left) – committed history + current turn output
- **Trace** (right) – model reasoning, tool calls, and results

Both panes autoscroll by default when new content arrives. Tab switches focus; arrow keys and PageUp/PageDown scroll the focused pane. A visual flash indicates when you hit the scroll boundary.

### Search (`/search` or Ctrl-F)

Search within either pane with `/search <query>`. Matches are highlighted and you can jump between them with `n`/`N` (or Ctrl-N/Ctrl-P). The search targets whichever pane has focus.

### Input History

Press Ctrl+Up / Ctrl+Down to recall previous prompts, shell-style.

### Clipboard

Agent output is automatically copied to the system clipboard when a turn completes (requires the `clipboard` feature, enabled by default).

## Tools the agent has

- `exec` – run shell commands (workspace as cwd, 60s timeout, kill on timeout)
- `read` / `write` / `patch` (with `near_line`) / `grep` / `list`
- `web_search`
- `browse` (single page or shallow spider)
- **Context tools**:
  - `update_goal(goal, tests?, pitfalls?)`
  - `record_discovery(text)`
  - `read_summary(path)`
  - `store_summary(path, mtime, summary)`

**All mutating tools** (`write`, `patch`, `exec`) are subject to the current `/mode` approval policy.

The `grep` tool is `.gitignore`-aware — it skips ignored files and directories automatically.

## Slash Commands

| Command | Description |
|---------|-------------|
| `/help` | Show available commands and keybindings |
| `/clear` | Clear conversation history |
| `/clear-trace` | Clear the trace pane |
| `/reset` | Reset conversation memory (persistent session kept) |
| `/status` | Show endpoint, model, workspace, exec mode |
| `/mode` | Change execution approval mode |
| `/settings` | Manage inference endpoints (add/edit/switch/delete) |
| `/search <query>` | Search conversation or trace pane |
| `/quit` | Exit the TUI |

Type `/` then use Up/Down to browse, Tab to complete.

## Flags & Environment Variables

| Flag | Env Var | Description |
|------|---------|-------------|
| `--base-url` | `LLM_BASE_URL` | Inference server URL (default: `http://127.0.0.1:8080/v1`) |
| `--model` | `LLM_MODEL` | Model name |
| `--workspace` | `WORKSPACE_DIR` | Project directory |
| `--api-key` | `LLM_API_KEY` | API key (also checks `OPENROUTER_API_KEY`) |
| `--context-size` | `LLM_CONTEXT_SIZE` | Override probed context window |
| `--max-rounds` | | Max tool-use rounds per turn |
| `--temperature` | | Sampling temperature |
| `--max-tokens` | | Max output tokens |
| `--prompt "..."` | | Non-interactive one-shot mode |
| | `RAVEN_VAULT_PASSWORD` | Unlock encrypted keystore without interactive prompt |

## Architecture

```
src/
├── main.rs              # CLI parsing, keystore init, vault unlock
├── tui_app.rs           # Main event loop, App struct, agent orchestration
├── tui_render.rs        # All rendering (status bar, panes, overlays, input bar)
├── input_dispatch.rs    # Slash command dispatch + navigation helpers
├── settings_modal.rs    # Settings modal state machine + key handling
├── search.rs            # In-pane search (match finding, scroll-to)
├── agent.rs             # Agent (conversation, tool loop, context management)
├── llm.rs               # OpenAI-compatible HTTP client + SSE streaming
├── session.rs           # Persistent session (meta.json, full_log, context.db)
├── config.rs            # Config + ContextBudget
├── keystore.rs          # Encrypted endpoint storage (AES-256-GCM + Argon2id)
└── tools/
    ├── mod.rs           # Tool definitions + dispatch
    ├── fs.rs            # File ops (read/write/patch/grep/list) with containment
    ├── exec.rs          # Shell execution with timeout + kill_on_drop
    └── web.rs           # web_search + browse
```

- The agent instance (conversation history + session) survives across turns via `Arc<Mutex<Agent>>`
- Streaming tool-use continuation is a real loop (handles multiple rounds of tool calls)
- All truncation is UTF-8 safe
- The draw loop uses a `needs_redraw` flag to skip idle frames
- Status bar caches values on agent lock contention to prevent flicker

## Tests

```bash
cargo test
```

17 tests covering: patch logic, line-range parsing, workspace containment, context budget bounds, keystore encrypt/decrypt round-trip, SSE stream parsing, tool call delta accumulation, UTF-8 safe truncation, and search matching.
