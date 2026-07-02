//! Tool execution backends — real workspace/shell/network vs eval mocks.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use super::{
    browse, browse_urls, exec, grep_files, list_dir, patch_file, read_file, safe_truncate, web_search,
    write_file,
};

/// How tool calls are fulfilled (real side effects vs scripted eval responses).
#[derive(Clone, Debug, Default)]
pub enum ToolBackend {
    #[default]
    Real,
    Mock(MockToolBackend),
}

/// Scripted tool responses for harness smoke (`RAVEN_EVAL=1`).
#[derive(Clone, Debug, Default)]
pub struct MockToolBackend {
    responses: HashMap<String, HashMap<String, String>>,
}

fn resolve_mock_value(s: &str) -> String {
    const PREFIX: &str = "@file:";
    if let Some(rel) = s.strip_prefix(PREFIX) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        return std::fs::read_to_string(&path).unwrap_or_else(|e| {
            format!("[eval mock] could not read fixture {}: {e}", path.display())
        });
    }
    s.to_string()
}

impl MockToolBackend {
    pub fn from_json(tools: &Value) -> Self {
        let mut responses = HashMap::new();
        let Some(obj) = tools.as_object() else {
            return Self { responses };
        };
        for (tool, entries) in obj {
            let mut map = HashMap::new();
            if let Some(e) = entries.as_object() {
                for (k, v) in e {
                    if let Some(s) = v.as_str() {
                        map.insert(k.clone(), resolve_mock_value(s));
                    }
                }
            } else if let Some(s) = entries.as_str() {
                map.insert("default".into(), resolve_mock_value(s));
            }
            responses.insert(tool.clone(), map);
        }
        Self { responses }
    }

    fn lookup_key(name: &str, args: &Value) -> String {
        match name {
            "read" | "write" | "patch" | "read_summary" | "store_summary" => args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("default")
                .to_string(),
            "grep" => args
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("default")
                .to_string(),
            "list" => args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".")
                .to_string(),
            "exec" => args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("default")
                .to_string(),
            "web_search" => args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("default")
                .to_string(),
            "browse" => args
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("default")
                .to_string(),
            _ => "default".into(),
        }
    }

    fn lookup(&self, name: &str, args: &Value) -> Option<String> {
        let map = self.responses.get(name)?;
        let key = Self::lookup_key(name, args);
        map.get(&key)
            .or_else(|| map.get("*"))
            .or_else(|| map.get("default"))
            .cloned()
    }

    pub fn execute_sync(
        &self,
        name: &str,
        args: &Value,
        workspace: &Path,
        max_read_lines: usize,
    ) -> Result<String> {
        if let Some(r) = self.lookup(name, args) {
            return Ok(r);
        }

        // Session tools are handled by Agent; keep acks uniform if they slip through.
        match name {
            "read" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let lines = args.get("lines").and_then(|v| v.as_str());
                let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
                return Ok(read_file(path, lines, workspace, max_read_lines, full));
            }
            "update_goal" => {
                let goal = args.get("goal").and_then(|v| v.as_str()).unwrap_or("");
                return Ok(format!(
                    "✅ Goal update requested: {}",
                    safe_truncate(goal, 80)
                ));
            }
            "record_discovery" => {
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
                return Ok(format!(
                    "✅ Discovery recorded: {}",
                    safe_truncate(text, 80)
                ));
            }

            "read_summary" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                return Ok(format!("✅ read_summary requested for {}", path));
            }
            "store_summary" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                return Ok(format!("✅ store_summary requested for {}", path));
            }
            _ => {}
        }

        Ok(format!(
            "[eval mock] no scripted response for `{}` (workspace: {}, max_read_lines: {})",
            name,
            workspace.display(),
            max_read_lines
        ))
    }
}

impl ToolBackend {
    pub async fn execute(
        &self,
        name: &str,
        arguments: &str,
        workspace: &Path,
        max_read_lines: usize,
        brave_key: Option<String>,
    ) -> Result<String> {
        let args = parse_tool_args(arguments);

        match self {
            Self::Mock(m) => m.execute_sync(name, &args, workspace, max_read_lines),
            Self::Real => real_execute(name, &args, workspace, max_read_lines, brave_key).await,
        }
    }
}

fn parse_tool_args(arguments: &str) -> Value {
    match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(_) => {
            let fixed = arguments.replace("\\$", "$");
            serde_json::from_str(&fixed).unwrap_or(json!({}))
        }
    }
}

async fn real_execute(
    name: &str,
    args: &Value,
    workspace: &Path,
    max_read_lines: usize,
    brave_key: Option<String>,
) -> Result<String> {
    match name {
        "exec" => {
            let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            Ok(exec(cmd, workspace).await)
        }
        "read" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let lines = args.get("lines").and_then(|v| v.as_str());
            let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            Ok(read_file(path, lines, workspace, max_read_lines, full))
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
            let include = args.get("include").and_then(|v| v.as_str());
            let context = args.get("context").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let files_only = args.get("files_only").and_then(|v| v.as_bool()).unwrap_or(false);
            let fixed = args.get("fixed").and_then(|v| v.as_bool()).unwrap_or(false);
            Ok(grep_files(pattern, path, include, context.min(5), files_only, fixed, workspace))
        }
        "list" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            Ok(list_dir(path, workspace))
        }
        "web_search" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(6) as usize;
            let bk = brave_key.clone();
            let res = tokio::task::spawn_blocking(move || web_search(&query, count, bk.as_deref()))
                .await
                .unwrap_or_else(|e| format!("web_search join error: {}", e));
            Ok(res)
        }
        "browse" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let extract = args
                .get("extract")
                .and_then(|v| v.as_str())
                .unwrap_or("text");
            Ok(browse(url, depth, extract).await)
        }
        "browse_urls" => {
            let urls: Vec<String> = args
                .get("urls")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let extract = args
                .get("extract")
                .and_then(|v| v.as_str())
                .unwrap_or("text");
            let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();
            Ok(browse_urls(&url_refs, extract).await)
        }
        "update_goal" => {
            let goal = args.get("goal").and_then(|v| v.as_str()).unwrap_or("");
            Ok(format!(
                "✅ Goal update requested: {}",
                safe_truncate(goal, 80)
            ))
        }
        "record_discovery" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            Ok(format!(
                "✅ Discovery recorded: {}",
                safe_truncate(text, 80)
            ))
        }

        "read_summary" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            Ok(format!("✅ read_summary requested for {}", path))
        }
        "store_summary" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            Ok(format!("✅ store_summary requested for {}", path))
        }
        "edit" | "str_replace" | "search_replace" | "modify" => {
            Ok("❌ There is no 'edit' tool (or str_replace, etc.). Use the `patch` tool for search/replace edits (args: path, search, replace, optional near_line and wiki). Or use `write` for full file replacement. Always call `read` first for context.".to_string())
        }
        other => Ok(format!("❌ Unknown tool: {}. Valid tools: exec, read, write, patch, grep, list, web_search, browse, browse_urls, update_goal, define_done, record_discovery, read_summary, store_summary. (For file edits use patch or write; there is no 'edit' tool.)", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mock_lookup_by_path_and_default() {
        let m = MockToolBackend::from_json(&json!({
            "list": { ".": "README.md\n", "default": "empty\n" },
            "read": { "README.md": "# mock\n" }
        }));
        let list = m.lookup("list", &json!({ "path": "." })).expect("list .");
        assert!(list.contains("README"));
        let read = m
            .lookup("read", &json!({ "path": "README.md" }))
            .expect("read");
        assert!(read.contains("mock"));
    }
}
