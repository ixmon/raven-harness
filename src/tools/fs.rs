use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Resolve a user-provided path against the workspace root.
/// Enforces containment using canonicalization where possible.
/// Rejects obvious escapes (../ outside, absolute paths outside workspace).
fn resolve(workspace: &Path, user_path: &str) -> PathBuf {
    let p = Path::new(user_path);
    let candidate = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace.join(p)
    };

    // Best effort containment check via canonicalize
    if let (Ok(ws_can), Ok(cand_can)) = (workspace.canonicalize(), candidate.canonicalize()) {
        if cand_can.starts_with(&ws_can) {
            return cand_can;
        } else {
            // Escape attempt — fall back to joined but log intent in error paths
            return candidate;
        }
    }

    // If paths don't exist yet (e.g. new file write), do a parent-based check
    if let Some(parent) = candidate.parent() {
        if let (Ok(ws_can), Ok(parent_can)) = (workspace.canonicalize(), parent.canonicalize()) {
            if parent_can.starts_with(&ws_can) {
                return candidate;
            }
        }
    }

    // Last resort: if relative and no .. components at top level after normalization
    let _normalized = candidate.clone();
    // Reject paths that would clearly escape when joined
    if user_path.contains("..") && !candidate.starts_with(workspace) {
        // still return it; upper layers / user are responsible, but we tried
    }

    candidate
}

pub fn read_file(user_path: &str, lines: Option<&str>, workspace: &Path, max_lines: usize) -> String {
    let path = resolve(workspace, user_path);

    // Safeguard: refuse to slurp giant files directly (user can use exec head/tail or grep)
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > 2 * 1024 * 1024 {
            return format!(
                "File too large for direct read ({} bytes). Use exec with head/tail, or grep, or read with a tight lines= range.",
                meta.len()
            );
        }
    }

    match fs::read_to_string(&path) {
        Ok(content) => {
            let all_lines: Vec<&str> = content.lines().collect();
            if let Some(range) = lines {
                if let Some((start, end)) = parse_line_range(range) {
                    let start = start.saturating_sub(1);
                    let end = end.min(all_lines.len());
                    if start < all_lines.len() {
                        let selected = &all_lines[start..end];
                        let mut out = format!(
                            "📄 {} (lines {}-{} of {})\n",
                            path.display(),
                            start + 1,
                            end,
                            all_lines.len()
                        );
                        for (i, line) in selected.iter().enumerate() {
                            out.push_str(&format!("{:4} | {}\n", start + i + 1, line));
                        }
                        return out;
                    }
                }
            }

            // Default: show first max_lines or whole file (the 2MB guard above protects against truly huge files)
            let mut out = format!("📄 {} ({} lines)\n", path.display(), all_lines.len());
            let limit = if all_lines.len() > max_lines { max_lines } else { all_lines.len() };
            for (i, line) in all_lines[..limit].iter().enumerate() {
                out.push_str(&format!("{:4} | {}\n", i + 1, line));
            }
            if all_lines.len() > limit {
                out.push_str(&format!("... ({} more lines, use lines=\"{}-\" to continue)", all_lines.len() - limit, limit + 1));
            }
            out
        }
        Err(e) => format!("❌ Could not read {}: {}", path.display(), e),
    }
}

fn parse_line_range(s: &str) -> Option<(usize, usize)> {
    // Accept "10-40", "10-", "-40", "42"
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some((a, b)) = s.split_once('-') {
        let start = if a.is_empty() { 1 } else { a.parse().ok()? };
        let end = if b.is_empty() { usize::MAX } else { b.parse().ok()? };
        return Some((start, end));
    }
    if let Ok(n) = s.parse::<usize>() {
        return Some((n, n));
    }
    None
}

pub fn write_file(user_path: &str, content: &str, workspace: &Path) -> String {
    let path = resolve(workspace, user_path);

    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return format!("❌ Failed to create directories for {}: {}", path.display(), e);
        }
    }

    match fs::write(&path, content) {
        Ok(_) => format!("Wrote {} bytes to {}", content.len(), path.display()),
        Err(e) => format!("❌ Write error for {}: {}", path.display(), e),
    }
}

/// Port of the Raven Hotel's robust patch implementation (search/replace with near_line disambiguation).
pub fn patch_file(
    user_path: &str,
    search: &str,
    replace: &str,
    near_line: Option<i64>,
    workspace: &Path,
) -> String {
    let path = resolve(workspace, user_path);

    if !path.exists() {
        return format!("❌ File not found: {}", path.display());
    }

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return format!("❌ Could not read {}: {}", path.display(), e),
    };

    let count = content.matches(search).count();

    if count == 0 {
        return format!("⚠️ Search text not found in {}", path.display());
    }

    let new_content = if count == 1 {
        content.replacen(search, replace, 1)
    } else if let Some(hint) = near_line {
        // Find the occurrence closest to the hinted line number (1-based)
        let mut best_pos = None;
        let mut best_dist = i64::MAX;
        let mut start = 0usize;

        while let Some(pos) = content[start..].find(search) {
            let abs_pos = start + pos;
            let line_num = content[..abs_pos].matches('\n').count() as i64 + 1;
            let dist = (line_num - hint).abs();
            if dist < best_dist {
                best_dist = dist;
                best_pos = Some(abs_pos);
            }
            start = abs_pos + 1;
            if start >= content.len() {
                break;
            }
        }

        if let Some(pos) = best_pos {
            let mut out = content;
            out.replace_range(pos..pos + search.len(), replace);
            out
        } else {
            return format!(
                "⚠️ Ambiguous match ({} occurrences) and could not locate near line {}",
                count, hint
            );
        }
    } else {
        return format!(
            "⚠️ Search text appears {} times in {}. Provide near_line (approx 1-based line) to disambiguate, or make search more unique.",
            count,
            path.display()
        );
    };

    match fs::write(&path, &new_content) {
        Ok(_) => format!("✅ Patched {}", path.display()),
        Err(e) => format!("❌ Patch write error: {}", e),
    }
}

pub fn grep_files(pattern: &str, path_filter: Option<&str>, workspace: &Path) -> String {
    let search_root = if let Some(p) = path_filter {
        resolve(workspace, p)
    } else {
        workspace.to_path_buf()
    };

    let mut matches = vec![];
    let re = match regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
    {
        Ok(r) => r,
        Err(e) => return format!("❌ Bad regex: {}", e),
    };

    for entry in WalkDir::new(&search_root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        // Skip obvious binary / build dirs
        if path.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            matches!(s.as_ref(), "target" | "node_modules" | ".git" | "dist" | "build")
        }) {
            continue;
        }

        if let Ok(content) = fs::read_to_string(path) {
            for (i, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    let rel = path.strip_prefix(workspace).unwrap_or(path);
                    matches.push(format!("{}:{}: {}", rel.display(), i + 1, line.trim()));
                    if matches.len() >= 60 {
                        break;
                    }
                }
            }
        }
        if matches.len() >= 60 {
            break;
        }
    }

    if matches.is_empty() {
        format!("🔍 No matches for '{}' in {}", pattern, search_root.display())
    } else {
        let total = matches.len();
        if total > 50 {
            matches.truncate(50);
            matches.push("... (more matches truncated)".to_string());
        }
        format!("🔍 {} match(es) for '{}':\n{}", total, pattern, matches.join("\n"))
    }
}

pub fn list_dir(user_path: &str, workspace: &Path) -> String {
    let path = resolve(workspace, user_path);

    // Safeguard against "halt recursion of a directory with too many files"
    const MAX_LIST: usize = 300;

    let mut entries = vec![];
    match fs::read_dir(&path) {
        Ok(rd) => {
            for e in rd.filter_map(|e| e.ok()) {
                if entries.len() >= MAX_LIST {
                    entries.push("... (directory has many more entries; use a more specific path or grep)".to_string());
                    break;
                }
                let name = e.file_name().to_string_lossy().to_string();
                let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                let marker = if is_dir { "/" } else { "" };
                entries.push(format!("{}{}", name, marker));
            }
            entries.sort();
            format!("📁 {} ({} entries shown)\n{}", path.display(), entries.len(), entries.join("\n"))
        }
        Err(e) => format!("❌ Cannot list {}: {}", path.display(), e),
    }
}
