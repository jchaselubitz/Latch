//! Snapshot renderings for observers.
//!
//! The live surface never sees any of this: it is painted by the child's own
//! bytes. These renderings serve `latch inspect`, the Conversation Hub, and
//! the gateway preview — the three consumers that read the screen without
//! holding it.

use latch_term::{CellAttrs, Color, Row, ScreenModel};
use serde_json::{json, Value};

use crate::protocol::MAX_TITLE_CHARS;

/// A window title as the daemon is willing to report it.
///
/// The title is the one string a child writes that observers print verbatim
/// — into a tab bar, a log line, a chat message — so it is display text only:
/// control characters (C0, DEL, C1) are removed, which is what keeps an
/// `OSC 0` payload from smuggling an escape sequence into whatever terminal
/// or log shows the title, and it is cut at [`MAX_TITLE_CHARS`].
pub fn sanitize_title(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TITLE_CHARS)
        .collect()
}

/// [`sanitize_title`] over an optional title; an empty result is no title.
pub fn sanitize_title_opt(raw: Option<String>) -> Option<String> {
    raw.map(|title| sanitize_title(&title))
        .filter(|title| !title.is_empty())
}

/// Plain text, one line per row, trailing blanks trimmed.
pub fn text(model: &ScreenModel) -> String {
    let mut out = String::new();
    for row in &model.rows {
        out.push_str(&row.text());
        out.push('\n');
    }
    out
}

/// One row as plain text.
pub fn row_text(row: &Row) -> String {
    row.text()
}

/// Text with SGR sequences, one line per row — the equivalent of
/// `capture-pane -e`.
pub fn styled(model: &ScreenModel) -> String {
    let mut out = String::new();
    for row in &model.rows {
        out.push_str(&styled_row(row));
        out.push('\n');
    }
    out
}

/// One row with SGR sequences; attributes are reset at the end of the line.
pub fn styled_row(row: &Row) -> String {
    let mut out = String::new();
    let mut current = CellAttrs::default();
    let mut pending_blanks = 0usize;
    for cell in &row.cells {
        if cell.width == 0 {
            continue;
        }
        let is_blank = cell.text.is_empty() && cell.attrs == CellAttrs::default();
        if is_blank {
            pending_blanks += 1;
            continue;
        }
        if pending_blanks > 0 {
            if current != CellAttrs::default() {
                out.push_str("\x1b[0m");
                current = CellAttrs::default();
            }
            out.extend(std::iter::repeat_n(' ', pending_blanks));
            pending_blanks = 0;
        }
        if cell.attrs != current {
            out.push_str(&sgr(&cell.attrs));
            current = cell.attrs;
        }
        if cell.text.is_empty() {
            out.push(' ');
        } else {
            out.push_str(&cell.text);
        }
    }
    if current != CellAttrs::default() {
        out.push_str("\x1b[0m");
    }
    out
}

fn sgr(attrs: &CellAttrs) -> String {
    let mut params: Vec<String> = vec!["0".into()];
    if attrs.bold {
        params.push("1".into());
    }
    if attrs.dim {
        params.push("2".into());
    }
    if attrs.italic {
        params.push("3".into());
    }
    if attrs.underline {
        params.push("4".into());
    }
    if attrs.blink {
        params.push("5".into());
    }
    if attrs.reverse {
        params.push("7".into());
    }
    if attrs.invisible {
        params.push("8".into());
    }
    if attrs.strikethrough {
        params.push("9".into());
    }
    match attrs.fg {
        Color::Default => {}
        Color::Indexed(n) if n < 8 => params.push((30 + n as u16).to_string()),
        Color::Indexed(n) if n < 16 => params.push((90 + n as u16 - 8).to_string()),
        Color::Indexed(n) => params.push(format!("38;5;{n}")),
        Color::Rgb(r, g, b) => params.push(format!("38;2;{r};{g};{b}")),
    }
    match attrs.bg {
        Color::Default => {}
        Color::Indexed(n) if n < 8 => params.push((40 + n as u16).to_string()),
        Color::Indexed(n) if n < 16 => params.push((100 + n as u16 - 8).to_string()),
        Color::Indexed(n) => params.push(format!("48;5;{n}")),
        Color::Rgb(r, g, b) => params.push(format!("48;2;{r};{g};{b}")),
    }
    format!("\x1b[{}m", params.join(";"))
}

/// The structured screen the Hub reads: cells with attributes, cursor, modes.
pub fn json(model: &ScreenModel) -> Value {
    let rows: Vec<Value> = model
        .rows
        .iter()
        .map(|row| {
            Value::Array(
                row.cells
                    .iter()
                    .map(|cell| {
                        let mut value = json!({ "t": cell.text, "w": cell.width });
                        if cell.attrs != CellAttrs::default() {
                            value["a"] = attrs_json(&cell.attrs);
                        }
                        value
                    })
                    .collect(),
            )
        })
        .collect();
    json!({
        "cols": model.size.cols,
        "rows": model.size.rows,
        "cursor": {
            "row": model.cursor.row,
            "col": model.cursor.col,
            "visible": model.cursor.visible,
        },
        "alternate_screen": model.alternate_screen,
        "title": sanitize_title_opt(model.title.clone()),
        "modes": {
            "bracketed_paste": model.modes.bracketed_paste,
            "application_cursor_keys": model.modes.application_cursor_keys,
            "application_keypad": model.modes.application_keypad,
            "autowrap": model.modes.autowrap,
            "origin": model.modes.origin,
            "insert": model.modes.insert,
            "focus_reporting": model.modes.focus_reporting,
            "mouse_tracking": format!("{:?}", model.modes.mouse_tracking).to_lowercase(),
        },
        "lines": model.rows.iter().map(Row::text).collect::<Vec<_>>(),
        "cells": rows,
    })
}

fn attrs_json(attrs: &CellAttrs) -> Value {
    let color = |color: Color| match color {
        Color::Default => Value::Null,
        Color::Indexed(n) => json!(n),
        Color::Rgb(r, g, b) => json!([r, g, b]),
    };
    let mut value = json!({});
    if attrs.fg != Color::Default {
        value["fg"] = color(attrs.fg);
    }
    if attrs.bg != Color::Default {
        value["bg"] = color(attrs.bg);
    }
    for (flag, name) in [
        (attrs.bold, "bold"),
        (attrs.dim, "dim"),
        (attrs.italic, "italic"),
        (attrs.underline, "underline"),
        (attrs.blink, "blink"),
        (attrs.reverse, "reverse"),
        (attrs.invisible, "invisible"),
        (attrs.strikethrough, "strikethrough"),
    ] {
        if flag {
            value[name] = json!(true);
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use latch_term::{Screen, Size, Terminal};

    #[test]
    fn titles_are_display_text_only() {
        assert_eq!(sanitize_title("plain title"), "plain title");
        assert_eq!(
            sanitize_title("bad\x1b]0;x\x07\r\ntitle\u{9b}31m\x7f"),
            "bad]0;xtitle31m"
        );
        assert_eq!(
            sanitize_title(&"x".repeat(5000)).chars().count(),
            MAX_TITLE_CHARS
        );
        assert_eq!(sanitize_title_opt(Some("\x01\x02".into())), None);
        assert_eq!(sanitize_title_opt(None), None);
        let mut term = Terminal::with_size(Size::new(10, 2));
        term.advance(b"\x1b]2;a\x01b\x07");
        assert_eq!(json(&term.model())["title"], "ab");
    }

    #[test]
    fn text_and_styled_render_the_screen() {
        let mut term = Terminal::with_size(Size::new(10, 2));
        term.advance(b"hi \x1b[1;31mred\x1b[0m\r\nline2");
        let model = term.model();
        assert_eq!(text(&model), "hi red\nline2\n");
        let styled = styled(&model);
        assert!(
            styled.starts_with("hi \x1b[0;1;31mred\x1b[0m\n"),
            "{styled:?}"
        );
        let value = json(&model);
        assert_eq!(value["lines"][1], "line2");
        assert_eq!(value["cells"][0][3]["a"]["bold"], true);
        assert_eq!(value["cells"][0][3]["a"]["fg"], 1);
    }
}
