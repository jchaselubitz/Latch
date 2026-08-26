//! The normalized screen model.
//!
//! Screens are asserted as data, never as screenshots. Everything here is plain
//! comparable data so that the round-trip assertion is a single equality:
//! stream a case into one emulator, snapshot it, replay the snapshot into a
//! fresh one, and the two [`ScreenModel`]s must be identical.
//!
//! The model deliberately describes only the **visible** screen. Scrollback is
//! read separately, because a snapshot restores what a reattaching client must
//! see and a reattaching client does not inherit the sender's ring.

/// A terminal screen size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

impl Size {
    /// Builds a size.
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

/// A cell color.
///
/// `Default` is distinct from any concrete color: a snapshot that emits an
/// explicit color where the screen held the default would look identical on the
/// recording terminal and diverge on a client with a different palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    /// The terminal's default foreground or background.
    #[default]
    Default,
    /// A palette index, 0-255.
    Indexed(u8),
    /// A direct 24-bit color.
    Rgb(u8, u8, u8),
}

/// The rendition of a single cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellAttrs {
    /// Foreground color.
    pub fg: Color,
    /// Background color.
    pub bg: Color,
    /// Bold.
    pub bold: bool,
    /// Faint / dim.
    pub dim: bool,
    /// Italic.
    pub italic: bool,
    /// Underline.
    pub underline: bool,
    /// Blink.
    pub blink: bool,
    /// Reverse video.
    pub reverse: bool,
    /// Concealed / invisible.
    pub invisible: bool,
    /// Crossed out.
    pub strikethrough: bool,
}

/// One screen cell.
///
/// A double-width character occupies two cells: the first carries the text and
/// `width == 2`, the second is a continuation with empty text and `width == 0`.
/// Representing the continuation explicitly is what makes a wide-character bug
/// visible as a failed assertion instead of as a one-column drift that only
/// shows up on a phone.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    /// The grapheme cluster in this cell: a base character plus any combining
    /// marks. Empty for a blank cell and for a wide character's continuation.
    pub text: String,
    /// Display width in columns: 0 for a continuation cell, 1 or 2 otherwise.
    pub width: u8,
    /// Rendition.
    pub attrs: CellAttrs,
}

impl Cell {
    /// Builds a plain single-width cell with default attributes.
    pub fn plain(text: &str) -> Self {
        Self {
            text: text.to_string(),
            width: 1,
            attrs: CellAttrs::default(),
        }
    }

    /// Builds the continuation half of a double-width character.
    pub fn continuation() -> Self {
        Self {
            text: String::new(),
            width: 0,
            attrs: CellAttrs::default(),
        }
    }
}

/// One row of cells.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    /// Cells, left to right, always exactly the screen width.
    pub cells: Vec<Cell>,
}

impl Row {
    /// The row's text with trailing blanks removed.
    ///
    /// Convenience for assertions that care about content rather than
    /// rendition; the cell-level comparison is what the round-trip uses.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for cell in &self.cells {
            if cell.width == 0 {
                continue;
            }
            out.push_str(if cell.text.is_empty() {
                " "
            } else {
                &cell.text
            });
        }
        out.trim_end().to_string()
    }
}

/// The shape the cursor is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    /// Whatever the client's own default is.
    #[default]
    Default,
    /// Block.
    Block,
    /// Underline.
    Underline,
    /// Vertical bar.
    Bar,
}

/// Cursor position and visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// Zero-based row.
    pub row: u16,
    /// Zero-based column.
    pub col: u16,
    /// Whether the cursor is shown (DECTCEM).
    pub visible: bool,
    /// Drawn shape.
    pub shape: CursorShape,
    /// Whether the cursor is blinking.
    pub blinking: bool,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            row: 0,
            col: 0,
            visible: true,
            shape: CursorShape::Default,
            blinking: true,
        }
    }
}

/// The vertical scrolling region (DECSTBM), zero-based and inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollRegion {
    /// First row of the region.
    pub top: u16,
    /// Last row of the region.
    pub bottom: u16,
}

impl ScrollRegion {
    /// The full-screen region for a screen of `rows` rows.
    pub const fn full(rows: u16) -> Self {
        Self {
            top: 0,
            bottom: rows.saturating_sub(1),
        }
    }
}

/// Mouse reporting level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseTracking {
    /// No reporting.
    #[default]
    Off,
    /// Press only (mode 9).
    X10,
    /// Press and release (mode 1000).
    Normal,
    /// Press, release, and drag (mode 1002).
    ButtonEvent,
    /// All motion (mode 1003).
    AnyEvent,
}

/// Mouse coordinate encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseEncoding {
    /// Original single-byte encoding.
    #[default]
    X11,
    /// UTF-8 (mode 1005).
    Utf8,
    /// SGR (mode 1006).
    Sgr,
}

/// The terminal modes a reattaching client must be put back into.
///
/// These are exactly the modes a byte-tail replay loses: they were set before
/// the replay window opened, so a client that only sees recent output never
/// learns about them. Every field here is something the snapshot has to
/// restore explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modes {
    /// Bracketed paste (mode 2004).
    pub bracketed_paste: bool,
    /// Application cursor keys (DECCKM).
    pub application_cursor_keys: bool,
    /// Application keypad (DECKPAM).
    pub application_keypad: bool,
    /// Origin mode (DECOM).
    pub origin: bool,
    /// Autowrap (DECAWM). Reset only by explicit request.
    pub autowrap: bool,
    /// Insert mode (IRM).
    pub insert: bool,
    /// Focus reporting (mode 1004).
    pub focus_reporting: bool,
    /// Mouse reporting level.
    pub mouse_tracking: MouseTracking,
    /// Mouse coordinate encoding.
    pub mouse_encoding: MouseEncoding,
}

impl Modes {
    /// The modes a terminal holds immediately after reset: autowrap on,
    /// everything else off.
    pub const fn after_reset() -> Self {
        Self {
            bracketed_paste: false,
            application_cursor_keys: false,
            application_keypad: false,
            origin: false,
            autowrap: true,
            insert: false,
            focus_reporting: false,
            mouse_tracking: MouseTracking::Off,
            mouse_encoding: MouseEncoding::X11,
        }
    }
}

/// A complete, comparable picture of the visible screen.
///
/// Equality of two of these is the round-trip assertion. Anything a
/// reattaching client would notice belongs in here; anything it would not must
/// stay out, or the round-trip fails for reasons that do not matter and the
/// test stops being trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenModel {
    /// Screen size.
    pub size: Size,
    /// Rows, top to bottom, always exactly `size.rows` of them.
    pub rows: Vec<Row>,
    /// Cursor position, visibility, and shape.
    pub cursor: Cursor,
    /// Whether the alternate screen is active.
    pub alternate_screen: bool,
    /// The current scrolling region.
    pub scroll_region: ScrollRegion,
    /// Modes in effect.
    pub modes: Modes,
    /// The window title last set by OSC 0/2, if any.
    ///
    /// Part of the model because reattach must reproduce what a continuously
    /// attached client shows, and Claude Code sets the title on every turn.
    pub title: Option<String>,
    /// The pending attributes new output would be written with.
    ///
    /// Not visible in any cell, and lost entirely by a byte-tail replay: a
    /// client that reattaches mid-line and then receives more output has to
    /// render it with the attributes that were in force before it connected.
    pub pen: CellAttrs,
}

impl ScreenModel {
    /// Row `row`, or `None` if out of range.
    pub fn row(&self, row: u16) -> Option<&Row> {
        self.rows.get(row as usize)
    }

    /// The cell at `(row, col)`, or `None` if out of range.
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        self.rows.get(row as usize)?.cells.get(col as usize)
    }

    /// The trailing-blank-trimmed text of row `row`, or an empty string.
    pub fn row_text(&self, row: u16) -> String {
        self.row(row).map(Row::text).unwrap_or_default()
    }
}
