# Grok Build → Raven TUI — Import Candidates

Date: 2026-07-15  
Status: **proposal / prioritization** (no implementation commitment)  
Audience: contributors deciding what to steal, adapt, or ignore from open-source Grok Build  
Reference tree: `/home/darkstar/cursor/grok-build` (or `github.com/xai-org/grok-build`)  
Related: `plan-mode-improvements.md`, `agent-operating-modes.md`, Raven README philosophy

## Purpose

Grok Build is a large productized agent harness (~470k LOC Rust, Apache-2.0, **no external PRs**). Raven is a smaller local-first harness (~40k LOC) optimized for ownership, multi-backend inference, and experimentability.

This doc lists **capabilities Grok Build does better** that are worth considering for Raven, ranked by fit with Raven’s goals — not a mandate to clone Grok.

**Do not:** copy large swaths of code without license/attribution review, or import product gravity (forced cloud auth, telemetry).  
**Do:** adopt *mechanisms* (sandbox primitives, MCP protocol, plan read-only gate, logging) when they raise Raven’s ceiling without abandoning local-first control.

---

## Quick matrix

| Candidate | Grok has | Raven today | Fit for Raven | Suggested priority |
|-----------|----------|-------------|-----------------|--------------------|
| OS-level sandbox | Landlock / Seatbelt via `nono`; bwrap deny; seccomp child-net | Workspace path containment + approval modes | High | **P0** |
| Plan phase = read-only (except plan file) | Hard tool reject outside plan.md | Plan mode still has tools; weaker hard gate | High | **P0** |
| Durable plan-loop / LLM I/O logging | Product session surface | Plan clarify not in `full_log`; easy to debug-blind | High | **P0** |
| MCP servers | First-class | None | High for ecosystem | **P1** |
| Subagents / worktrees | Parallel agents, isolation | Single agent | Medium–high | **P1** |
| Skills / hooks / plugins | Documented product surface | Domain packs (nascent), no hooks | Medium | **P2** |
| ACP / IDE embed | stdio agent protocol | TUI + thin CLI | Medium if IDE users matter | **P2** |
| Headless / CI output formats | Mature | `--prompt` / evals | Medium | **P2** |
| Memory product | Dedicated memory crate | Session wiki + meta | Low–medium (wiki may be enough) | **P3** |
| Crate split (pager/shell/tools/workspace) | Many crates | Monolithic `src/` modules | Medium long-term | **P3** |
| Enterprise auth / OIDC | Yes | Local vault | Low (out of scope) | Skip |
| Browser login / vendor default | Yes | Explicit endpoints | Low | Skip |

---

## P0 — High leverage, aligned with Raven

### 1. Real OS sandbox (not just app-level containment)

**What Grok does**

- Process-level policy via **`nono`**: **Landlock** (Linux), **Seatbelt** (macOS).
- Profiles: `off`, `workspace`, `devbox`, `read-only`, `strict`.
- Linux: **seccomp** blocks network syscalls on **child** processes for some profiles; parent keeps net for LLM.
- Optional **bubblewrap (`bwrap`)** re-exec for bind-over deny paths.
- Applied once at startup; violations logged (e.g. `~/.grok/sandbox-events.jsonl`).

**What Raven does**

- Path containment in tools (`workspace` root).
- `/mode` ladder: Babysitter → SpringBreak → Vegas → Thunderdome.
- No kernel enforcement; a malicious or mistaken `exec` can still reach the host if approved or yolo’d.

**Why import**

- Closes the gap between “approval theater” and actual blast-radius control.
- Maps cleanly onto existing modes (e.g. Vegas ≈ sandbox on; Thunderdome ≈ off or wide profile).
- MicroVMs are a different, heavier tier; Grok shows you can get far with **Landlock + optional bwrap + child seccomp** without Firecracker.

**Suggested Raven design**

| Mode | Sandbox sketch |
|------|----------------|
| Babysitter | `workspace` or `strict` + always ask |
| SpringBreak | `workspace`, session yolo for in-sandbox |
| Vegas | `workspace` / `strict`, ask only for escapes |
| Thunderdome | `off` or explicit wide profile (user opted in) |

Implement as optional feature / Linux-first; degrade gracefully when Landlock unavailable.  
**Not P0:** full microVM — track separately as “untrusted eval / multi-tenant.”

**Pointers in Grok**

- `crates/codegen/xai-grok-sandbox/` (`lib.rs`, `profiles.rs`, `child_net.rs`, deny + bwrap)
- User guide: `docs/user-guide/18-sandbox.md`

---

### 2. Plan mode hard read-only (except plan artifact)

**What Grok does**

- Plan phase: explore/search OK; **edits only to plan file**; other writes **fail the tool** even in always-approve.
- Separates “design” from “implement” at the **tool policy** layer, not only the prompt.

**What Raven does**

- Rich JSON clarify → proposal → step verification (often *more* structured than Grok’s plan product).
- Plan mode still tool-scoped via prompts/`tools_for_agent`; not as hard a kernel/tool wall against silent patches.

**Why import**

- Stops the model from “planning” by rewriting `src/`.
- Complements Raven’s plan recipes and verification — don’t drop those; **add a policy gate**.

**Suggested Raven design**

- While `loop_phase` is clarify/proposal (pre-proceed): allow read/list/grep/web; allow write only `wiki/plan.md` (and maybe session meta).
- On proceed → current execution tools resume.
- Surface clear tool error: `plan mode: only wiki/plan.md is writable`.

**Pointers in Grok**

- User guide: `docs/user-guide/19-plan-mode.md`

---

### 3. Plan-loop and agent I/O observability

**What Grok does**

- Mature session persistence and inspectability (productized sessions, monitoring docs).

**What Raven does poorly today**

- Plan clarify LLM calls often **missing from `full_log.jsonl`**.
- Operators see double fallback questions with no durable “model returned garbage” trail.
- `full_log` sometimes thin vs `trace_log` tool events.

**Why import (idea, not code)**

- Every plan-loop request/response (or hash + finish_reason + latency) → `full_log`.
- Trace breadcrumb on clarify fail / fallback / ready→proposal.
- Optional: OpenRouter/request id, model id, TTFT for “is the provider paused?” debugging.

**Suggested Raven design**

- `agent.log_harness_event` / `append_log` from `plan_loop` paths.
- UI line already partially exists (`Plan clarify failed`); persist the same.

This is small, high ROI, pure Raven — Grok only motivates the standard.

---

## P1 — Strong product gaps, do when ready

### 4. MCP (Model Context Protocol)

**Grok:** first-class MCP servers, config, tools bridge.  
**Raven:** no MCP; custom tools only.

**Why:** ecosystem (browsers, issue trackers, DBs) without reinventing every integration.  
**Caveat:** keep MCP **opt-in** and sandbox-aware; don’t force cloud.

**Scope sketch:** config file for servers; tool registry merge; approval mode applies to MCP side effects.

---

### 5. Subagents / worktree isolation

**Grok:** parallel subagents, roles, worktree isolation.  
**Raven:** single agent thread (plus Super Judge as a special review pass).

**Why:** long tasks (explore vs implement), parallel research, safer experiments.  
**Raven-shaped version:** start with **one** background explore agent + optional git worktree; avoid full multi-agent OS.

**Pointers:** `xai-grok-subagent-resolution`, shell README subagents section, user-guide `16-subagents.md`.

---

## P2 — Nice when the core is solid

### 6. Skills / hooks / plugins

Grok packages reusable prompts (skills), lifecycle hooks, plugin marketplace.  
Raven has **domain packs** (e.g. Jellyfin) and AGENTS.md — keep those; optionally align pack format with a simple “skill” directory convention rather than a marketplace.

### 7. ACP / IDE embedding

Grok: Agent Client Protocol / stdio for editors.  
Raven: TUI-primary. Add only if users ask for editor integration.

### 8. Headless output quality

Grok: polished `-p` / CI story.  
Raven: strengthen `--prompt`, JSON result, exit codes for eval — without bloating the interactive path.

---

## P3 — Structural / optional

### 9. Crate split

Grok separates pager / shell / tools / workspace / sandbox.  
Raven modules already approximate this; **full workspace split** is a refactor tax. Consider only when compile times or ownership boundaries hurt.

### 10. Product memory service

Grok’s memory product vs Raven **session wiki**. Prefer improving wiki + optional search over cloning a memory service.

---

## Explicit non-goals (do not import)

| Grok thing | Why skip / defer |
|------------|------------------|
| Browser login as default | Conflicts with local-first |
| OIDC / enterprise deploy | Out of Raven’s scope |
| Telemetry / mixpanel-style defaults | Opt-in only if ever |
| Copying Codex/OpenCode tool ports wholesale | Re-implement with attribution if needed; understand licenses |
| MicroVM as default sandbox | Different problem; Grok doesn’t use microVMs either |

---

## MicroVMs vs Grok sandbox (decision note)

You considered **microVMs** for Raven. Grok’s answer is **not VMs**:

| Approach | Strength | Cost |
|----------|----------|------|
| Landlock / Seatbelt / bwrap / seccomp | Fast, native FS, good for coding agent | Same kernel; not multi-tenant hard isolation |
| MicroVM (Firecracker, Cloud HV, …) | Strong isolation | Images, boot, mounts, GPU pain |

**Recommendation:** implement **Grok-like OS sandbox as P0**; reserve microVMs for a later “untrusted plugin / eval tenant” mode if needed.

---

## Suggested roadmap (practical)

1. **Observability** — plan-loop + model metadata in `full_log` (days).  
2. **Plan write gate** — pre-proceed read-only except `wiki/plan.md` (days).  
3. **Linux Landlock workspace profile** behind flag / tied to Vegas (weeks).  
4. **MCP opt-in** (weeks).  
5. **Single subagent + optional worktree** (weeks–months).  
6. Revisit crate split / ACP only with concrete pain.

---

## What Raven should keep (don’t replace with Grok clones)

These are Raven strengths; import *around* them, not over them:

- Local / multi-endpoint first (vault, probe, llama.cpp-friendly)
- Talk / think / research / work / dream behavioral modes
- JSON plan loop + verification recipes + Super Judge / nudges
- Dual-pane conversation + tool trace (improve logging, don’t drop)
- Session private wiki
- Small codebase an agent can modify
- Accept external contributions (unlike Grok Build)

---

## References

| Resource | Location |
|----------|----------|
| Grok Build source | `github.com/xai-org/grok-build` |
| Sandbox crate | `crates/codegen/xai-grok-sandbox/` |
| Plan / sandbox user guides | `crates/codegen/xai-grok-pager/docs/user-guide/18-sandbox.md`, `19-plan-mode.md` |
| Tools lineage (Codex/OpenCode) | `crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md` |
| Raven plan design | `docs/plan-mode-improvements.md` |
| Raven modes | `docs/agent-operating-modes.md` |

---

## Changelog

- **2026-07-15** — Initial write from Grok Build open-source read + Raven comparison discussion.
