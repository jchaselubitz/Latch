//! The concrete screen implementation.
//!
//! The emulator behind [`Terminal`] was chosen by evaluation in M1 — see
//! `docs/DECISION_EMULATOR.md` — and is not named outside this module. The
//! worker holds a [`Screen`], never a backend type, which is what makes the
//! choice reversible.
//!
//! Three things live here that the backend does not provide:
//!
//! * **Mode tracking.** The backend publishes part of the terminal state; the
//!   rest is read off the same byte stream in [`crate::modes`].
//! * **The scrollback ring.** The backend keeps its own scrolled-off lines but
//!   offers no way to drain them, so a ring that must both retain a bounded
//!   window *and* count what it dropped has to mirror them out as they arrive.
//! * **Snapshot composition.** The backend serializes a grid; a reattaching
//!   client also needs the alternate-screen flag, the modes, the scroll region
//!   and the title, none of which are in the grid.

use std::collections::VecDeque;
use std::fmt::Write as _;

use crate::model::{
    Cell, CellAttrs, Color, Cursor, Modes, MouseEncoding, MouseTracking, Row, ScreenModel,
    ScrollRegion, Size,
};
use crate::modes::{EscapeFilter, ModeState};
use crate::Screen;

/// Default scrollback depth, in lines.
///
/// Scrollback is a bounded structured ring, not an unbounded byte log: a
/// session that runs for a week must not grow without limit, and the cap is the
/// only thing that makes the memory cost of a session predictable.
pub const DEFAULT_SCROLLBACK_LINES: usize = 5_000;

/// Default number of scrollback lines sent with a snapshot on attach.
///
/// Chosen against measurement rather than roundness; the reasoning, the numbers
/// it was chosen from, and what would justify changing it are in
/// `docs/DECISION_SCROLLBACK.md`. Two hundred lines is four screens at a desk
/// geometry and ten on a phone: enough to see the command that produced what is
/// on screen, and not enough to be a log-reading surface.
pub const DEFAULT_ATTACH_HISTORY_LINES: usize = 200;

/// Default ceiling on the bytes of history sent with a snapshot on attach.
///
/// The line count is the useful bound and this is the safety net. They are not
/// interchangeable: a line of ordinary build output serializes to about 70
/// bytes at 200 columns, but a line that changes color in every cell serializes
/// to about 2.7 KB, so a line count alone bounds nothing that a link cares
/// about. Thirty-two kibibytes is roughly one second on a genuinely bad cell
/// link, which is where a reattach stops reading as instant.
pub const DEFAULT_ATTACH_HISTORY_BYTES: usize = 32 * 1024;

/// How much scrollback accompanies a snapshot when a client attaches.
///
/// Both bounds apply and the tighter one wins. `max_bytes` covers everything
/// emitted before the screen, including the newlines that scroll the history
/// above it, so it is a bound a transport can be reasoned about with rather
/// than an estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryPolicy {
    /// Most scrollback lines to send. The newest are kept.
    pub max_lines: usize,
    /// Most bytes of history to send, before the screen itself.
    pub max_bytes: usize,
}

impl HistoryPolicy {
    /// Send no history: the snapshot restores the visible screen and nothing
    /// above it.
    ///
    /// This is what a backpressure resync uses. A client being resynced already
    /// received the history when it attached, and a link slow enough to have
    /// overflowed its queue is the last one that should be sent scrollback
    /// again.
    pub const NONE: Self = Self {
        max_lines: 0,
        max_bytes: 0,
    };

    /// The measured default.
    pub const fn new() -> Self {
        Self {
            max_lines: DEFAULT_ATTACH_HISTORY_LINES,
            max_bytes: DEFAULT_ATTACH_HISTORY_BYTES,
        }
    }

    /// Whether this policy can carry anything at all.
    pub const fn is_none(&self) -> bool {
        self.max_lines == 0 || self.max_bytes == 0
    }
}

impl Default for HistoryPolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// The floor on the backend's own scrollback depth.
///
/// The ring is mirrored out of the backend's buffer, and that buffer has to be
/// drained before it fills or the lines that fall out of it are lost uncounted.
/// Draining costs a screen reconstruction, so the buffer is kept comfortably
/// larger than a screen even when the configured ring is small or switched off
/// entirely.
const MIN_BACKEND_SCROLLBACK: usize = 256;

/// How a [`Terminal`] is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalConfig {
    /// Initial screen size.
    pub size: Size,
    /// Maximum number of lines retained in scrollback. Oldest lines are
    /// dropped first once the ring is full.
    pub scrollback_limit: usize,
}

impl TerminalConfig {
    /// A config at `size` with the default scrollback depth.
    pub const fn new(size: Size) -> Self {
        Self {
            size,
            scrollback_limit: DEFAULT_SCROLLBACK_LINES,
        }
    }

    /// The same config with a different scrollback depth.
    pub const fn with_scrollback_limit(mut self, limit: usize) -> Self {
        self.scrollback_limit = limit;
        self
    }
}

/// A headless terminal screen.
///
/// Construct one, feed it PTY output with [`Screen::advance`], and read it back
/// as a [`ScreenModel`]. A terminal built with the same config as another and
/// fed that other's [`Screen::snapshot`] must compare equal to it — that
/// property is the reattach guarantee, stated as a test.
pub struct Terminal {
    config: TerminalConfig,
    parser: vt100::Parser,
    modes: ModeState,
    filter: EscapeFilter,
    ring: VecDeque<Row>,
    dropped: u64,
    /// How many lines have been mirrored out of the backend's buffer since it
    /// was last drained.
    mirrored: usize,
    /// Reused between calls so the hot path does not allocate per PTY read.
    scratch: Vec<u8>,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("config", &self.config)
            .field("size", &self.size())
            .field("alternate_screen", &self.alternate_screen_active())
            .field("scrollback_len", &self.ring.len())
            .field("scrollback_dropped", &self.dropped)
            .finish()
    }
}

impl Terminal {
    /// Builds a terminal.
    pub fn new(config: TerminalConfig) -> Self {
        Self {
            config,
            parser: vt100::Parser::new(
                config.size.rows,
                config.size.cols,
                backend_scrollback(config, config.size),
            ),
            modes: ModeState::new(config.size),
            filter: EscapeFilter::new(),
            ring: VecDeque::new(),
            dropped: 0,
            mirrored: 0,
            scratch: Vec::new(),
        }
    }

    /// Builds a terminal at `size` with the default scrollback depth.
    pub fn with_size(size: Size) -> Self {
        Self::new(TerminalConfig::new(size))
    }

    /// The config this terminal was built with.
    pub fn config(&self) -> TerminalConfig {
        self.config
    }

    fn backend_size(&self) -> Size {
        let (rows, cols) = self.parser.screen().size();
        Size::new(cols, rows)
    }

    // --- The scrollback ring ------------------------------------------------

    /// Copies out every line that has entered the backend's buffer since the
    /// last call, and retires the oldest of ours to stay inside the limit.
    fn mirror_scrolled_lines(&mut self) {
        if self.parser.screen().alternate_screen() {
            // The alternate screen has no scrollback — that is most of the
            // point of it. Its buffer reads as empty, which must not be
            // mistaken for the primary screen's having been emptied.
            return;
        }
        let depth = self.backend_scrollback_depth();
        if depth <= self.mirrored {
            // A reset (RIS) empties the backend's buffer under us. There is
            // nothing new to take; re-anchor and carry on.
            self.mirrored = depth;
            return;
        }

        let cols = self.backend_size().cols;
        let mut harvested = Vec::with_capacity(depth - self.mirrored);
        for index in self.mirrored..depth {
            // Offset `k` puts the k-th line back from the newest at the top of
            // the visible window, so line `index` counting from the oldest sits
            // at offset `depth - index`.
            self.parser.screen_mut().set_scrollback(depth - index);
            let screen = self.parser.screen();
            let mut cells = Vec::with_capacity(usize::from(cols));
            for col in 0..cols {
                cells.push(convert_cell(screen.cell(0, col)));
            }
            harvested.push(Row { cells });
        }
        self.parser.screen_mut().set_scrollback(0);
        self.mirrored = depth;

        for row in harvested {
            self.retain(row);
        }
    }

    fn retain(&mut self, row: Row) {
        if self.config.scrollback_limit == 0 {
            self.dropped += 1;
            return;
        }
        self.ring.push_back(row);
        while self.ring.len() > self.config.scrollback_limit {
            self.ring.pop_front();
            self.dropped += 1;
        }
    }

    /// How many lines the backend is currently holding.
    ///
    /// There is no accessor for this, but the scrollback *offset* saturates at
    /// the depth, so asking for an impossible offset and reading back what was
    /// accepted answers the question.
    fn backend_scrollback_depth(&mut self) -> usize {
        let screen = self.parser.screen_mut();
        let saved = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        let depth = screen.scrollback();
        screen.set_scrollback(saved);
        depth
    }

    /// Empties the backend's buffer by rebuilding the emulator from a snapshot
    /// of the current screen.
    ///
    /// The backend drops its oldest lines silently once its buffer is full, and
    /// a line dropped there is one this ring never saw and never counted. The
    /// only way to empty it is to start a new one, and a snapshot is exactly
    /// the thing that carries a screen across that boundary intact — the same
    /// operation, and the same guarantee, as a client reattaching.
    fn drain_backend(&mut self) {
        let snapshot = self.snapshot();
        let size = self.backend_size();
        let mut parser =
            vt100::Parser::new(size.rows, size.cols, backend_scrollback(self.config, size));
        parser.process(&snapshot);
        self.parser = parser;
        self.mirrored = 0;
    }

    /// Puts the grid back into a state its own serialization can reproduce.
    ///
    /// Narrowing the screen truncates each row in place, which can cut a
    /// double-width character in half and leave its first cell alone in the
    /// last column. No terminal can be told to draw that — a wide character
    /// that will not fit wraps whole — so a snapshot of it does not round-trip.
    /// Rebuilding through the snapshot resolves it exactly the way a real
    /// terminal would, by wrapping the character to the next row, and leaves
    /// the grid representable again.
    fn repair_split_wide_characters(&mut self) {
        let (rows, cols) = self.parser.screen().size();
        if cols == 0 {
            return;
        }
        let screen = self.parser.screen();
        let split = (0..rows).any(|row| {
            screen
                .cell(row, cols - 1)
                .is_some_and(|cell| cell.is_wide() && !cell.is_wide_continuation())
        });
        if split {
            self.drain_backend();
        }
    }

    // --- Snapshot -----------------------------------------------------------

    /// Serializes the newest retained lines, oldest first, inside `policy`.
    ///
    /// Walked from the newest backwards because that is the end worth keeping:
    /// what a person scrolls up to see is what just left the screen. A line
    /// that would cross either bound stops the walk rather than being
    /// truncated — half a line of build output is worse than one line less of
    /// it, and a truncated line would also be a lie about what the session
    /// printed.
    fn write_history(&self, policy: HistoryPolicy, out: &mut Vec<u8>) {
        if policy.is_none() || self.ring.is_empty() {
            return;
        }
        let rows = usize::from(self.backend_size().rows);
        // The newlines that scroll the history above the screen are reserved
        // up front, so `max_bytes` bounds the whole preamble and not merely the
        // part of it that happens to be text.
        let Some(budget) = policy.max_bytes.checked_sub(rows) else {
            return;
        };

        let mut lines: Vec<Vec<u8>> = Vec::new();
        let mut spent = 0usize;
        for row in self.ring.iter().rev().take(policy.max_lines) {
            let encoded = encode_row(row);
            if spent + encoded.len() > budget {
                break;
            }
            spent += encoded.len();
            lines.push(encoded);
        }
        if lines.is_empty() {
            return;
        }
        for line in lines.iter().rev() {
            out.extend_from_slice(line);
        }
        // Exactly enough newlines to push every line just written above the
        // visible screen, wherever printing them left the cursor. Without this
        // the newest lines — the ones worth having — would sit in the visible
        // region and be erased by the screen paint that follows, rather than
        // landing in the client's own scrollback where a continuously attached
        // client would have them.
        out.resize(out.len() + rows, b'\n');
    }

    fn write_extra_modes(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(if self.modes.autowrap {
            b"\x1b[?7h"
        } else {
            b"\x1b[?7l"
        });
        out.extend_from_slice(if self.modes.insert {
            b"\x1b[4h"
        } else {
            b"\x1b[4l"
        });
        out.extend_from_slice(if self.modes.focus_reporting {
            b"\x1b[?1004h"
        } else {
            b"\x1b[?1004l"
        });
        let style = cursor_style_param(&self.modes);
        let _ = write!(ByteWriter(out), "\x1b[{style} q");
        if let Some(title) = &self.modes.title {
            out.extend_from_slice(b"\x1b]2;");
            out.extend_from_slice(title.as_bytes());
            out.push(0x07);
        }
    }

    /// Restores the scroll region and origin mode, then puts the cursor back
    /// where setting them moved it from.
    ///
    /// Neither can be emitted before the screen is painted: painting walks the
    /// rows top to bottom and steps between them with a newline, which inside a
    /// scroll region scrolls it.
    fn write_region_and_origin(&self, out: &mut Vec<u8>) {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let region = self.modes.scroll_region();
        let origin = self.modes.origin();
        let full = ScrollRegion::full(rows);
        if region == full && !origin {
            return;
        }

        if region != full {
            let _ = write!(
                ByteWriter(out),
                "\x1b[{};{}r",
                region.top + 1,
                region.bottom + 1
            );
        }
        if origin {
            out.extend_from_slice(b"\x1b[?6h");
            // Under origin mode, cursor addressing is relative to the region,
            // and there is no way to address a cursor parked past the last
            // column. That combination is rare enough to name here rather than
            // build machinery for.
            let (crow, ccol) = screen.cursor_position();
            let row = crow.saturating_sub(region.top) + 1;
            let col = ccol.min(cols.saturating_sub(1)) + 1;
            let _ = write!(ByteWriter(out), "\x1b[{row};{col}H");
        } else {
            // The backend's own cursor restoration, which reproduces a cursor
            // parked past the last column by redrawing the character that put
            // it there. That redraw disturbs the pen, so the pen follows it.
            out.extend_from_slice(&screen.cursor_state_formatted());
            out.extend_from_slice(&screen.attributes_formatted());
        }
    }
}

impl Screen for Terminal {
    fn advance(&mut self, bytes: &[u8]) {
        let mut filtered = std::mem::take(&mut self.scratch);
        let mut rest = bytes;
        while !rest.is_empty() {
            let size = self.backend_size();
            let rows = usize::from(size.rows).max(1);
            let capacity = backend_scrollback(self.config, size);
            if capacity.saturating_sub(self.mirrored) < rows {
                self.drain_backend();
            }
            // A slice of N bytes can scroll at most N lines through plain
            // newlines, and at most `rows` lines per scroll-up sequence, which
            // is itself at least three bytes long. `N * rows` bounds both, so
            // the backend's buffer cannot overflow before the next mirror.
            let budget = capacity.saturating_sub(self.mirrored);
            let take = rest.len().min((budget / rows).max(1));

            // The raw bytes are sliced, not the filtered ones: the filter holds
            // back an incomplete sequence or character, so whatever it does
            // emit ends on a unit boundary no matter where this slice fell.
            filtered.clear();
            self.filter
                .push(&rest[..take], &mut self.modes, &mut filtered);
            if !filtered.is_empty() {
                self.parser.process(&filtered);
            }
            rest = &rest[take..];
            self.mirror_scrolled_lines();
        }

        self.scratch = filtered;
    }

    fn resize(&mut self, size: Size) {
        self.parser.screen_mut().set_size(size.rows, size.cols);
        self.modes.resize(size);
        // Retained lines are full rows at the screen's width, so a resize has
        // to reach them too or a scrolled-back view renders ragged.
        for row in &mut self.ring {
            row.cells.resize(usize::from(size.cols), Cell::default());
        }
        self.repair_split_wide_characters();
    }

    fn reset(&mut self) {
        let size = self.backend_size();
        self.parser =
            vt100::Parser::new(size.rows, size.cols, backend_scrollback(self.config, size));
        self.modes = ModeState::new(size);
        self.filter.reset();
        self.ring.clear();
        self.dropped = 0;
        self.mirrored = 0;
    }

    fn size(&self) -> Size {
        self.backend_size()
    }

    fn alternate_screen_active(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    fn cursor(&self) -> Cursor {
        let screen = self.parser.screen();
        let (row, col) = screen.cursor_position();
        Cursor {
            row,
            col,
            visible: !screen.hide_cursor(),
            shape: self.modes.cursor_shape,
            blinking: self.modes.cursor_blinking,
        }
    }

    fn scroll_region(&self) -> ScrollRegion {
        self.modes.scroll_region()
    }

    fn modes(&self) -> Modes {
        let screen = self.parser.screen();
        Modes {
            bracketed_paste: screen.bracketed_paste(),
            application_cursor_keys: screen.application_cursor(),
            application_keypad: screen.application_keypad(),
            origin: self.modes.origin(),
            autowrap: self.modes.autowrap,
            insert: self.modes.insert,
            focus_reporting: self.modes.focus_reporting,
            mouse_tracking: match screen.mouse_protocol_mode() {
                vt100::MouseProtocolMode::None => MouseTracking::Off,
                vt100::MouseProtocolMode::Press => MouseTracking::X10,
                vt100::MouseProtocolMode::PressRelease => MouseTracking::Normal,
                vt100::MouseProtocolMode::ButtonMotion => MouseTracking::ButtonEvent,
                vt100::MouseProtocolMode::AnyMotion => MouseTracking::AnyEvent,
            },
            mouse_encoding: match screen.mouse_protocol_encoding() {
                vt100::MouseProtocolEncoding::Default => MouseEncoding::X11,
                vt100::MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
                vt100::MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
            },
        }
    }

    fn title(&self) -> Option<String> {
        self.modes.title.clone()
    }

    fn model(&self) -> ScreenModel {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let mut model_rows = Vec::with_capacity(usize::from(rows));
        for row in 0..rows {
            let mut cells = Vec::with_capacity(usize::from(cols));
            for col in 0..cols {
                cells.push(convert_cell(screen.cell(row, col)));
            }
            model_rows.push(Row { cells });
        }
        ScreenModel {
            size: Size::new(cols, rows),
            rows: model_rows,
            cursor: self.cursor(),
            alternate_screen: screen.alternate_screen(),
            scroll_region: self.modes.scroll_region(),
            modes: self.modes(),
            title: self.modes.title.clone(),
            pen: CellAttrs {
                fg: convert_color(screen.fgcolor()),
                bg: convert_color(screen.bgcolor()),
                bold: screen.bold(),
                dim: screen.dim(),
                italic: screen.italic(),
                underline: screen.underline(),
                blink: false,
                reverse: screen.inverse(),
                invisible: false,
                strikethrough: false,
            },
        }
    }

    fn scrollback_limit(&self) -> usize {
        self.config.scrollback_limit
    }

    fn scrollback_len(&self) -> usize {
        self.ring.len()
    }

    fn scrollback_line(&self, index: usize) -> Option<Row> {
        self.ring.get(index).cloned()
    }

    fn scrollback_dropped(&self) -> u64 {
        self.dropped
    }

    fn snapshot(&self) -> Vec<u8> {
        self.attach_snapshot(HistoryPolicy::NONE)
    }

    fn attach_snapshot(&self, history: HistoryPolicy) -> Vec<u8> {
        let screen = self.parser.screen();
        let mut out = Vec::new();

        // A hard reset first. The contract is reconstruction from a reset
        // terminal, and saying so in the bytes is what makes the snapshot
        // self-sufficient rather than dependent on what the receiver happened
        // to be showing.
        out.extend_from_slice(b"\x1bc");
        // History goes after the reset and before everything else. It cannot go
        // first: a hard reset clears the receiving terminal's saved lines, so
        // history emitted ahead of it would be erased by it. It cannot go after
        // the screen either, because it has to scroll *past* the screen to
        // reach the client's scrollback. This one position is the only one that
        // works, which is why the composition lives here rather than being
        // assembled by the worker out of two calls.
        self.write_history(history, &mut out);
        // Stated in both directions: a client sitting on the alternate screen
        // that receives a snapshot of a primary one has to be taken off it.
        // This is also the marker that separates history from the screen — no
        // cell's contents can contain an escape, so nothing above can imitate
        // it.
        out.extend_from_slice(if screen.alternate_screen() {
            b"\x1b[?1049h"
        } else {
            b"\x1b[?1049l"
        });
        // Cells, cursor visibility, cursor position, and the pen.
        out.extend_from_slice(&screen.contents_formatted());
        // Application keypad and cursor keys, bracketed paste, mouse reporting.
        out.extend_from_slice(&screen.input_mode_formatted());
        self.write_extra_modes(&mut out);
        self.write_region_and_origin(&mut out);
        out
    }
}

/// One retained line as bytes a terminal can print, rendition included.
///
/// Terminated with `\r\n` rather than `\n`: a line exactly as wide as the
/// screen leaves the backend's deferred wrap pending, and a carriage return is
/// what cancels it. A bare newline would wrap the next line by one column.
fn encode_row(row: &Row) -> Vec<u8> {
    // Trailing cells are dropped, but only the ones that carry nothing at all.
    // A blank cell with a rendition is not padding: a diff that paints its
    // background to the end of the line is exactly that, and trimming it would
    // change what the line means rather than merely what it costs.
    let Some(last) = row.cells.iter().rposition(|cell| {
        cell.width > 0 && (!cell.text.trim().is_empty() || cell.attrs != CellAttrs::default())
    }) else {
        return b"\r\n".to_vec();
    };

    let mut out = Vec::new();
    let mut pen = CellAttrs::default();
    for cell in &row.cells[..=last] {
        // Continuation halves carry no text; the wide character before them
        // already occupies both columns when printed.
        if cell.width == 0 {
            continue;
        }
        if cell.attrs != pen {
            write_sgr(&cell.attrs, &mut out);
            pen = cell.attrs;
        }
        out.extend_from_slice(if cell.text.is_empty() {
            b" "
        } else {
            cell.text.as_bytes()
        });
    }
    // Closed off so a line's rendition cannot bleed into the next one or into
    // the screen paint that follows the history block.
    if pen != CellAttrs::default() {
        out.extend_from_slice(b"\x1b[m");
    }
    out.extend_from_slice(b"\r\n");
    out
}

/// Writes a complete rendition, reset first, so it depends on no prior state.
fn write_sgr(attrs: &CellAttrs, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b[0");
    for (enabled, code) in [
        (attrs.bold, "1"),
        (attrs.dim, "2"),
        (attrs.italic, "3"),
        (attrs.underline, "4"),
        (attrs.blink, "5"),
        (attrs.reverse, "7"),
        (attrs.invisible, "8"),
        (attrs.strikethrough, "9"),
    ] {
        if enabled {
            out.push(b';');
            out.extend_from_slice(code.as_bytes());
        }
    }
    write_sgr_color(attrs.fg, 3, out);
    write_sgr_color(attrs.bg, 4, out);
    out.push(b'm');
}

/// Appends one color to an SGR sequence already in progress. `plane` is 3 for
/// foreground and 4 for background.
fn write_sgr_color(color: Color, plane: u8, out: &mut Vec<u8>) {
    let mut write = ByteWriter(out);
    let _ = match color {
        // The default is the absence of a color, and the leading reset has
        // already selected it. Emitting a concrete color here would make a
        // snapshot look right on the recording terminal and wrong on a client
        // with a different palette.
        Color::Default => Ok(()),
        Color::Indexed(index) if index < 8 => {
            write!(write, ";{}", u16::from(plane) * 10 + u16::from(index))
        }
        Color::Indexed(index) if index < 16 => write!(
            write,
            ";{}",
            u16::from(plane) * 10 + 60 + u16::from(index) - 8
        ),
        Color::Indexed(index) => write!(write, ";{plane}8;5;{index}"),
        Color::Rgb(r, g, b) => write!(write, ";{plane}8;2;{r};{g};{b}"),
    };
}

/// Adapts a byte vector to `fmt::Write` so short sequences can be formatted
/// straight into it.
struct ByteWriter<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for ByteWriter<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

fn cursor_style_param(modes: &ModeState) -> u16 {
    use crate::model::CursorShape::{Bar, Block, Default, Underline};
    match (modes.cursor_shape, modes.cursor_blinking) {
        (Default, _) => 0,
        (Block, true) => 1,
        (Block, false) => 2,
        (Underline, true) => 3,
        (Underline, false) => 4,
        (Bar, true) => 5,
        (Bar, false) => 6,
    }
}

/// The backend's buffer must hold more than a screenful between drains, or a
/// single scroll-up sequence could overrun it.
fn backend_scrollback(config: TerminalConfig, size: Size) -> usize {
    config
        .scrollback_limit
        .max(MIN_BACKEND_SCROLLBACK)
        .max(usize::from(size.rows).saturating_mul(2))
}

fn convert_cell(cell: Option<&vt100::Cell>) -> Cell {
    let Some(cell) = cell else {
        return Cell::default();
    };
    let width = if cell.is_wide_continuation() {
        0
    } else if cell.is_wide() {
        2
    } else {
        1
    };
    Cell {
        text: cell.contents().to_string(),
        width,
        attrs: CellAttrs {
            fg: convert_color(cell.fgcolor()),
            bg: convert_color(cell.bgcolor()),
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline(),
            // The backend does not model blink, conceal or strikethrough. No
            // recorded stream from Claude Code or Codex uses any of the three;
            // see docs/DECISION_EMULATOR.md.
            blink: false,
            reverse: cell.inverse(),
            invisible: false,
            strikethrough: false,
        },
    }
}

fn convert_color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Default,
        vt100::Color::Idx(index) => Color::Indexed(index),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
