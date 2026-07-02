# AGENTS.md — Instructions for AI Coding Agents

This file contains guidelines for AI agents (including myself) working on the Raven TUI.

**Work from inside the `tui/` directory** when running cargo commands.

## Mandatory Pre-PR Checks

Before considering a change complete or proposing a PR, the following **must** pass cleanly:

```bash
cd tui

# No errors
cargo check --no-default-features

# No warnings or errors
cargo clippy --no-default-features -- -D warnings
```

Additional recommended checks:

```bash
# Run relevant tests
cargo test --no-default-features

# Ensure a clean release build
cargo build --release --no-default-features -q
```

If either of the two main commands above produces any output (errors or warnings), fix them before stopping.

## Core Development Principles

- **No direct terminal output while the TUI is active.** Once `enable_raw_mode()` + alternate screen is in use, do **not** use `println!`, `eprintln!`, `print!`, or write raw bytes to stdout/stderr. This will corrupt the display. All agent-visible output must go through tool results or the `UiUpdate` channel.

- **Prefer `patch` over `write`** for code changes. Always call `read` (or `read` with a `lines=` range) to get the *exact current text* immediately before calling `patch`. Use a sufficiently unique `search` string. Use `near_line` when there are multiple similar matches.

- **Context efficiency matters.** Use `read_summary` before raw `read` when appropriate. Be thoughtful about what goes into the prompt.

- **Use the private wiki** (`wiki=true` on read/write/patch/list tools) for research notes, findings, hypotheses, experiment results, and long-term memory. This is the preferred place to externalize thinking during `think`/`research`/`dream` modes.

- **Keep the harness simple and improvable.** The goal is a transparent, local-first agent harness that can itself be evolved by agents.

## Common Commands

```bash
cd tui

# Interactive TUI (recommended)
cargo run -q --

# One-shot / non-interactive
cargo run -q -- --prompt "Your task here..."

# Skip vault password prompt (useful for agents)
RAVEN_VAULT_PASSWORD=raven cargo run -q --
```

Always prefer `cargo run -q` for interactive work so compiler noise doesn't interfere with the TUI.

## File Locations & Documentation

- `tui/README.md` — high-level overview and installation
- `tui/docs/` — design notes and ideas (llm-wiki.md, agent-operating-modes.md, etc.)
- Session data lives in `~/.raven-hotel/sessions/<id>/` (including the private `wiki/` directory)
- Persistent settings and encrypted keys are in `~/.raven-hotel/`

## Tool Usage Reminders

- Use the `patch` tool for edits (not a non-existent "edit" tool).
- Wiki tools require `wiki=true` + relative paths (never prefix with `wiki/`).
- The `exec` tool captures both stdout and stderr — do not rely on side effects visible on the host terminal.
- Approval modes (`/mode`) control what the agent can do without human confirmation.

When in doubt, read the relevant source first, then make the smallest precise change possible.

---

These instructions apply to any agent (local or cloud) working on this codebase. Keep this file up to date as the project evolves.