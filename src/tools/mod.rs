//! Tool system for the agent.
//!
//! We deliberately keep the surface small and high-signal for agentic coding:
//! exec, read, write, patch, grep, list, web_search, browse, download.
//! There is **no** `edit` tool — use `patch` for search/replace edits.
//!
//! Tool schemas are the standard OpenAI function format so they work with
//! llama.cpp and other OpenAI-compatible servers (including the one used by Raven Hotel).

use anyhow::Result;
use serde_json::json;

pub use self::exec::{
    command_uses_sudo, exec, exec_with_exit_code, is_privileged_package_install,
};
pub use self::fs::{grep_files, list_dir, patch_file, read_file, write_file};
pub use self::web::{
    brave_auth_disabled, browse, browse_urls, download_url, reset_brave_auth_disabled,
    take_brave_auth_ui_notice, web_search, web_search_reports_brave_auth_rejected,
    BRAVE_AUTH_REJECTED_MARKER,
};

pub mod backend;

mod exec;
mod fs;
mod web;

pub use backend::ToolBackend;

/// Safely truncate a string to at most `max_bytes` bytes without splitting
/// a multi-byte UTF-8 codepoint.
pub(crate) fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

use crate::llm::ToolDef;

/// Returns the complete list of tools the agent can use, in OpenAI function format.
/// Goal tracking / update_goal availability is controlled via `RuntimeFlags`.
pub fn all_tools(flags: &crate::runtime::RuntimeFlags) -> Vec<ToolDef> {
    let disable_update_goal = flags.disable_goal_tool || !flags.goal_tracking;

    let mut tools = vec![
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "exec".into(),
                description: "Run a shell command in the workspace. Use for cargo check, git status, tests, unzip, etc. For HTTP file downloads prefer the download tool (browser-like client) instead of curl/wget which often get bot-blocked. Before any package install, verify with non-sudo probes (dpkg -l, pkg-config, which). Avoid sudo — the TUI cannot enter passwords; ask the user to install system packages manually if needed.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute (bash). Will run with the workspace as cwd."
                        }
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "read".into(),
                description: "Read a file's contents. Set wiki=true to target the session wiki root (path relative e.g. \"index.md\"; NEVER prefix 'wiki/' or mkdir wiki/wiki). Use lines=... .".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path. If wiki=true then relative to the wiki ROOT ONLY (do not use 'wiki/xxx', 'wiki/wiki/xxx' or any prefix; just 'index.md' or 'notes/x.md')" },
                        "lines": { "type": "string", "description": "Optional range like \"10-40\" or \"1-\" for from start" },
                        "full": { "type": "boolean", "description": "If true, read as much as possible (bypasses small default cap)" },
                        "wiki": { "type": "boolean", "description": "If true, path is relative to the session's private research wiki (not the workspace)" }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "write".into(),
                description: "Write (or overwrite) a *file*. Path must point to a file, not a directory. Set wiki=true to target session wiki root (path relative e.g. 'foo.md', do NOT prefix 'wiki/'). Wiki writes always allowed. In 'plan' mode, non-wiki writes are denied until the user says 'proceed'.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path relative to workspace or (if wiki=true) to wiki ROOT. NEVER start with 'wiki/' or 'wiki/wiki/'." },
                        "content": { "type": "string", "description": "Full file content to write" },
                        "wiki": { "type": "boolean", "description": "Target the session wiki (relative paths, no 'wiki/' prefix)" }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "patch".into(),
                description: "Search/replace edit on a *file*. Path must be a file (not dir). Set wiki=true for wiki files (path e.g. 'index.md'). In 'plan' mode, non-wiki patches are denied until the user says 'proceed'.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path (relative to wiki ROOT if wiki=true; NEVER start with 'wiki/' or 'wiki/wiki/')" },
                        "search": { "type": "string", "description": "Exact text to find and replace (must match precisely)" },
                        "replace": { "type": "string", "description": "Replacement text" },
                        "near_line": { "type": "integer", "description": "Optional hint: the approximate line number of the occurrence you want (1-based)" },
                        "wiki": { "type": "boolean", "description": "If true, patches a file in the session's private research wiki" }
                    },
                    "required": ["path", "search", "replace"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "grep".into(),
                description: "Search for a pattern across files in the workspace. Returns matching lines with context. Use include to filter by file type (e.g. '*.rs', '*.py').".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Regex or literal pattern (case-insensitive). Use fixed=true for literal strings containing special chars." },
                        "path": { "type": "string", "description": "Optional subdirectory or file to limit the search" },
                        "include": { "type": "string", "description": "Glob filter e.g. '*.rs', '*.py', '*.toml'. Only search files matching this pattern." },
                        "context": { "type": "integer", "description": "Lines of context around each match (default 0, max 5)" },
                        "files_only": { "type": "boolean", "description": "If true, only list filenames containing matches (not the matching lines)" },
                        "fixed": { "type": "boolean", "description": "If true, treat pattern as a literal string (no regex). Useful for searching code with special chars like foo.bar() or arr[0]." }
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "list".into(),
                description: "List dir. wiki=true lists session wiki ROOT (path relative to it, e.g. '' or 'subdir'; do not use path='wiki/..').".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Subdir relative (or to wiki ROOT if wiki=true). NEVER prefix 'wiki/' or 'wiki/wiki/'." },
                        "wiki": { "type": "boolean", "description": "List the wiki directory instead" }
                    },
                    "required": []
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "web_search".into(),
                description: "Search the web. Returns titles, URLs, and short descriptions. Use this first to find pages, then browse() for full content.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query" },
                        "count": { "type": "integer", "description": "Number of results (1-10)" }
                    },
                    "required": ["query"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "browse".into(),
                description: "Fetch and extract the main text content of a web page. depth > 0 follows links (spider mode). Prefer extract=links when hunting for a direct .zip/.png URL.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "Full http(s) URL" },
                        "depth": { "type": "integer", "description": "0 = single page, 1+ = spider that many levels" },
                        "extract": { "type": "string", "description": "text (default), links, or html" }
                    },
                    "required": ["url"]
                }),
            },
        },
        // Session / context management tools (model can call these to keep long-running work on track)
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "browse_urls".into(),
                description: "Fetch and extract content from multiple URLs in parallel. Use after web_search to read the most promising results at once.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "urls": { "type": "array", "items": { "type": "string" }, "description": "List of full http(s) URLs to fetch" },
                        "extract": { "type": "string", "description": "text (default), links, or html" }
                    },
                    "required": ["urls"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "download".into(),
                description: "Download a file from an http(s) URL into the workspace (binary-safe). Uses a browser-like client — prefer this over exec curl/wget which often get bot-blocked. Pass a direct asset URL (e.g. raw.githubusercontent.com, github.com/.../archive/....zip). After download, use exec to unzip if needed.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "Full http(s) URL of the file to download" },
                        "path": { "type": "string", "description": "Workspace-relative destination file path (e.g. galaga/assets/sprites.zip)" }
                    },
                    "required": ["url", "path"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "update_goal".into(),
                description: "Update the tracked goal for this session, plus optional achievement tests and pitfalls. Call this when the user's intent clearly shifts or is refined.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "goal": { "type": "string", "description": "The new primary goal / objective" },
                        "tests": { "type": "array", "items": { "type": "string" }, "description": "Concrete ways to know the goal is achieved (optional)" },
                        "pitfalls": { "type": "array", "items": { "type": "string" }, "description": "Things to avoid or known risks (optional)" }
                    },
                    "required": ["goal"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "define_done".into(),
                description: "Define what 'done' looks like for this task (set once, early, derived from the *initial user request*). The judge uses this to decide completion and clears it when fulfilled. Only the agent can set it; only the judge clears it.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "definition": { "type": "string", "description": "Clear description of what success/completion looks like (e.g. 'the bug is fixed and all relevant tests pass')" }
                    },
                    "required": ["definition"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "record_discovery".into(),
                description: "Record an important finding, file, or insight so it is remembered across turns and restarts (goes into the session context block).".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "Concise description of the discovery or fact" }
                    },
                    "required": ["text"]
                }),
            },
        },
        // Wiki tools have been folded into read/write/patch/list via the wiki=true flag.
        // The wiki is a private per-session markdown store for research/think/dream modes.
        // File summary cache tools — use these to avoid repeatedly reading large or unchanged source files.
        // The cache lives in ~/.raven-hotel/{session}/context.db and is keyed by (relative_path, mtime).
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "read_summary".into(),
                description: "Read a cached summary for a file if its on-disk mtime has not changed since the summary was stored. On cache miss or stale mtime, returns the current mtime plus (capped) file content so you can produce and store a fresh summary.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file (relative to workspace)" }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "store_summary".into(),
                description: "Persist a concise summary for a file at the exact mtime obtained from a prior read_summary cache-miss response.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file (relative to workspace)" },
                        "mtime": { "type": "integer", "description": "The mtime value returned by the previous read_summary miss for this path" },
                        "summary": { "type": "string", "description": "Your concise summary of the file content" }
                    },
                    "required": ["path", "mtime", "summary"]
                }),
            },
        },
    ];

    if disable_update_goal {
        tools.retain(|t| t.function.name != "update_goal");
    }
    tools
}

/// Tools intended for use while the agent is in "plan" / clarification mode.
/// 
/// The goal is to let the agent explore the codebase and existing tests so it
/// can ask *better* questions, while still preventing it from making changes
/// to the main workspace until the user has approved the plan.
///
/// We deliberately give it read-oriented tools + update_goal + the ability
/// to write to the session wiki (for plan.md).
pub fn plan_mode_tools(flags: &crate::runtime::RuntimeFlags) -> Vec<ToolDef> {
    let disable_update_goal = flags.disable_goal_tool || !flags.goal_tracking;

    let mut tools = vec![
        // Exploration / understanding tools - very useful during planning
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "read".into(),
                description: "Read a file's contents. Set wiki=true to target the session wiki. Use this to understand existing code and tests so you can ask good clarification questions.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path (wiki=true: relative to wiki root only)" },
                        "lines": { "type": "string", "description": "Optional range like \"10-40\"" },
                        "full": { "type": "boolean" },
                        "wiki": { "type": "boolean" }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "grep".into(),
                description: "Search for a pattern across files. Great for finding how something is done in the existing code or tests during planning.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "path": { "type": "string" },
                        "include": { "type": "string" },
                        "context": { "type": "integer" },
                        "fixed": { "type": "boolean" }
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "list".into(),
                description: "List a directory. Use wiki=true for the session wiki.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "wiki": { "type": "boolean" }
                    }
                }),
            },
        },
        // Web research tools can still be useful while planning
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "web_search".into(),
                description: "Search the web. Returns titles, URLs, and short descriptions.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "count": { "type": "integer" }
                    },
                    "required": ["query"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "browse".into(),
                description: "Fetch content from a URL. Useful for researching approaches while planning.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" },
                        "depth": { "type": "integer" },
                        "extract": { "type": "string" }
                    },
                    "required": ["url"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "browse_urls".into(),
                description: "Fetch multiple URLs in parallel.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "urls": { "type": "array", "items": { "type": "string" } },
                        "extract": { "type": "string" }
                    },
                    "required": ["urls"]
                }),
            },
        },
        // Planning-specific state tools
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "update_goal".into(),
                description: "Update the tracked goal and success criteria for the plan you are developing.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "goal": { "type": "string" },
                        "tests": { "type": "array", "items": { "type": "string" } },
                        "pitfalls": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["goal"]
                }),
            },
        },
        // Wiki write is how the agent maintains the living plan.md during clarification
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "write".into(),
                description: "Write a file. In plan mode you should almost always use wiki=true to write to the session's plan.md.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" },
                        "wiki": { "type": "boolean" }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        // Exec is allowed in plan mode *for environment and dependency checks only*
        // (compilers present? needed libs? disk space? verification tools?).
        // Do not use it to build or run the main deliverable until the plan is approved.
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "exec".into(),
                description: "Run a shell command (for planning: check compilers, libraries, disk space, versions). Use non-sudo probes first (dpkg -l, pkg-config, which). No sudo — TUI cannot enter passwords.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute (bash). Will run with the workspace as cwd."
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "Optional timeout in seconds (default 60)"
                        }
                    },
                    "required": ["command"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "patch".into(),
                description: "Edit a file with search/replace. Prefer wiki=true in plan mode.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "search": { "type": "string" },
                        "replace": { "type": "string" },
                        "near_line": { "type": "integer" },
                        "wiki": { "type": "boolean" }
                    },
                    "required": ["path", "search", "replace"]
                }),
            },
        },
    ];

    if disable_update_goal {
        tools.retain(|t| t.function.name != "update_goal");
    }
    tools
}

/// Comma-separated tool names in schema order (for system prompt / help text).
pub fn format_tool_names(tools: &[ToolDef]) -> String {
    tools
        .iter()
        .map(|t| t.function.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether an approved plan is actively executing (work mode + steps populated).
#[derive(Clone, Copy, Debug, Default)]
pub struct PlanToolContext {
    pub plan_executing: bool,
}

fn revise_plan_step_tool() -> ToolDef {
    ToolDef {
        r#type: "function".into(),
        function: crate::llm::ToolFunction {
            name: "revise_plan_step".into(),
            description: "Edit one step in wiki/plan.md JSON (tier, verification, prompt, note, description, workdir). Plan or executing work mode.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "step": { "type": "integer", "description": "1-indexed step number" },
                    "description": { "type": "string" },
                    "tier": { "type": "string", "description": "exec | check | attested | observe" },
                    "verification": { "type": "string" },
                    "prompt": { "type": "string", "description": "User question for observe tier" },
                    "note": { "type": "string" },
                    "workdir": { "type": "string", "description": "Project subdirectory for verifications (executing mode only)" }
                },
                "required": ["step"]
            }),
        },
    }
}

fn complete_plan_step_tool() -> ToolDef {
    ToolDef {
        r#type: "function".into(),
        function: crate::llm::ToolFunction {
            name: "complete_plan_step".into(),
            description: "Mark the current plan step complete after verification. Work mode with approved plan only.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "step": { "type": "integer", "description": "1-indexed step (must match current step)" },
                    "evidence": { "type": "string", "description": "Required for attested tier" },
                    "force": { "type": "boolean", "description": "Skip verification gate (discouraged)" }
                }
            }),
        },
    }
}

/// Tools for the current agent mode and plan execution state.
pub fn tools_for_agent(
    agent_mode: &str,
    flags: &crate::runtime::RuntimeFlags,
    plan_ctx: PlanToolContext,
) -> Vec<ToolDef> {
    match agent_mode {
        "plan" => {
            let mut tools = plan_mode_tools(flags);
            tools.push(revise_plan_step_tool());
            tools
        }
        "work" => {
            let mut tools = all_tools(flags);
            if plan_ctx.plan_executing {
                tools.push(complete_plan_step_tool());
                tools.push(revise_plan_step_tool());
            }
            tools
        }
        // Talk: full tools for optional help, but no goal machinery (avoids re-seeding
        // a stale current_goal mid-conversation). Super Judge already skips talk.
        "talk" => {
            let mut tools = all_tools(flags);
            tools.retain(|t| {
                t.function.name != "update_goal" && t.function.name != "define_done"
            });
            tools
        }
        _ => all_tools(flags),
    }
}

/// Read/verify tools only — Super Judge must not edit the workspace.
pub fn tools_for_super_judge(flags: &crate::runtime::RuntimeFlags) -> Vec<ToolDef> {
    const ALLOWED: &[&str] = &[
        "read",
        "read_summary",
        "list",
        "grep",
        "exec",
        "web_search",
        "browse",
        // no download — Super Judge is review-only (no workspace file writes)
    ];
    all_tools(flags)
        .into_iter()
        .filter(|t| ALLOWED.contains(&t.function.name.as_str()))
        .collect()
}

/// Tool names exposed to the model for the current run mode.
pub fn tools_list_for_prompt(
    agent_mode: &str,
    flags: &crate::runtime::RuntimeFlags,
    plan_ctx: PlanToolContext,
) -> String {
    format_tool_names(&tools_for_agent(agent_mode, flags, plan_ctx))
}

/// Execute a tool call (name + JSON arguments string) and return the result as a string
/// that will be fed back to the model as a `tool` message.
pub async fn execute(
    backend: &ToolBackend,
    name: &str,
    arguments: &str,
    workspace: &std::path::Path,
    max_read_lines: usize,
    brave_key: Option<String>,
) -> Result<String> {
    backend
        .execute(name, arguments, workspace, max_read_lines, brave_key)
        .await
}

#[cfg(test)]
mod tests {
    use super::safe_truncate;

    #[test]
    fn test_safe_truncate_basic() {
        assert_eq!(safe_truncate("hello", 10), "hello");
        assert_eq!(safe_truncate("hello", 3), "hel");
        assert_eq!(safe_truncate("", 5), "");
    }

    fn plan_mode_test_flags() -> crate::runtime::RuntimeFlags {
        crate::runtime::RuntimeFlags {
            goal_tracking: true,
            ..crate::runtime::RuntimeFlags::default()
        }
    }

    #[test]
    fn plan_mode_tool_names_match_schema() {
        let flags = plan_mode_test_flags();
        let names = super::format_tool_names(&super::plan_mode_tools(&flags));
        assert!(names.contains("read"));
        assert!(names.contains("exec"));
        assert!(names.contains("update_goal"));
        assert!(!names.contains("define_done"));
        assert!(!names.contains("record_discovery"));
        assert!(!names.contains("read_summary"));
    }

    #[test]
    fn tools_list_for_prompt_plan_vs_work() {
        let flags = plan_mode_test_flags();
        let plan = super::tools_list_for_prompt("plan", &flags, super::PlanToolContext::default());
        let work = super::tools_list_for_prompt("work", &flags, super::PlanToolContext::default());
        let work_exec = super::tools_list_for_prompt(
            "work",
            &flags,
            super::PlanToolContext {
                plan_executing: true,
            },
        );
        assert!(!plan.contains("define_done"));
        assert!(plan.contains("revise_plan_step"));
        assert!(!plan.contains("complete_plan_step"));
        assert!(work.contains("define_done"));
        assert!(work.contains("exec"));
        assert!(!work.contains("complete_plan_step"));
        assert!(work_exec.contains("complete_plan_step"));
        assert!(work_exec.contains("revise_plan_step"));
    }

    #[test]
    fn tools_for_agent_mode_scoping() {
        let flags = plan_mode_test_flags();
        let plan_names = super::format_tool_names(&super::tools_for_agent(
            "plan",
            &flags,
            super::PlanToolContext::default(),
        ));
        assert!(plan_names.contains("revise_plan_step"));
        assert!(!plan_names.contains("complete_plan_step"));

        let work_names = super::format_tool_names(&super::tools_for_agent(
            "work",
            &flags,
            super::PlanToolContext {
                plan_executing: true,
            },
        ));
        assert!(work_names.contains("complete_plan_step"));
        assert!(work_names.contains("revise_plan_step"));

        let talk_names = super::format_tool_names(&super::tools_for_agent(
            "talk",
            &flags,
            super::PlanToolContext::default(),
        ));
        assert!(talk_names.contains("exec") || talk_names.contains("read"));
        assert!(
            !talk_names.contains("update_goal"),
            "talk must not expose update_goal"
        );
        assert!(
            !talk_names.contains("define_done"),
            "talk must not expose define_done"
        );
    }

    #[test]
    fn tools_for_super_judge_excludes_write_and_patch() {
        let flags = plan_mode_test_flags();
        let names: Vec<String> = super::tools_for_super_judge(&flags)
            .into_iter()
            .map(|t| t.function.name)
            .collect();
        assert!(names.contains(&"read".into()));
        assert!(names.contains(&"exec".into()));
        assert!(!names.iter().any(|n| n == "write"));
        assert!(!names.iter().any(|n| n == "patch"));
        assert!(!names.iter().any(|n| n == "download"));
        assert!(!names.iter().any(|n| n == "complete_plan_step"));
    }

    #[test]
    fn all_tools_includes_download() {
        let flags = plan_mode_test_flags();
        let names = super::format_tool_names(&super::all_tools(&flags));
        assert!(names.contains("download"));
        assert!(names.contains("browse"));
    }

    #[test]
    fn test_safe_truncate_utf8_boundary() {
        // "café" = c a f é  (é is 2 bytes in UTF-8)
        let s = "café";
        // Cut in the middle of é (at byte 3 would be inside the char)
        let t = safe_truncate(s, 3);
        assert!(t.len() <= 3);
        assert!(std::str::from_utf8(t.as_bytes()).is_ok());
        assert_eq!(t, "caf"); // é skipped cleanly

        // Multi-char boundary safety
        let emoji = "hello😀world"; // 😀 is 4 bytes
        let t2 = safe_truncate(emoji, 7); // inside the emoji?
        assert!(std::str::from_utf8(t2.as_bytes()).is_ok());
        // Should not split the emoji
        assert!(t2.ends_with("hello") || t2 == "hello");
    }
}
