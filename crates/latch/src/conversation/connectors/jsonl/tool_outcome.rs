//! How a Claude tool call is described: its input by named descriptive
//! fields only, and its result by shape, or by its first error line.
use super::*;

/// Upper bound of the input description or failure detail within a summary.
const MAX_SUMMARY_PART_CHARS: usize = 160;

/// A safe, human-readable description of a tool call's input. Only named,
/// descriptive fields are used; raw commands, file contents, and arbitrary
/// input objects are never copied.
pub(super) fn safe_tool_summary(name: &str, input: Option<&Value>) -> String {
    let field = |key: &str| {
        input
            .and_then(|input| input.get(key))
            .and_then(Value::as_str)
            .map(|value| sanitize_part(value, MAX_SUMMARY_PART_CHARS))
            .filter(|value| !value.is_empty())
    };
    field("description")
        .or_else(|| {
            ["file_path", "notebook_path", "path", "pattern", "url"]
                .into_iter()
                .find_map(field)
        })
        .unwrap_or_else(|| {
            if name == "Bash" {
                "Bash command".to_owned()
            } else {
                String::new()
            }
        })
}

/// The status and summary of a finished Claude tool call, from its
/// `tool_result` block. A successful result is described by its shape only:
/// its content can be a file, a command's output, or anything else the tool
/// read, so it never reaches the summary. A failure carries the first line of
/// its error, sanitized, because that line is what the user needs to know.
pub(super) fn claude_tool_outcome(input: &str, result: &Value) -> (ToolStatus, String) {
    let failed = result.get("is_error").and_then(Value::as_bool) == Some(true);
    let mut text = String::new();
    let mut images = 0usize;
    match result.get("content") {
        Some(Value::String(content)) => text.push_str(content),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                match string_value(block, "type").as_deref() {
                    Some("text") => {
                        if let Some(part) = block.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(part);
                        }
                    }
                    Some("image") => images += 1,
                    _ => {}
                }
            }
        }
        _ => {}
    }
    let outcome = if failed {
        let detail = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| sanitize_part(line, MAX_SUMMARY_PART_CHARS))
            .unwrap_or_default();
        if detail.is_empty() {
            "failed".to_owned()
        } else {
            format!("failed: {detail}")
        }
    } else {
        let lines = text.lines().filter(|line| !line.trim().is_empty()).count();
        match (lines, images) {
            (0, 0) => "no output".to_owned(),
            (0, 1) => "returned an image".to_owned(),
            (0, n) => format!("returned {n} images"),
            (1, _) => "returned 1 line".to_owned(),
            (n, _) => format!("returned {n} lines"),
        }
    };
    let summary = if input.is_empty() {
        outcome
    } else {
        format!("{input} · {outcome}")
    };
    let status = if failed {
        ToolStatus::Failed
    } else {
        ToolStatus::Succeeded
    };
    (status, sanitize_summary(&summary))
}
