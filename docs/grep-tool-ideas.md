# Agent-Oriented Grep — Design Ideas for Raven

Date: 2026-07-16  
Status: **ideas / prioritization** (not implemented)  
Audience: contributors evolving Raven’s `grep` (and related search) tools  
Related: `docs/grok-build-import-candidates.md`, `src/tools/fs.rs` (`grep_files`)

## Purpose

Classic line-oriented grep (ripgrep-class) is solved. Raven already implements a
cross-platform **Rust** `grep` via `ignore` + walk — a better base than shell
`grep`/`ugrep` shadows.

This doc captures **agent-shaped** search variants: ranking, match kinds,
structural context, and result protocols that reduce multi-hop tool use and
context waste. The goal is a small local tool, not a Semgrep/Sourcegraph clone.

---

## Current Raven baseline

| Piece | Today |
|-------|--------|
| Tool | `grep` in `tools/mod.rs` → `grep_files` in `tools/fs.rs` |
| Engine | `ignore::WalkBuilder` + regex (approx.) |
| Context | Line-oriented (typical `-C` / max-matches style limits) |
| Order | Walk order (not mtime / session-hot) |
| Platform | Pure Rust — consistent on Linux/macOS/Windows |

**Principle:** keep structured search in-process Rust; leave `exec` as the
host-shell escape hatch. Do **not** depend on bundling bfs/ugrep/rg for core
behavior (see Grok Build notes: those are Unix-oriented shell upgrades).

---

## Problem (what agents struggle with)

1. **Arbitrary ±N lines** slice mid-function; model re-`read`s the file.  
2. **No ranking** — hits in cold vendor/generated files crowd recent work.  
3. **No match kind** — string literals, comments, and identifiers mixed.  
4. **Huge dumps** — no scout/count-first or pagination.  
5. **Stemming-style fuzzy** on code identifiers creates false friends.

---

## Non-goals

- Replacing LSP / full semantic or embedding search (separate product).  
- Full AST *pattern* languages (ast-grep / Semgrep territory) as v1.  
- Default **stemming** on source identifiers.  
- Shipping or requiring external CLI binaries for core grep.

---

## Design thesis

Ship progressive modes on one tool (or a tight family), always returning
**stable JSON** the model can page through:

1. **Scout** — cheap path/name + ordering + counts.  
2. **Hit** — content match with smart filters.  
3. **Pack** — structural or compressed context around hits.

Exact string/regex remains the core; intelligence is in **order**, **window**,
and **shape of results**.

---

## Priority roadmap

### P0 — High ROI, small surface

#### 1. Optimistic ordering: mtime + session-hot

Search (or rank results from) files in this order:

1. Paths already touched this session (`read` / `patch` / `write` / prior grep hits).  
2. Newest mtime under workspace (capped sample or full walk with mtime sort).  
3. Optional: files changed on current git branch / last N commits.  
4. Everything else (still respect gitignore).

**Early-exit:** stop after K high-quality hits or byte budget.

**Why first:** no parsers; large perceived speed for “where did we just break X?”

#### 2. Match kind filters

Optional constraint on where the pattern may land:

| Kind | Meaning |
|------|---------|
| `code` | non-string, non-comment (best-effort per language) |
| `string` | string / char literals |
| `comment` | line/block comments |
| `ident` | identifier-like tokens only |
| `any` | default (today’s behavior) |

v1 can be heuristic (quotes, `//`, `#`) for a few languages; tree-sitter later.

#### 3. Identifier-aware matching (not stemming)

When `ident` or `smart_ident=true`:

- Word boundaries.  
- Split `camelCase` / `snake_case` / `PascalCase` for subtoken match.  
- Optional casefold.

**Do not** enable Porter-style stemming on code by default.  
Optional later: `stem=docs` for markdown/comments only.

#### 4. Result budget protocol

- Cap matches and total bytes (existing spirit of `GREP_MAX_MATCHES`).  
- **`count_only` / scout mode:** per-file counts before bodies.  
- **`offset` / `next_page`:** deterministic pagination.  
- Trace pane: short human summary; model gets structured payload.

---

### P1 — Structural context

#### 5. Enclosing-scope context (`mode=scopes`)

On each hit:

1. Locate line (content grep as today).  
2. Parse file with **tree-sitter** (or skip if unsupported / invalid).  
3. Find smallest enclosing node of allowed kinds:  
   `function` | `method` | `class` | `impl` | optional `block` (`if`/`match`).  
4. Return that span as context (with size cap).

**Fallback:** classic ±N lines if no parser or node too large.

**Compression for huge scopes:**

- Signature / header line(s)  
- Match line(s)  
- Closing line  
- Ellipsis marker for omitted middle  

**Languages (suggested order):** Rust, Python, TypeScript/JavaScript, Go, C/C++.  
Unsupported files: line context only.

This is **not** ast-grep (pattern query language). It is **string hit + AST window**.

#### 6. Sibling collapse

Multiple hits in the same scope → one result object listing all line numbers +
one shared snippet (highlight all matches).

#### 7. Noise defaults

Hard-skip unless opted in: lockfiles, minified `*.min.js`, large generated
paths, binary extensions, common vendor dirs. Configurable `.ravenignore`
(or reuse/extend gitignore patterns).

---

### P2 — Grep family / modes

Expose as `mode=` on one tool or thin aliases:

| Mode | Role |
|------|------|
| `content` (default) | Line or scope content search |
| `path` | Filename / path segment only |
| `symbol` | Definitions / exports (tree-sitter or ctags later) |
| `tests` | Prefer test trees + test-shaped scopes |
| `config` | yaml/toml/json/env-ish only |
| `history` | Optional git pickaxe (`git log -S`) — exec or libgit2 later |
| `todos` | TODO/FIXME/XXX with light heuristics |

Keep the default path simple; advanced modes are opt-in.

---

### P3 — Optional / careful

| Idea | Note |
|------|------|
| Import-neighborhood boost | After hit in A, boost files that import A |
| Blame / “introduced in” | git blame line — slow; async or on demand |
| Secrets-lite | High-entropy warn only; not a security product |
| Embedding / semantic search | Separate tool; don’t overload `grep` |
| LSP documentSymbol | Optional backend for `symbol` mode |
| Stemming | Docs/comments only if ever |

---

## Proposed tool sketch

Illustrative schema (names flexible):

```text
grep(
  pattern: string,                 # regex or fixed string
  path?: string,                   # subtree; default workspace root
  mode?: "content" | "path" | "symbol" | "tests" | "config" | "scout",
  context?: "lines" | "scope",     # default lines today; scope = P1
  context_lines?: number,          # when context=lines
  scope_kinds?: ["function", "class", ...],
  match_in?: "any" | "code" | "string" | "comment" | "ident",
  order?: "walk" | "mtime" | "session" | "git_recent",
  fixed_string?: bool,
  case_insensitive?: bool,
  smart_ident?: bool,              # camel/snake split
  count_only?: bool,
  max_matches?: number,
  max_bytes?: number,
  offset?: number,                 # pagination
  include_globs? / exclude_globs?
)
```

### Example result object

```json
{
  "path": "src/foo.rs",
  "line": 42,
  "column": 10,
  "match_text": "handle_request",
  "match_kind": "ident",
  "scope_kind": "function",
  "scope_name": "handle_request",
  "context_mode": "scope",
  "snippet": "fn handle_request(...) {\n    ...\n    handle_request(x);\n    ...\n}\n",
  "sibling_lines": [42, 88],
  "score": 0.91,
  "mtime": "2026-07-16T12:00:00Z",
  "session_hot": true
}
```

Envelope:

```json
{
  "hits": [ /* ... */ ],
  "files_searched": 1204,
  "files_skipped": 300,
  "order": "session+mtime",
  "truncated": false,
  "next_offset": 60,
  "summary": "12 hits in 5 files (ordered session-hot, mtime)"
}
```

**Trace pane:** show `summary` + collapsed paths; full JSON/detail for expand or model channel (same dual-path idea as other tools).

---

## Implementation notes

### Stacking on current code

- Extend `grep_files` rather than a parallel shell tool.  
- Session-hot requires a small **session path touch set** in agent/tool backend (paths from tool calls).  
- mtime: `std::fs::metadata` during walk; consider collecting then sorting before deep read for scout.  
- tree-sitter: optional Cargo features per language to keep default binary lean (`--features grep-scopes-rust,...`).

### Consistency vs Grok

| Approach | Grok Build | Raven target |
|----------|------------|--------------|
| Shell find/grep upgrades | bfs / ugrep shadows (Unix) | Not required |
| Bundled rg | Yes (Unix auto; Windows PATH) | Optional only |
| Structured agent grep | Native tools + line search | **JSON + modes + scope context** |
| Windows | Weaker shell shadows | **Same Rust tool everywhere** |

### Testing

- Unit: ordering, pagination, ident split, sibling collapse.  
- Fixture repos: multi-lang samples for scope extraction.  
- Golden JSON for one file with known functions.  
- Windows CI: path separators, casefold policy documented.

### Risks

| Risk | Mitigation |
|------|------------|
| tree-sitter weight / build complexity | Feature-gate; fallback lines |
| Wrong scope on macros / invalid syntax | Fallback; never fail the whole grep |
| Over-filtering `code` kind | Default `any`; kinds opt-in |
| Slow mtime full-tree sort | Cap, sample, or scout-then-deep |

---

## Suggested implementation order

1. **P0.1** — `order=mtime|session`, document defaults.  
2. **P0.2** — `count_only` + harder noise excludes + clearer truncation envelope.  
3. **P0.3** — `smart_ident` + optional `match_in` heuristics.  
4. **P1.1** — tree-sitter scope context for Rust only.  
5. **P1.2** — sibling collapse + signature compression.  
6. **P1.3** — TS/Python/Go.  
7. **P2** — `path` / `tests` / `config` modes as needed.

---

## Relationship to “has this been done?”

| Layer | Market status |
|-------|----------------|
| Fast line grep | Done (rg, ugrep) |
| AST pattern search | Crowded (ast-grep, Semgrep) |
| Embeddings | Crowded in products |
| **Agent tool: exact hit + AST window + recency order + JSON budget** | Still open |

Raven’s niche is the last row: local, deterministic, harness-integrated.

---

## Changelog

- **2026-07-16** — Initial write from design discussion (scope context, mtime, stemming caveats, extra modes).
