//! The escape-sequence filter and the mode state the backend does not expose.
//!
//! The emulator behind [`Terminal`](crate::Terminal) keeps a complete grid but
//! publishes only part of the terminal state that a reattaching client needs.
//! The scroll region, origin mode, autowrap, insert mode, focus reporting, the
//! cursor shape and the window title are all tracked here instead, from the
//! same byte stream, so that [`Screen::model`](crate::Screen::model) can report
//! them and [`Screen::snapshot`](crate::Screen::snapshot) can restore them.
//!
//! The filter exists for a second reason as well. `CSI s` and `CSI u` — the
//! ANSI.SYS spelling of save and restore cursor — are what spinners and
//! progress lines are written with, and the backend does not implement them.
//! They are rewritten here to `ESC 7` and `ESC 8`, which it does. Without the
//! rewrite a thirty-frame spinner lands thirty times across three lines instead
//! of once in place.
//!
//! Both jobs are done by one pass over the bytes, and the pass keeps its state
//! across calls: a PTY read boundary falls wherever it falls, including inside
//! an escape sequence.

use crate::model::{CursorShape, ScrollRegion, Size};

/// The longest control sequence the filter will hold back before deciding it is
/// not looking at a sequence at all.
///
/// A sequence this long is malformed or hostile. Forwarding it rather than
/// buffering without bound is what keeps a wedged child process from growing
/// the worker's memory one byte at a time.
const MAX_HELD: usize = 64;

/// The longest OSC payload whose content is retained.
///
/// Only the window title is read out of one, and a title longer than this is
/// not a title.
const MAX_OSC: usize = 4096;

/// The per-grid state that the primary and alternate screens each hold
/// separately.
///
/// The backend gives each grid its own scroll region and origin mode, so
/// tracking one of each here would report the alternate screen's region while
/// the primary screen was active. Entering the alternate screen resets its
/// half; leaving restores the primary's, untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GridState {
    scroll_region: ScrollRegion,
    origin: bool,
}

impl GridState {
    const fn new(rows: u16) -> Self {
        Self {
            scroll_region: ScrollRegion::full(rows),
            origin: false,
        }
    }
}

/// Terminal state read from the stream rather than from the backend.
#[derive(Debug, Clone)]
pub(crate) struct ModeState {
    size: Size,
    primary: GridState,
    alternate: GridState,
    alternate_active: bool,
    /// Autowrap (DECAWM). On after reset.
    pub autowrap: bool,
    /// Insert mode (IRM).
    pub insert: bool,
    /// Focus reporting (mode 1004).
    pub focus_reporting: bool,
    /// Cursor shape (DECSCUSR).
    pub cursor_shape: CursorShape,
    /// Whether the cursor blinks (DECSCUSR).
    pub cursor_blinking: bool,
    /// Window title (OSC 0/2).
    pub title: Option<String>,
}

impl ModeState {
    pub(crate) fn new(size: Size) -> Self {
        Self {
            size,
            primary: GridState::new(size.rows),
            alternate: GridState::new(size.rows),
            alternate_active: false,
            autowrap: true,
            insert: false,
            focus_reporting: false,
            cursor_shape: CursorShape::Default,
            cursor_blinking: true,
            title: None,
        }
    }

    /// The state of a terminal that has just been reset, at the same size.
    pub(crate) fn reset(&mut self) {
        *self = Self::new(self.size);
    }

    /// The scroll region of whichever grid is active.
    pub(crate) fn scroll_region(&self) -> ScrollRegion {
        self.active().scroll_region
    }

    /// Origin mode on whichever grid is active.
    pub(crate) fn origin(&self) -> bool {
        self.active().origin
    }

    fn active(&self) -> &GridState {
        if self.alternate_active {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn active_mut(&mut self) -> &mut GridState {
        if self.alternate_active {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    /// Mirrors the backend's own resize handling of the scroll region: a region
    /// that reached the old bottom follows the new one, and one that would now
    /// be out of range collapses back to the full screen.
    pub(crate) fn resize(&mut self, size: Size) {
        let old_rows = self.size.rows;
        self.size = size;
        for grid in [&mut self.primary, &mut self.alternate] {
            if grid.scroll_region.bottom == old_rows.saturating_sub(1) {
                grid.scroll_region.bottom = size.rows.saturating_sub(1);
            }
            if grid.scroll_region.bottom >= size.rows {
                grid.scroll_region.bottom = size.rows.saturating_sub(1);
            }
            if grid.scroll_region.bottom < grid.scroll_region.top {
                grid.scroll_region.top = 0;
            }
        }
    }

    /// Applies a complete CSI sequence, `seq` being the whole thing from `ESC`
    /// through its final byte.
    fn apply_csi(&mut self, seq: &[u8]) {
        let body = &seq[2..];
        let Some((&final_byte, rest)) = body.split_last() else {
            return;
        };
        let private = rest.first() == Some(&b'?');
        let params_end = rest
            .iter()
            .position(|b| (0x20..=0x2f).contains(b))
            .unwrap_or(rest.len());
        let (params_bytes, intermediates) = rest.split_at(params_end);
        let params_bytes = if private {
            &params_bytes[1..]
        } else {
            params_bytes
        };
        let params = parse_params(params_bytes);

        match (private, intermediates, final_byte) {
            (true, [], b'h') => {
                for param in &params {
                    self.set_private(*param, true);
                }
            }
            (true, [], b'l') => {
                for param in &params {
                    self.set_private(*param, false);
                }
            }
            (false, [], b'h') => {
                for param in &params {
                    if *param == Some(4) {
                        self.insert = true;
                    }
                }
            }
            (false, [], b'l') => {
                for param in &params {
                    if *param == Some(4) {
                        self.insert = false;
                    }
                }
            }
            (false, [], b'r') => self.set_scroll_region(&params),
            (false, [b' '], b'q') => self.set_cursor_style(params.first().copied().flatten()),
            _ => {}
        }
    }

    fn set_private(&mut self, param: Option<u16>, on: bool) {
        match param {
            Some(6) => self.active_mut().origin = on,
            Some(7) => self.autowrap = on,
            Some(1004) => self.focus_reporting = on,
            Some(47) | Some(1047) | Some(1049) => {
                if on && !self.alternate_active {
                    // The backend clears the alternate grid on entry, which
                    // resets its region and origin mode with it.
                    self.alternate = GridState::new(self.size.rows);
                }
                self.alternate_active = on;
            }
            _ => {}
        }
    }

    /// DECSTBM, in the backend's own terms: one-based inclusive parameters
    /// defaulting to the whole screen, and a region that is not strictly
    /// increasing collapses to the full screen.
    fn set_scroll_region(&mut self, params: &[Option<u16>]) {
        let rows = self.size.rows;
        let top = params.first().copied().flatten().unwrap_or(1).max(1) - 1;
        let bottom = params
            .get(1)
            .copied()
            .flatten()
            .unwrap_or(rows)
            .max(1)
            .saturating_sub(1)
            .min(rows.saturating_sub(1));
        let region = if top < bottom {
            ScrollRegion { top, bottom }
        } else {
            ScrollRegion::full(rows)
        };
        self.active_mut().scroll_region = region;
    }

    fn set_cursor_style(&mut self, param: Option<u16>) {
        let (shape, blinking) = match param.unwrap_or(0) {
            1 => (CursorShape::Block, true),
            2 => (CursorShape::Block, false),
            3 => (CursorShape::Underline, true),
            4 => (CursorShape::Underline, false),
            5 => (CursorShape::Bar, true),
            6 => (CursorShape::Bar, false),
            _ => (CursorShape::Default, true),
        };
        self.cursor_shape = shape;
        self.cursor_blinking = blinking;
    }

    /// Applies a complete escape sequence that is not a CSI, OSC or string.
    fn apply_esc(&mut self, seq: &[u8]) {
        if seq == b"\x1bc" {
            self.reset();
        }
    }

    /// Applies an OSC payload: everything between the introducer and the
    /// terminator, terminator excluded.
    fn apply_osc(&mut self, payload: &[u8]) {
        let mut parts = payload.splitn(2, |b| *b == b';');
        let Some(ps) = parts.next() else { return };
        let Some(value) = parts.next() else { return };
        if ps != b"0" && ps != b"2" {
            return;
        }
        // Lossy is deliberate: a title is display metadata from a program that
        // can emit anything, and refusing to decode it would lose the title
        // rather than lose a byte of it.
        self.title = Some(String::from_utf8_lossy(value).into_owned());
    }
}

/// Parses a CSI parameter list, keeping the distinction between an omitted
/// parameter and an explicit zero, and keeping only the first sub-parameter of
/// each — nothing tracked here uses the rest.
fn parse_params(bytes: &[u8]) -> Vec<Option<u16>> {
    if bytes.is_empty() {
        return vec![];
    }
    bytes
        .split(|b| *b == b';')
        .map(|part| {
            let part = part.split(|b| *b == b':').next().unwrap_or(part);
            if part.is_empty() {
                return None;
            }
            let mut value: u32 = 0;
            for byte in part {
                if !byte.is_ascii_digit() {
                    return None;
                }
                value = value
                    .saturating_mul(10)
                    .saturating_add(u32::from(byte - b'0'));
                if value > u32::from(u16::MAX) {
                    return Some(u16::MAX);
                }
            }
            Some(value as u16)
        })
        .collect()
}

/// How many continuation bytes follow a UTF-8 lead byte, or `None` if the byte
/// does not start a multi-byte character.
fn utf8_continuation_count(byte: u8) -> Option<usize> {
    match byte {
        0xc2..=0xdf => Some(1),
        0xe0..=0xef => Some(2),
        0xf0..=0xf4 => Some(3),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    /// Collecting the continuation bytes of a multi-byte character. Carries how
    /// many are still outstanding.
    Utf8(usize),
    Escape,
    Csi,
    Osc,
    OscEscape,
    Str,
    StrEscape,
}

/// Splits a byte stream into escape sequences and everything else.
///
/// The output is the input with `CSI s` and `CSI u` rewritten; every other byte
/// passes through unchanged and in order. Bytes belonging to a sequence or a
/// multi-byte character that is still incomplete at the end of a chunk are held
/// until the rest arrives.
///
/// Holding them is not only about the rewrite. The backend's screen is supposed
/// to depend on the bytes and not on how they were delivered, and it very
/// nearly does — but a `process` boundary landing inside a character can move a
/// column, which was found by running a recorded Claude Code stream through it
/// at every chunk size. Emitting only whole units means the backend never sees
/// such a boundary, whatever the PTY hands us.
#[derive(Debug)]
pub(crate) struct EscapeFilter {
    state: State,
    held: Vec<u8>,
    osc: Vec<u8>,
}

impl EscapeFilter {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Ground,
            held: Vec::new(),
            osc: Vec::new(),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.state = State::Ground;
        self.held.clear();
        self.osc.clear();
    }

    /// Feeds `input`, appending the filtered stream to `out` and updating
    /// `modes`.
    pub(crate) fn push(&mut self, input: &[u8], modes: &mut ModeState, out: &mut Vec<u8>) {
        for &byte in input {
            match self.state {
                State::Ground => {
                    if byte == 0x1b {
                        self.state = State::Escape;
                        self.held.clear();
                        self.held.push(byte);
                    } else if let Some(count) = utf8_continuation_count(byte) {
                        self.state = State::Utf8(count);
                        self.held.clear();
                        self.held.push(byte);
                    } else {
                        out.push(byte);
                    }
                }
                State::Utf8(remaining) => {
                    if (0x80..=0xbf).contains(&byte) {
                        self.held.push(byte);
                        if remaining <= 1 {
                            self.flush_held(out);
                        } else {
                            self.state = State::Utf8(remaining - 1);
                        }
                    } else {
                        // Not a continuation byte: the character was truncated
                        // by whatever produced it. Hand both on and let the
                        // backend decide; withholding would lose them.
                        self.flush_held(out);
                        self.push(&[byte], modes, out);
                    }
                }
                State::Escape => match byte {
                    b'[' => {
                        self.held.push(byte);
                        self.state = State::Csi;
                    }
                    b']' => {
                        self.held.push(byte);
                        out.extend_from_slice(&self.held);
                        self.held.clear();
                        self.osc.clear();
                        self.state = State::Osc;
                    }
                    b'P' | b'_' | b'^' | b'X' => {
                        self.held.push(byte);
                        out.extend_from_slice(&self.held);
                        self.held.clear();
                        self.state = State::Str;
                    }
                    0x1b => {
                        // A second introducer abandons the first.
                        out.extend_from_slice(&self.held);
                        self.held.clear();
                        self.held.push(byte);
                    }
                    0x18 | 0x1a => {
                        // CAN and SUB abort the sequence and are consumed.
                        self.held.clear();
                        self.state = State::Ground;
                    }
                    0x20..=0x2f => {
                        self.held.push(byte);
                        if self.held.len() > MAX_HELD {
                            self.flush_held(out);
                        }
                    }
                    _ => {
                        self.held.push(byte);
                        modes.apply_esc(&self.held);
                        self.flush_held(out);
                    }
                },
                State::Csi => match byte {
                    0x20..=0x3f => {
                        self.held.push(byte);
                        if self.held.len() > MAX_HELD {
                            self.flush_held(out);
                        }
                    }
                    0x40..=0x7e => {
                        self.held.push(byte);
                        // The rewrite. Only the bare forms are ANSI.SYS save
                        // and restore; anything with parameters is a different
                        // sequence that happens to end in the same byte.
                        match self.held.as_slice() {
                            b"\x1b[s" => out.extend_from_slice(b"\x1b7"),
                            b"\x1b[u" => out.extend_from_slice(b"\x1b8"),
                            other => out.extend_from_slice(other),
                        }
                        modes.apply_csi(&self.held);
                        self.held.clear();
                        self.state = State::Ground;
                    }
                    0x18 | 0x1a => {
                        self.held.clear();
                        self.state = State::Ground;
                    }
                    0x1b => {
                        out.extend_from_slice(&self.held);
                        self.held.clear();
                        self.held.push(byte);
                        self.state = State::Escape;
                    }
                    _ => {
                        self.held.push(byte);
                        self.flush_held(out);
                    }
                },
                State::Osc => {
                    out.push(byte);
                    match byte {
                        0x07 => {
                            modes.apply_osc(&self.osc);
                            self.osc.clear();
                            self.state = State::Ground;
                        }
                        0x1b => self.state = State::OscEscape,
                        _ => {
                            if self.osc.len() < MAX_OSC {
                                self.osc.push(byte);
                            }
                        }
                    }
                }
                State::OscEscape => {
                    out.push(byte);
                    if byte == b'\\' {
                        modes.apply_osc(&self.osc);
                        self.osc.clear();
                        self.state = State::Ground;
                    } else {
                        self.state = State::Osc;
                        if self.osc.len() + 2 <= MAX_OSC {
                            self.osc.push(0x1b);
                            self.osc.push(byte);
                        }
                    }
                }
                State::Str => {
                    out.push(byte);
                    if byte == 0x1b {
                        self.state = State::StrEscape;
                    }
                }
                State::StrEscape => {
                    out.push(byte);
                    self.state = if byte == b'\\' {
                        State::Ground
                    } else {
                        State::Str
                    };
                }
            }
        }
    }

    fn flush_held(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.held);
        self.held.clear();
        self.state = State::Ground;
    }
}
