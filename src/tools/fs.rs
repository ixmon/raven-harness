use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

// Limits (from glm.md review for magic numbers)
const GREP_MAX_MATCHES: usize = 60;
const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;

/// Resolve a user-provided path against the workspace root.
/// Enforces containment using canonicalization where possible.
/// Rejects obvious escapes (../ outside, absolute paths outside workspace).
/// Returns Err with message on escape (enforced per glm.md review).
fn resolve(workspace: &Path, user_path: &str) -> Result<PathBuf, String> {
    let p = Path::new(user_path);
    let candidate = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace.join(p)
    };

    // Best effort containment check via canonicalize
    if let (Ok(ws_can), Ok(cand_can)) = (workspace.canonicalize(), candidate.canonicalize()) {
        if cand_can.starts_with(&ws_can) {
            return Ok(cand_can);
        } else {
            return Err(format!("Path escapes workspace containment: {}", user_path));
        }
    }

    // If paths don't exist yet (e.g. new file write), do a parent-based check
    if let Some(parent) = candidate.parent() {
        if let (Ok(ws_can), Ok(parent_can)) = (workspace.canonicalize(), parent.canonicalize()) {
            if parent_can.starts_with(&ws_can) {
                return Ok(candidate);
            } else {
                return Err(format!("Path escapes workspace containment: {}", user_path));
            }
        }
    }

    // Last resort: reject on obvious ..
    if user_path.contains("..") && !candidate.starts_with(workspace) {
        return Err(format!("Path escapes workspace containment (contains ..): {}", user_path));
    }

    Ok(candidate)
}

pub fn read_file(user_path: &str, lines: Option<&str>, workspace: &Path, max_lines: usize) -> String {
    let path = match resolve(workspace, user_path) {
        Ok(p) => p,
        Err(e) => return format!("❌ {}", e),
    };

    // Safeguard: refuse to slurp giant files directly (user can use exec head/tail or grep)
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > MAX_READ_BYTES {
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
    let path = match resolve(workspace, user_path) {
        Ok(p) => p,
        Err(e) => return format!("❌ {}", e),
    };

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
    let path = match resolve(workspace, user_path) {
        Ok(p) => p,
        Err(e) => return format!("❌ {}", e),
    };

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
        match resolve(workspace, p) {
            Ok(pp) => pp,
            Err(e) => return format!("❌ {}", e),
        }
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

    let walker = ignore::WalkBuilder::new(&search_root)
        .standard_filters(true)
        .build();

    'files: for entry in walker.filter_map(|e| e.ok()) {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();

        let file = match fs::File::open(path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let reader = BufReader::new(file);
        for (i, line_result) in reader.lines().enumerate() {
            let line = match line_result {
                Ok(l) => l,
                Err(_) => break,
            };
            if re.is_match(&line) {
                let rel = path.strip_prefix(workspace).unwrap_or(path);
                matches.push(format!("{}:{}: {}", rel.display(), i + 1, line.trim()));
                if matches.len() >= GREP_MAX_MATCHES {
                    break 'files;
                }
            }
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
    let path = match resolve(workspace, user_path) {
        Ok(p) => p,
        Err(e) => return format!("❌ {}", e),
    };

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_line_range() {
        assert_eq!(parse_line_range("10-40"), Some((10, 40)));
        assert_eq!(parse_line_range("10-"), Some((10, usize::MAX)));
        assert_eq!(parse_line_range("-40"), Some((1, 40)));
        assert_eq!(parse_line_range("42"), Some((42, 42)));
        assert_eq!(parse_line_range(""), None);
        assert_eq!(parse_line_range("garbage"), None);
        assert_eq!(parse_line_range("  5-10  "), Some((5, 10)));
    }

    // safe_truncate tested via shared in mod.rs tests if added; here focus parse

    #[test]
    fn test_patch_file() {
        use std::fs;
        use std::path::PathBuf;

        let dir = std::env::temp_dir();
        let path: PathBuf = dir.join("raven_hotel_test_patch.txt");
        let _ = fs::remove_file(&path);

        // Initial content
        fs::write(&path, "line1\nline2\nline3\n").unwrap();

        // Patch single occurrence
        let result = patch_file(
            path.to_str().unwrap(),
            "line2",
            "LINE2",
            None,
            &std::env::temp_dir(),
        );
        assert!(result.contains("Patched"), "single match should patch: {}", result);

        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "line1\nLINE2\nline3\n");

        // --- Multiple matches without near_line should warn
        fs::write(&path, "foo\nfoo\nfoo\n").unwrap();
        let res = patch_file(path.to_str().unwrap(), "foo", "BAR", None, &std::env::temp_dir());
        assert!(res.contains("appears 3 times"), "ambiguous should be reported: {}", res);

        // Content should be unchanged
        let c = fs::read_to_string(&path).unwrap();
        assert_eq!(c, "foo\nfoo\nfoo\n");

        // --- Multiple matches + near_line should disambiguate (pick closest)
        // Lines: 1:foo 2:foo 3:foo . We want the one near line 3.
        let res2 = patch_file(path.to_str().unwrap(), "foo", "BAZ", Some(3), &std::env::temp_dir());
        assert!(res2.contains("Patched"), "near_line disambiguate: {}", res2);
        let c2 = fs::read_to_string(&path).unwrap();
        // The last occurrence should have been replaced (closest to line 3)
        assert!(c2.contains("BAZ"), "should contain replacement");
        assert_eq!(c2.matches("foo").count(), 2);

        // --- No match
        let res3 = patch_file(path.to_str().unwrap(), "NONEXISTENT", "X", None, &std::env::temp_dir());
        assert!(res3.contains("not found"), "missing should report: {}", res3);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_patch_file_zero_matches() {
        use std::fs;
        let path = std::env::temp_dir().join("raven_test_patch_nomatch.txt");
        let _ = fs::remove_file(&path);
        fs::write(&path, "hello world\n").unwrap();

        let ws = std::env::temp_dir();
        let res = patch_file(path.to_str().unwrap(), "nope", "replaced", None, &ws);
        assert!(res.contains("not found"));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_resolve_rejects_escape() {
        let ws = std::env::temp_dir();
        // ../ outside workspace
        let res = resolve(&ws, "../../etc/passwd");
        assert!(res.is_err(), "escape via .. should be rejected");
        // absolute path outside workspace
        let res = resolve(&ws, "/etc/passwd");
        assert!(res.is_err(), "absolute path outside workspace should be rejected");
    }

    #[test]
    fn test_resolve_accepts_inside() {
        let ws = std::env::temp_dir();
        // relative inside workspace
        let res = resolve(&ws, "some/relative/file.txt");
        assert!(res.is_ok(), "relative path inside workspace should be accepted");
        // workspace itself
        let res = resolve(&ws, ".");
        assert!(res.is_ok(), "'.' should resolve to workspace");
    }
}
