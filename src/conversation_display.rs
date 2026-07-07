//! Hydrate persisted conversation log entries into left-pane display lines.

/// Convert `(role, content)` pairs from `Session::load_recent_conversation` into
/// display strings for the conversation pane.
pub fn format_conversation_lines(recent: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::with_capacity(recent.len());
    for (role, content) in recent {
        let disp = if role == "user" {
            format!("> {}", content)
        } else {
            raven_tui::llm::strip_xml_tool_call_blocks(content)
        };
        if !disp.trim().is_empty() {
            out.push(disp);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_lines_get_prompt_prefix() {
        let recent = vec![("user".into(), "hello".into())];
        assert_eq!(format_conversation_lines(&recent), vec!["> hello"]);
    }

    #[test]
    fn assistant_xml_stripped() {
        let recent = vec![(
            "assistant".into(),
            "hi<tool_call>x</tool_call>".into(),
        )];
        let lines = format_conversation_lines(&recent);
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains("tool_call"));
    }

    #[test]
    fn blank_lines_skipped() {
        let recent = vec![("assistant".into(), "   ".into())];
        assert!(format_conversation_lines(&recent).is_empty());
    }
}