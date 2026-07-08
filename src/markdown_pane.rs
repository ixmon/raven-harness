//! Shared helpers for scrollable markdown panes (wiki viewer, picker summary, overview).

use crate::list_pane::list_selection_style;
use crate::md_render;
use ratatui::text::Text;

/// Render markdown and return a viewport slice `[scroll, scroll + max_lines)`.
pub fn markdown_viewport(md: &str, scroll: usize, max_lines: usize) -> Text<'static> {
    let full = md_render::render_markdown(md);
    Text::from(
        full.lines
            .into_iter()
            .skip(scroll)
            .take(max_lines)
            .collect::<Vec<_>>(),
    )
}

/// Highlight lines matching `search` (used when a nav item targets a heading/link).
pub fn highlight_markdown_viewport(
    text: &mut Text<'_>,
    search: &str,
    scroll: usize,
    _max_lines: usize,
) {
    if search.is_empty() && scroll == 0 {
        return;
    }
    for (src_idx, line) in text.lines.iter_mut().enumerate() {
        let abs_line = scroll + src_idx;
        let is_active_region = if !search.is_empty() {
            line.spans.iter().any(|s| s.content.contains(search))
        } else {
            abs_line == scroll || abs_line == scroll.saturating_add(1)
        };
        if !is_active_region {
            continue;
        }
        for span in &mut line.spans {
            let matches = !search.is_empty() && span.content.contains(search);
            if matches || is_active_region {
                span.style = list_selection_style();
            }
        }
    }
}

/// Nav highlight search string from a selected wiki nav item label/kind.
pub fn nav_highlight_search(
    kind: crate::wiki_doc::NavItemKind,
    label: &str,
) -> String {
    match kind {
        crate::wiki_doc::NavItemKind::Header => label.trim().trim_start_matches('#').trim().to_string(),
        crate::wiki_doc::NavItemKind::Link => label.trim_start_matches("→ ").trim().to_string(),
        _ => String::new(),
    }
}