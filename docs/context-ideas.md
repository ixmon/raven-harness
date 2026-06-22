# Context, Session Memory & Compression — Current Behavior & Ideas

Date noted: 2026-06-22

This document describes how Raven Harness stores session data, what the model actually sees each turn, how compression works today, and proposed improvements — especially around **sparse injection + on-demand retrieval**, **churn-to-answer summarization**, **cross-session continuity**, **credential handling**, and **smarter tool-budget nudges**.

Related: [headroom-ideas.md](./headroom-ideas.md) (CCR, `raven learn`, type-aware compressors).

---

## 1. What We Store Long-Term

**Base directory:** `~/.raven-hotel/sessions/<session-id>/`

Session id is stable per workspace path (`make_session_id()` in `src/session.rs`).

| File | Purpose |
|------|---------|
| `meta.json` | Persistent headline context: goal, tests, pitfalls, discoveries, repo cache, rolling summary |
| `full_log.jsonl` | Append-only audit trail of user / assistant / tool messages |
| `context.db` | SQLite cache of mtime-matched per-file summaries |

Also separate from sessions: `~/.raven-hotel/endpoints.json` (encrypted API key vault).

### `meta.json` fields (`SessionMeta`)

| Field | Injected into LLM every turn? |
|-------|-------------------------------|
| `current_goal` | Yes |
| `achievement_tests` | Yes (all) |
| `pitfalls` | Yes (all) |
| `discoveries` | Yes — **last 8 only** (up to 30 stored on disk) |
| `last_user_request` | Yes |
| `repo_cache` (tree, important paths, language hint, short summary) | Yes (partial — see below) |
| `recent_turns_summary` | Yes (capped ~1600–1800 chars) |
| `exec_approval_mode` | No (UI / runtime only) |
| `initial_analysis` | **No** (stored on first trust, never injected) |
| `session_id`, timestamps, `trusted` | Partially (id + workspace in header) |

`RepoCache` injected: `tree_text` (~28 lines), top 18 `important_paths`, `language_hint`, `short_summary`. Not injected: `project_type`, `indexed_at`, file counts.

### `full_log.jsonl` format

Written by `Agent::persist_turn()` and related helpers in `src/agent.rs`. Each line:

```json
{
  "ts": "<RFC3339>",
  "role": "user|assistant|tool",
  "content": "<full message text>",
  "has_tool_calls": true,
  "tool_call_id": "<optional>",
  "tool_names": ["read", "grep"]
}
```

Notes:

- Append-only; **not truncated** by `/clear` or `/reset`.
- Tool results are stored **after** `truncate_for_context()` — still potentially large.
- Tool call **arguments** are not logged as separate fields (only `tool_names` on assistant rows).

### `context.db` (file summaries)

- Table: `file_summaries(path, mtime, summary, updated_at, file_size)`.
- Matched by relative path + **exact** on-disk mtime.
- Invalidated on `write` / `patch`.
- **Not injected** — retrieved only via `read_summary` / written via `store_summary`.

---

## 2. What the Model Sees Each Turn

Built in `Agent::build_messages_for_model()` (`src/agent.rs`):

```
[system: system_message() + session.get_injection_block()]
+ conversation[]   (user / assistant / tool, pruned to ≤48 messages)
+ tools[]          (12 tool schemas, every call)
```

The injection block is assembled in `Session::get_injection_block()` (`src/session.rs`). It is intentionally **small and high-signal** — a headline layer, not full history.

### On-demand via tools (not in system prompt)

| Tool | Role |
|------|------|
| `read_summary` / `store_summary` | File-level cache in `context.db` |
| `read`, `grep`, `list`, `exec`, `web_search`, `browse` | Fresh workspace / web data |
| `update_goal`, `record_discovery` | Mutate `meta.json`; ack only in tool result |

There is **no** tool today to search or excerpt `full_log.jsonl` or past session summaries.

### Context budgets (`ContextBudget` in `src/config.rs`)

Derived from probed context window (or `--context-size` / 8192 default):

```
context_bytes     = n_ctx × 3.5
tool_budget       = context_bytes × 60%
tool_result_bytes = clamp(tool_budget / max_rounds, 500, 50_000)
read_line_limit   = clamp(tool_result_bytes / 45, 20, 1_000)
```

Example at 65536 ctx, 10 rounds: ~27 KB per tool result, ~612 lines per default read.

Token estimate in status bar: `estimated_context_tokens()` = total prompt bytes / 3.5.

---

## 3. Compression — What Happens Today

### 3.1 Conversation pruning (`prune_history`)

When in-memory conversation exceeds **48 messages**:

1. Older messages are dropped (keeps the most recent 48).
2. If ≥6 messages were dropped, `summarize_messages()` runs a **separate LLM call** (no tools, `max_tokens: 800`, `temperature: 0.3`).
3. Summary is merged into `meta.recent_turns_summary` (trimmed to **1800 chars**).
4. Dropped messages are **gone from the live prompt** — originals are **not** stored for later retrieval.

Summarization prompt (paraphrased): “key actions, files discovered, current understanding — omit low-value details.”

**Gap:** It does not specialize in **problem → failed attempts → final answer**. Long tool churn may compress to vague bullets; repeat questions may re-trigger the same exploration.

### 3.2 Rolling summaries (lighter weight)

| Trigger | What gets summarized | Limit |
|---------|----------------------|-------|
| Every 4 messages (`persist_turn`) | Last 4 conversation messages | Merged into `recent_turns_summary`, **1600 chars** |
| End of each user turn (`force_flush_session`) | Last 6 messages | **Replaces** summary, **1600 chars** |

### 3.3 Tool output truncation

Large tool results are cut to `tool_result_bytes` before entering conversation (`truncate_for_context`). This is **lossy** with no `retrieve(hash)` escape hatch (contrast Headroom CCR in `headroom-ideas.md`).

### 3.4 Separate summarization LLM calls

Dropped history and rolling summaries are sent to the **same configured endpoint** as the main agent. Anything in those messages (file contents, exec output, accidental secrets) goes to the remote/local LLM again.

---

## 4. Between Sessions & Restarts

### Same workspace

| Data | Persists? | Restored into model on restart? |
|------|-----------|----------------------------------|
| `meta.json` | Yes | Yes — injection block every turn |
| `context.db` | Yes | On-demand via `read_summary` only |
| `full_log.jsonl` | Yes (full audit) | **No** — only last **20** user/assistant lines via `load_recent_conversation()` |
| Tool messages in log | Logged | **Skipped** on restore |
| In-memory conversation | Lost | Partial replay (20 msgs, no tools) |

`/clear` and `/reset` clear in-memory conversation but **keep** `meta.json` and `full_log.jsonl`.

### Different workspace

New session directory — **no cross-project memory** today.

### Asymmetry to be aware of

`full_log.jsonl` is the complete record; the model after restart mostly relies on:

- Sparse `meta.json` injection block
- Last 20 user/assistant utterances
- Re-discovery via tools

Older tool traces and multi-step reasoning paths are not automatically available.

---

## 5. Tool-Budget Nudges (Current)

Defaults: `--max-rounds 10`, hard cap `MAX_TOOL_ROUNDS = 12` (`src/agent.rs`).

Streaming path (`src/tui_app.rs`):

| Mechanism | Count | Behavior |
|-----------|-------|----------|
| **Text-only nudges** | 2 | If model used tools then stopped with text only: inject fake user message via `push_continuation_nudge()` — “call the next tool, do not narrate” |
| **Auto-continue** on round limit | 3 extra cycles | If still calling tools when rounds exhausted, trace “auto-continuing…” and resume with fresh round budget |
| **Final exhaustion** | — | “Send another message to continue” |
| **Approval denials** | 3 | Stops tool loop |

**Not implemented:**

- “Are you making progress or looping?”
- “Should you ask the user for guidance?”
- “Stop and record what you learned before continuing”

Current nudges assume the failure mode is **narration instead of action**, not **wrong direction or infinite loop**.

---

## 6. Credential & Secret Exposure

### High risk today

| Vector | Notes |
|--------|-------|
| Remote LLM API | Full prompt (system block + conversation + tool results) sent to `base_url/chat/completions` |
| `full_log.jsonl` | Plaintext on disk; may contain `.env`, tokens from `read` / `exec` |
| `meta.json` | `record_discovery` / goals may embed secrets the model chose to remember |
| `context.db` | Summaries may embed sensitive patterns |
| Summarization calls | Dropped conversation including tool output sent to LLM for compression |

### Lower risk / mitigations

- API keys in `endpoints.json`: AES-256-GCM + Argon2id (vault password **not** injected).
- Workspace path containment for file tools.
- Approval UI truncates command **display** only — not LLM payload.

### Storing credentials in meta (idea — not built)

**Do not inject secrets into the system block.** If we store them for later:

- Dedicated `secrets` or `env_hints` bucket in meta or separate encrypted store.
- **Never** auto-inject; tool-only access (`get_credential(name)`).
- Redaction pass before `full_log.jsonl` append and before summarization LLM calls.
- User confirmation when a discovery looks like a secret.

---

## 7. Architecture (Current)

```
Per LLM turn:
┌─────────────────────────────────────────────────────────┐
│ system = base_instructions + get_injection_block()      │
│   ← meta.json (goal, pitfalls, discoveries×8, repo,     │
│      recent_turns_summary, last_user_request)           │
├─────────────────────────────────────────────────────────┤
│ conversation[] (≤48 msgs: user/assistant/tool)          │
│   ← pruned; tool results truncated to budget            │
├─────────────────────────────────────────────────────────┤
│ tools[] (12 function schemas)                           │
└─────────────────────────────────────────────────────────┘

On disk (persistent):
meta.json ──────────► injected every turn (sparse)
full_log.jsonl ─────► audit / future mining; NOT auto-loaded
context.db ─────────► on-demand via read_summary / store_summary
```

---

## 8. Design Goals (Target State)

Aligns with operator intent from 2026-06-22 discussion:

1. **Sparse initial RAG** — inject headlines only; avoid replaying full history or large tool dumps by default.
2. **Easy on-demand depth** — model can tool-call for excerpts when needed (session log, compressed blobs, discoveries, resolutions).
3. **Churn → answer** — when the model spends many tool rounds exploring, then succeeds, compress that into a **reusable resolution** so repeat questions skip the maze.
4. **Cross-session continuity** — same workspace should feel like “it remembers how we fixed this last time” without stuffing the whole log into context.
5. **Safe secrets** — never surprise-inject credentials; optional structured storage with redaction.
6. **Honest budget nudges** — at end of tool budget, ask about progress / looping / user guidance, not only “keep calling tools.”

---

## 9. Proposed Improvements

### 9.1 `retrieve_session` tool (high leverage)

Search `full_log.jsonl` and/or structured meta by keyword or time range; return capped excerpts.

- Not injected by default.
- BM25 or simple ripgrep-style search first; embeddings later.
- Parameters: `query`, `max_chars`, optional `since`, `roles`.

Enables: “What did we do last time about X?” without loading 20+ turns into system prompt.

### 9.2 Resolution records (churn → answer)

At end of heavy tool turns (or on `force_flush_session`), run a structured extraction:

```json
{
  "problem": "How to fix context probing for llama.cpp",
  "failed_paths": ["exact model name match only"],
  "solution": "probe_server with single-model fallback + meta.n_ctx",
  "files": ["src/llm.rs", "src/main.rs"],
  "commands": []
}
```

Store in `meta.json` as `resolutions[]` (capped). Inject **titles + one-line answer** only; full record via `retrieve_session` or `get_resolution(id)`.

Summarization prompt should explicitly ask: *What was tried and rejected? What is the final answer?*

### 9.3 CCR for tool results (from Headroom)

When truncating large `grep` / `read` / `exec` output:

1. Store full blob locally under content hash.
2. Return compact marker + sample in tool result.
3. Add `retrieve(hash, query?)` tool.

See `headroom-ideas.md` § CCR. Eliminates silent loss from `truncate_for_context`.

### 9.4 `raven learn` — offline session mining

Scan `full_log.jsonl` across sessions (or per workspace):

- Success correlations: “path A failed → path B worked.”
- Environment facts, command patterns, user preferences.
- Write actionable block to workspace `AGENTS.md` or meta `learned_patterns`.

Documented in `headroom-ideas.md`; not implemented.

### 9.5 Smarter end-of-budget nudge

When `max_auto_continues` is exhausted or on the **last** auto-continue cycle, inject a different nudge:

```
[Tool budget nearly exhausted. Reflect: (1) Are you making progress or looping?
 (2) Should you ask the user a clarifying question?
 (3) If you found the answer, call record_discovery with the conclusion and stop.]
```

Optional: force a short non-tool reflection turn before allowing more tools.

### 9.6 Discoveries & injections tuning

- Inject discovery **count** + “use `search_discoveries` for more” instead of always showing 8.
- Tag discoveries: `fact`, `resolution`, `pitfall`, `secret` (secret never injected).
- Promote resolutions from rolling summary into dedicated store automatically.

### 9.7 Cross-session / cross-project (later)

- Optional global `~/.raven-hotel/learned/` for patterns that apply across repos.
- Session linking: “same goal as session X” for related workspaces.

### 9.8 Observation masking (hybrid condenser)

Mask old **tool** observations while keeping assistant reasoning and tool-call metadata. Replace stale `read` / `exec` / `grep` bodies with placeholders + retrieve hook.

- Mask by default (cheap, no extra LLM call).
- Summarize only when masked count exceeds a threshold (JetBrains hybrid).
- See §12.4 and §11.1 (JetBrains research).

### 9.9 Dual-endpoint routing (`primary` + `condenser`)

Extend `endpoints.json` with a `role` field so summarization, resolution extraction, and loop/progress checks use a **cheap/local** model while the tool loop uses **frontier** or primary local.

See §13.

---

## 10. Context Engineering Framework

[LangChain — Context Engineering for Agents](https://www.langchain.com/blog/context-engineering-for-agents) (2025) groups agent context work into four strategies. Raven already implements parts of each; the gap is mostly **routing** and **masking**.

| Strategy | Meaning | Raven today | Gap |
|----------|---------|-------------|-----|
| **Write** | Persist outside the context window | `meta.json`, `full_log.jsonl`, `context.db`, `record_discovery` | No structured resolutions; secrets unsafe |
| **Select** | Pull in only what’s needed this step | Injection block + `read_summary` | No `retrieve_session`; discoveries not searchable |
| **Compress** | Keep tokens required for the task | `prune_history`, rolling summaries, `truncate_for_context` | Lossy truncate; summarize-only; same model for condense |
| **Isolate** | Split context across stores / agents | Per-workspace session; file cache separate from chat | No observation masking; single agent thread |

Related failure modes ([Drew Breunig](https://www.dbreunig.com/2025/06/22/how-contexts-fail-and-how-to-fix-them.html)): context poisoning, distraction, confusion, clash — all relevant when tool dumps accumulate.

**Takeaway:** Recent harness work is less about bigger windows and more about **the right mix of write / select / compress / isolate** plus **tiered inference by step type**.

---

## 11. Recent Research & Products Worth Studying

Ordered by relevance to a local-first coding harness that must scale from cheap inference to frontier.

### 11.1 Observation masking vs LLM summarization (high priority)

- [JetBrains — Cutting Through the Noise](https://blog.jetbrains.com/research/2025/12/efficient-context-management/) (Dec 2025)
- Paper: [arxiv 2508.21433](https://arxiv.org/pdf/2508.21433)
- Code: [the-complexity-trap](https://github.com/JetBrains-Research/the-complexity-trap)

**Finding (SWE-bench, long trajectories):** Masking old tool **observations** while keeping reasoning + actions often **matches or beats** LLM summarization on cost and sometimes solve rate. Summarization can **extend** trajectories (~15% more turns) by smoothing over “stop / loop” signals. Summarization API calls can be **>7%** of total cost.

**Hybrid:** Observation masking first; LLM summarization only after a large batch of turns.

**Product note:** JetBrains cites **observation masking** in Cursor and Warp proprietary SE agents. OpenHands popularized condenser-style summarization.

### 11.2 Reversible compression (CCR)

- [Headroom](https://github.com/chopratejas/headroom) — already summarized in [headroom-ideas.md](./headroom-ideas.md)

Compress → store blob by hash → `retrieve(hash)`. Fits sparse inject + on-demand depth better than `truncate_for_context` alone.

### 11.3 OpenHands context condenser

- [Condenser docs](https://docs.openhands.dev/sdk/guides/context-condenser)
- [Blog — context condensation](https://openhands.dev/blog/openhands-context-condensensation-for-more-efficient-ai-agents)

Pluggable `LLMSummarizingCondenser` with separate `usage_id: "condenser"` for cost accounting. `max_size` + `keep_first` (pin system + initial user message). ~2× cost reduction on long SE tasks in their benchmarks.

### 11.4 Long-running agents (Cognition)

- [Don’t build multi-agents / long-running agents theory](https://cognition.ai/blog/dont-build-multi-agents)

Context engineering as “job #1.” Summarization at trajectory boundaries with emphasis on preserving **decisions and events** (they use fine-tuned summarizers). Single-threaded trajectory + explicit state over fan-out.

### 11.5 Production compression & pruning

- [VentureBeat — context compression in production (~16× input)](https://venturebeat.com/data/context-compression-finally-works-in-production-new-research-cuts-llm-input-16x-without-the-accuracy-hit)
- [Provence — trained context pruner](https://arxiv.org/abs/2501.16214) (trim by importance, not summarize)

### 11.6 Agent memory ecosystems (2025–2026)

| Resource | Idea |
|----------|------|
| [Mem0 — State of AI Agent Memory 2026](https://mem0.ai/blog/state-of-ai-agent-memory-2026) | Extract + retrieve memories across sessions |
| [Letta](https://www.letta.com/) | Memory blocks the agent edits; tool-mediated context |
| [xMemory / ACT-RAG](https://arxiv.org/abs/2602.02007) | Hierarchical memory, active retrieval |
| [Awesome Memory for Agents](https://github.com/TsinghuaC3I/Awesome-Memory-for-Agents) | Curated paper list |

Raven is closest to Letta-lite (meta + tools) without searchable memory or agent-controlled blocks.

### 11.7 Tiered / routed inference

Emerging pattern: one harness, **many models by step** — frontier for planning/hard debug, cheap for summarize/extract/classify, deterministic for index/grep/crush.

- OpenHands: `llm.model_copy(update={"usage_id": "condenser"})` on a separate call path.
- [Model routers in 2026](https://www.developersdigest.tech/blog/model-routers-optionality-advantage-2026) — optionality across providers and price tiers.

Raven today: **one endpoint for everything** (main loop + `summarize_messages` + `force_flush_session`).

### 11.8 Product patterns

| Product | Pattern |
|---------|---------|
| **Claude Code** | Auto-compact near 95% of context window; full-trajectory summarize ([docs](https://docs.anthropic.com/en/docs/claude-code/costs)) |
| **Cursor / Warp** | Observation masking (per JetBrains) |
| **Windsurf** | Code RAG: grep + KG + rerank, not embedding-only ([LangChain article](https://www.langchain.com/blog/context-engineering-for-agents)) |
| **ChatGPT / Cursor memories** | Auto-generated cross-session facts; selection risk if over-fetched |

### 11.9 Suggested reading order

1. JetBrains context management — immediate coding-harness relevance
2. LangChain context engineering — vocabulary + patterns
3. OpenHands condenser — dual-LLM accounting
4. Cognition long-running agents — what to preserve in summaries
5. Headroom CCR — reversible tool output
6. Mem0 memory state 2026 — cross-session retrieval patterns

---

## 12. Tiered Inference & Probed-Context Math

Raven already derives tool/read limits from probed `n_ctx`. Extend that single probe into a full **`InferenceBudget`** so cheap and frontier inference share one coherent scaling law.

### 12.1 Today: `ContextBudget`

```
context_bytes     = n_ctx × 3.5
tool_budget       = context_bytes × 60%
tool_result_bytes = clamp(tool_budget / max_rounds, 500, 50_000)
read_line_limit   = clamp(tool_result_bytes / 45, 20, 1_000)
```

`max_rounds`, `max_auto_continues`, `text_nudges`, conversation cap (48), and summary caps are **mostly fixed** — not derived from probe.

### 12.2 Proposed: `InferenceBudget::from_context_tokens(n_ctx, …)`

Derive all caps from the same probe:

| Parameter | Formula sketch |
|-----------|----------------|
| `max_conversation_msgs` | `clamp(n_ctx / 2000, 24, 96)` |
| `observation_mask_window` | `clamp(max_rounds × 2, 8, 24)` turns of full tool output |
| `inject_summary_chars` | `clamp(n_ctx × 0.02, 1200, 4000)` |
| `max_auto_continues` | `1` if n_ctx < 16k; `3` if ≥ 32k; `5` if ≥ 128k |
| `text_nudges` | `0` on small ctx; `2` on large |
| `summarize_batch` | trigger when dropped msgs exceed `n_ctx / 4000` |

**Intent:** 8k local model → tight window, fewer continues, more masking. 65k Qwen → more rounds, wider mask window, richer injection.

### 12.3 Dual-endpoint config

Extend `endpoints.json`:

```json
{
  "label": "local-qwen",
  "base_url": "http://127.0.0.1:8080/v1",
  "model": "qwen3-coder-next",
  "role": "primary"
},
{
  "label": "local-small",
  "base_url": "http://127.0.0.1:8080/v1",
  "model": "qwen2.5-3b-instruct",
  "role": "condenser"
}
```

| Call site | Tier |
|-----------|------|
| Tool loop / `build_messages_for_model` | `primary` |
| `summarize_messages`, `force_flush_session` | `condenser` |
| Resolution extraction, loop-vs-progress check | `condenser` |
| Repo index, grep, session log search | deterministic (no LLM) |

**Cost win:** Summarization is a recurring tax; on frontier APIs it dominates auxiliary spend.

### 12.4 Mask-first hybrid condenser

Per conversation turn:

1. Keep last *W* tool results in full (*W* from `observation_mask_window`).
2. Older `tool` messages → `[observation omitted — retrieve_session(query) or hash X]`.
3. Always keep assistant messages + tool **names/args** (decisions).
4. Only when masked count > threshold → one condenser call → merge into `recent_turns_summary`.

Combines JetBrains hybrid + Headroom CCR + Raven’s sparse inject model.

### 12.5 Goal-driven round budget

`achievement_tests` exist in meta but do not control the loop. Ideas:

- After each round (condenser tier): “any test satisfied / blocked / need user?” → early stop.
- When `turn_rounds > max_rounds × 0.8`, switch nudge from “call next tool” to progress/loop/user-guidance (§9.5).

### 12.6 Churn → resolution (condenser post-pass)

After turns with `tool_calls > N`, condenser emits structured JSON (`attempts[]`, `dead_ends[]`, `answer`, `files`, `commands`) → `meta.resolutions[]`. Inject headline only; full record via tool. Cognition-style boundary memory on a cheap model.

### 12.7 Prefix stability (local KV cache)

Keep stable prefix ordering for llama.cpp / local servers:

```
[static system instructions]      ← stable
[SESSION CONTEXT — slow-changing] ← append-only discoveries
[conversation — volatile]
```

See CacheAligner in [headroom-ideas.md](./headroom-ideas.md). Matters when milking long `-c` on local inference.

### 12.8 Effort routing (frontier optimization)

After **successful** tool results: lower temperature, shorter `max_tokens`, optional “no thinking” for supported models. Full effort on new user questions and errors. Reduces frontier output cost without changing harness architecture much.

### 12.9 Where Raven is ahead vs gaps

| Ahead | Gap |
|-------|-----|
| Probed `n_ctx` → tool/read budgets | Masking < summarizing; no condenser tier |
| Persistent meta + goal/pitfalls | No resolutions; generic summarize prompt |
| File summary cache (`read_summary`) | No session log retrieval |
| Local-first, encrypted endpoints | Same model for agent + compress |
| Approval modes / workspace containment | No log redaction |

---

## 13. Suggested Implementation Order

| Priority | Item | Rationale |
|----------|------|-----------|
| 1 | Observation masking + retrieve hook | JetBrains: cheaper than summarize; preserves decisions |
| 2 | `condenser` endpoint role in keystore | Cheap model for summarize/extract; immediate cost win |
| 3 | `InferenceBudget` extends `ContextBudget` | One probe drives rounds, mask window, injection size |
| 4 | `retrieve_session` tool | Sparse inject + access to `full_log` |
| 5 | End-of-budget progress nudge (condenser) | Loop detection without more tool pressure |
| 6 | Resolution records + better summarize prompt | Churn → answer |
| 7 | CCR for truncated tool output | No silent loss at scale |
| 8 | Secrets bucket + log redaction | Security before more persistence |
| 9 | `raven learn` offline miner | Highest long-term leverage, larger scope |

---

## 14. Key Code References

| Function / type | File | Role |
|-----------------|------|------|
| `Session::init`, `get_injection_block` | `src/session.rs` | Disk layout + injection block |
| `load_recent_conversation` | `src/session.rs` | Restart replay (20 user/assistant) |
| `build_messages_for_model` | `src/agent.rs` | Final prompt assembly |
| `prune_history`, `summarize_messages` | `src/agent.rs` | Conversation compression |
| `persist_turn`, `force_flush_session` | `src/agent.rs` | Log + rolling summary |
| `push_continuation_nudge` | `src/agent.rs` | Text-only “keep working” nudge |
| `ContextBudget::from_context_tokens` | `src/config.rs` | Dynamic tool/read limits |
| Auto-continue / nudge loops | `src/tui_app.rs` | Streaming round budget |
| `read_summary`, `store_summary`, `record_discovery` | `src/tools/mod.rs` | On-demand / meta tools |

---

## 15. Open Questions

- Should `initial_analysis` (first trust) be injected once, or stay tool-only?
- Should restart replay include **tool** messages (with truncation), or stay user/assistant only?
- Should resolution extraction run on every turn or only when tool count > N?
- Local-only mode: route summarization to condenser endpoint only (never frontier API)?
- Should `max_rounds` CLI default become derived from probed `n_ctx` instead of fixed 10?
- Vault password UX: prompt in settings wizard instead of silent `raven` default (see separate keystore discussion).
- Observation mask window: same for all agents or tuned per scaffold (JetBrains hyperparameter note)?

---

*This document captures current behavior and design notes as of v0.1.3+ probe work. §10–12 added 2026-06-22 (research survey + tiered inference). Update when implementation lands.*