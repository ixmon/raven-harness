# Project secrets & vault ideas

**Branch:** `feat/project-secrets-vault`  
**Status:** design / backlog — no implementation yet  
**Motivation:** During real work (install scripts, private registries, cloud CLIs, download tokens), the agent needs credentials that are **not** the inference endpoint key and **not** the global Brave key. Those should live in the **same encrypted vault** (`~/.raven-hotel/endpoints.json` AES-GCM + Argon2id), scoped so the wrong project does not see the wrong secret.

Related existing pieces:

- Keystore: endpoint API keys + `brave_api_key` (`src/keystore.rs`)
- Session meta: goal / discoveries (unencrypted) — **must not** store secrets
- Env: `BRAVE_API_KEY`, `RAVEN_VAULT_PASSWORD` — escape hatches, not the long-term model

---

## 1. Primary feature: project-scoped secrets

### Problem

Today the vault is global:

| Stored | Scope |
|--------|--------|
| Inference endpoint keys | Global |
| Brave Search key | Global |
| *(nothing else)* | — |

Real sessions need things like:

- GitHub / GitLab token for private clones or `gh`
- npm / PyPI / crates private registry token
- AWS / cloud access key (or profile name + encrypted secret)
- Docker registry password
- One-off download cookies / asset CDN tokens
- DB URL for a **this repo** integration test

Without a store, the agent either:

1. Asks every session (bad UX), or  
2. Puts secrets in wiki / plan / env files in the workspace (dangerous), or  
3. Hopes the user exported env vars outside Raven

### Shape (proposed)

Extend the keystore file (or a sibling `secrets.json` next to it, same password):

```json
{
  "salt": "...",
  "endpoints": [ ... ],
  "brave_api_key": "...",
  "project_secrets": [
    {
      "id": "uuid",
      "project_key": "git:github.com/ixmon/raven-harness",
      "name": "gh_token",
      "encrypted_value": "base64(nonce||ciphertext)",
      "created_at": "...",
      "updated_at": "...",
      "notes": "optional non-secret hint"
    }
  ]
}
```

**Project key** resolution (first match wins at runtime):

1. Explicit workspace override: `RAVEN_PROJECT_KEY` or settings field  
2. Git remote origin: `git:host/owner/repo` (normalized)  
3. Canonical workspace path hash: `path:<sha256(abs path)>` as fallback when not a git repo  

**Name** is a short id the agent/tools use (`gh_token`, `npm_token`, `aws_access_key_id`). Values never appear in tool *schemas* as examples of real secrets.

### Agent / tool surface

Prefer a **narrow** tool API (not “dump all secrets”):

| Tool | Behavior |
|------|----------|
| `secret_list` | Names (+ notes) for **current project only** — no values |
| `secret_get(name)` | Decrypt value for current project; requires vault unlocked; redacted in trace by default |
| `secret_set(name, value)` | Encrypt + store; user approval mode applies (always confirm in Babysitter) |
| `secret_delete(name)` | Remove |

**UI:** Settings → “Project secrets” when a workspace is active; or `/secrets` slash command.

**Injection policy:** Do **not** put secret values in the system prompt or `get_injection_block()`. Only: “Project secrets available: `gh_token`, `npm_token` (use `secret_get`).”

### Security rules (non-negotiable)

1. Same vault password as endpoints; locked when vault locked.  
2. Values never written to `full_log.jsonl` / trace in cleartext (mask like we should for API keys).  
3. Wiki / plan.md / discoveries **refuse** or scrub secret-shaped strings when possible.  
4. Super Judge / export / “share session” paths exclude decrypted material.  
5. Optional: mark secret as `once` (read once then delete) for installers.  
6. Optional: TTL / “expires_at” for temporary tokens.

### Implementation sketch

| Area | Work |
|------|------|
| `KeystoreFile` | `project_secrets: Vec<StoredProjectSecret>` |
| Keystore API | get/set/list/delete by `(project_key, name)` |
| Workspace | resolve `project_key` once per session open |
| Tools | list/get/set/delete; approval on set/delete/get if policy says so |
| Settings UI | list names, add/edit (password field), delete |
| Redaction | extend sanitize / trace masking for secret values in memory |

### Open questions

- One vault file vs per-project encrypted files under `~/.raven-hotel/secrets/<project_key>/`?  
  - **Single file:** simpler unlock; larger blast radius if password leaked.  
  - **Per-project files:** better isolation; more I/O and migration.  
- Should **session** inherit only a allowlist of names declared in `meta` / plan constraints?  
- Cross-machine sync? (Out of scope; local-first.)

---

## 2. One-off / adjacent ideas

Ideas that showed up while thinking about project credentials — each is independently shippable.

### 2.1 Named env profiles per project

Store non-secret **and** secret env as a profile:

```text
RAVEN_ENV[project] = { "DATABASE_URL": <encrypted>, "RUST_LOG": "info" }
```

On session start (or `/env load`), export into the **agent exec** environment only (not the host shell permanently). Closest UX to “direnv + vault.”

### 2.2 Credential-backed tool presets

Map secret names to tool behavior:

| Secret name convention | Behavior |
|------------------------|----------|
| `gh_token` | `exec` of `gh` / `git` gets `GH_TOKEN` / `GITHUB_TOKEN` injected for that process only |
| `brave_api_key` | already global; could allow project override |
| `aws_*` | inject into `aws` CLI env for one exec |

Reduces “agent must call `secret_get` then paste into command line” (pasting is a leak vector in logs).

### 2.3 Prompted capture on 401/403

When `exec` / `download` / `web_search` returns auth failure:

> “Looks like auth failed for github.com. Store a token for this project? [Save] [Skip]”

One-shot modal → `secret_set`. Complements Brave auth-disable UX.

### 2.4 Secret-aware redaction in logs

Maintain a session bloom/list of decrypted secret values (or hashes) and redact any echo in tool results before append_log / UI. Required once `secret_get` exists.

### 2.5 “Deploy key” / SSH private key slot

Optional secret type `ssh_private_key` written to a **temp file** with `0600` for a single `git`/`ssh` invocation, then wiped. Never leave keys in the workspace tree.

### 2.6 Plan-step verification with secrets

Plan recipes should **never** embed tokens. Allow:

```text
verification: env:GH_TOKEN | gh api user
```

Harness injects secret into env for the verify subprocess only.

### 2.7 Discoveries vs secrets (discipline)

`record_discovery` and Super Judge feedback must not store tokens. Lint: if text matches `ghp_`, `sk-`, `AKIA`, `xoxb-`, etc., block or force “store as secret instead?”

### 2.8 Multi-project workspaces

Workspace root with multiple git submodules: allow secrets keyed by **subdir** project_key, selected by current plan `project_workdir`.

### 2.9 Export / backup

`/secrets export` → encrypted blob (same password or separate backup password) for moving machines. Import merges by `(project_key, name)`.

### 2.10 Audit trail (metadata only)

Append-only log in vault or session: `secret_get gh_token at 12:04 by agent` — **no values**. Helps “who used the deploy key?” without leaking material.

### 2.11 Global vs project Brave (and other global APIs)

Allow project override of Brave key for org-proxied search; fall back to global. Same pattern for future third-party search.

### 2.12 Human-only secrets

Flag `agent_readable: false` — only settings UI / user can view; agent tools get “denied (human-only)”. For break-glass prod keys.

---

## 3. Suggested delivery order

| Phase | Deliverable | Why |
|-------|-------------|-----|
| **A** | Schema + keystore CRUD + project_key resolution | Foundation |
| **B** | Tools list/get/set + redaction in logs | Agent can use it safely |
| **C** | Settings UI / `/secrets` | Humans can manage without tools |
| **D** | Exec env injection presets (`gh_token`, etc.) | Fewer leaky command lines |
| **E** | 401 capture modal + discovery lint | Closes the loop from failure → store |
| **F** | Plan verification `env:NAME \| cmd` | Plans stay token-free |

---

## 4. Non-goals (for this branch)

- Cloud KMS / 1Password / Bitwarden sync (maybe later as backends behind the same API)  
- Sharing secrets across users  
- Storing secrets **inside** the workspace git tree  
- Replacing OS keyrings (optional future backend)

---

## 5. Success criteria (when we implement)

1. Unlock vault once → agent can `secret_get` for **this** project only.  
2. Other projects’ secret **names** are invisible (or at least values unreachable).  
3. Trace/log of a turn that used a secret does not contain the plaintext value.  
4. User can add/remove secrets without editing JSON by hand.  
5. Ubuntu/curl-install users: no behavior change until they store a project secret.

---

## 6. References

- `src/keystore.rs` — encrypt/decrypt, Brave key pattern to copy  
- `docs/context-ideas.md` — vault vs session meta boundary  
- Session learning: Brave 422 auth + empty discoveries — secrets need a first-class path, not ad-hoc meta fields  
