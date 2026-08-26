//! Snapshot renderings for observers.
//!
//! The live surface never sees any of this: it is painted by the child's own
//! bytes. These renderings serve `latch inspect`, the Conversation Hub, and
//! the gateway preview — the three consumers that read the screen without
//! holding it.

use latch_term::{CellAttrs, Color, Row, ScreenModel};
use serde_json::{json, Value};

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
        "title": model.title,
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
