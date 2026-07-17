# Git & GitHub Tooling — Prior Art Before Reinvention

Date: 2026-07-16  
Status: **design / prioritization** (not implemented)  
Audience: contributors shaping Raven’s git/GitHub tools, domain packs, and plan recipes  
Related: `docs/grep-tool-ideas.md`, `docs/grok-build-import-candidates.md`, `docs/plan-mode-improvements.md`, session wiki (`wiki/`)

## Purpose

When humans code well, they almost always ask first:

> Does this already exist? Can I reuse, extend, or learn from it instead of reinventing it?

A coding agent that never looks outside the open workspace will **reimplement Galaga, OAuth, OCR pipelines, and half of crates.io** from scratch—burning tokens, time, and quality. If more harnesses **defaulted to prior-art checks** before greenfield invention, collective software quality would improve.

This document proposes Raven capabilities and workflow hooks so the agent:

1. Uses **local git history** when the problem is *this* repository.  
2. Uses **GitHub (and friends)** when the problem might already be solved in the open.  
3. Records findings in the **session wiki** and plan, with **license and containment** discipline.  
4. Still **implements in the user’s workspace**—inspired by prior art, not silent wholesale copy-paste of foreign trees.

**Slogan:** *Search the world, then build in the workspace.*

---

## Design thesis

| Job | Human habit | Raven capability |
|-----|-------------|------------------|
| **A. Repo archaeology** | `git log`, blame, bisect intuition | Local **read-only git** tools |
| **B. Solution scout** | Google / GitHub search before writing a framework | **GitHub search + inspect + optional cache clone** |
| **C. Institutional memory** | README, ADRs, “we tried X” | **Wiki + plan notes** (`research/prior-art.md`) |

Do **not** collapse A and B into one “git MCP” with unconstrained write. Different trust, network, and approval rules.

---

## Why this belongs in the harness (not only the prompt)

Prompts that say “don’t reinvent the wheel” are ignored under time pressure and tool blindness.

Harness leverage:

- **Tools** that make scout cheap (structured repo cards, capped README reads).  
- **Recipes / plan steps** that *require* a prior-art note for certain goals.  
- **Nudges** when the goal looks like a known product class (`clone`, `remake`, `like Galaga`, `OAuth provider`).  
- **Approvals** for clone/network vs local history.  
- **Logging** so operators see “checked GitHub” not only “wrote 40 files.”

Same philosophy as scope-aware grep: change the **default loop**, not only the sermon.

---

## Non-goals

- Auto-vendor entire third-party repos into the user’s tree as the deliverable.  
- Replace package managers (`cargo add`, npm) with random GitHub clones.  
- Full GitHub App / PR automation in v1.  
- Unrestricted `git push` / `reset --hard` from the model.  
- Depending on a third-party “git MCP” as the only design (optional later packaging).

---

## Capability A — Local git (history leverage)

### When to use

- Regression: “this broke recently”  
- Ownership: who touched this line  
- Intent: commit that introduced a symbol  
- Scope: files changed on this branch vs `main`  
- Pair with pickaxe grep: history of a string/symbol  

### Proposed tools (read-only v1)

| Tool | Behavior | Caps |
|------|----------|------|
| `git_status` | branch, dirty summary, ahead/behind | small fixed output |
| `git_log` | commits by path / since / author | N commits, subject+hash first |
| `git_show` | one rev or path@rev | max bytes; no unbounded diffs |
| `git_blame` | line range → commit + subject | range required |
| `git_diff` | vs ref or staged; **file list first**, patches optional | max files / bytes |
| `git_pickaxe` | `log -S` / `-G` style | max commits; pair with `grep` doc |

**Write operations** (`commit`, `checkout`, `push`): separate tools or explicit allowlist later; default **off** or Babysitter-only.

### Implementation notes

- Prefer **allowlisted `git` subprocess** with fixed flags over full MCP, for output budgets and Windows consistency—or **git2** for status/log/blame.  
- Never run arbitrary `git` strings from the model without parsing/allowlist.  
- Secrets: history can contain leaked keys; future redaction is P2.

---

## Capability B — GitHub solution scout

### When to use

| User intent (examples) | Scout? |
|------------------------|--------|
| “Build a Galaga-style SDL game” | **Yes** — clones, engines, asset pipelines |
| “Add OAuth2 login like industry standard” | **Yes** — reference implementations |
| “Fix off-by-one in our parser” | **No** — local git + grep |
| “Rename this function” | **No** |
| “What’s the popular crate for X?” | **Partial** — docs.rs / crates.io may beat GitHub |

### Proposed tools (v1)

| Tool | Behavior |
|------|----------|
| `github_search_repos` | query, language, sort (`stars` / `updated`), per_page; returns name, stars, license, description, default branch, pushed_at |
| `github_repo_overview` | owner/repo → README excerpt, license SPDX, topics, languages, open issues count |
| `github_fetch_paths` | list directory or get file blob(s) by path — **API-first, no full clone** |
| `github_search_code` | optional P1; needs auth; stricter rate limits |
| `github_cache_clone` | shallow `--depth 1` into **cache root** (not project root); approval-gated |

### Cache & containment

```text
~/.raven-hotel/cache/github/<owner>__<repo>/   # or session-scoped under session dir
```

Rules:

- Clone **only** into cache (or explicit `scratch/`).  
- Workspace tools still cannot escape project unless user opts in.  
- Agent **reads** cache; **writes product code** only in workspace.  
- TTL / manual clear documented; large clones warned.

### License discipline

Every scout result should surface **license** when available.

Plan / wiki checklist:

- [ ] License identified (or “unknown — do not copy code”)  
- [ ] What we reuse: architecture / algorithm / deps only vs code excerpts  
- [ ] Attribution note if required  

Unknown or no-license repos: **learn structure, don’t paste**.

### Output shape (repo card)

```json
{
  "full_name": "owner/galaga-sdl",
  "html_url": "https://github.com/owner/galaga-sdl",
  "stars": 1200,
  "license": "MIT",
  "description": "...",
  "default_branch": "main",
  "pushed_at": "2024-…",
  "readme_excerpt": "…",
  "why_relevant": "model-filled optional"
}
```

Trace: short list of names+stars; detail in fold / model channel.

---

## Capability C — Wiki & plan integration

### Artifacts

| Path | Content |
|------|---------|
| `wiki/research/prior-art.md` | Shortlist, licenses, decisions (reuse vs original) |
| `wiki/plan.md` | Optional step 0: “Solution scout complete” with verification `wiki_exists:research/prior-art.md` |
| Session meta / discoveries | One-line “prior art: owner/repo” when useful |

### Plan recipe fragment (greenfield product-like goals)

When goal matches heuristics (see below), inject or recommend:

1. **Scout** — `github_search_repos` + 1–3 `github_repo_overview`.  
2. **Note** — write `wiki/research/prior-art.md` (even if “nothing suitable”).  
3. **Decide** — original implementation vs fork-inspired architecture.  
4. **Build** — normal steps in workspace with Raven verification tiers.

“Nothing suitable” is a valid outcome—the point is the **check**, not forced dependency.

---

## Workflow recipes

### Recipe: “Build me a Galaga clone”

```text
1. github_search_repos("galaga SDL2 OR SDL", language=C++, sort=stars)
2. Filter: license in {MIT, Apache-2.0, BSD-*} when possible
3. overview + fetch_paths on entrypoints / CMakeLists / src layout
4. optional shallow cache clone of winner for deeper read
5. wiki/research/prior-art.md — patterns to adopt (entity loop, assets, input)
6. plan.md — implement in ./galaga/ with success criteria (build, smoke, features)
7. implement — original code + Kenney assets path; cite inspiration in README
```

### Recipe: “Bug in our tree”

```text
1. git_log / git_blame / git_pickaxe on symbol
2. grep scopes on current code
3. No GitHub scout unless error is clearly upstream-of-deps
```

### Recipe: “Add feature like $POPULAR_APP”

```text
1. Scout 2–3 reference repos or known open implementations
2. prior-art.md: protocol choices, not code dump
3. Implement against *our* stack
```

---

## Triggers & nudges (harness)

### Goal heuristics → suggest scout once

Examples (non-exhaustive):

- `\b(clone|remake|replica|like|inspired by|alternative to)\b`  
- Well-known product classes: game clones, todo apps, “build a chat gpt”, static site generators  
- Explicit: “don’t reinvent”, “is there an open source”, “use something existing”

**Nudge (system/tool result), not hard block:**

> This goal looks greenfield/product-shaped. Consider solution scout (GitHub search) and write wiki/research/prior-art.md before large code generation.

### Modes

| Mode | Behavior |
|------|----------|
| **talk** | May discuss prior art; no clone |
| **plan** | Scout + prior-art wiki preferred before proceed |
| **work** | May scout if missing prior-art note; prefer implement after note exists |
| **research / dream** | Scout-heavy allowed |

### Super Judge / verification (optional)

- Soft criterion: for goals tagged `product_clone` / `greenfield_app`, wiki prior-art exists.  
- Not a universal hard gate (would annoy small tasks).

---

## MCP vs native tools

| Approach | Role |
|----------|------|
| **Native Raven tools** | v1: budgets, approvals, wiki hooks, dual-path logging |
| **GitHub / git MCP** | Optional later for interop; still wrap with Raven policy |
| **`gh` via exec** | Escape hatch if authenticated; unstructured—prefer structured tools |

**Auth:** GitHub token in vault / env (`GITHUB_TOKEN`); anonymous search is rate-limited—document floor.

**Approvals:**

| Action | Babysitter | SpringBreak / Vegas | Thunderdome |
|--------------------|---------------------|-------------|
| git read | auto | auto | auto |
| github search / overview / fetch file | auto or once | auto | auto |
| shallow cache clone | ask | ask or session yolo | auto |
| git write / push | ask | ask | ask or rare auto |

Network sandbox (future Landlock): outbound GitHub must remain allowed for scout profiles.

---

## Risks & mitigations

| Risk | Mitigation |
|------|------------|
| License contamination | Always show license; unknown ⇒ no code paste |
| Agent vendors junk repos into product path | Cache-only clones; plan success ≠ “copied repo” |
| Supply chain (curl \| bash from README) | Never auto-run foreign install scripts; exec still gated |
| Rate limits | Cache search results; API-first; shallow clone rare |
| Context blowup | Repo cards + excerpts; not full trees in model context |
| Scout paralysis | Timebox: max 3 repos, then decide |
| Wrong prior art | prior-art.md must include “rejected because…” |
| Secrets in git history | Cap show/diff; future redact |

---

## World-better framing (product principle)

**Manual good practice → harness default:**

1. Check whether the wheel exists.  
2. Note what you found (even absence).  
3. Build the smallest original piece that serves *this* user and workspace.  
4. Prefer dependency or clear fork over silent reimplementation of large systems.

Raven should make step 1–2 **cheaper than skipping them**. That is the intervention.

If every agent harness did this for greenfield asks, we would see fewer half-broken rewrites of solved problems and more energy on integration, verification, and product fit—the parts that actually need a human’s project context.

---

## Implementation phases

### P0 — Docs + prompts only (days)

- System / plan recipe text for greenfield goals.  
- Nudge when heuristics match.  
- Wiki template snippet for `research/prior-art.md`.  
- Optional: `exec` allowlist examples for `git log` / `git status` with strong prompt discipline (temporary).

### P1 — Local git read tools (week)

- `git_status`, `git_log`, `git_show`, `git_blame` with hard caps.  
- Dual-path UI (summary vs detail).  
- Tests with a tiny fixture repo.

### P2 — GitHub scout tools (week+)

- `github_search_repos`, `github_repo_overview`, `github_fetch_paths`.  
- Token from env/vault.  
- prior-art plan verification helper (`wiki` file exists + non-empty).

### P3 — Cache clone + polish

- `github_cache_clone` shallow + approval.  
- `github_search_code`.  
- Domain pack / skill: `solution-scout.md`.  
- Optional MCP packaging of the same surface.

---

## Success criteria (for the feature itself)

- [ ] Greenfield “clone a game/app” runs produce a **prior-art wiki note** before large codegen (nudge or plan step).  
- [ ] Local regressions use **git history tools** without GitHub noise.  
- [ ] Clones never land in project root without explicit user-facing path choice.  
- [ ] License fields visible on scout hits.  
- [ ] Operators can see scout in **trace** (tool starts/results), not only final code.  
- [ ] Windows/Linux/macOS: same tool names and JSON (no bash-only git wrappers as the only path).

---

## Related tool docs

- **Grep:** mtime/session-hot + scope context — `docs/grep-tool-ideas.md` (local code search).  
- **Grok imports:** MCP as bus later — `docs/grok-build-import-candidates.md`.  
- **Plan mode:** verify scout step — `docs/plan-mode-improvements.md`.

Together: *find code in-tree (grep), find history in-tree (git), find solutions out-of-tree (GitHub), then plan and build.*

---

## Changelog

- **2026-07-16** — Initial design: local git vs GitHub scout, recipes, nudges, phases, “prior art before reinvention” principle.
