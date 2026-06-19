# Development Ideas: Headroom-Inspired Context & Session Management

Source exploration: https://github.com/chopratejas/headroom (README, wiki/ccr.md, wiki/learn.md, wiki/transforms.md, docs/output-token-reduction-guide.md, etc.)

Date noted: 2026-06-18

This document collects promising ideas from Headroom for future development of Raven Harness (the local-first ratatui agentic coding TUI). Raven already implements many similar concepts (persistent sessions, file summary cache, repo discovery + injection, recent turns summary, goal/pitfall tracking, context budgets). Headroom offers systematic, reversible, and learnable refinements.

Keep this document as a reference for later implementation phases. No changes are required now.

## Core Headroom Concepts

### 1. CCR — Compress-Cache-Retrieve (Reversible Compression)

- Problem: Traditional aggressive compression is lossy. If the model later needs details, the data is gone.
- Solution: When compressing (tool outputs, logs, search results, dropped history), store the **original** locally under a stable hash. Insert a compact marker + offer a `retrieve` (or `headroom_retrieve`) tool.
- Flow:
  - Compress large result (e.g. 1000 grep matches or search results → 30 representative items).
  - Marker example: `[1000 items compressed to 30. Retrieve more: hash=abc123]`
  - Inject `retrieve(hash: string, query?: string)` tool.
  - On tool call, proxy/handler fetches original (fast local cache) and continues automatically.
- Extra: **Context Tracker** remembers prior compressions across turns and can **proactively** expand relevant compressed content before the model explicitly asks.
- BM25 / embedding search inside the cached blob via the retrieve tool.

**Why it matters**: Gives 70-95% token savings with **zero** data loss risk. The LLM can always get the full data on demand.

### 2. Specialized / Type-Aware Compressors

Headroom routes content to the right compressor via ContentRouter:

- **SmartCrusher** (JSON / tool output arrays):
  - Always keep: first N (pagination/context), last N (recency), 100% of error items.
  - Keep anomalies (statistical outliers >2σ).
  - Keep top-K relevant to current query (BM25 or embeddings).
  - Sample the rest.
  - Configurable: `keep_first`, `keep_last`, `max_items_after_crush`, `anomaly_std_threshold`, etc.
- **CodeCompressor**: AST-aware for source code (preserve structure/signatures better than naive text compression).
- **Kompress** (ML): HF model (Kompress-v2-base) for general text.
- **RollingWindow** / **IntelligentContextManager**: For conversation history.
  - Intelligent version uses learned multi-factor importance scoring (recency, semantic similarity, TOIN-learned error indicators, forward references, token density) rather than just "drop oldest".
  - Always keeps tool call + result pairs atomically.
  - Protects system prompt + recent turns.

### 3. CacheAligner (Prefix Stabilization)

- LLM providers (and local engines) use KV/prefix caching.
- Dynamic content (dates, varying user context, non-deterministic prompts) breaks prefix hits.
- CacheAligner extracts dynamic parts to the end of the prompt and normalizes stable prefixes.
- Result: dramatically higher cache hit rates on repeated similar interactions.

### 4. Output Token Reduction (not just input)

Headroom also trims what the model **writes back** (often 5× more expensive than input):

- Verbosity steering appended to end of system prompt ("be terse, don't restate context or code").
- Effort routing: dial down "thinking" effort on routine continuations after successful tool results (keep full effort for new questions and errors).
- Optional learning of the user's preferred terseness level from past behavior (interrupts, fast replies).

### 5. headroom learn — Offline Session Mining + Auto-Generated Agent Memory

This is one of the highest-leverage ideas.

- Scans past session logs (for other agents: claude projects jsonl, codex sessions, etc.).
- Uses an LLM analyzer to find:
  - **Success correlations** (not just failures): "Read path A failed; then Read path B succeeded → correct path for X is B".
  - Environment facts (right commands, runtimes, e.g. `uv run python` vs `python3`).
  - File path corrections (wrong locations the agent repeatedly guesses).
  - Search scope guidance (avoid certain dirs; prefer others).
  - Command patterns and user preferences ("never auto-run gradle; show command instead").
  - Known large files that always need offset/limit.
  - Permission/retry patterns.
- Writes **specific, actionable** corrections (not generic advice) into agent-native files:
  - Claude → CLAUDE.md + MEMORY.md
  - Codex → AGENTS.md / instructions.md
- Uses clear marker-delimited sections so re-running only replaces the generated block:
  ```markdown
  <!-- headroom:learn:start -->
  ## Headroom Learned Patterns
  ...
  <!-- headroom:learn:end -->
  ```

The same analyzer can be pointed at different log formats by writing small plugins.

## Current Raven Parallels (Already Implemented)

Raven's session system (`session.rs`, `agent.rs`, tools) already delivers a lot of the value:

- Per-workspace persistent dir: `~/.raven-hotel/sessions/<id>/`
  - `meta.json`: `current_goal`, `pitfalls`, `discoveries`, `repo_cache`, `recent_turns_summary`, `last_user_request`
  - `full_log.jsonl`: append-only turn history (perfect raw material for mining)
  - `context.db` (SQLite): mtime-matched file summaries
- Session tools: `update_goal`, `record_discovery`, `read_summary`, `store_summary`
- `get_injection_block()`: injects repo tree + ranked important files + goal/pitfalls/discoveries + recent summary into every prompt
- Safe repo discovery with limits + importance ranking (READMEs, manifests, mtime, source files)
- Context budget probing + per-tool caps + safe truncation
- Agent persistence (Arc<Mutex<Agent>>), multi-round streaming tool continuation
- Preference for summaries over full file reads

Raven's approach is "external working memory" injected fresh each turn so the model sees a stable, compact view. This already makes small local models viable for long tasks.

## Specific Development Ideas for Raven (Save for Later)

### A. Generalize File Summary Cache → Full CCR + Retrieve Tool

- Extend the existing `read_summary` / `store_summary` + `context.db` pattern.
- For large **tool results** (grep hits, list results, exec output, web_search, browse), apply SmartCrusher-style compression on the fly.
- Store original under a content hash in the session (or a dedicated `compressed/` dir + index).
- Insert reversible marker + expose a `retrieve(hash, [query])` tool (or enhance an existing one).
- Implement a lightweight Context Tracker in the Session that can proactively re-inject relevant previously-compressed blobs on new turns.
- Bonus: support `retrieve` inside the agent loop transparently (similar to Headroom's response handler).

This turns the current "file summaries only" into a general reversible compression layer for any large context contributor.

### B. Implement `raven learn` (Offline Mining)

- Add a subcommand or separate small binary/script: `raven learn [--apply] [--project /path]`
- Point it at `~/.raven-hotel/sessions/*/full_log.jsonl` + `meta.json`.
- Use an LLM (configurable, can be the same local endpoint or a stronger one) to perform success-correlation analysis.
- Write learnings into a project-level file that the harness will read:
  - Suggested: `<workspace>/AGENTS.md` or `<workspace>/.raven/AGENTS.md` (or append to existing).
  - Or store in `meta.json` and inject via the existing injection block.
  - Use marker comments so re-runs are safe and non-duplicative.
- Categories to mine (start small):
  - File path corrections
  - Preferred command patterns / runtimes
  - Known large files + recommended read strategy
  - Directories to prefer/avoid for search/grep
  - Environment facts
- The harness prompt can explicitly tell the agent "consult AGENTS.md / the learned patterns section for project-specific facts".

This directly closes the "harness itself is AI-improvable" loop.

### C. Smarter History / Context Management

- Adopt ideas from IntelligentContextManager + RollingWindow.
- Current pruning/summarization in agent.rs can evolve to:
  - Drop whole tool call/result pairs atomically.
  - Score messages by more than just recency (reference tracking, error signals, semantic importance).
  - Preserve a compact CCR marker + hash when dropping.
- Add a small `recent_turns_summary` updater that is more structured.

### D. Cache Alignment & KV Efficiency

- For local llama.cpp and remote providers: experiment with CacheAligner-style normalization.
- Move highly variable content (timestamps, absolute paths that change, "current date is...") to the end of the injection block or system prompt.
- Normalize whitespace and stable prefixes in the injected SESSION CONTEXT block.
- Measure cache hit improvements via usage stats (when provided).

### E. Output Reduction / Verbosity Steering

- Add a small system note (at end of prompt so prefix cache still benefits) steering terseness:
  - "Be concise. Do not restate code or tool results already visible. Report only what changed or the key finding."
- Optional effort routing: detect "continuation after successful tool with no error" and ask for lower `max_tokens` or a "brief continuation" instruction on that round.
- Expose a verbosity level or let a future `raven learn --verbosity` infer preference.

### F. Content-Aware Routing + Specialized Transforms

- In the tool result path (or a new compression layer), detect content type:
  - JSON arrays → SmartCrusher
  - Source code → lighter AST/text hybrid (or future CodeCompressor)
  - Plain text / logs → standard or ML compressor
- Start simple: rule-based router + one strong general compressor for tool results (SmartCrusher-like logic implemented in Rust).

### G. Cross-Session / Cross-Project Memory (Later)

- Headroom has shared memory across agents and projects.
- For raven: optional global or workspace-grouped store of high-value learned patterns.
- Could feed into per-project AGENTS.md on init.

### H. Proxy / MCP Angle (Optional Future)

- Headroom can sit as a transparent proxy.
- If desired, document how to run raven-tui behind a headroom proxy for zero-code compression benefits.
- Or expose some of raven's context tools via MCP if we grow in that direction.

## Implementation Notes & Priorities (Rough)

1. **Highest immediate leverage**: `raven learn` mining + writing to AGENTS.md (or equivalent) + injection of the learned block. Uses existing `full_log.jsonl` and session tools with almost no new runtime cost.
2. **Next**: Reversible compression on tool results (start with one high-value tool like `grep` or `web_search` results) + `retrieve` tool. Build on the existing summary cache design.
3. **Polish**: CacheAligner on the injection block, output steering, better history pruning that is CCR-aware.
4. **Longer term**: Specialized compressors, proactive Context Tracker, learned importance scoring.

Many of these can be implemented incrementally inside the existing session + agent architecture without breaking the simple local-first design.

## References

- Headroom repo: https://github.com/chopratejas/headroom
- Key pages (local temp clone was at headroom_temp/):
  - wiki/ccr.md
  - wiki/learn.md
  - wiki/transforms.md
  - docs/output-token-reduction-guide.md
- Raven current relevant code:
  - tui/src/session.rs (Session, meta, repo_cache, injection, context.db)
  - tui/src/agent.rs (pruning, summarization, session tools)
  - tui/src/tools/mod.rs (read_summary, store_summary, etc.)
  - tui/src/main.rs (session bootstrap)
  - ~/.raven-hotel/sessions/... layout

---

Save for later. Add items here as new inspirations arise. When implementing, move concrete tasks into issues, a todo list, or instructions.md as appropriate.
