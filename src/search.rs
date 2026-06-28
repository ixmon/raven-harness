//! In-TUI conversation and trace pane search.

use crate::tui_render::Pane;

#[derive(Clone, Debug, Default)]
pub struct SearchState {
    pub active: bool,
    pub query: String,
    pub match_lines: Vec<usize>,
    pub match_idx: usize,
    pub pane: Pane,
}

impl SearchState {
    pub fn status_label(&self) -> String {
        if !self.active || self.query.is_empty() {
            return String::new();
        }
        if self.match_lines.is_empty() {
            format!("search: '{}' (0)", self.query)
        } else {
            format!(
                "search: '{}' ({}/{})",
                self.query,
                self.match_idx + 1,
                self.match_lines.len()
            )
        }
    }
}

pub fn collect_pane_lines(
    pane: Pane,
    left_committed: &[String],
    current_response: &str,
    trace_lines: &[String],
    current_thinking: &str,
) -> Vec<String> {
    match pane {
        Pane::Left => {
            let mut lines = Vec::new();
            for entry in left_committed {
                for line in entry.lines() {
                    lines.push(line.to_string());
                }
                lines.push(String::new());
            }
            if !current_response.is_empty() {
                lines.push("Agent (streaming):".to_string());
                for line in current_response.lines() {
                    lines.push(line.to_string());
                }
            }
            lines
        }
        Pane::Right => {
            let mut lines: Vec<String> = trace_lines.to_vec();
            if !current_thinking.is_empty() {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.push("Thinking (live):".to_string());
                for line in current_thinking.lines() {
                    lines.push(line.to_string());
                }
            }
            lines
        }
    }
}

pub fn find_matches(lines: &[String], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return vec![];
    }
    let q = query.to_lowercase();
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

pub fn run_search(
    state: &mut SearchState,
    left_committed: &[String],
    current_response: &str,
    trace_lines: &[String],
    current_thinking: &str,
    focused_pane: Pane,
) -> Option<usize> {
    state.pane = focused_pane;
    let lines = collect_pane_lines(
        state.pane,
        left_committed,
        current_response,
        trace_lines,
        current_thinking,
    );
    state.match_lines = find_matches(&lines, &state.query);
    state.match_idx = 0;
    state.match_lines.first().copied()
}

pub fn scroll_to_line(line: usize, content_height: u16) -> u16 {
    line.saturating_sub(content_height as usize / 2) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_matches_case_insensitive() {
        let lines = vec!["Hello World".into(), "foo".into(), "HELLO again".into()];
        let m = find_matches(&lines, "hello");
        assert_eq!(m, vec![0, 2]);
    }
}
