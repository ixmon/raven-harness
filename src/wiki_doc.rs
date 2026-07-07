//! Wiki markdown structure: link extraction, nav building, path normalization.

use std::collections::HashSet;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WikiLink {
    pub text: String,
    pub target: String,
    pub line: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavItemKind {
    #[default]
    Header,
    Link,
    Back,
    /// "Coding Harness" — opens main conv/trace/input UI.
    Harness,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WikiNavItem {
    pub label: String,
    pub target_file: String,
    pub scroll_to: usize,
    pub kind: NavItemKind,
}

/// Normalize a wiki-relative path from links or tool args.
pub fn normalize_wiki_path(p: &str) -> String {
    let mut s = p.trim().to_string();
    while s.starts_with("./") {
        s = s[2..].to_string();
    }
    s = s.trim_start_matches(['/', '\\']).to_string();
    if s.is_empty() {
        return "index.md".to_string();
    }
    if !s.ends_with(".md") && (!s.contains('.') || s.ends_with('/')) {
        s.push_str(".md");
    }
    s
}

fn clean_link_target(raw: &str) -> String {
    let mut target = raw.to_string();
    if let Some(h) = target.find('#') {
        target = target[..h].to_string();
    }
    normalize_wiki_path(&target)
}

/// Extract navigable wiki links from markdown (markdown links + `[[wiki]]` style).
pub fn extract_links(content: &str) -> Vec<WikiLink> {
    let mut links = Vec::new();
    if let Ok(re) = regex::Regex::new(r"\[([^\]]+)\]\(([^)]+?\.md[^)]*)\)") {
        for (line_idx, line) in content.lines().enumerate() {
            for cap in re.captures_iter(line) {
                let text = cap.get(1).map_or("", |m| m.as_str()).to_string();
                let target = clean_link_target(cap.get(2).map_or("", |m| m.as_str()));
                links.push(WikiLink {
                    text,
                    target,
                    line: line_idx,
                });
            }
        }
    }
    if let Ok(re) = regex::Regex::new(r"\[\[([^\]]+?)(?:\.md)?\]\]") {
        for (line_idx, line) in content.lines().enumerate() {
            for cap in re.captures_iter(line) {
                let raw = cap.get(1).map_or("", |m| m.as_str());
                let target = clean_link_target(raw);
                links.push(WikiLink {
                    text: target.clone(),
                    target,
                    line: line_idx,
                });
            }
        }
    }
    links
}

pub fn nav_is_harness(items: &[WikiNavItem], selected: usize) -> bool {
    items
        .get(selected)
        .is_some_and(|it| it.kind == NavItemKind::Harness)
}

fn parse_heading(line: &str) -> Option<(String, usize)> {
    line.strip_prefix("# ")
        .map(|h| (h.trim().to_string(), 0))
        .or_else(|| line.strip_prefix("## ").map(|h| (format!("  {}", h.trim()), 2)))
        .or_else(|| line.strip_prefix("### ").map(|h| (format!("    {}", h.trim()), 4)))
}

fn parse_heading_browser(line: &str) -> Option<(String, usize)> {
    line.strip_prefix("# ")
        .map(|h| (h.trim().to_string(), 2))
        .or_else(|| line.strip_prefix("## ").map(|h| (format!("  {}", h.trim()), 4)))
        .or_else(|| line.strip_prefix("### ").map(|h| (format!("    {}", h.trim()), 6)))
}

fn append_line_links(
    items: &mut Vec<WikiNavItem>,
    line: &str,
    line_idx: usize,
    link_indent: &str,
    link_re: &Option<regex::Regex>,
    wikilink_re: &Option<regex::Regex>,
) {
    if let Some(re) = link_re {
        for cap in re.captures_iter(line) {
            let text = cap.get(1).map_or("", |m| m.as_str()).to_string();
            let tgt = cap.get(2).map_or("", |m| m.as_str());
            items.push(WikiNavItem {
                label: format!("{}→ {}", link_indent, text),
                target_file: clean_link_target(tgt),
                scroll_to: line_idx,
                kind: NavItemKind::Link,
            });
        }
    }
    if let Some(re) = wikilink_re {
        for cap in re.captures_iter(line) {
            let tgt = cap.get(1).map_or("", |m| m.as_str()).to_string();
            let clean_tgt = clean_link_target(&tgt);
            items.push(WikiNavItem {
                label: format!(
                    "{}→ {}",
                    link_indent,
                    clean_tgt.trim_end_matches(".md")
                ),
                target_file: clean_tgt,
                scroll_to: line_idx,
                kind: NavItemKind::Link,
            });
        }
    }
}

fn dedup_nav_items(items: Vec<WikiNavItem>) -> Vec<WikiNavItem> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for it in items {
        let key = (it.label.clone(), it.target_file.clone());
        if seen.insert(key) {
            deduped.push(it);
        }
    }
    deduped
}

/// Nav for the full wiki viewer (current file + back + in-document headings/links).
pub fn build_viewer_nav(content: &str, current_file: &str) -> Vec<WikiNavItem> {
    let mut items = vec![WikiNavItem {
        label: "Coding Harness".to_string(),
        target_file: String::new(),
        scroll_to: 0,
        kind: NavItemKind::Harness,
    }];
    let cur = normalize_wiki_path(current_file);

    if cur != "index.md" {
        items.push(WikiNavItem {
            label: "⬆ Back (index.md)".into(),
            target_file: "index.md".to_string(),
            scroll_to: 0,
            kind: NavItemKind::Back,
        });
    }

    let link_re = regex::Regex::new(r"\[([^\]]+)\]\(([^)]+?\.md[^)]*)\)").ok();
    let wikilink_re = regex::Regex::new(r"\[\[([^\]]+?)(?:\.md)?\]\]").ok();
    let mut current_heading_indent = 0usize;

    for (i, line) in content.lines().enumerate() {
        if let Some((label, indent)) = parse_heading(line) {
            current_heading_indent = indent;
            items.push(WikiNavItem {
                label,
                target_file: cur.clone(),
                scroll_to: i,
                kind: NavItemKind::Header,
            });
        }
        let link_indent = " ".repeat(current_heading_indent + 4);
        append_line_links(
            &mut items,
            line,
            i,
            &link_indent,
            &link_re,
            &wikilink_re,
        );
    }

    if items.is_empty() {
        items.push(WikiNavItem {
            label: "(empty document)".into(),
            target_file: cur,
            scroll_to: 0,
            kind: NavItemKind::Header,
        });
    }

    dedup_nav_items(items)
}

/// Nav for overview screen: harness + wiki root + index.md outline.
pub fn build_browser_overview_nav(index_content: &str) -> Vec<WikiNavItem> {
    let mut items = vec![
        WikiNavItem {
            label: "Coding Harness".to_string(),
            target_file: String::new(),
            scroll_to: 0,
            kind: NavItemKind::Harness,
        },
        WikiNavItem {
            label: "Wiki".to_string(),
            target_file: "index.md".to_string(),
            scroll_to: 0,
            kind: NavItemKind::Header,
        },
    ];

    let link_re = regex::Regex::new(r"\[([^\]]+)\]\(([^)]+?\.md[^)]*)\)").ok();
    let wikilink_re = regex::Regex::new(r"\[\[([^\]]+?)(?:\.md)?\]\]").ok();
    let mut current_heading_indent = 2usize;

    for (i, line) in index_content.lines().enumerate() {
        if let Some((label, indent)) = parse_heading_browser(line) {
            current_heading_indent = indent;
            items.push(WikiNavItem {
                label,
                target_file: "index.md".to_string(),
                scroll_to: i,
                kind: NavItemKind::Header,
            });
        }
        let link_indent = " ".repeat(current_heading_indent + 2);
        append_line_links(
            &mut items,
            line,
            i,
            &link_indent,
            &link_re,
            &wikilink_re,
        );
    }

    if items.len() == 2 {
        items.push(WikiNavItem {
            label: "  (no wiki files)".into(),
            target_file: "index.md".to_string(),
            scroll_to: 0,
            kind: NavItemKind::Header,
        });
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_wiki_path_strips_junk() {
        assert_eq!(normalize_wiki_path("./notes/foo"), "notes/foo.md");
        assert_eq!(normalize_wiki_path("index.md"), "index.md");
        assert_eq!(normalize_wiki_path(""), "index.md");
    }

    #[test]
    fn extract_links_finds_markdown_and_wiki_style() {
        let md = "# Top\n\nSee [Plan](plan.md) and [[research/notes]].";
        let links = extract_links(md);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].text, "Plan");
        assert_eq!(links[0].target, "plan.md");
        assert_eq!(links[1].target, "research/notes.md");
    }

    #[test]
    fn build_viewer_nav_includes_harness_and_back() {
        let nav = build_viewer_nav("# Hello\n", "other.md");
        assert!(nav.iter().any(|i| i.kind == NavItemKind::Harness));
        assert!(nav.iter().any(|i| i.kind == NavItemKind::Back));
    }

    #[test]
    fn build_browser_overview_nav_includes_wiki_root() {
        let nav = build_browser_overview_nav("");
        assert_eq!(nav.len(), 3); // harness, wiki, empty fallback
        assert_eq!(nav[1].label, "Wiki");
    }
}