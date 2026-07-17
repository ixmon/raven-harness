# Privacy Modes — Cloud Redaction vs Local Air-Gap

Date: 2026-07-16  
Status: **design / prioritization** (not implemented)  
Audience: contributors thinking about multi-backend privacy in Raven  
Related: keystore / endpoints, dual-path tool logging, `docs/git-github-tooling.md`, session wiki

## Purpose

Doctors (and many engineers) face a real split:

- **Best intelligence** often lives in **cloud** models.  
- **Best privacy** often means **data never leaves the machine**.

A medical AI harness pattern: **obfuscate / de-identify sensitive fields before any cloud call**, run inference remotely, then **re-identify only on the local side** for the clinician. That unlocks frontier models without shipping raw PHI.

Raven already sits at this fork: **local llama.cpp** and **OpenRouter / remote endpoints** in one harness. Privacy should be a **first-class mode**, not an afterthought prompt (“please don’t log secrets”).

At the same time: **local models often get worse** when the harness redacts aggressively—from traces, tool results, and prompts. Guardrails that help cloud safety can **tax local quality**. Operators may prefer **turbo / air-gap mode**: no fancy redaction, because **nothing leaves the box**.

**Slogan:** *Cloud path transforms; local path trusts the air gap.*

---

## Design thesis

| Situation | Default privacy strategy |
|-----------|---------------------------|
| Traffic to **remote** LLM / remote MCP / web that leaves host | **Minimize & transform** (redact, tokenize, scope) |
| Traffic **only** to localhost inference + local tools | **Air-gap trust** — prefer full fidelity; optional light redaction for shared screen/logs |
| **Git / shared artifacts** | Always careful — secrets don’t belong in commits regardless of model locality |
| **Session logs / wiki on disk** | User-controlled retention & redaction of *exports*, separate from model I/O |

One setting family should answer: **what may leave this machine, and in what form?**

---

## Two philosophies (both valid)

### A. Guardrail path (cloud-safe)

Inspired by clinical de-identification harnesses:

1. Detect or classify sensitive spans (rules + optional local NER).  
2. Replace with **stable tokens** (`[PATIENT_001]`, `[SECRET_API_KEY]`, `[PATH_HOME]`).  
3. Send transformed prompt/tool excerpts to cloud.  
4. Optionally **map tokens back** in local UI only (never write reverse map to cloud logs).  
5. Dual-path: model sees redacted; operator may expand “local-only detail” in TUI when policy allows.

**Wins:** use strong cloud models on semi-sensitive work.  
**Costs:** latency, false positives/negatives, worse model performance if over-redacted, complex mapping.

### B. Turbo / air-gap path (local-first)

- Endpoint is loopback / known-local only.  
- **No outbound LLM** (and optionally no web / no GitHub scout / no remote MCP).  
- Prompts, traces, and tool bodies stay **full fidelity**.  
- Privacy = **network policy + physical trust**, not NLP redaction.

**Wins:** max local quality; simple mental model; matches Raven’s origin story.  
**Costs:** weaker models; user must not exfil manually (copy/paste, screenshots, git push).

Neither path replaces **don’t commit secrets**. Git hygiene is orthogonal.

---

## Proposed privacy modes (settings)

User-facing mode (name sketch):

| Mode | Model I/O | Trace pane (default view) | Network extras | Intent |
|------|-----------|---------------------------|----------------|--------|
| **`airgap` / turbo local** | Full text to **local-only** endpoints; refuse remote LLM | Full (or lightly truncated for size) | Block or warn: web, GitHub scout, remote MCP | Max quality + air gap |
| **`local_open`** | Full to local; remote LLM allowed **unredacted** with explicit warn | Full | User choice | Power user, trusted network |
| **`redact_cloud`** | Remote LLM gets **redacted** payloads; local unchanged | Model channel redacted; optional local-only fold | Cloud LLM OK | Medical-style hybrid |
| **`strict`** | Remote requires redaction; deny if redaction fails confidence; local full | Prefer redacted in shared views | Minimal egress | Paranoid hybrid |
| **`export_safe`** | Whatever runtime mode | **Always** scrub when copying/exporting logs | — | Screen share / bug reports |

**Default suggestion for Raven:**

- If active endpoint is **loopback** → default **`airgap`** behavior for LLM (no redaction tax).  
- If active endpoint is **remote** → default **`redact_cloud`** or at least **prompt once** to choose.  
- Never default to silent unredacted cloud for first-run without consent.

Store in config + per-session override (`/privacy`, settings modal).

---

## What gets redacted (cloud path)

### Categories (priority order)

| Class | Examples | Action |
|-------|----------|--------|
| **Secrets** | API keys, tokens, `Bearer`, PEM blocks | Always strip/tokenize before remote |
| **Credentials files** | `.env`, `id_rsa`, vault paths | Don’t send raw; summarize “present” |
| **PII-like** | emails, phones, SSNs (rule packs) | Tokenize on `redact_cloud` / `strict` |
| **Absolute paths** | `/home/alice/...`, `C:\Users\...` | Optional relativize or `[HOME]/...` |
| **Host identifiers** | hostname, LAN IPs | Optional |
| **Large tool dumps** | full file reads | Already size-capped; cloud may get tighter caps |

**Stable tokens** so the model can still reason (“use [PATH_1]”) and local UI can reverse-map.

### What usually should *not* be redacted into uselessness

- Public source in the repo the user is editing (code is often not secret).  
- Generic error messages.  
- Dependency names, language keywords.

**Repo code vs secrets:** redact secrets *inside* code; don’t blank entire files by default.

---

## Local models & the “redaction hurts quality” problem

Observed pain:

- Trace pane / system injections scrubbed → model loses referents.  
- Over-redaction → hallucinations or empty tool use.  
- Local 1-bit / small models especially need **concrete strings**.

**Policy:**

| Layer | Local endpoint | Remote endpoint |
|-------|----------------|-----------------|
| Prompt assembly | Full (airgap/turbo) | Redacted view |
| Tool results to model | Full | Redacted or summarized |
| Trace pane | Full by default | Show **what the model saw** + optional “local raw” fold gated by mode |
| Wiki / full_log on disk | Full local store | Prefer full local; **export** scrubbed |

So: **don’t degrade local prompts to match cloud hygiene.**  
Degrade only the **egress channel**.

Dual-path logging (already a Raven theme):

- `output_for_model` — policy-transformed  
- `output_for_ui` / operator detail — may be richer locally  

---

## Air-gap enforcement (turbo mode)

Redaction is soft. Air-gap should be **hard where possible**:

| Control | Behavior |
|---------|----------|
| Endpoint allowlist | Only `127.0.0.1` / `localhost` / configured local sockets |
| Refuse remote base_url | Clear error: switch mode or endpoint |
| Optional net deny | Future Landlock/seccomp: block child net except none; parent only to local LLM |
| Disable web_search / browse / github_scout | Or require mode upgrade |
| Remote MCP | Off in airgap |
| Clipboard export | Optional warn when copying large traces |

**Turbo mode** = air-gap + full fidelity + fewer confirmations for local tools (still not Thunderdome for `rm -rf` unless user says so).

---

## UI / UX sketches

- Status bar chip: `privacy: airgap | redact-cloud | …`  
- On switching endpoint local → remote: modal  
  > “Send full context to cloud, or enable redaction?”  
- Trace: badge on tool results `sent to model: redacted` vs `full (local)`  
- `/privacy airgap` `/privacy redact-cloud`  
- Settings: pattern packs (secrets always; PII pack optional; path pack optional)

---

## Implementation sketch (phased)

### P0 — Mode flag + endpoint coupling (no ML)

- Enum `PrivacyMode` in config/session.  
- If remote + `redact_cloud`: run **regex secret scrubber** on outbound chat messages (keys, PEM, common env assignments).  
- If `airgap` and base_url not local → hard error.  
- Trace annotation: whether outbound was scrubbed.  
- Document: local turbo = full fidelity.

### P1 — Structured redaction pipeline

- Pipeline stages: secrets → paths → PII rules.  
- Stable token table **memory-only** (or encrypted session file, never to cloud).  
- Apply to: user message, tool results injected into conversation, maybe system snippets.  
- Tighter max bytes on remote tool injection.

### P2 — UI dual view + export scrub

- Operator can view local raw in TUI when mode allows.  
- `raven export-log --redact` for bug reports.  
- Never put reverse map in export.

### P3 — Smarter detection / domain packs

- Optional local NER for PII (medical pack).  
- Domain pack: healthcare / finance pattern lists.  
- Integration with future OS sandbox egress policy.

---

## Interaction with other Raven ideas

| Feature | Privacy interaction |
|---------|---------------------|
| **GitHub solution scout** | Off or ask in airgap; cloud mode OK | 
| **MCP** | Remote MCP = egress; airgap disables |
| **Session wiki** | Local disk; user responsibility; export redacted |
| **full_log / trace_log** | Local full; export redacted |
| **Vault keys** | Never appear in prompts (already principle) |
| **Plan clarify JSON** | Same pipeline as other LLM calls |

---

## Risks

| Risk | Mitigation |
|------|------------|
| Redaction false negative → leak to cloud | Secrets-first high-precision rules; never claim “HIPAA compliant” without audit |
| Redaction false positive → useless model | Prefer airgap for local; tune packs; allow “send full” override with confirm |
| Users think airgap = safe git | Docs: airgap ≠ “commit .env” |
| Token map leak | Memory-only / encrypted; not in wiki by default |
| Performance | Regex P0; NER optional |

**Legal note:** This design enables **technical** minimization. It is **not** a compliance certification (HIPAA, GDPR processing agreements, etc.).

---

## Success criteria

- [ ] User can run **local turbo**: full traces/prompts, remote LLM refused.  
- [ ] User can run **redact-cloud**: secrets (at least) never appear in outbound remote bodies (tested).  
- [ ] Switching endpoint forces an explicit privacy decision when risk increases.  
- [ ] Local quality not forced through cloud redaction path.  
- [ ] Export/share path can scrub even when live UI was full.  
- [ ] Operators see whether the **model** saw redacted or full tool output.

---

## Open questions

1. Default for first remote message: always prompt, or remember per-endpoint?  
2. Should wiki writes from the agent be redacted when mode is `redact_cloud`? (Probably **no**—wiki is local; optional.)  
3. Medical-grade de-identification: ship as optional **domain pack**, not core default.  
4. Multi-endpoint: local summarizer + cloud planner (redact between stages)—future “cascade” mode?

---

## Changelog

- **2026-07-16** — Initial design: medical-style cloud obfuscation, local turbo/air-gap, settings matrix, phased implementation.
