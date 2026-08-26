//! The screen model: a headless terminal emulator plus snapshot serialization.
//!
//! This is the substrate, not an optimization. A byte-replay buffer cannot
//! restore an alternate-screen application: replaying a byte tail can begin
//! mid-escape-sequence, paint only part of a full-screen redraw, and never
//! restores modes set before the replay window — alternate screen, bracketed
//! paste, cursor visibility, scroll region, current attributes. Claude Code and
//! Codex use all of them, and every mobile app-backgrounding is a detach and a
//! reattach.
//!
//! The model is load-bearing for four separate requirements:
//!
//! | Requirement | How the screen model serves it |
//! | --- | --- |
//! | Correct reattach | Serialize the current screen; no byte-tail guessing. |
//! | Slow-client backpressure | Drop a stalled client's queue and resend a snapshot instead of buffering without bound. |
//! | Scrollback bounding | A bounded line ring in a structured model, not an unbounded byte log. |
//! | Conversation-view fallback (M3) | "Is the alternate screen active?" is a property read, not a heuristic over raw bytes. |
//!
//! # Backend choice
//!
//! `vt100` won the M1 evaluation against `wezterm-term`; the reasoning, the
//! measurements, and what would justify revisiting it are in
//! `docs/DECISION_EMULATOR.md`. It sits behind [`Screen`] and is named nowhere
//! outside [`terminal`], so it can be swapped without reaching into the
//! worker.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod model;
mod modes;
pub mod terminal;

pub use model::{
    Cell, CellAttrs, Color, Cursor, CursorShape, Modes, MouseEncoding, MouseTracking, Row,
    ScreenModel, ScrollRegion, Size,
};
pub use terminal::{
    HistoryPolicy, Terminal, TerminalConfig, DEFAULT_ATTACH_HISTORY_BYTES,
    DEFAULT_ATTACH_HISTORY_LINES, DEFAULT_SCROLLBACK_LINES,
};

/// A live headless terminal screen fed by PTY output.
///
/// The implementation behind this trait is the emulator chosen in M1. Keeping
/// the worker on the trait rather than a concrete type is what makes that
/// choice reversible, and keeping the conformance suite on the trait is what
/// makes the choice checkable: the same tests must pass whichever backend wins.
pub trait Screen {
    /// Feeds raw PTY output into the emulator.
    ///
    /// Chunk boundaries are not sequence boundaries. A PTY read can split an
    /// escape sequence, a UTF-8 code point, or a grapheme cluster anywhere, so
    /// the resulting screen must depend only on the concatenation of the bytes
    /// fed, never on how they arrived.
    fn advance(&mut self, bytes: &[u8]);

    /// Resizes the screen, reflowing according to the emulator's rules.
    fn resize(&mut self, size: Size);

    /// Returns the screen to its just-constructed state, keeping the configured
    /// size and scrollback limit.
    ///
    /// This is the state a snapshot is defined to reconstruct from.
    fn reset(&mut self);

    /// Returns the current size.
    fn size(&self) -> Size;

    /// Reports whether the alternate screen is currently active.
    ///
    /// M3's conversation view reads this directly rather than inferring it, and
    /// falls back to terminal presentation when it is true.
    fn alternate_screen_active(&self) -> bool;

    /// Cursor position, visibility, and shape.
    fn cursor(&self) -> Cursor;

    /// The current vertical scrolling region.
    fn scroll_region(&self) -> ScrollRegion;

    /// The modes currently in effect.
    fn modes(&self) -> Modes;

    /// The window title last set by the program, if any.
    fn title(&self) -> Option<String>;

    /// A complete normalized picture of the visible screen.
    ///
    /// Equality of two of these is how the round-trip is asserted, so this must
    /// contain everything a reattaching client would notice and nothing it
    /// would not.
    fn model(&self) -> ScreenModel;

    /// The configured maximum number of scrollback lines.
    fn scrollback_limit(&self) -> usize;

    /// How many scrollback lines are currently retained. Never exceeds
    /// [`scrollback_limit`](Screen::scrollback_limit).
    fn scrollback_len(&self) -> usize;

    /// A retained scrollback line, `0` being the oldest still held.
    fn scrollback_line(&self, index: usize) -> Option<Row>;

    /// How many lines have been dropped from the bottom of the ring over the
    /// session's life.
    ///
    /// A client that has scrolled back needs to know its view was truncated
    /// rather than silently shortened.
    fn scrollback_dropped(&self) -> u64;

    /// Serializes the current screen into a self-contained byte sequence that
    /// reconstructs it exactly from a freshly reset terminal.
    ///
    /// This is the operation the whole product rests on. The round-trip test —
    /// stream, snapshot, replay into a fresh emulator, assert identical screens
    /// — is the single most important test in the suite.
    ///
    /// The snapshot restores the visible screen, not the scrollback ring: a
    /// reattaching client is owed what a continuously attached one shows.
    fn snapshot(&self) -> Vec<u8>;

    /// The bytes a client is sent when it attaches: bounded scrollback, then
    /// the screen, as one self-contained sequence.
    ///
    /// [`snapshot`](Screen::snapshot) is this with [`HistoryPolicy::NONE`], and
    /// that is not an accident of implementation. A backpressure resync sends
    /// the screen alone; only an attach carries history. Composing the two here
    /// rather than in the caller is what keeps the ordering — reset, history,
    /// screen — impossible to get wrong from outside, and the ordering is
    /// load-bearing in both directions: a hard reset clears the receiving
    /// terminal's saved lines, and history has to scroll past the screen to
    /// reach them.
    fn attach_snapshot(&self, history: HistoryPolicy) -> Vec<u8>;
}
