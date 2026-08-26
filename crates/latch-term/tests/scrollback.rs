//! The bounded scrollback ring.
//!
//! Scrollback is a bounded structured ring, not an unbounded byte log. A
//! session that runs for a week with an agent printing build output must have a
//! predictable memory cost, and the cap is the only thing that gives it one.
//!
//! The subtle requirement is the last test in this file: once the ring has
//! wrapped, the snapshot of the *visible* screen must still be exact. Losing
//! old lines is the design; losing accuracy about the current screen is a bug,
//! and the two are easy to confuse in an implementation that reconstructs the
//! screen out of the ring.

mod support;

use latch_term::{Screen, Size, Terminal, TerminalConfig};
use support::{replay, scrollback_expectation};

const ROWS: u16 = 10;
const COLS: u16 = 40;
const LIMIT: usize = 100;

/// `n` numbered lines, each terminated, as a PTY would deliver them.
fn numbered_lines(n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 1..=n {
        out.extend_from_slice(format!("{i}\r\n").as_bytes());
    }
    out
}

fn bounded_terminal() -> Terminal {
    Terminal::new(TerminalConfig::new(Size::new(COLS, ROWS)).with_scrollback_limit(LIMIT))
}

#[test]
fn the_ring_never_exceeds_its_configured_limit() {
    let mut term = bounded_terminal();
    assert_eq!(term.scrollback_limit(), LIMIT);
    assert_eq!(term.scrollback_len(), 0, "nothing has scrolled yet");

    term.advance(&numbered_lines(5));
    assert_eq!(
        term.scrollback_len(),
        0,
        "five lines on a ten-row screen have not scrolled off it"
    );

    term.advance(&numbered_lines(1000));
    assert_eq!(
        term.scrollback_len(),
        LIMIT,
        "the ring is capped at its configured limit"
    );
}

/// Oldest first. Dropping the newest would make scrollback useless: what a user
/// scrolls up to see is what just left the screen.
#[test]
fn the_ring_drops_the_oldest_line_first() {
    let mut term = bounded_terminal();
    term.advance(&numbered_lines(1000));

    // 1000 lines plus the blank line the cursor sits on, on a 10-row screen:
    // lines 1..=991 have scrolled off, and the last LIMIT of those are kept.
    let newest = term
        .scrollback_line(term.scrollback_len() - 1)
        .expect("the newest retained line");
    assert_eq!(
        newest.text(),
        "991",
        "the newest scrollback line is the one that just left the screen"
    );

    let oldest = term.scrollback_line(0).expect("the oldest retained line");
    assert_eq!(
        oldest.text(),
        "892",
        "the oldest retained line is exactly LIMIT lines back"
    );

    assert!(
        term.scrollback_line(term.scrollback_len()).is_none(),
        "there is no line past the end of the ring"
    );
}

/// A client that has scrolled back needs to know its view was truncated rather
/// than that the session simply started later.
#[test]
fn dropped_lines_are_counted() {
    let mut term = bounded_terminal();
    term.advance(&numbered_lines(1000));
    assert_eq!(
        term.scrollback_dropped(),
        991 - LIMIT as u64,
        "every line that left the ring is counted"
    );

    let mut fresh = bounded_terminal();
    fresh.advance(&numbered_lines(5));
    assert_eq!(
        fresh.scrollback_dropped(),
        0,
        "nothing is dropped before the ring fills"
    );
}

#[test]
fn a_zero_limit_retains_nothing_and_still_scrolls() {
    let mut term =
        Terminal::new(TerminalConfig::new(Size::new(COLS, ROWS)).with_scrollback_limit(0));
    term.advance(&numbered_lines(50));
    assert_eq!(term.scrollback_len(), 0);
    assert_eq!(
        term.model().row_text(0),
        "42",
        "the visible screen still scrolls normally"
    );
}

/// The alternate screen has no scrollback — that is most of the point of it.
/// Letting a full-screen redraw push a thousand lines into the ring would evict
/// the session's real history every time an agent repaints.
#[test]
fn alternate_screen_output_does_not_enter_the_ring() {
    let mut term = bounded_terminal();
    term.advance(&numbered_lines(40));
    let before = term.scrollback_len();
    let dropped_before = term.scrollback_dropped();

    term.advance(b"\x1b[?1049h");
    term.advance(&numbered_lines(500));
    assert_eq!(
        term.scrollback_len(),
        before,
        "alternate-screen output did not enter the ring"
    );
    assert_eq!(term.scrollback_dropped(), dropped_before);

    term.advance(b"\x1b[?1049l");
    assert_eq!(
        term.scrollback_len(),
        before,
        "leaving the alternate screen did not flush its lines into the ring"
    );
}

/// The test this file exists for. Once the ring has wrapped, the visible screen
/// must still snapshot exactly — a long-running session is the normal case, not
/// the edge case.
#[test]
fn a_snapshot_after_the_ring_wrapped_is_exact_for_the_visible_screen() {
    let mut term = bounded_terminal();
    term.advance(&numbered_lines(5000));
    assert_eq!(
        term.scrollback_len(),
        LIMIT,
        "the ring has wrapped many times"
    );
    assert!(term.scrollback_dropped() > 0);

    let restored = replay(&term.snapshot(), term.size());
    support::assert_screens_identical(
        "after the scrollback ring wrapped",
        &term.model(),
        &restored.model(),
    );
}

/// A snapshot restores the screen a reattaching client is owed, not the
/// sender's ring. Carrying scrollback would put the history question — how much
/// of it, and paged how — inside the snapshot, where it does not belong.
#[test]
fn a_snapshot_does_not_carry_the_ring() {
    let mut term = bounded_terminal();
    term.advance(&numbered_lines(1000));
    let restored = replay(&term.snapshot(), term.size());
    assert_eq!(
        restored.scrollback_len(),
        0,
        "the restored terminal starts with an empty ring"
    );
    assert_eq!(restored.scrollback_dropped(), 0);
}

/// The recorded high-rate case says what its ring should look like; hold the
/// implementation to it at the default limit rather than only at a small one
/// chosen by a test.
#[test]
fn the_high_rate_case_matches_its_scrollback_expectation() {
    let case = support::case("high-rate-output");
    let expectation =
        scrollback_expectation(&case).expect("the high-rate case states a scrollback expectation");
    let term = case.play();

    if expectation.at_limit {
        assert_eq!(
            term.scrollback_len(),
            term.scrollback_limit(),
            "the ring is saturated after 50000 lines"
        );
    }
    if let Some(len) = expectation.len {
        assert_eq!(term.scrollback_len(), len);
    }
    if let Some(text) = expectation.newest_text {
        let newest = term
            .scrollback_line(term.scrollback_len() - 1)
            .expect("newest retained line");
        assert_eq!(newest.text(), text);
    }
    if let Some(dropped) = expectation.dropped_at_least {
        assert!(
            term.scrollback_dropped() >= dropped,
            "expected at least {dropped} dropped lines, got {}",
            term.scrollback_dropped()
        );
    }
}

/// Retained lines are full rows, same as visible ones. A ring of trimmed
/// strings cannot render a scrolled-back screen with its colors.
#[test]
fn retained_lines_are_full_width_rows() {
    let mut term = bounded_terminal();
    term.advance(&numbered_lines(500));
    for index in [0usize, LIMIT / 2, LIMIT - 1] {
        let line = term.scrollback_line(index).expect("a retained line");
        assert_eq!(
            line.cells.len(),
            COLS as usize,
            "scrollback line {index} is not the full screen width"
        );
    }
}

/// Resizing must not corrupt the ring's accounting, whatever reflow the
/// emulator performs.
#[test]
fn resizing_keeps_the_ring_within_its_limit() {
    let mut term = bounded_terminal();
    term.advance(&numbered_lines(1000));
    for size in [Size::new(20, 6), Size::new(120, 40), Size::new(COLS, ROWS)] {
        term.resize(size);
        assert!(
            term.scrollback_len() <= term.scrollback_limit(),
            "the ring exceeded its limit after resizing to {size:?}"
        );
    }
}
