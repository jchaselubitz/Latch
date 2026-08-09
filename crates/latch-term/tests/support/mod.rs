//! Loading `fixtures/vt/` and asserting screens against it.
//!
//! Like the protocol suite, this module interprets the fixture format by hand
//! rather than deriving it from the crate under test. `expected.json` states
//! what the recorded bytes mean; the emulator is measured against that reading,
//! not against a serialization of itself.
//!
//! The comparison helpers here are also where failures are made legible. A
//! screen is 3000 cells, and `assert_eq!` on two of those prints something
//! nobody will read — so a mismatch is reported as the first differing cell,
//! with both rows rendered.

#![allow(dead_code)]

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use latch_term::{
    Cell, CellAttrs, Color, MouseEncoding, MouseTracking, Screen, ScreenModel, Size, Terminal,
    TerminalConfig,
};
use serde_json::Value;

/// A resize applied partway through a stream.
#[derive(Debug, Clone, Copy)]
pub struct Resize {
    /// Byte offset in `input.bin` at which the resize landed when recorded.
    pub at_byte: usize,
    /// New size.
    pub size: Size,
}

/// One recorded case.
pub struct Case {
    /// Directory name, which is also the case name.
    pub name: String,
    /// Why the case exists. Printed on failure — a bare case name is not enough
    /// to know what a reviewer was protecting.
    pub description: String,
    /// Size at the start of the stream.
    pub size: Size,
    /// Resizes applied partway through, in stream order.
    pub resizes: Vec<Resize>,
    /// The recorded PTY output.
    pub input: Vec<u8>,
    /// The hand-authored partial screen model, if the case carries one.
    pub expected: Option<Value>,
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/vt")
        .canonicalize()
        .expect("fixtures/vt must exist")
}

/// Loads every case, sorted by name.
pub fn cases() -> Vec<Case> {
    let dir = fixtures_dir();
    let mut cases: Vec<Case> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|path| path.is_dir())
        .map(|path| load_case(&path))
        .collect();
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    assert!(!cases.is_empty(), "fixtures/vt/ holds no cases");
    cases
}

/// Loads one case by name.
pub fn case(name: &str) -> Case {
    load_case(&fixtures_dir().join(name))
}

fn load_case(dir: &Path) -> Case {
    let name = dir
        .file_name()
        .expect("case dir has a name")
        .to_string_lossy()
        .into_owned();
    let meta: Value = serde_json::from_slice(
        &fs::read(dir.join("meta.json")).unwrap_or_else(|e| panic!("{name}: meta.json: {e}")),
    )
    .unwrap_or_else(|e| panic!("{name}: meta.json is not JSON: {e}"));

    let size = parse_size(&meta["size"], &name);
    let resizes = meta
        .get("resizes")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| Resize {
                    at_byte: usize::try_from(
                        item["at_byte"]
                            .as_u64()
                            .expect("resize.at_byte is a number"),
                    )
                    .expect("resize.at_byte fits usize"),
                    size: parse_size(item, &name),
                })
                .collect()
        })
        .unwrap_or_default();

    let input =
        fs::read(dir.join("input.bin")).unwrap_or_else(|e| panic!("{name}: input.bin: {e}"));

    let expected_path = dir.join("expected.json");
    let expected = if expected_path.exists() {
        Some(
            serde_json::from_slice(&fs::read(&expected_path).expect("expected.json readable"))
                .unwrap_or_else(|e| panic!("{name}: expected.json is not JSON: {e}")),
        )
    } else {
        None
    };

    Case {
        name,
        description: meta["description"]
            .as_str()
            .unwrap_or("(no description)")
            .to_string(),
        size,
        resizes,
        input,
        expected,
    }
}

fn parse_size(value: &Value, name: &str) -> Size {
    let cols = value["cols"]
        .as_u64()
        .unwrap_or_else(|| panic!("{name}: size.cols missing"));
    let rows = value["rows"]
        .as_u64()
        .unwrap_or_else(|| panic!("{name}: size.rows missing"));
    Size::new(cols as u16, rows as u16)
}

impl Case {
    /// Builds a terminal at the case's initial size.
    pub fn terminal(&self) -> Terminal {
        Terminal::new(TerminalConfig::new(self.size))
    }

    /// Feeds the whole stream, applying the case's resizes at the offsets they
    /// landed at when recorded.
    pub fn play(&self) -> Terminal {
        self.play_chunked(usize::MAX)
    }

    /// The same, but delivering the bytes in chunks of at most `chunk_size`.
    ///
    /// A PTY read boundary falls wherever it falls, and it will land in the
    /// middle of an escape sequence, a code point, and a grapheme cluster. The
    /// resulting screen must not depend on where.
    pub fn play_chunked(&self, chunk_size: usize) -> Terminal {
        let mut term = self.terminal();
        self.feed_into(&mut term, chunk_size);
        term
    }

    /// Feeds the stream into an existing terminal.
    pub fn feed_into(&self, term: &mut Terminal, chunk_size: usize) {
        let chunk_size = chunk_size.max(1);
        let mut offset = 0usize;
        let mut pending = self.resizes.iter().peekable();
        while offset < self.input.len() {
            while pending
                .peek()
                .is_some_and(|resize| resize.at_byte <= offset)
            {
                term.resize(pending.next().expect("peeked").size);
            }
            let next_stop = pending
                .peek()
                .map(|resize| resize.at_byte)
                .unwrap_or(self.input.len());
            let end = self
                .input
                .len()
                .min(next_stop.max(offset + 1))
                .min(offset.saturating_add(chunk_size));
            term.advance(&self.input[offset..end]);
            offset = end;
        }
        for resize in pending {
            term.resize(resize.size);
        }
    }

    /// The size in effect at the end of the stream.
    pub fn final_size(&self) -> Size {
        self.resizes.last().map(|r| r.size).unwrap_or(self.size)
    }
}

/// Replays `snapshot` into a terminal freshly reset at `size`.
///
/// "Freshly reset" is the contract: the snapshot must carry everything, because
/// the client it is sent to has just connected and knows nothing.
pub fn replay(snapshot: &[u8], size: Size) -> Terminal {
    let mut term = Terminal::new(TerminalConfig::new(size));
    term.reset();
    term.advance(snapshot);
    term
}

// --- Comparison -------------------------------------------------------------

/// Asserts two screens are identical, reporting the first difference in a form
/// a person can act on.
pub fn assert_screens_identical(context: &str, before: &ScreenModel, after: &ScreenModel) {
    if let Some(diff) = describe_difference(before, after) {
        panic!("{context}: screens differ after snapshot round-trip\n{diff}");
    }
}

/// The first meaningful difference between two screens, if any.
pub fn describe_difference(a: &ScreenModel, b: &ScreenModel) -> Option<String> {
    let mut out = String::new();
    if a.size != b.size {
        let _ = writeln!(out, "  size: {:?} vs {:?}", a.size, b.size);
    }
    if a.alternate_screen != b.alternate_screen {
        let _ = writeln!(
            out,
            "  alternate_screen: {} vs {}",
            a.alternate_screen, b.alternate_screen
        );
    }
    if a.cursor != b.cursor {
        let _ = writeln!(out, "  cursor: {:?} vs {:?}", a.cursor, b.cursor);
    }
    if a.scroll_region != b.scroll_region {
        let _ = writeln!(
            out,
            "  scroll_region: {:?} vs {:?}",
            a.scroll_region, b.scroll_region
        );
    }
    if a.modes != b.modes {
        let _ = writeln!(out, "  modes: {:?} vs {:?}", a.modes, b.modes);
    }
    if a.title != b.title {
        let _ = writeln!(out, "  title: {:?} vs {:?}", a.title, b.title);
    }
    if a.pen != b.pen {
        let _ = writeln!(out, "  pen: {:?} vs {:?}", a.pen, b.pen);
    }
    if a.rows.len() != b.rows.len() {
        let _ = writeln!(out, "  row count: {} vs {}", a.rows.len(), b.rows.len());
    }
    for (index, (left, right)) in a.rows.iter().zip(b.rows.iter()).enumerate() {
        if left == right {
            continue;
        }
        let _ = writeln!(out, "  row {index}:");
        let _ = writeln!(out, "    left : {:?}", left.text());
        let _ = writeln!(out, "    right: {:?}", right.text());
        for (col, (lc, rc)) in left.cells.iter().zip(right.cells.iter()).enumerate() {
            if lc != rc {
                let _ = writeln!(out, "    first differing cell, col {col}:");
                let _ = writeln!(out, "      left : {lc:?}");
                let _ = writeln!(out, "      right: {rc:?}");
                break;
            }
        }
        break;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

// --- expected.json ----------------------------------------------------------

/// Asserts a screen against a case's partial expectation.
///
/// Only keys present in the fixture are checked. Every assertion names the case
/// and its description, so a failure says what the case was protecting rather
/// than only what did not match.
pub fn assert_expected(case: &Case, model: &ScreenModel) {
    let Some(expected) = case.expected.as_ref() else {
        return;
    };
    let ctx = format!("{}: {}", case.name, case.description);

    if let Some(size) = expected.get("size") {
        assert_eq!(
            model.size,
            parse_size(size, &case.name),
            "{ctx}\n  size mismatch"
        );
    }
    if let Some(alt) = expected.get("alternate_screen").and_then(Value::as_bool) {
        assert_eq!(
            model.alternate_screen, alt,
            "{ctx}\n  alternate_screen mismatch"
        );
    }
    if let Some(cursor) = expected.get("cursor") {
        if let Some(row) = cursor.get("row").and_then(Value::as_u64) {
            assert_eq!(model.cursor.row, row as u16, "{ctx}\n  cursor row mismatch");
        }
        if let Some(col) = cursor.get("col").and_then(Value::as_u64) {
            assert_eq!(model.cursor.col, col as u16, "{ctx}\n  cursor col mismatch");
        }
        if let Some(visible) = cursor.get("visible").and_then(Value::as_bool) {
            assert_eq!(
                model.cursor.visible, visible,
                "{ctx}\n  cursor visibility mismatch"
            );
        }
    }
    if let Some(region) = expected.get("scroll_region") {
        let top = region["top"].as_u64().expect("scroll_region.top") as u16;
        let bottom = region["bottom"].as_u64().expect("scroll_region.bottom") as u16;
        assert_eq!(model.scroll_region.top, top, "{ctx}\n  scroll region top");
        assert_eq!(
            model.scroll_region.bottom, bottom,
            "{ctx}\n  scroll region bottom"
        );
    }
    if let Some(modes) = expected.get("modes").and_then(Value::as_object) {
        for (key, want) in modes {
            assert_mode(&ctx, model, key, want);
        }
    }
    if let Some(title) = expected.get("title") {
        let want = title.as_str().map(str::to_string);
        assert_eq!(model.title, want, "{ctx}\n  title mismatch");
    }
    if let Some(pen) = expected.get("pen") {
        if pen.as_str() == Some("default") {
            assert_eq!(
                model.pen,
                CellAttrs::default(),
                "{ctx}\n  pen should be the default rendition"
            );
        } else {
            let mut want = CellAttrs::default();
            apply_attrs(&mut want, pen);
            assert_eq!(model.pen, want, "{ctx}\n  pen mismatch");
        }
    }
    if let Some(rows) = expected.get("rows").and_then(Value::as_object) {
        for (index, want) in rows {
            let row: u16 = index.parse().expect("row key is a number");
            let got = model.row_text(row);
            assert_eq!(
                got,
                want.as_str().expect("row expectation is a string"),
                "{ctx}\n  row {row} text mismatch"
            );
        }
    }
    if let Some(cells) = expected.get("cells").and_then(Value::as_array) {
        for want in cells {
            assert_cell(&ctx, model, want);
        }
    }
    if let Some(items) = expected.get("contains").and_then(Value::as_array) {
        let screen: Vec<String> = (0..model.size.rows).map(|r| model.row_text(r)).collect();
        for item in items {
            let needle = item.as_str().expect("contains entry is a string");
            assert!(
                screen.iter().any(|row| row.contains(needle)),
                "{ctx}\n  no row contains {needle:?}\n  screen:\n{}",
                screen.join("\n")
            );
        }
    }
}

fn assert_mode(ctx: &str, model: &ScreenModel, key: &str, want: &Value) {
    let modes = model.modes;
    let flag = |name: &str, got: bool| {
        assert_eq!(
            got,
            want.as_bool()
                .unwrap_or_else(|| panic!("{ctx}\n  mode {name} expectation is not a bool")),
            "{ctx}\n  mode {name} mismatch"
        );
    };
    match key {
        "bracketed_paste" => flag(key, modes.bracketed_paste),
        "application_cursor_keys" => flag(key, modes.application_cursor_keys),
        "application_keypad" => flag(key, modes.application_keypad),
        "origin" => flag(key, modes.origin),
        "autowrap" => flag(key, modes.autowrap),
        "insert" => flag(key, modes.insert),
        "focus_reporting" => flag(key, modes.focus_reporting),
        "mouse_tracking" => {
            let want = match want.as_str().expect("mouse_tracking is a string") {
                "off" => MouseTracking::Off,
                "x10" => MouseTracking::X10,
                "normal" => MouseTracking::Normal,
                "button_event" => MouseTracking::ButtonEvent,
                "any_event" => MouseTracking::AnyEvent,
                other => panic!("{ctx}\n  unknown mouse_tracking {other:?}"),
            };
            assert_eq!(modes.mouse_tracking, want, "{ctx}\n  mouse_tracking");
        }
        "mouse_encoding" => {
            let want = match want.as_str().expect("mouse_encoding is a string") {
                "x11" => MouseEncoding::X11,
                "utf8" => MouseEncoding::Utf8,
                "sgr" => MouseEncoding::Sgr,
                other => panic!("{ctx}\n  unknown mouse_encoding {other:?}"),
            };
            assert_eq!(modes.mouse_encoding, want, "{ctx}\n  mouse_encoding");
        }
        other => panic!("{ctx}\n  expected.json names unknown mode {other:?}"),
    }
}

fn assert_cell(ctx: &str, model: &ScreenModel, want: &Value) {
    let row = want["row"].as_u64().expect("cell.row") as u16;
    let col = want["col"].as_u64().expect("cell.col") as u16;
    let got = model
        .cell(row, col)
        .unwrap_or_else(|| panic!("{ctx}\n  no cell at row {row} col {col}"));

    if let Some(text) = want.get("text").and_then(Value::as_str) {
        // A blank cell may be stored as an empty string or as a space; both
        // render the same and neither is worth failing a suite over.
        let got_text = if got.text.is_empty() { " " } else { &got.text };
        let want_text = if text.is_empty() { " " } else { text };
        assert_eq!(
            got_text, want_text,
            "{ctx}\n  cell text at row {row} col {col}: got {:?}",
            got.text
        );
    }
    if let Some(width) = want.get("width").and_then(Value::as_u64) {
        assert_eq!(
            got.width, width as u8,
            "{ctx}\n  cell width at row {row} col {col}"
        );
    }
    let mut expected_attrs = got.attrs;
    let mut checked = false;
    if want.get("fg").is_some() || want.get("bg").is_some() || has_attr_flags(want) {
        expected_attrs = CellAttrs::default();
        apply_attrs(&mut expected_attrs, want);
        checked = true;
    }
    if checked {
        assert_attrs(ctx, row, col, &got.attrs, &expected_attrs, want);
    }
}

fn has_attr_flags(want: &Value) -> bool {
    const FLAGS: [&str; 8] = [
        "bold",
        "dim",
        "italic",
        "underline",
        "blink",
        "reverse",
        "invisible",
        "strikethrough",
    ];
    FLAGS.iter().any(|flag| want.get(*flag).is_some())
}

/// Applies the attribute keys present in `value` onto `attrs`.
fn apply_attrs(attrs: &mut CellAttrs, value: &Value) {
    if let Some(fg) = value.get("fg") {
        attrs.fg = parse_color(fg);
    }
    if let Some(bg) = value.get("bg") {
        attrs.bg = parse_color(bg);
    }
    let flag = |name: &str, slot: &mut bool| {
        if let Some(on) = value.get(name).and_then(Value::as_bool) {
            *slot = on;
        }
    };
    flag("bold", &mut attrs.bold);
    flag("dim", &mut attrs.dim);
    flag("italic", &mut attrs.italic);
    flag("underline", &mut attrs.underline);
    flag("blink", &mut attrs.blink);
    flag("reverse", &mut attrs.reverse);
    flag("invisible", &mut attrs.invisible);
    flag("strikethrough", &mut attrs.strikethrough);
}

/// Compares only the attributes the fixture actually stated.
fn assert_attrs(ctx: &str, row: u16, col: u16, got: &CellAttrs, want: &CellAttrs, stated: &Value) {
    if stated.get("fg").is_some() {
        assert_eq!(got.fg, want.fg, "{ctx}\n  fg at row {row} col {col}");
    }
    if stated.get("bg").is_some() {
        assert_eq!(got.bg, want.bg, "{ctx}\n  bg at row {row} col {col}");
    }
    let flag = |name: &str, got: bool, want: bool| {
        if stated.get(name).is_some() {
            assert_eq!(got, want, "{ctx}\n  {name} at row {row} col {col}");
        }
    };
    flag("bold", got.bold, want.bold);
    flag("dim", got.dim, want.dim);
    flag("italic", got.italic, want.italic);
    flag("underline", got.underline, want.underline);
    flag("blink", got.blink, want.blink);
    flag("reverse", got.reverse, want.reverse);
    flag("invisible", got.invisible, want.invisible);
    flag("strikethrough", got.strikethrough, want.strikethrough);
}

fn parse_color(value: &Value) -> Color {
    if value.as_str() == Some("default") || value.is_null() {
        return Color::Default;
    }
    if let Some(index) = value.get("indexed").and_then(Value::as_u64) {
        return Color::Indexed(index as u8);
    }
    if let Some(rgb) = value.get("rgb").and_then(Value::as_array) {
        assert_eq!(rgb.len(), 3, "rgb color needs three components");
        let component = |i: usize| rgb[i].as_u64().expect("rgb component") as u8;
        return Color::Rgb(component(0), component(1), component(2));
    }
    panic!("unrecognized color: {value}");
}

/// The scrollback expectation, if the case carries one.
pub struct ScrollbackExpectation {
    /// Whether the ring is expected to be saturated at its limit.
    pub at_limit: bool,
    /// An exact retained line count, when the fixture gives one.
    pub len: Option<usize>,
    /// Text of the newest scrollback line — the one just above the screen.
    pub newest_text: Option<String>,
    /// A lower bound on lines dropped over the session.
    pub dropped_at_least: Option<u64>,
}

/// Reads the `scrollback` block of a case's expectation.
pub fn scrollback_expectation(case: &Case) -> Option<ScrollbackExpectation> {
    let block = case.expected.as_ref()?.get("scrollback")?;
    Some(ScrollbackExpectation {
        at_limit: block.get("len").and_then(Value::as_str) == Some("at_limit"),
        len: block.get("len").and_then(Value::as_u64).map(|n| n as usize),
        newest_text: block
            .get("newest_text")
            .and_then(Value::as_str)
            .map(str::to_string),
        dropped_at_least: block.get("dropped_at_least").and_then(Value::as_u64),
    })
}

/// Renders a screen as plain text. For failure messages only.
pub fn render(model: &ScreenModel) -> String {
    (0..model.size.rows)
        .map(|row| model.row_text(row))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A single-width cell holding `text` with the default rendition.
pub fn plain(text: &str) -> Cell {
    Cell::plain(text)
}
