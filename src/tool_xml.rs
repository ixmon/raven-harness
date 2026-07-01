//! XML tool-call parsing and stripping.
//!
//! Qwen / some llama.cpp templates emit tool calls as XML inside the
//! assistant content stream rather than as structured `tool_calls` in the
//! API response.  This module contains all logic for:
//!
//! - **Parsing** those XML fragments into [`ToolCall`] structs
//! - **Detecting** whether text contains malformed/partial tool syntax
//! - **Stripping** tool XML from visible text (for display and history)
//!
//! Extracted from `llm.rs` to isolate the most fragile subsystem behind
//! a clean boundary.

use crate::llm::{FunctionCall, ToolCall};

// ── Parsing ──────────────────────────────────────────────────────────────────

/// Parse XML-style tool calls from assistant content.
///
/// Handles two variants:
/// 1. Full: `<tool_call>\n<function=name>\n<parameter=key>value</parameter>\n</function>\n</tool_call>`
/// 2. Truncated (llama.cpp streaming often strips the leading `<tool_call>\n<`):
///    `function=name>\n<parameter=key>value</parameter>\n</function>\n</tool_call>`
pub fn parse_xml_tool_calls_from_content(content: &str) -> Vec<ToolCall> {
    // Quick reject: need either `<function=` or bare `function=` or parameter fragments
    let has_xml_function = content.contains("<function=") || content.contains("<function>");
    let has_bare_function = content.contains("function=") || content.contains("function>");
    let has_parameter = content.contains("<parameter")
        || content.contains("parameter=")
        || content.contains("parameter ");
    if !has_xml_function && !has_bare_function && !has_parameter {
        return vec![];
    }

    // Normalize: if the content starts with the truncated form (missing `<`),
    // prepend `<` so the rest of the parser sees `<function=`.
    let normalized: std::borrow::Cow<str> = if !has_xml_function && has_bare_function {
        // Find where `function=` or `function>` appears and insert `<` before it
        if let Some(pos) = content
            .find("function=")
            .or_else(|| content.find("function>"))
        {
            let mut s = content.to_string();
            if pos == 0 || content.as_bytes().get(pos.wrapping_sub(1)) != Some(&b'<') {
                s.insert(pos, '<');
            }
            std::borrow::Cow::Owned(s)
        } else {
            std::borrow::Cow::Borrowed(content)
        }
    } else {
        std::borrow::Cow::Borrowed(content)
    };

    let blocks: Vec<&str> = if normalized.contains("<tool_call>") {
        normalized
            .split("<tool_call>")
            .skip(1)
            .filter_map(|s| s.split("</tool_call>").next())
            .collect()
    } else {
        vec![&normalized]
    };

    let mut out = vec![];
    for (idx, block) in blocks.iter().enumerate() {
        let Some(name) = xml_function_name(block) else {
            continue;
        };
        let arguments =
            serde_json::to_string(&xml_parameters(block)).unwrap_or_else(|_| "{}".into());
        out.push(ToolCall {
            id: format!("xml-{name}-{idx}"),
            r#type: "function".into(),
            function: FunctionCall { name, arguments },
        });
    }
    out
}

fn xml_function_name(block: &str) -> Option<String> {
    let start = block.find("<function=")? + "<function=".len();
    let rest = &block[start..];
    let end = rest.find('>')?;
    let name = rest[..end].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn xml_parameters(block: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    let mut cursor = block;
    // Support <parameter , <parameter= , bare parameter=
    while let Some(start) = cursor
        .find("<parameter")
        .or_else(|| cursor.find("parameter="))
    {
        let after_start = &cursor[start..];
        let marker_len = if after_start.starts_with("<parameter=") {
            "<parameter=".len()
        } else if after_start.starts_with("parameter=") {
            "parameter=".len()
        } else if after_start.starts_with("<parameter") {
            // find = or >
            if let Some(eq) = after_start.find('=') {
                eq + 1
            } else if let Some(gt) = after_start.find('>') {
                gt + 1 // key might be missing, skip
            } else {
                cursor = &cursor[start + 1..];
                continue;
            }
        } else {
            "parameter=".len()
        };
        let after_key = &cursor[start + marker_len..];
        let Some(key_end) = after_key.find('>') else {
            break;
        };
        let key = after_key[..key_end].trim().to_string();
        let value_block = &after_key[key_end + 1..];
        let Some(value_end) = value_block
            .find("</parameter>")
            .or_else(|| value_block.find("<parameter"))
            .or_else(|| value_block.find("parameter="))
        else {
            // take to end if no close
            let value = value_block.trim();
            if !value.is_empty() {
                map.insert(key, serde_json::Value::String(value.to_string()));
            }
            break;
        };
        let value = value_block[..value_end].trim();
        if !value.is_empty() {
            let v = parse_xml_value(value);
            map.insert(key, v);
        }
        cursor = &value_block[value_end..];
    }
    map
}

fn parse_xml_value(s: &str) -> serde_json::Value {
    let t = s.trim();
    if t.eq_ignore_ascii_case("true") {
        return serde_json::Value::Bool(true);
    }
    if t.eq_ignore_ascii_case("false") {
        return serde_json::Value::Bool(false);
    }
    if let Ok(n) = t.parse::<i64>() {
        return serde_json::Value::Number(n.into());
    }
    if let Ok(n) = t.parse::<f64>() {
        if let Some(num) = serde_json::Number::from_f64(n) {
            return serde_json::Value::Number(num);
        }
    }
    serde_json::Value::String(t.to_string())
}

// ── Detection ────────────────────────────────────────────────────────────────

/// Check whether text contains XML-style tool call syntax fragments.
///
/// Used by the steering engine to detect malformed/partial tool calls
/// that didn't parse as structured tool_calls.
pub fn contains_tool_xml_syntax(text: &str) -> bool {
    let t = text.to_lowercase();
    if t.contains("<tool_call") || t.contains("</tool_call") || t.contains("tool_call") ||
       t.contains("function=") || t.contains("<function=") || t.contains("function>") ||
       t.contains("<parameter") || t.contains("parameter=") || t.contains("parameter ") ||
       t.contains("</parameter") || t.contains("</function") ||
       t.contains("path>") ||
       // partial outputs like "read><parameter" or "read>"
       t.contains("read>") || t.contains("write>") || t.contains("patch>") ||
       t.contains("exec>") || t.contains("grep>") || t.contains("list>") ||
       // Additional common XML / pseudo-XML tool call formats from various servers/models
       t.contains("<invoke") || t.contains("invoke tool") || t.contains("tool request") ||
       t.contains("call tool") || t.contains("tool call") || t.contains("name=") ||
       t.contains("</invoke") || t.contains("xai:")
    {
        return true;
    }
    // Also flag very short texts that are exactly / primarily a bare tool verb.
    // Safe because this only fires for short fragments; long narrative containing
    // the word "read" etc. will have been caught (or not) by the above and will
    // have other sentence structure.
    let trimmed = text.trim();
    if trimmed.len() <= 12 {
        let l = trimmed.to_lowercase();
        if matches!(
            l.as_str(),
            "function" | "read" | "write" | "patch" | "exec" | "grep" | "list" | "parameter" | "invoke"
        ) || l == "function"
            || l.starts_with("function")
            || l.starts_with("read>")
            || l.starts_with("write>")
            || l.starts_with("invoke")
        {
            return true;
        }
    }
    false
}

// ── Stripping ────────────────────────────────────────────────────────────────

/// Remove XML-style tool call blocks (and any immediately preceding stray "}"
/// or punctuation that was part of a mixed-generation prefix) from assistant
/// content.
///
/// This is used for the Qwen/llama.cpp-style fallback where the model emits
/// `function=...` or `<tool_call>...</tool_call>` (or `}<function...` or stray
/// closing tags like `}</parameter></function></tool_call>`, or bare fragments
/// like `parameter=path>...` or `=path>...`) inside the normal content stream.
///
/// We keep any real narrative the model produced before the tool syntax,
/// but we do *not* want raw/partial tool XML or a dangling "}" stored in
/// conversation history or treated as the model's "final text" / Agent output.
pub fn strip_xml_tool_call_blocks(content: &str) -> String {
    if content.trim().is_empty() {
        return String::new();
    }

    // Use regex for more robust detection of any tool call syntax fragment, including partials and variants like "read><parameter "
    // This is more reliable than a static list of contains for catching model-specific or truncated emissions.
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)(<tool_call|</tool_call|tool_call|function=|<function=|function>|invoke|tool request|call tool|<parameter|parameter=|parameter |</parameter|</function|path>|read>|write>|patch>|exec>|grep>|list>|name=|<invoke|</invoke)") .unwrap()
    });

    let mut s = if let Some(mat) = RE.find(content) {
        content[0..mat.start()].to_string()
    } else {
        content.to_string()
    };

    // Aggressively remove any remaining fragments (regex replace for variants)
    static REMOVE_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)</?tool_call>|</?function[^>]*>|</?parameter[^>]*>|</?invoke[^>]*>|\bfunction=[^>\s]*>|\bpath>[^\s<]*|read>|write>|patch>|exec>|grep>|list>|tool request|call tool|name=").unwrap()
    });
    s = REMOVE_RE.replace_all(&s, "").to_string();

    let mut out = s.trim().to_string();

    // Drop pure junk / dangling closer that was right before the XML (the exact case reported)
    let cleaned = out
        .trim_end_matches(|c: char| " \t\n\r{}[],.;:()".contains(c))
        .trim()
        .to_string();
    if out == "}"
        || out == "},"
        || cleaned.is_empty()
        || (out.len() <= 5 && !out.chars().any(|c| c.is_alphanumeric()))
    {
        return String::new();
    }

    // Treat very short responses that are exactly a bare tool verb (or common prefix of one)
    // as XML leakage even if the completing "=..." or ">" never arrived in this content stream.
    // These can leak when the first token(s) are forwarded before full marker is visible in full_text.
    let lower = out.to_lowercase();
    if matches!(
        lower.as_str(),
        "function" | "read" | "write" | "patch" | "exec" | "grep" | "list" | "parameter"
    ) || (out.len() <= 12
        && (lower == "function"
            || lower.starts_with("function")
            || lower.starts_with("read")
            || lower.starts_with("write")
            || lower.starts_with("patch")))
    {
        return String::new();
    }

    // If the remaining ends with a stray closer, trim it.
    if out.ends_with('}') || out.ends_with("},") {
        let core = out
            .trim_end_matches(|c: char| " \t\n\r{}[],.;:()".contains(c))
            .trim()
            .to_string();
        if core.is_empty() || core.len() + 3 >= out.len() {
            out = core;
        }
    }

    out.trim().to_string()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_xml_tool_calls_from_qwen_style_content() {
        let content = r#"<tool_call>
<function=list>
<parameter=path>
.
</parameter>
</function>
</tool_call>
<tool_call>
<function=read_summary>
<parameter=path>
README.md
</parameter>
</function>
</tool_call>"#;
        let calls = parse_xml_tool_calls_from_content(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "list");
        assert_eq!(calls[0].function.arguments, r#"{"path":"."}"#);
        assert_eq!(calls[1].function.name, "read_summary");
        assert_eq!(calls[1].function.arguments, r#"{"path":"README.md"}"#);
    }

    #[test]
    fn parse_xml_tool_calls_truncated_streaming_format() {
        // llama.cpp streaming sometimes strips the leading `<tool_call>\n<`
        // so the content starts with bare `function=name>`.
        let content =
            "function=list>\n<parameter=path>\n.\n</parameter>\n</function>\n</tool_call>";
        let calls = parse_xml_tool_calls_from_content(content);
        assert_eq!(calls.len(), 1, "should parse truncated XML: {:?}", calls);
        assert_eq!(calls[0].function.name, "list");
        assert_eq!(calls[0].function.arguments, r#"{"path":"."}"#);
    }

    #[test]
    fn strip_xml_tool_call_blocks_removes_stray_closer_plus_closing_tags() {
        // Exact case reported: model emitted "}" + tail of XML tool call syntax
        // (no opening "function=" seen by this fragment). Must become empty so we
        // never push or display it as Agent text / assistant content.
        let bad = "}\n</parameter>\n</function>\n</tool_call>";
        assert_eq!(strip_xml_tool_call_blocks(bad), "");

        // Also handles mixed with opening
        let mixed = "I will call the tool now}\n<tool_call>\n<function=read>\n<parameter=path>src/foo.py</parameter>\n</function>\n</tool_call>";
        let cleaned = strip_xml_tool_call_blocks(mixed);
        assert!(
            !cleaned.contains("function="),
            "should strip XML: {}",
            cleaned
        );
        assert!(
            !cleaned.contains("</tool_call>"),
            "should strip XML: {}",
            cleaned
        );
        assert!(
            cleaned.contains("I will call"),
            "should keep prefix narrative"
        );

        // Pure opening form also stripped to empty when no narrative
        let pure = "function=patch>\n<parameter=path>bar.rs</parameter>\n</function>\n</tool_call>";
        assert_eq!(strip_xml_tool_call_blocks(pure), "");

        // Fragment like the reported case (model emitted tail of XML syntax)
        let frag = "=path>  src/marshmallow/schema.py";
        assert_eq!(strip_xml_tool_call_blocks(frag), "");

        let frag2 = "parameter=path> src/marshmallow/schema.py\n</parameter>";
        assert_eq!(strip_xml_tool_call_blocks(frag2), "");
    }
}
