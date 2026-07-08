//! Tool block detection and fold/unfold logic for the trace pane.
//!
//! A "tool block" is a `🔧 name(args)` header line followed by zero or more
//! result lines (`   ↳ ...` or `   ` indented continuations).
//!
//! When collapsed, a block shows only its header + a synthetic summary line.
//! When expanded, all lines are visible.

use std::collections::HashSet;

/// A detected tool block in the trace lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBlock {
    /// Index of the `🔧 name(args)` header line in trace_lines.
    pub header_idx: usize,
    /// Index of first result/body line (may equal `end_idx` if no body).
    pub body_start: usize,
    /// Exclusive end of the block (next block's header or end of trace).
    pub end_idx: usize,
    /// Short tool name extracted from the header.
    pub tool_name: String,
    /// True if any body line contains error/failure indicators.
    pub is_error: bool,
    /// First result summary line (the `↳` line content), if any.
    pub summary: String,
}

impl ToolBlock {
    /// Number of body lines (result + continuation lines, excluding header).
    pub fn body_len(&self) -> usize {
        self.end_idx.saturating_sub(self.body_start)
    }
}

/// Scan `trace_lines` and identify all tool blocks.
///
/// A block starts at any line beginning with `🔧` and extends until
/// the next `🔧` header, `🧠` thinking header, or `⭐` judge line.
pub fn detect_tool_blocks(trace_lines: &[String]) -> Vec<ToolBlock> {
    let mut blocks = Vec::new();
    let mut i = 0;

    while i < trace_lines.len() {
        if trace_lines[i].starts_with('🔧') {
            let header_idx = i;
            let tool_name = extract_tool_name(&trace_lines[i]);
            let body_start = i + 1;

            // Scan forward for block body lines.
            // Body lines: `   ↳ ...` or `   ` (3+ space indent) continuations.
            let mut end = body_start;
            while end < trace_lines.len() {
                let line = &trace_lines[end];
                if line.starts_with('🔧')
                    || line.starts_with('🧠')
                    || line.starts_with('⭐')
                    || line.starts_with('🔍')
                    || line.starts_with('📌')
                    || line.starts_with('⏹')
                {
                    break;
                }
                end += 1;
            }

            let is_error = trace_lines[body_start..end]
                .iter()
                .any(|l| l.contains("ERROR") || l.contains("❌") || l.contains("FAIL"));

            let summary = trace_lines
                .get(body_start)
                .map(|l| l.trim().to_string())
                .unwrap_or_default();

            blocks.push(ToolBlock {
                header_idx,
                body_start,
                end_idx: end,
                tool_name,
                is_error,
                summary,
            });

            i = end;
        } else {
            i += 1;
        }
    }

    blocks
}

/// Extract the tool name from a `🔧 name(args)` line.
fn extract_tool_name(line: &str) -> String {
    // Skip the 🔧 prefix (4 bytes + space)
    let after = line
        .strip_prefix('🔧')
        .unwrap_or(line)
        .trim_start();
    // Take up to the first '(' or end
    after
        .split('(')
        .next()
        .unwrap_or(after)
        .trim()
        .to_string()
}

/// A line in the visible output after folding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisibleLine {
    /// An original line from trace_lines (index into trace_lines).
    Original(usize),
    /// A synthetic fold summary replacing a collapsed block body.
    FoldSummary {
        /// Index of the ToolBlock in the blocks vec (for toggling).
        block_idx: usize,
        /// The header_idx this summary belongs to (for fold toggle).
        header_idx: usize,
        /// Number of hidden body lines.
        line_count: usize,
        /// Whether the block had errors.
        is_error: bool,
        /// Short summary text.
        summary: String,
    },
}

/// Given tool blocks and the set of expanded block header indices,
/// compute which lines are visible (with synthetic summaries for collapsed blocks).
pub fn compute_visible_lines(
    trace_lines: &[String],
    blocks: &[ToolBlock],
    expanded: &HashSet<usize>,
) -> Vec<VisibleLine> {
    if blocks.is_empty() {
        return (0..trace_lines.len())
            .map(VisibleLine::Original)
            .collect();
    }

    let mut visible = Vec::new();
    let mut pos = 0; // current position in trace_lines

    for (block_idx, block) in blocks.iter().enumerate() {
        // Add any non-block lines before this block
        while pos < block.header_idx {
            visible.push(VisibleLine::Original(pos));
            pos += 1;
        }

        // Always show the header
        visible.push(VisibleLine::Original(block.header_idx));
        pos = block.body_start;

        let is_expanded = expanded.contains(&block.header_idx) || block.is_error;

        if is_expanded || block.body_len() == 0 {
            // Show all body lines
            while pos < block.end_idx {
                visible.push(VisibleLine::Original(pos));
                pos += 1;
            }
        } else {
            // Collapsed: show synthetic summary
            visible.push(VisibleLine::FoldSummary {
                block_idx,
                header_idx: block.header_idx,
                line_count: block.body_len(),
                is_error: block.is_error,
                summary: block.summary.clone(),
            });
            pos = block.end_idx;
        }
    }

    // Add any trailing lines after the last block
    while pos < trace_lines.len() {
        visible.push(VisibleLine::Original(pos));
        pos += 1;
    }

    visible
}

/// Find which block (by header_idx) the given trace_lines index belongs to.
pub fn block_for_line(blocks: &[ToolBlock], line_idx: usize) -> Option<usize> {
    blocks.iter().find_map(|b| {
        if line_idx >= b.header_idx && line_idx < b.end_idx {
            Some(b.header_idx)
        } else {
            None
        }
    })
}

/// Visible-line index that corresponds to `trace_cursor` (raw trace_lines index).
pub fn visible_index_for_cursor(
    visible: &[VisibleLine],
    trace_cursor: usize,
    blocks: &[ToolBlock],
) -> usize {
    for (i, v) in visible.iter().enumerate() {
        match v {
            VisibleLine::Original(idx) if *idx == trace_cursor => return i,
            VisibleLine::FoldSummary { header_idx, .. }
                if blocks.iter().any(|b| {
                    b.header_idx == *header_idx
                        && trace_cursor >= b.header_idx
                        && trace_cursor < b.end_idx
                }) =>
            {
                return i;
            }
            _ => {}
        }
    }
    visible.len().saturating_sub(1)
}

/// Raw trace_lines index to store in `trace_cursor` for a visible row.
pub fn cursor_line_for_visible(visible: &[VisibleLine], vis_idx: usize) -> usize {
    match visible.get(vis_idx) {
        Some(VisibleLine::Original(i)) => *i,
        Some(VisibleLine::FoldSummary { header_idx, .. }) => *header_idx,
        None => 0,
    }
}

/// Block header to toggle when activating a visible trace row (click or Enter).
pub fn fold_toggle_header(
    trace_lines: &[String],
    blocks: &[ToolBlock],
    vline: &VisibleLine,
) -> Option<usize> {
    match vline {
        VisibleLine::FoldSummary { header_idx, .. } => Some(*header_idx),
        VisibleLine::Original(idx) => {
            if trace_lines.get(*idx).is_some_and(|l| l.starts_with('🔧')) {
                Some(*idx)
            } else {
                block_for_line(blocks, *idx)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fold_toggle_header_on_summary_and_tool_header() {
        let trace = lines(&[
            "🔧 exec(cargo test)",
            "   ↳ ✅ 150 passed",
        ]);
        let blocks = detect_tool_blocks(&trace);
        let visible = compute_visible_lines(&trace, &blocks, &HashSet::new());
        assert!(matches!(visible[1], VisibleLine::FoldSummary { .. }));
        assert_eq!(fold_toggle_header(&trace, &blocks, &visible[0]), Some(0));
        assert_eq!(fold_toggle_header(&trace, &blocks, &visible[1]), Some(0));
    }

    #[test]
    fn detect_single_block() {
        let trace = lines(&[
            "🔧 exec(cargo test)",
            "   ↳ ✅ 150 passed",
        ]);
        let blocks = detect_tool_blocks(&trace);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].header_idx, 0);
        assert_eq!(blocks[0].body_start, 1);
        assert_eq!(blocks[0].end_idx, 2);
        assert_eq!(blocks[0].tool_name, "exec");
        assert!(!blocks[0].is_error);
    }

    #[test]
    fn detect_consecutive_blocks() {
        let trace = lines(&[
            "🔧 exec(cargo test)",
            "   ↳ ✅ ok",
            "🔧 read(src/main.rs)",
            "   ↳ ✅ 42 lines",
        ]);
        let blocks = detect_tool_blocks(&trace);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].end_idx, 2);
        assert_eq!(blocks[1].header_idx, 2);
    }

    #[test]
    fn detect_error_block() {
        let trace = lines(&[
            "🔧 exec(cargo build)",
            "   ↳ ❌ FAILED",
            "   ERROR: compilation failed",
        ]);
        let blocks = detect_tool_blocks(&trace);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].is_error);
    }

    #[test]
    fn detect_with_thinking_lines() {
        let trace = lines(&[
            "🧠 Thinking about the problem...",
            "🔧 exec(ls)",
            "   ↳ ✅ ok",
            "🧠 Now I see...",
        ]);
        let blocks = detect_tool_blocks(&trace);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].header_idx, 1);
        assert_eq!(blocks[0].end_idx, 3);
    }

    #[test]
    fn collapsed_produces_summary() {
        let trace = lines(&[
            "🔧 exec(cargo test)",
            "   ↳ ✅ 150 passed",
            "   Compiling...",
            "   Finished...",
        ]);
        let blocks = detect_tool_blocks(&trace);
        let expanded = HashSet::new(); // nothing expanded
        let visible = compute_visible_lines(&trace, &blocks, &expanded);
        // Header + fold summary = 2 visible lines
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0], VisibleLine::Original(0)); // header
        match &visible[1] {
            VisibleLine::FoldSummary { line_count, .. } => assert_eq!(*line_count, 3),
            _ => panic!("expected FoldSummary"),
        }
    }

    #[test]
    fn expanded_shows_all_lines() {
        let trace = lines(&[
            "🔧 exec(cargo test)",
            "   ↳ ✅ 150 passed",
            "   Compiling...",
        ]);
        let blocks = detect_tool_blocks(&trace);
        let mut expanded = HashSet::new();
        expanded.insert(0); // expand the block at header_idx 0
        let visible = compute_visible_lines(&trace, &blocks, &expanded);
        assert_eq!(visible.len(), 3);
    }

    #[test]
    fn error_blocks_auto_expand() {
        let trace = lines(&[
            "🔧 exec(cargo build)",
            "   ↳ ❌ FAILED",
            "   ERROR: compilation failed",
        ]);
        let blocks = detect_tool_blocks(&trace);
        let expanded = HashSet::new(); // nothing manually expanded
        let visible = compute_visible_lines(&trace, &blocks, &expanded);
        // Error blocks auto-expand
        assert_eq!(visible.len(), 3);
    }

    #[test]
    fn non_block_lines_preserved() {
        let trace = lines(&[
            "🧠 Thinking...",
            "🔧 exec(ls)",
            "   ↳ ✅ ok",
            "📌 Interject queued",
        ]);
        let blocks = detect_tool_blocks(&trace);
        let expanded = HashSet::new();
        let visible = compute_visible_lines(&trace, &blocks, &expanded);
        // 🧠 line + header + fold summary + 📌 line = 4
        assert_eq!(visible.len(), 4);
        assert_eq!(visible[0], VisibleLine::Original(0)); // thinking
        assert_eq!(visible[3], VisibleLine::Original(3)); // interject
    }

    #[test]
    fn block_for_line_finds_correct_block() {
        let trace = lines(&[
            "🧠 Thinking...",
            "🔧 exec(ls)",
            "   ↳ ✅ ok",
            "   details...",
            "🔧 read(foo)",
            "   ↳ content",
        ]);
        let blocks = detect_tool_blocks(&trace);
        assert_eq!(block_for_line(&blocks, 0), None); // thinking line
        assert_eq!(block_for_line(&blocks, 1), Some(1)); // exec header
        assert_eq!(block_for_line(&blocks, 2), Some(1)); // exec body
        assert_eq!(block_for_line(&blocks, 3), Some(1)); // exec body
        assert_eq!(block_for_line(&blocks, 4), Some(4)); // read header
        assert_eq!(block_for_line(&blocks, 5), Some(4)); // read body
    }

    #[test]
    fn extract_tool_name_works() {
        assert_eq!(extract_tool_name("🔧 exec(cargo test --lib)"), "exec");
        assert_eq!(extract_tool_name("🔧 read(src/main.rs)"), "read");
        assert_eq!(extract_tool_name("🔧 system note"), "system note");
    }

    #[test]
    fn empty_trace() {
        let blocks = detect_tool_blocks(&[]);
        assert!(blocks.is_empty());
        let visible = compute_visible_lines(&[], &blocks, &HashSet::new());
        assert!(visible.is_empty());
    }
}
