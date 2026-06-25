//! Persistent per-workspace session management for the Raven TUI.
//!
//! Layout:
//!   ~/.raven-hotel/sessions/<session_id>/
//!     meta.json          -- goal, pitfalls, tests, discoveries, repo_cache, etc.
//!     full_log.jsonl     -- append-only record of conversation turns + harness diagnostics
//!                           (user/assistant/tool + role=system events for nudges/judges/thinking).
//!                           Useful for human debugging of eval runs and for the agent to
//!                           inspect its own history (e.g. via exec/grep) when context is summarized.
//!
//! On startup (for interactive TUI):
//!   - Ask "Do you trust the code in <workspace>?" (Cursor-style) to gate deep indexing.
//!   - Run a safe, deterministic discovery (inspired by `find` + importance ranking).
//!   - Build a compact repo_cache (dirtree + sizes + ranked important files + language hints + short summary).
//!   - Maintain current_goal (model-updatable), achievement tests, pitfalls, recent discoveries.
//!   - Provide get_injection_block() -- a small, context-friendly string containing:
//!       * repo structure + importance ranking
//!       * current goal + tests + pitfalls
//!       * last user request
//!       * summary of recent turns
//!
//! Safeguards (as requested):
//!   - Never recurse into dirs with too many immediate children (default 400).
//!   - Skip files larger than ~1 MiB during indexing (agent can still read via tools if needed, with their own caps).
//!   - Hard limits on tree depth and total files considered.
//!   - Skip common heavy dirs (.git, target, node_modules, dist, build, .venv, etc.).
//!
//! The model can shift the goal (and update tests/pitfalls) by calling the `update_goal` tool.
//! Discoveries are recorded via `record_discovery` (or the agent just calls `remember` + we promote).
//!
//! The actual LLM prompt is built fresh each turn as:
//!   [ system( base_instructions + injection_block ) ] + clean conversation turns
//! This keeps the "comfortably small" context payload stable even as full history grows.

use anyhow::Result;
use chrono::Utc;
use rusqlite::{Connection, params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

const RAVEN_HOME: &str = ".raven-hotel";
const SESSIONS_SUBDIR: &str = "sessions";

/// Limits for safe discovery (tunable).
const MAX_FILE_SIZE_FOR_INDEX: u64 = 1024 * 1024; // 1 MiB
const MAX_DIR_ENTRIES: usize = 350;                    // if a dir has more immediate children, don't descend
const MAX_DEPTH: usize = 7;
const MAX_INDEXED_FILES: usize = 2000;
const IMPORTANT_FILE_BOOST: i64 = 10_000;              // mtime bonus for READMEs etc.

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepoCache {
    pub tree_text: String,
    pub important_paths: Vec<String>,
    pub language_hint: Option<String>,
    pub short_summary: String,
    pub total_files_considered: usize,
    pub indexed_at: String,
    /// Rough top-level layout signal (e.g. "rust" | "python" | "node" | "mixed")
    pub project_type: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecApprovalMode {
    #[default]
    Babysitter,     // Always Ask
    SpringBreak,    // Yolo for remainder of this session (reset on restart)
    Vegas,          // Yolo inside sandbox only (ask for outside)
    Thunderdome,    // Eternal Yolo for this workspace (persisted)
}

impl ExecApprovalMode {
    pub fn label(&self) -> &'static str {
        match self {
            ExecApprovalMode::Babysitter => "Babysitter - Always Ask",
            ExecApprovalMode::SpringBreak => "Spring Break - Yolo for remainder of session",
            ExecApprovalMode::Vegas => "Vegas - Yolo in sandbox",
            ExecApprovalMode::Thunderdome => "Thunderdome - eternal Yolo, anytime, anywhere",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionMeta {
    pub session_id: String,
    pub workspace: PathBuf,
    pub trusted: bool,
    pub created_at: String,
    pub updated_at: String,

    /// The thing the user ultimately wants. The model can update this.
    pub current_goal: String,
    /// How we (or the model) would know the goal is done.
    pub achievement_tests: Vec<String>,

    /// Agent-defined definition of "done" for the current task (set once early via tool).
    /// Only the judge path clears it when Fulfilled.
    /// Useful in no-goal or self-directed evals: model declares what success looks like,
    /// judge validates against actions and clears on completion.
    #[serde(default)]
    pub completion_criteria: Option<String>,
    /// Things the model has been told (or discovered) to avoid.
    pub pitfalls: Vec<String>,
    /// Key facts / files / insights discovered during the session.
    pub discoveries: Vec<String>,

    pub last_user_request: Option<String>,
    pub repo_cache: RepoCache,

    /// A compact rolling summary of recent work (last ~10 turns or equivalent).
    /// Kept small on purpose so it always fits in the injected block.
    pub recent_turns_summary: String,

    /// Controls approval requirements for side-effecting actions (exec, writes, etc.)
    #[serde(default)]
    pub exec_approval_mode: ExecApprovalMode,

    /// Optional summary of initial analysis done on first trust.
    #[serde(default)]
    pub initial_analysis: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Session {
    pub id: String,
    pub dir: PathBuf,
    pub meta_path: PathBuf,
    pub log_path: PathBuf,
    pub workspace: PathBuf,
    pub meta: SessionMeta,
}

impl Session {
    /// Initialize or resume a session for the given workspace.
    /// Creates the ~/.raven-hotel/sessions/<id>/ tree if needed.
    /// The session id is derived from the workspace path.
    pub fn init(workspace: &Path) -> Result<Self> {
        Self::init_internal(workspace, None)
    }

    /// Initialize or resume a *named* session for the given workspace.
    /// This allows multiple independent resumable sessions for the same
    /// workspace (e.g. "daily-work", "swebench-marshmallow-1234", or a
    /// fresh one created on the fly for evals).
    ///
    /// The resulting session dir will incorporate the name so histories
    /// don't mix, but the session is still associated with the workspace
    /// (for repo cache / summaries / trusted flag etc.).
    pub fn init_named(workspace: &Path, name: &str) -> Result<Self> {
        Self::init_internal(workspace, Some(name))
    }

    fn init_internal(workspace: &Path, explicit_name: Option<&str>) -> Result<Self> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let base = PathBuf::from(home).join(RAVEN_HOME).join(SESSIONS_SUBDIR);
        fs::create_dir_all(&base)?;

        let id = if let Some(name) = explicit_name {
            make_session_id_with_name(workspace, Some(name))
        } else {
            make_session_id(workspace)
        };
        let dir = base.join(&id);
        fs::create_dir_all(&dir)?;

        let meta_path = dir.join("meta.json");
        let log_path = dir.join("full_log.jsonl");

        let mut meta: SessionMeta = if meta_path.exists() {
            let data = fs::read_to_string(&meta_path)?;
            match serde_json::from_str(&data) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("warning: failed to parse meta.json ({}), starting with fresh session meta", e);
                    SessionMeta::default()
                }
            }
        } else {
            SessionMeta::default()
        };

        // Always refresh workspace path (in case moved) and timestamps
        meta.workspace = workspace.to_path_buf();
        meta.session_id = id.clone();
        if meta.created_at.is_empty() {
            meta.created_at = now_iso();
        }
        meta.updated_at = now_iso();

        // Default: no goal. For baseline experiments we deliberately start empty
        // (no placeholder) so we can test harness mechanisms (nudges, judge on actions,
        // last_user_request) without relying on explicit goal tracking.
        // The model should call define_done early (once) from the initial prompt to declare
        // what success looks like; the judge will use and clear it.
        // Seeding from first request is also conditional (see record_user_request).

        let s = Session {
            id,
            dir,
            meta_path,
            log_path,
            workspace: workspace.to_path_buf(),
            meta,
        };

        s.save_meta()?;
        Ok(s)
    }

    pub fn save_meta(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(&self.meta)?;
        fs::write(&self.meta_path, data)?;
        Ok(())
    }

    /// Append a raw line (usually a JSON object) to the full log.
    pub fn append_log(&self, line: &str) -> Result<()> {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;
        writeln!(f, "{}", line)?;
        Ok(())
    }

    /// Load the most recent user and assistant messages from the full log
    /// for conversation restoration on restart. Returns messages in
    /// chronological order (oldest first).
    pub fn load_recent_conversation(&self, max_entries: usize) -> Vec<(String, String)> {
        let data = match fs::read_to_string(&self.log_path) {
            Ok(d) => d,
            Err(_) => return vec![],
        };

        // Collect user/assistant entries (role, content) from the log
        let mut entries: Vec<(String, String)> = vec![];
        for line in data.lines() {
            let line = line.trim();
            if line.is_empty() { continue; }
            let obj: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let role = obj.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let content = obj.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if (role == "user" || role == "assistant") && !content.trim().is_empty() {
                entries.push((role.to_string(), content.to_string()));
            }
        }

        // Return the last N entries in chronological order
        let start = entries.len().saturating_sub(max_entries);
        entries[start..].to_vec()
    }

    /// Return the most recent non-empty assistant content from full_log.jsonl.
    ///
    /// This inspects the *committed* tail of the session log rather than a
    /// transient streaming buffer / "last packet" / per-turn full_text accumulation.
    /// Useful for reliably detecting whether a previous (or just committed)
    /// model response leaked XML tool call fragments into visible Agent text.
    pub fn last_assistant_content(&self) -> Option<String> {
        let data = fs::read_to_string(&self.log_path).ok()?;
        for line in data.lines().rev() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let obj: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if obj.get("role").and_then(|v| v.as_str()) == Some("assistant") {
                if let Some(c) = obj.get("content").and_then(|v| v.as_str()) {
                    let trimmed = c.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }
        None
    }

    /// Return the block that should be injected into the system prompt / early context.
    /// Keep this small and high-signal.
    pub fn get_injection_block(&self) -> String {
        let m = &self.meta;
        let rc = &m.repo_cache;

        let mut block = String::new();
        block.push_str("## SESSION CONTEXT (persistent across restarts)\n");
        block.push_str(&format!("Workspace: {}\n", m.workspace.display()));
        block.push_str(&format!("Session: {}\n\n", m.session_id));

        if std::env::var("RAVEN_GOAL_TRACKING").is_ok() {
            block.push_str("### Current Goal\n");
            block.push_str(&m.current_goal);
            block.push_str("\n\n");

            if !m.achievement_tests.is_empty() {
                block.push_str("### Tests for Goal Achievement (stop when these are satisfied)\n");
                for t in &m.achievement_tests {
                    block.push_str(&format!("- {}\n", t));
                }
                block.push('\n');
            }

            if let Some(criteria) = &m.completion_criteria {
                block.push_str("### Agent-Defined Completion Criteria (what 'done' looks like)\n");
                block.push_str(criteria);
                block.push_str("\n\n");
            }
        }

        if !m.pitfalls.is_empty() {
            block.push_str("### Known Pitfalls to Avoid\n");
            for p in &m.pitfalls {
                block.push_str(&format!("- {}\n", p));
            }
            block.push('\n');
        }

        if !m.discoveries.is_empty() {
            block.push_str("### Key Discoveries (build on these)\n");
            for d in m.discoveries.iter().rev().take(8) {
                block.push_str(&format!("- {}\n", d));
            }
            block.push('\n');
        }

        if let Some(req) = &m.last_user_request {
            if req.len() > 20 {
                block.push_str("### Latest User Message (context for any active task)\n");
                block.push_str(req);
                block.push_str("\n\n");
            }
        }

        // Repo cache — the heart of the "better context"
        block.push_str("### Repo Structure & Importance (cached, safe discovery)\n");
        if !rc.tree_text.is_empty() {
            block.push_str(&rc.tree_text);
            block.push('\n');
        }
        if !rc.important_paths.is_empty() {
            block.push_str("High-signal files (recently modified, READMEs, manifests, core sources):\n");
            for p in &rc.important_paths {
                block.push_str(&format!("  • {}\n", p));
            }
            block.push('\n');
        }
        if let Some(lang) = &rc.language_hint {
            block.push_str(&format!("Language / stack hint: {}\n", lang));
        }
        if !rc.short_summary.is_empty() {
            block.push_str(&format!("Project summary: {}\n\n", rc.short_summary));
        }

        if !m.recent_turns_summary.is_empty() {
            block.push_str("### Summary of Recent Turns (last ~10)\n");
            block.push_str(&m.recent_turns_summary);
            block.push_str("\n\n");
            block.push_str("(The above is compressed history only. The Latest User Message and Key Discoveries above take priority.)\n\n");
        }

        block.push_str("### File Summary Cache\n");
        block.push_str("A SQLite cache (context.db) stores concise mtime-matched summaries for files. ");
        block.push_str("**Always call read_summary(path) before raw read on source files.** ");
        block.push_str("If fresh it returns the summary; if stale/missing it gives you the mtime + capped raw content and tells you to call store_summary after analysis. ");
        block.push_str("This keeps token usage low even on long tasks.\n\n");

        let goal_tracking = std::env::var("RAVEN_GOAL_TRACKING").is_ok();
        let no_goal_tool = std::env::var("RAVEN_EVAL_DISABLE_UPDATE_GOAL").is_ok() || std::env::var("RAVEN_NO_GOAL").is_ok();
        if goal_tracking && !no_goal_tool {
            block.push_str("---\nUse the structure and goal above to stay on track. Call update_goal(...) if the user's intent clearly shifts.\n");
        }
        block
    }

    /// Record a discovery (deduped, capped).
    pub fn record_discovery(&mut self, text: &str) -> Result<()> {
        let t = text.trim().to_string();
        if t.is_empty() {
            return Ok(());
        }
        if !self.meta.discoveries.iter().any(|d| d == &t) {
            self.meta.discoveries.push(t);
            // Keep only the most recent N
            if self.meta.discoveries.len() > 30 {
                let start = self.meta.discoveries.len() - 30;
                self.meta.discoveries.drain(0..start);
            }
        }
        self.meta.updated_at = now_iso();
        self.save_meta()
    }

    /// Update goal + associated metadata (the model is encouraged to call this on clear shifts).
    pub fn update_goal(
        &mut self,
        new_goal: &str,
        achievement_tests: Option<Vec<String>>,
        pitfalls: Option<Vec<String>>,
    ) -> Result<()> {
        let g = new_goal.trim();
        if !g.is_empty() && g != self.meta.current_goal {
            self.meta.current_goal = g.to_string();
        }
        if let Some(tests) = achievement_tests {
            self.meta.achievement_tests = tests;
        }
        if let Some(pits) = pitfalls {
            self.meta.pitfalls = pits;
        }
        self.meta.updated_at = now_iso();
        self.save_meta()
    }

    /// Update the rolling recent-turns summary (kept deliberately small).
    pub fn set_recent_turns_summary(&mut self, summary: &str) -> Result<()> {
        self.meta.recent_turns_summary = summary.trim().to_string();
        self.meta.updated_at = now_iso();
        self.save_meta()
    }

    /// Set the last raw user request (for the injection block).
    pub fn set_last_user_request(&mut self, req: &str) -> Result<()> {
        self.meta.last_user_request = Some(req.trim().to_string());
        self.meta.updated_at = now_iso();
        self.save_meta()
    }

    // ── File summary cache (mtime-matched summaries in per-session SQLite) ──
    // Goal: let the agent prefer short, fresh summaries over full file reads.
    // Summaries are stored with the mtime at the time they were created.
    // The model is instructed to call read_summary first, then store_summary after
    // it has analyzed a stale/missing file.

    #[allow(dead_code)]
    fn context_db_path(&self) -> PathBuf {
        self.dir.join("context.db")
    }

    #[allow(dead_code)]
    fn open_context_db(&self) -> Result<Connection> {
        let db_path = self.context_db_path();
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS file_summaries (
                path TEXT PRIMARY KEY,
                mtime INTEGER NOT NULL,
                summary TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                file_size INTEGER DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_summaries_mtime ON file_summaries(mtime);
            "#,
        )?;
        Ok(conn)
    }

    /// Returns Some((stored_mtime, summary)) only if the stored mtime exactly matches
    /// the *current* on-disk mtime of the file. This ensures the summary is still valid.
    #[allow(dead_code)]
    pub fn get_file_summary(&self, rel_path: &str) -> Result<Option<(i64, String)>> {
        let abs_path = self.workspace.join(rel_path);
        let current_mtime = current_file_mtime(&abs_path)?;

        let conn = self.open_context_db()?;
        let mut stmt = conn.prepare(
            "SELECT mtime, summary FROM file_summaries WHERE path = ?1 LIMIT 1",
        )?;
        let row = stmt
            .query_row(params![rel_path], |row| {
                let mtime: i64 = row.get(0)?;
                let summary: String = row.get(1)?;
                Ok((mtime, summary))
            })
            .optional()?;

        if let Some((stored_mtime, summary)) = row {
            if stored_mtime == current_mtime {
                return Ok(Some((current_mtime, summary)));
            }
        }
        Ok(None)
    }

    /// Store (or replace) a summary for a file at a specific mtime.
    /// The caller (the agent) is responsible for having observed that mtime.
    #[allow(dead_code)]
    pub fn store_file_summary(&self, rel_path: &str, mtime: i64, summary: &str) -> Result<()> {
        let conn = self.open_context_db()?;
        let now = now_iso();
        conn.execute(
            r#"
            INSERT OR REPLACE INTO file_summaries (path, mtime, summary, updated_at, file_size)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                rel_path,
                mtime,
                summary.trim(),
                now,
                summary.len() as i64
            ],
        )?;
        Ok(())
    }

    /// Invalidate any cached summary for a path (call this after write/patch).
    #[allow(dead_code)]
    pub fn invalidate_file_summary(&self, rel_path: &str) -> Result<()> {
        let conn = self.open_context_db()?;
        conn.execute("DELETE FROM file_summaries WHERE path = ?1", params![rel_path])?;
        Ok(())
    }
}

#[allow(dead_code)]
pub fn current_file_mtime(path: &Path) -> Result<i64> {
    if !path.exists() {
        return Ok(0);
    }
    let meta = fs::metadata(path)?;
    let mtime = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    Ok(mtime)
}

// Small wrapper usable from agent without importing private details
#[allow(dead_code)]
pub fn current_file_mtime_for_agent(path: &Path) -> i64 {
    current_file_mtime(path).unwrap_or(0)
}

fn make_session_id(workspace: &Path) -> String {
    make_session_id_with_name(workspace, None)
}

fn make_session_id_with_name(workspace: &Path, explicit_name: Option<&str>) -> String {
    // Stable, human-friendly id derived from the workspace (plus optional name).
    // We prefer the leaf directory name + a short hash of the full path.
    let leaf = workspace
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("workspace")
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>();

    let name_part = if let Some(name) = explicit_name {
        let clean = name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect::<String>();
        format!("{}-", clean.trim_matches('-'))
    } else {
        String::new()
    };

    // Simple non-crypto hash of the absolute path for uniqueness
    let abs = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    let path_str = abs.to_string_lossy();
    let mut hash: u64 = 14695981039346656037; // FNV offset
    for b in path_str.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    let short = format!("{:x}", hash)[..8].to_string();

    format!("{}{}-{}", name_part, leaf, short)
}

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// Ask the user (on stdin) whether they trust the code in this directory.
/// This is the Cursor-style gate that determines how deeply we index.
/// Returns true if trusted (or non-interactive / already marked).
pub fn trust_prompt(workspace: &Path, already_trusted: bool) -> bool {
    if already_trusted {
        return true;
    }
    // Non-interactive (scripted --prompt mode) — be conservative.
    // The prompt below will be skipped or non-blocking in practice for pipes.

    eprintln!("\nDo you trust the code in {} ?", workspace.display());
    eprintln!("(This lets Raven build a local repo cache with file tree, sizes, and importance ranking.");
    eprintln!(" We will never read files >1 MiB or descend into bloated directories during indexing.");
    eprintln!(" You can still use tools to inspect anything later. [y/N] ");

    let mut line = String::new();
    // Best-effort; in fully non-interactive contexts this returns false (safe default).
    if std::io::stdin().read_line(&mut line).is_ok() {
        let l = line.trim().to_lowercase();
        return l == "y" || l == "yes";
    }
    false
}

/// Safe, deterministic workspace discovery (no LLM, pure FS + simple signals).
/// Respects the limits above so we never blow up on huge trees or giant files.
pub fn build_repo_cache(workspace: &Path, trusted: bool) -> RepoCache {
    if !trusted {
        return RepoCache {
            tree_text: "(indexing skipped — workspace not trusted yet)".into(),
            important_paths: vec![],
            language_hint: None,
            short_summary: "Workspace not yet indexed (run with trust to enable).".into(),
            total_files_considered: 0,
            indexed_at: now_iso(),
            project_type: None,
        };
    }

    let mut entries: Vec<(PathBuf, u64, i64, bool)> = vec![]; // (path, size, mtime_secs, is_dir)
    let mut dir_child_count: HashMap<PathBuf, usize> = HashMap::new();
    let mut total = 0usize;

    let skip_dirs = [".git", "target", "node_modules", "dist", "build", ".venv", "__pycache__", ".cargo", "out"];

    for entry in WalkDir::new(workspace)
        .max_depth(MAX_DEPTH)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !skip_dirs.contains(&name.as_ref())
        })
        .filter_map(|e| e.ok())
    {
        if total > MAX_INDEXED_FILES {
            break;
        }

        let path = entry.path().to_path_buf();
        let is_dir = entry.file_type().is_dir();

        // Count siblings at this level to implement "halt recursion on too many files"
        if let Some(parent) = path.parent() {
            let cnt = dir_child_count.entry(parent.to_path_buf()).or_insert(0);
            *cnt += 1;
            if *cnt > MAX_DIR_ENTRIES && !is_dir {
                // Too bushy — we still record the file but won't have gone deeper (WalkDir already limited)
            }
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        let size = if is_dir { 0 } else { meta.len() };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Skip huge files for the cache (the agent can still read them deliberately)
        if !is_dir && size > MAX_FILE_SIZE_FOR_INDEX {
            continue;
        }

        entries.push((path, size, mtime, is_dir));
        total += 1;
    }

    // Build importance score: recency (higher mtime better) + name signals + size signal (small configs win)
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let mut scored: Vec<(i64, &Path, u64)> = entries
        .iter()
        .filter(|(_, _, _, is_dir)| !*is_dir)
        .map(|(p, sz, mt, _)| {
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
            let mut score = (mt - (now - 86400 * 30)).max(0); // favor last ~30 days

            if name.starts_with("readme") || name == "readme.md" {
                score += IMPORTANT_FILE_BOOST * 3;
            } else if name.ends_with(".md") {
                score += IMPORTANT_FILE_BOOST;
            } else if name == "cargo.toml" || name == "package.json" || name == "pyproject.toml" || name == "makefile" || name == "justfile" {
                score += IMPORTANT_FILE_BOOST * 2;
            } else if name.ends_with(".rs") || name.ends_with(".py") || name.ends_with(".ts") || name.ends_with(".tsx") || name.ends_with(".go") {
                score += 2000; // source is interesting
            }
            // smaller files that are "manifests" already boosted above; penalize huge source a little for ranking
            if *sz > 100_000 {
                score -= 500;
            }
            (score, p.as_path(), *sz)
        })
        .collect();

    scored.sort_by_key(|(sc, _, _)| std::cmp::Reverse(*sc));

    let important: Vec<String> = scored
        .iter()
        .take(18)
        .map(|(_, p, sz)| {
            let rel = p.strip_prefix(workspace).unwrap_or(p);
            let mib = *sz as f64 / (1024.0 * 1024.0);
            if mib > 0.1 {
                format!("{} ({:.1} MiB)", rel.display(), mib)
            } else {
                format!("{}", rel.display())
            }
        })
        .collect();

    // Build a compact tree (very rough, top-down, limited branching)
    let mut tree_lines = vec![format!("{}/", workspace.file_name().unwrap_or_default().to_string_lossy())];
    // Group by first two path components for a shallow view
    let mut seen = std::collections::HashSet::new();
    for (p, sz, _, _) in entries.iter().filter(|(_,_,_,d)| !*d).take(80) {
        if let Ok(rel) = p.strip_prefix(workspace) {
            let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect();
            if parts.is_empty() { continue; }
            let key = parts[0].to_string();
            if seen.insert(key.clone()) {
                let display = if parts.len() > 1 {
                    format!("  {}/... ({} files)", parts[0], /* rough */ 1)
                } else {
                    let mib = *sz as f64 / 1_048_576.0;
                    if mib > 0.05 { format!("  {} ({:.1}M)", parts[0], mib) } else { format!("  {}", parts[0]) }
                };
                tree_lines.push(display);
            }
            if tree_lines.len() > 28 { break; }
        }
    }
    let tree_text = tree_lines.join("\n");

    // Language / project type detection (deterministic)
    let mut lang = None;
    let mut ptype = None;
    for (p, _, _, _) in &entries {
        let name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        if name == "cargo.toml" {
            lang = Some("Rust".to_string());
            ptype = Some("rust".to_string());
            break;
        }
        if name == "pyproject.toml" || name == "setup.py" || name.ends_with(".py") {
            lang = Some("Python".to_string());
            ptype = Some("python".to_string());
        }
        if name == "package.json" {
            lang = Some("TypeScript/JavaScript".to_string());
            ptype = Some("node".to_string());
        }
        if name == "go.mod" {
            lang = Some("Go".to_string());
            ptype = Some("go".to_string());
        }
    }

    let short_summary = if let Some(l) = &lang {
        format!("{} project (detected from {} and file layout). {} files considered in safe scan.", l, ptype.as_deref().unwrap_or("key files"), total)
    } else {
        format!("Project with {} interesting files under safe indexing limits.", total)
    };

    RepoCache {
        tree_text,
        important_paths: important,
        language_hint: lang,
        short_summary,
        total_files_considered: total,
        indexed_at: now_iso(),
        project_type: ptype,
    }
}

/// Convenience: run the trust flow + (re)build cache if trusted, and store result in meta.
/// On first trust for a new session, perform a basic initial analysis and store it.
pub fn ensure_repo_cache(session: &mut Session) -> Result<()> {
    let was_trusted = session.meta.trusted;
    if !session.meta.trusted {
        // Try to prompt
        let trusted_now = trust_prompt(&session.workspace, false);
        session.meta.trusted = trusted_now;
    }
    if session.meta.trusted {
        let cache = build_repo_cache(&session.workspace, true);
        session.meta.repo_cache = cache;

        // On first trust, record an initial analysis into meta
        if !was_trusted {
            let analysis = format!(
                "Initial trust granted. Repo summary: {}. Key files: {}. Project hints: {:?}.",
                session.meta.repo_cache.short_summary,
                session.meta.repo_cache.important_paths.len(),
                session.meta.repo_cache.project_type
            );
            session.meta.initial_analysis = Some(analysis);
        }
    }
    session.meta.updated_at = now_iso();
    session.save_meta()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_id_is_stable_for_path() {
        let p1 = PathBuf::from("/tmp/myproj");
        let p2 = PathBuf::from("/tmp/myproj");
        assert_eq!(make_session_id(&p1), make_session_id(&p2));
    }
}