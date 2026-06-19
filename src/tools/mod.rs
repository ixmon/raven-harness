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
pub use self::fs::{read_file, write_file, patch_file, grep_files, list_dir};
pub use self::web::{web_search, browse};

mod exec;
mod fs;
mod web;

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
pub fn all_tools() -> Vec<ToolDef> {
    vec![
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
                description: "Read a file's contents. Use lines=\"N-M\" for a specific range. Always read before editing.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file (relative to workspace or absolute)" },
                        "lines": { "type": "string", "description": "Optional range like \"10-40\" or \"1-\" for from start" }
                    },
                    "required": ["path"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "write".into(),
                description: "Write (overwrite) a file. Prefer patch for modifications to existing code.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Destination path" },
                        "content": { "type": "string", "description": "Full file content to write" }
                    },
                    "required": ["path", "content"]
                }),
            },
        },
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "patch".into(),
                description: "Safe search-and-replace edit. Strongly preferred over write for modifications. Use near_line when the search text appears multiple times.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File to edit" },
                        "search": { "type": "string", "description": "Exact text to find and replace (must match precisely)" },
                        "replace": { "type": "string", "description": "Replacement text" },
                        "near_line": { "type": "integer", "description": "Optional hint: the approximate line number of the occurrence you want (1-based)" }
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
                description: "List files and directories in a path (relative to workspace). Great for exploration.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Directory to list (default: workspace root)" }
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
        // File summary cache tools — use these to avoid repeatedly reading large or unchanged source files.
        // The cache lives in ~/.raven-hotel/{session}/context.db and is keyed by (relative_path, mtime).
        ToolDef {
            r#type: "function".into(),
            function: crate::llm::ToolFunction {
                name: "read_summary".into(),
                description: "Check the mtime-matched summary cache first. Returns a fresh cached summary if the file has not changed since the summary was stored. If stale or missing, returns the current mtime plus a capped raw view and instructions to call store_summary after you analyze it. Always prefer this over raw 'read' for understanding code.".into(),
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
                description: "Cache a concise, factual summary you produced for a file. You must pass the exact mtime you observed when you analyzed the file. Future calls to read_summary for the same mtime will return this instead of the full source.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file (relative to workspace)" },
                        "mtime": { "type": "integer", "description": "The mtime (unix seconds) of the file at the time you read/analyzed it" },
                        "summary": { "type": "string", "description": "Concise summary (< ~500 tokens). Include purpose, key public items/signatures, core logic, dependencies, and any non-obvious behavior. Be factual." }
                    },
                    "required": ["path", "mtime", "summary"]
                }),
            },
        },
    ]
}

/// Execute a tool call (name + JSON arguments string) and return the result as a string
/// that will be fed back to the model as a `tool` message.
pub async fn execute(name: &str, arguments: &str, workspace: &std::path::Path, max_read_lines: usize) -> Result<String> {
    // Arguments come as a JSON string from the model (may contain escaped stuff).
    let args: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(_) => {
            // Try to fix common issues (unquoted keys etc.) — very defensive
            let fixed = arguments
                .replace("\\$", "$");
            serde_json::from_str(&fixed).unwrap_or(json!({}))
        }
    };

    match name {
        "exec" => {
            let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            Ok(exec(cmd, workspace).await)
        }
        "read" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let lines = args.get("lines").and_then(|v| v.as_str());
            Ok(read_file(path, lines, workspace, max_read_lines))
        }
        "write" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            Ok(write_file(path, content, workspace))
        }
        "patch" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let search = args.get("search").and_then(|v| v.as_str()).unwrap_or("");
            let replace = args.get("replace").and_then(|v| v.as_str()).unwrap_or("");
            let near_line = args.get("near_line").and_then(|v| v.as_i64());
            Ok(patch_file(path, search, replace, near_line, workspace))
        }
        "grep" => {
            let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let path = args.get("path").and_then(|v| v.as_str());
            Ok(grep_files(pattern, path, workspace))
        }
        "list" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            Ok(list_dir(path, workspace))
        }
        "web_search" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(6) as usize;
            // Must run blocking reqwest on a separate thread to avoid "Cannot drop a runtime
            // in a context where blocking is not allowed" when the blocking Client is dropped
            // from inside a tokio worker thread.
            let res = tokio::task::spawn_blocking(move || web_search(&query, count))
                .await
                .unwrap_or_else(|e| format!("web_search join error: {}", e));
            Ok(res)
        }
        "browse" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let extract = args.get("extract").and_then(|v| v.as_str()).unwrap_or("text");
            Ok(browse(url, depth, extract).await)
        }
        // These are handled specially by the Agent (they mutate session meta or the file summary cache).
        // We still return a friendly ack here so the tool result path stays uniform.
        "update_goal" => {
            let goal = args.get("goal").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Ok(format!("✅ Goal update requested: {}", safe_truncate(&goal, 80)))
        }
        "record_discovery" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Ok(format!("✅ Discovery recorded: {}", safe_truncate(&text, 80)))
        }
        "read_summary" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            Ok(format!("✅ read_summary requested for {}", path))
        }
        "store_summary" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            Ok(format!("✅ store_summary requested for {}", path))
        }
        other => Ok(format!("❌ Unknown tool: {}", other)),
    }
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
        assert_eq!(t, "caf");  // é skipped cleanly

        // Multi-char boundary safety
        let emoji = "hello😀world"; // 😀 is 4 bytes
        let t2 = safe_truncate(emoji, 7); // inside the emoji?
        assert!(std::str::from_utf8(t2.as_bytes()).is_ok());
        // Should not split the emoji
        assert!(t2.ends_with("hello") || t2 == "hello");
    }
}
