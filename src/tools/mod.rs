//! Tool system for the agent.
//!
//! We deliberately keep the surface small and high-signal for agentic coding:
//! exec, read, write, patch, grep, list, web_search, browse.
//!
//! Tool schemas are the standard OpenAI function format so they work with
//! llama.cpp and other OpenAI-compatible servers (including the one used by Raven Hotel).

use anyhow::Result;
use serde_json::json;

pub use self::exec::exec;
pub use self::fs::{grep_files, list_dir, patch_file, read_file, write_file};
pub use self::web::{browse, web_search};

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
                description: "Run a shell command in the workspace. Use for cargo check, git status, tests, etc. No sudo. Prefer non-interactive commands.".into(),
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
                description: "Read a file's contents. Use lines=\"N-M\" or full=true to read more/entire file (useful for refactoring). Always read before editing. Set wiki=true to read from your private session wiki instead of the workspace.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file (relative to workspace, or relative to wiki/ if wiki=true)" },
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
                description: "Write (overwrite) a file. Prefer patch for modifications to existing code. Set wiki=true to write to your private session wiki (always allowed, no approval needed).".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Destination path (or relative to wiki/ if wiki=true)" },
                        "content": { "type": "string", "description": "Full file content to write" },
                        "wiki": { "type": "boolean", "description": "If true, writes to the session's private research wiki (always allowed, no approval needed)" }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "patch".into(),
                description: "Safe search-and-replace edit. Strongly preferred over write for modifications. Use near_line when the search text appears multiple times. Set wiki=true to patch a file in your private session wiki.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File to edit (or relative to wiki/ if wiki=true)" },
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
                description: "Search for a pattern across files in the workspace. Returns matching lines with context.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Regex or literal pattern (case-insensitive)" },
                        "path": { "type": "string", "description": "Optional subdirectory or file to limit the search" }
                    },
                    "required": ["pattern"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "list".into(),
                description: "List files and directories in a path (relative to workspace). Great for exploration. Set wiki=true to list the session's private research wiki.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory to list (default: workspace root, or wiki root if wiki=true)" },
                        "wiki": { "type": "boolean", "description": "If true, lists the session's private research wiki directory" }
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
                description: "Fetch and extract the main text content of a web page. depth > 0 follows links (spider mode).".into(),
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

/// Execute a tool call (name + JSON arguments string) and return the result as a string
/// that will be fed back to the model as a `tool` message.
pub async fn execute(
    backend: &ToolBackend,
    name: &str,
    arguments: &str,
    workspace: &std::path::Path,
    max_read_lines: usize,
) -> Result<String> {
    backend
        .execute(name, arguments, workspace, max_read_lines)
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
