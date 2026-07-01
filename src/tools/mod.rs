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
                description: "Write a file. Set wiki=true to target session wiki root (path relative e.g. 'foo.md', do NOT prefix 'wiki/'). Wiki writes always allowed.".into(),
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
                description: "Search/replace edit. Set wiki=true for wiki root (path e.g. 'index.md' -- no 'wiki/' prefix).".into(),
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
