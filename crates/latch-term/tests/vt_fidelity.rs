//! Screen fidelity against recorded byte streams.
//!
//! Every assertion here is against a normalized model — cells with text and
//! attributes, cursor, alternate-screen flag, scroll region, modes — never a
//! screenshot. A screenshot test tells you something changed; it does not tell
//! you a wide character lost its continuation cell, and that is the bug that
//! makes a phone reattach drift one column at a time.
//!
//! The bytes are recorded from real Claude Code and Codex sessions by
//! `scripts/capture-vt.py`. Hand-written escape sequences only test what the
//! author already knew about.

mod support;

use latch_term::{Screen, Size};
use support::{assert_expected, case, cases, render};

/// The behaviors the fixture set must keep covering.
///
/// Without this, a case can be deleted or renamed and the suite still passes
/// with nothing to say. Each name here is a behavior the objective requires a
/// recording of.
const REQUIRED_CASES: [&str; 9] = [
    "claude-code-startup",
    "claude-code-turn",
    "codex-startup",
    "alternate-screen-enter-exit",
    "resize-midstream",
    "unicode-wide-combining-emoji",
    "bracketed-paste",
    "cursor-rewrite-progress",
    "high-rate-output",
];

#[test]
fn fixture_set_covers_the_required_behaviors() {
    let present: Vec<String> = cases().into_iter().map(|c| c.name).collect();
    for required in REQUIRED_CASES {
        assert!(
            present.iter().any(|name| name == required),
            "fixtures/vt/ is missing the {required} case; present: {present:?}"
        );
    }
}

#[test]
fn every_case_is_well_formed() {
    for case in cases() {
        assert!(
            !case.input.is_empty(),
            "{}: input.bin is empty; a case that feeds nothing asserts nothing",
            case.name
        );
        assert!(
            case.expected.is_some(),
            "{}: no expected.json. Every case states what it is protecting, even \
             if that is only the alternate-screen flag and the cursor.",
            case.name
        );
        assert!(
            case.size.cols > 0 && case.size.rows > 0,
            "{}: meta.json must give a non-zero starting size",
            case.name
        );
        for resize in &case.resizes {
            assert!(
                resize.at_byte <= case.input.len(),
                "{}: a resize is recorded past the end of the stream",
                case.name
            );
        }
    }
}

#[test]
fn every_case_matches_its_expected_screen() {
    for case in cases() {
        let term = case.play();
        assert_expected(&case, &term.model());
    }
}

/// The resize in `meta.json` must actually take effect: a suite that silently
/// ignored it would pass every other assertion and test the wrong stream.
#[test]
fn a_mid_stream_resize_leaves_the_screen_at_the_new_size() {
    let case = case("resize-midstream");
    assert!(!case.resizes.is_empty(), "the case carries a resize");
    let term = case.play();
    assert_eq!(
        term.size(),
        case.final_size(),
        "the screen is at the size the stream ended on"
    );
    assert_ne!(
        case.final_size(),
        case.size,
        "the case must actually change size, or it is not testing a resize"
    );
}

/// `alternate_screen_active` is a property read, not a heuristic over bytes:
/// M3's conversation view falls back to terminal presentation on it, so a wrong
/// answer shows the wrong UI for the whole session.
#[test]
fn the_alternate_screen_flag_follows_the_stream() {
    let case = case("alternate-screen-enter-exit");
    let mut term = case.terminal();

    let enter = case
        .input
        .windows(8)
        .position(|w| w == b"\x1b[?1049h")
        .expect("the recording enters the alternate screen");
    term.advance(&case.input[..enter]);
    assert!(
        !term.alternate_screen_active(),
        "not on the alternate screen before the sequence that enters it"
    );

    let leave = case
        .input
        .windows(8)
        .position(|w| w == b"\x1b[?1049l")
        .expect("the recording leaves the alternate screen");
    term.advance(&case.input[enter..leave]);
    assert!(
        term.alternate_screen_active(),
        "on the alternate screen while the pager is running"
    );

    term.advance(&case.input[leave..]);
    assert!(
        !term.alternate_screen_active(),
        "back on the primary screen after the pager exits"
    );
    assert_eq!(
        term.model().row_text(0),
        "before the pager",
        "leaving the alternate screen restores what the primary screen held:\n{}",
        render(&term.model())
    );
}

/// Claude Code sets the title on every turn. A reattaching client that never
/// learns it shows a stale tab for the rest of the session.
#[test]
fn the_window_title_is_tracked() {
    let term = case("claude-code-startup").play();
    assert_eq!(term.title().as_deref(), Some("\u{2733} Claude Code"));

    let term = case("unicode-wide-combining-emoji").play();
    assert_eq!(term.title(), None, "a stream that sets no title has none");
}

/// A double-width character occupies two cells. The second is a continuation:
/// empty text, zero width. Collapsing it into one cell drifts everything to its
/// right by a column, which is invisible in a diff and obvious on a phone.
#[test]
fn wide_characters_occupy_two_cells() {
    let model = case("unicode-wide-combining-emoji").play().model();
    let base = model.cell(1, 5).expect("row 1 col 5 exists");
    let continuation = model.cell(1, 6).expect("row 1 col 6 exists");
    assert_eq!(base.text, "\u{4f60}");
    assert_eq!(base.width, 2);
    assert_eq!(continuation.width, 0);
    assert!(
        continuation.text.is_empty(),
        "a continuation cell holds no text of its own"
    );
}

/// A combining mark belongs to the cell of the character it modifies, not to a
/// cell of its own.
#[test]
fn combining_marks_stay_in_their_base_cell() {
    let model = case("unicode-wide-combining-emoji").play().model();
    let cell = model.cell(2, 11).expect("row 2 col 11 exists");
    assert_eq!(cell.text, "e\u{301}");
    assert_eq!(cell.width, 1);
    assert_eq!(
        model.cell(2, 12).expect("row 2 col 12 exists").text.trim(),
        "",
        "the mark did not take a cell of its own"
    );
}

/// A wide character that will not fit in the last column wraps to the next row
/// and leaves the column blank. It is never split across the margin.
#[test]
fn a_wide_character_that_does_not_fit_wraps_whole() {
    let model = case("unicode-wide-combining-emoji").play().model();
    assert_eq!(
        model.cell(9, 99).expect("row 9 col 99 exists").text.trim(),
        "",
        "the last column is left blank rather than holding half a character"
    );
    assert_eq!(
        model.cell(10, 0).expect("row 10 col 0 exists").text,
        "\u{6f22}",
        "the character that did not fit starts the next row"
    );
}

/// Bracketed paste is one of the modes a byte-tail replay loses entirely, and
/// getting it wrong means a pasted block runs line by line as it arrives.
#[test]
fn bracketed_paste_mode_is_tracked() {
    let case = case("bracketed-paste");
    let mut term = case.terminal();
    let enable = case
        .input
        .windows(8)
        .position(|w| w == b"\x1b[?2004h")
        .expect("the recording enables bracketed paste");
    term.advance(&case.input[..enable + 8]);
    assert!(term.modes().bracketed_paste);

    term.advance(&case.input[enable + 8..]);
    assert!(
        !term.modes().bracketed_paste,
        "the shell disabled bracketed paste before exiting"
    );
}

/// Claude Code turns on four mouse modes and SGR encoding at startup. A client
/// that reattaches without them sends nothing when the user taps.
#[test]
fn mouse_and_paste_modes_survive_a_full_startup() {
    let term = case("claude-code-startup").play();
    let modes = term.modes();
    assert_eq!(modes.mouse_tracking, latch_term::MouseTracking::AnyEvent);
    assert_eq!(modes.mouse_encoding, latch_term::MouseEncoding::Sgr);
    assert!(modes.bracketed_paste);
    assert!(modes.focus_reporting);
}

/// Every cell of every row is present at the screen's width, always. A model
/// with ragged rows makes a cell comparison depend on where a program stopped
/// writing.
#[test]
fn models_are_rectangular_at_the_screen_size() {
    for case in cases() {
        let model = case.play().model();
        assert_eq!(
            model.rows.len(),
            model.size.rows as usize,
            "{}: row count must equal the screen height",
            case.name
        );
        for (index, row) in model.rows.iter().enumerate() {
            assert_eq!(
                row.cells.len(),
                model.size.cols as usize,
                "{}: row {index} is not the full screen width",
                case.name
            );
        }
    }
}

/// The cursor is always on the screen. A cursor reported outside it is a client
/// crash, not a rendering difference.
#[test]
fn the_cursor_stays_within_the_screen() {
    for case in cases() {
        let model = case.play().model();
        assert!(
            model.cursor.row < model.size.rows,
            "{}: cursor row {} outside a {}-row screen",
            case.name,
            model.cursor.row,
            model.size.rows
        );
        assert!(
            model.cursor.col <= model.size.cols,
            "{}: cursor col {} outside a {}-column screen",
            case.name,
            model.cursor.col,
            model.size.cols
        );
    }
}

/// Resizing is not a redraw request: the screen must still be coherent
/// afterwards, at the new size, with the cursor inside it.
#[test]
fn resizing_leaves_a_coherent_screen() {
    for case in cases() {
        let mut term = case.play();
        for size in [Size::new(20, 6), Size::new(200, 60), Size::new(80, 24)] {
            term.resize(size);
            let model = term.model();
            assert_eq!(model.size, size, "{}: resize to {size:?}", case.name);
            assert_eq!(model.rows.len(), size.rows as usize);
            assert!(
                model.cursor.row < size.rows,
                "{}: cursor left the screen after resize",
                case.name
            );
        }
    }
}
