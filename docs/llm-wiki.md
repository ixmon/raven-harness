# LLM-Wiki: Structured Session Memory for Thinking & Collaboration

**Goal**: Give both you and the agent a persistent, link-rich place to collect and organize what was learned during a session — especially in `think`, `research`, and `dream` run modes.

The wiki is *externalized durable thinking surface*, separate from the live conversation trace and the compact `meta.json` fields (goal, pitfalls, discoveries, recent_turns_summary).

## Why Markdown (not HTML)

- The agent writes it naturally with the normal tools (or dedicated `*_wiki` tools).
- You can edit it with any editor (`vim`, `code`, `cat`).
- Native support for the things that matter here:
  - Links: `[Paper Title](https://arxiv.org/...)`
  - Tables for experiment results
  - Code fences + output
  - Headings and lists for structure
- HTML is great for "save the raw page", but the agent should distill into clean notes + prominent source links. We keep the URL, not a 200 kB blob.

Hybrid in practice: agent uses `web_search` → `browse` → writes distilled markdown + the URL.

## Storage (per-session, private)

```
~/.raven-hotel/sessions/<session-id>/
  meta.json
  full_log.jsonl
  context.db
  wiki/
    index.md
    sources.md
    experiments.md
    findings.md
    ...
```

- Private to the session (named sessions keep separate wikis).
- Does not pollute your workspace.
- You can always open it from outside the TUI.

The agent learns the location from the injected SESSION CONTEXT and is told to use the wiki tools (see below).

## Recommended Structure (starter template)

```markdown
# Research Wiki — <goal or topic>

## Open Questions
- 

## Key Links & Sources
- [Title or short desc](https://...) — one-line takeaway (YYYY-MM-DD)

## Hypotheses & Experiments
| Date       | Hypothesis                        | Command / Method          | Result                  | Conclusion |
|------------|-----------------------------------|---------------------------|-------------------------|------------|
| 2026-06-30 | ...                               | cargo test ...            | all pass / "output: X"  | ...        |

## Findings & Insights

## Pitfalls & Dead-ends (remember for later)

## Next Steps
```

The harness can seed this template the first time you switch into a research-oriented mode.

## How the Agent Uses It (think / research / dream)

In these modes the prompt tells the agent:
- Treat the wiki as your external brain / shared blackboard.
- `web_search` first, then `browse` the interesting pages.
- Distill: title + URL + 1-3 key claims or numbers + date.
- Use tables for any experiment / run / measurement.
- Always re-`read_wiki` the relevant section before editing it.
- Record links prominently.

The same tools the agent already knows (`web_search`, `browse`) become the "research input". The wiki tools become the "write to long-term memory" path.

## Tools the Agent Gets for the Wiki

Dedicated private tools (implemented like `record_discovery` and the summary cache tools — they never touch your workspace):

- `read_wiki` (path, optional lines/full) — relative to the wiki/ dir
- `write_wiki` (path, content)
- `list_wiki` (optional subdir)

These operate safely inside the session's wiki directory and auto-create parent dirs.

(Existing workspace `read`/`write` continue to work on your project. The wiki tools keep session data cleanly separated.)

## TUI Support (picker + viewer)

- The existing 3-pane picker (Workspaces | Sessions | Summary) already shows meta. It will also surface a short "Wiki preview" (key links or top of index.md) when present.
- From the picker or workspace you can open a basic wiki viewer (key `w` or similar) that renders the markdown nicely (headings, lists, links highlighted).
- Phase 2+ will add heading navigation, file switching inside wiki/, and "follow link" (external links trigger an in-TUI browse preview; relative links switch the viewed file).

You can still edit the files directly with any editor at any time; the TUI viewer has a reload action.

## Run Mode Tie-in

- `talk` — wiki is available but de-emphasized.
- `work` — focus is on action + verification (wiki secondary).
- `think` / `research` / `dream` — the agent is explicitly instructed to explore, cite sources, run experiments, and maintain the wiki. This is where the structure pays off.

You change mode with `/run-mode research` (or the menu). The mode is persisted in the session meta.

## Companion Tools (optional)

For power users who want a richer standalone viewer while the TUI adds features:
- `md-tui` (`cargo install md-tui`) — excellent keyboard-driven markdown browser.
- `glow`, `bat`, or `mdcat` for quick terminal rendering.

These are nice-to-haves. The integrated viewer inside raven-tui stays the primary experience so you never have to leave the session.

## Getting Started (manual for now)

1. Start or resume a session.
2. `/run-mode research`
3. Give the agent a research-oriented prompt.
4. Watch it call web tools then write to the wiki.
5. In the picker, select the session — the right summary pane will start showing wiki hints.
6. Press the wiki key (or open the file yourself) to read the structured notes with links.

Later phases will make discovery and navigation even smoother.

## Future Ideas (not in initial scope)

- Compact "wiki digest" rolled into the injection block (like recent_turns_summary).
- `grep_wiki` tool.
- One-click "record this browse result" helper.
- Image / PDF paper handling.
- Export the wiki as a nice report.

The core value is already there once the agent and you have a clean place to put the links and results that matter.

See also:
- `docs/agent-operating-modes.md`
- Session layout and `get_injection_block` in the code
- The web tools (`web_search`, `browse`) in `src/tools/web.rs`
