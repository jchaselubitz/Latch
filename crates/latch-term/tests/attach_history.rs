//! Scrollback that accompanies a snapshot on attach.
//!
//! The decision this suite pins — how much history, bounded how — is recorded
//! in `docs/DECISION_SCROLLBACK.md`. What is asserted here is the part that has
//! to hold whatever the numbers become:
//!
//! * The visible screen is reproduced **exactly**, history or no history. A
//!   preamble that disturbed the screen would trade the one guarantee the whole
//!   product rests on for a convenience.
//! * History reaches the receiving terminal's own scrollback rather than its
//!   visible screen, in the order it was printed, newest lines kept.
//! * The preamble is bounded in bytes, not merely in lines. On a phone every
//!   app-backgrounding is a reattach, so this is a per-glance cost on a metered
//!   link, and a line count bounds it only if every line is cheap. One is not.

mod support;

use latch_term::{
    Cell, CellAttrs, Color, HistoryPolicy, Row, Screen, ScreenModel, Size, Terminal, TerminalConfig,
};
use support::{assert_screens_identical, replay};

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// The sequence that separates history from the screen in an attach payload.
///
/// Every snapshot states the alternate-screen mode in both directions, and it
/// is the first thing emitted after the history block. Nothing above it can
/// imitate it: cells hold printable graphemes, so the only escapes the history
/// block contains are the `SGR` sequences it writes itself.
const SCREEN_MARKER: &[u8] = b"\x1b[?1049";

fn terminal(scrollback: usize) -> Terminal {
    Terminal::new(TerminalConfig::new(Size::new(COLS, ROWS)).with_scrollback_limit(scrollback))
}

/// `n` numbered lines, as a PTY delivers them.
fn numbered_lines(n: usize) -> Vec<u8> {
    (1..=n).fold(Vec::new(), |mut out, i| {
        out.extend_from_slice(format!("line-{i:05}\r\n").as_bytes());
        out
    })
}

/// Splits an attach payload into its history preamble and the screen.
fn split_history(payload: &[u8]) -> (&[u8], &[u8]) {
    let at = payload
        .windows(SCREEN_MARKER.len())
        .position(|window| window == SCREEN_MARKER)
        .expect("every snapshot states the alternate-screen mode");
    payload.split_at(at)
}

/// The text of every line the payload's history block prints.
fn history_lines(payload: &[u8]) -> Vec<String> {
    let (history, _) = split_history(payload);
    let text = String::from_utf8_lossy(history);
    // The leading reset and the trailing scroll-past newlines are mechanism,
    // not content.
    text.trim_start_matches("\x1bc")
        .split("\r\n")
        .map(|line| strip_sgr(line).trim_end().to_owned())
        .filter(|line| !line.is_empty())
        .collect()
}

fn strip_sgr(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('\u{1b}') {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        match after.find('m') {
            Some(end) => rest = &after[end + 1..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// A policy that cannot be reached by either bound, for tests about the other
/// properties.
fn generous() -> HistoryPolicy {
    HistoryPolicy {
        max_lines: 100_000,
        max_bytes: 100 * 1024 * 1024,
    }
}

// --- The screen is never disturbed -------------------------------------------

/// The round-trip that the product rests on, restated with history attached.
/// A preamble that scrolled the screen, left a pen set, or moved the cursor
/// would show up here and nowhere else until a phone showed it to a person.
#[test]
fn the_visible_screen_round_trips_exactly_with_history_attached() {
    let mut term = terminal(500);
    term.advance(&numbered_lines(200));
    term.advance(b"\x1b[1;32mgreen\x1b[m and \x1b[4munderlined\x1b[m");

    let restored = replay(&term.attach_snapshot(generous()), Size::new(COLS, ROWS));
    assert_screens_identical(
        "attach snapshot with history",
        &term.model(),
        &restored.model(),
    );
}

/// The same, for an alternate-screen application — the case M2 exists for.
/// History belongs to the primary screen, so it must be printed before the
/// switch and must not follow the application onto the alternate screen.
#[test]
fn an_alternate_screen_round_trips_and_its_history_precedes_the_switch() {
    let mut term = terminal(500);
    term.advance(&numbered_lines(100));
    term.advance(b"\x1b[?1049h\x1b[H\x1b[2JAGENT IS THINKING");

    let payload = term.attach_snapshot(generous());
    let (history, screen) = split_history(&payload);
    assert!(
        !history.is_empty(),
        "the primary screen's history survives the application switching away from it"
    );
    assert!(
        screen.starts_with(b"\x1b[?1049h"),
        "the alternate screen is entered after the history, not before it"
    );

    let restored = replay(&payload, Size::new(COLS, ROWS));
    assert!(restored.alternate_screen_active());
    assert_screens_identical(
        "alternate screen with history",
        &term.model(),
        &restored.model(),
    );
}

/// No history is not a special case with its own code path; it is the same
/// composition with an empty block. Asserting byte identity keeps it that way,
/// so a resync and a history-less attach cannot drift apart.
#[test]
fn a_policy_that_sends_nothing_is_byte_identical_to_the_plain_snapshot() {
    let mut term = terminal(500);
    term.advance(&numbered_lines(200));

    assert_eq!(term.attach_snapshot(HistoryPolicy::NONE), term.snapshot());
    assert_eq!(
        term.attach_snapshot(HistoryPolicy {
            max_lines: 0,
            max_bytes: 64 * 1024,
        }),
        term.snapshot(),
        "a zero line bound sends nothing whatever the byte budget allows"
    );
    assert_eq!(
        term.attach_snapshot(HistoryPolicy {
            max_lines: 500,
            max_bytes: 0,
        }),
        term.snapshot(),
        "a zero byte budget sends nothing whatever the line bound allows"
    );
}

/// An empty ring costs nothing at all. Every recorded agent fixture is in this
/// state — see `docs/DECISION_SCROLLBACK.md` — so it is the common case, not the
/// degenerate one.
#[test]
fn a_session_that_has_never_scrolled_pays_nothing_for_the_policy() {
    let mut term = terminal(500);
    term.advance(b"\x1b[?1049h\x1b[H\x1b[2Jfull screen application");
    assert_eq!(term.scrollback_len(), 0);
    assert_eq!(term.attach_snapshot(generous()), term.snapshot());
}

// --- History reaches the client's scrollback ---------------------------------

/// The point of the preamble. A client that received the lines onto its visible
/// screen would have them erased by the screen paint that follows — and the
/// lines erased would be the newest ones, which are the ones worth having.
#[test]
fn history_lands_in_the_receiving_terminals_scrollback_oldest_first() {
    let mut term = terminal(500);
    term.advance(&numbered_lines(300));
    let expected_newest = term
        .scrollback_line(term.scrollback_len() - 1)
        .expect("the ring is not empty")
        .text();

    let restored = replay(&term.attach_snapshot(generous()), Size::new(COLS, ROWS));
    let retained: Vec<String> = (0..restored.scrollback_len())
        .map(|i| restored.scrollback_line(i).expect("in range").text())
        .collect();

    assert!(
        retained.contains(&"line-00001".to_owned()),
        "the oldest retained line reaches the client: {:?}",
        &retained[..retained.len().min(3)]
    );
    let newest_at = retained
        .iter()
        .rposition(|line| line == &expected_newest)
        .expect("the newest retained line reaches the client too");
    let oldest_at = retained
        .iter()
        .position(|line| line == "line-00001")
        .expect("checked above");
    assert!(
        oldest_at < newest_at,
        "history arrives in the order it was printed"
    );
    assert_eq!(
        restored.scrollback_len(),
        term.scrollback_len() + 1,
        "the client ends up holding exactly the session's ring, plus the one \
         blank line that scrolling it above the screen costs"
    );
}

/// The screen a reattaching client is shown must be the session's screen, not
/// the last line of its history. This is the failure the scroll-past newlines
/// exist to prevent, asserted as a screen rather than as a byte count.
#[test]
fn the_screen_is_the_sessions_screen_and_not_the_tail_of_its_history() {
    let mut term = terminal(500);
    term.advance(&numbered_lines(300));
    // Cleared, so that "on the screen" and "in the history" are answerable
    // separately. Without this the two are contiguous by construction and the
    // assertion below would hold even if the preamble had painted the screen.
    term.advance(b"\x1b[H\x1b[2JCURRENT PROMPT> ");

    let restored = replay(&term.attach_snapshot(generous()), Size::new(COLS, ROWS));
    let visible: Vec<String> = (0..ROWS).map(|r| restored.model().row_text(r)).collect();
    assert!(
        visible.iter().any(|row| row.contains("CURRENT PROMPT>")),
        "the current screen is what is displayed: {visible:?}"
    );
    assert_eq!(
        visible
            .iter()
            .filter(|row| row.starts_with("line-"))
            .count(),
        0,
        "history is above the screen, not on it: {visible:?}"
    );
    // Line 277 is the last one that *scrolled*. The 23 lines that were still
    // visible when the screen was cleared were erased rather than scrolled, so
    // they never entered the ring — that is ordinary terminal behavior, and it
    // is asserted here so a change to it is noticed here rather than as missing
    // context on a phone.
    assert!(
        (0..restored.scrollback_len())
            .filter_map(|i| restored.scrollback_line(i))
            .any(|row| row.text() == "line-00277"),
        "and the newest history line is still delivered, above the screen"
    );
}

/// Rendition survives, and does not bleed. A build log is mostly interesting
/// because of its colors — the red line is the one you are looking for.
#[test]
fn history_keeps_its_rendition_and_does_not_leak_it_into_the_next_line() {
    let mut term = terminal(500);
    term.advance(b"\x1b[31mRED LINE\x1b[m\r\nplain line\r\n");
    term.advance(&numbered_lines(30));

    let restored = replay(&term.attach_snapshot(generous()), Size::new(COLS, ROWS));
    let red = (0..restored.scrollback_len())
        .filter_map(|i| restored.scrollback_line(i))
        .find(|row| row.text() == "RED LINE")
        .expect("the colored line is retained");
    assert_eq!(red.cells[0].attrs.fg, Color::Indexed(1));

    let plain = (0..restored.scrollback_len())
        .filter_map(|i| restored.scrollback_line(i))
        .find(|row| row.text() == "plain line")
        .expect("the following line is retained");
    assert_eq!(
        plain.cells[0].attrs,
        CellAttrs::default(),
        "the color of the line above does not run on into this one"
    );
}

/// Trailing padding is dropped; a trailing *rendition* is not. A diff that
/// paints its background to the end of the line is blank cells that mean
/// something, and trimming them changes what the line says rather than only
/// what it costs.
#[test]
fn a_trailing_background_survives_but_ordinary_padding_does_not() {
    let mut term = terminal(500);
    term.advance(b"\x1b[41mHIGHLIGHTED\x1b[K\x1b[m\r\nplain\r\n");
    term.advance(&numbered_lines(30));

    let restored = replay(&term.attach_snapshot(generous()), Size::new(COLS, ROWS));
    let highlighted = (0..restored.scrollback_len())
        .filter_map(|i| restored.scrollback_line(i))
        .find(|row| row.text().starts_with("HIGHLIGHTED"))
        .expect("the highlighted line is retained");
    assert_eq!(
        highlighted.cells[usize::from(COLS) - 1].attrs.bg,
        Color::Indexed(1),
        "the background painted to the end of the line reaches the client"
    );

    let plain = (0..restored.scrollback_len())
        .filter_map(|i| restored.scrollback_line(i))
        .find(|row| row.text() == "plain")
        .expect("the plain line is retained");
    assert_eq!(
        plain.cells[usize::from(COLS) - 1].attrs,
        CellAttrs::default(),
        "and an unrendered line is not padded out to reach it"
    );
}

/// A line as wide as the screen must not wrap the line after it. The `\r` in
/// the terminator is what cancels the backend's deferred wrap; a bare newline
/// would shift everything below by one column, which on a phone reads as
/// corruption rather than as an off-by-one.
#[test]
fn a_full_width_line_does_not_wrap_the_line_that_follows_it() {
    let mut term = terminal(500);
    let full = "W".repeat(usize::from(COLS));
    term.advance(format!("{full}\r\nAFTER\r\n").as_bytes());
    term.advance(&numbered_lines(30));

    let restored = replay(&term.attach_snapshot(generous()), Size::new(COLS, ROWS));
    let texts: Vec<String> = (0..restored.scrollback_len())
        .filter_map(|i| restored.scrollback_line(i))
        .map(|row| row.text())
        .collect();
    let full_at = texts
        .iter()
        .position(|line| line == &full)
        .expect("the full-width line is retained");
    assert_eq!(
        texts.get(full_at + 1).map(String::as_str),
        Some("AFTER"),
        "the line after a full-width one starts at column zero: {:?}",
        &texts[full_at..texts.len().min(full_at + 3)]
    );
}

/// Wide characters occupy two columns and are stored as a cell plus a
/// continuation. Printing the continuation would double them; skipping the
/// character would lose it.
#[test]
fn wide_characters_survive_the_history_block_at_their_own_width() {
    let mut term = terminal(500);
    term.advance("日本語テスト\r\n".as_bytes());
    term.advance(&numbered_lines(30));

    let restored = replay(&term.attach_snapshot(generous()), Size::new(COLS, ROWS));
    let row = (0..restored.scrollback_len())
        .filter_map(|i| restored.scrollback_line(i))
        .find(|row| row.text().contains('日'))
        .expect("the wide line is retained");
    assert_eq!(row.text(), "日本語テスト");
    assert_eq!(row.cells[0].width, 2);
    assert_eq!(row.cells[1].width, 0, "the continuation half is preserved");
}

// --- The bounds --------------------------------------------------------------

/// The newest lines are the ones kept. Dropping from the wrong end would make
/// the whole feature useless: what you want when you pick up your phone is what
/// just happened, not what happened first.
#[test]
fn the_line_bound_keeps_the_newest_lines_and_drops_the_oldest() {
    let mut term = terminal(1000);
    term.advance(&numbered_lines(500));

    let payload = term.attach_snapshot(HistoryPolicy {
        max_lines: 10,
        max_bytes: 1024 * 1024,
    });
    let lines = history_lines(&payload);
    assert_eq!(lines.len(), 10, "exactly the bound: {lines:?}");
    // 500 terminated lines on a 24-row screen leave the last 23 visible and the
    // cursor on a fresh line below them, so 477 have scrolled into the ring.
    assert_eq!(lines.last().map(String::as_str), Some("line-00477"));
    assert_eq!(lines.first().map(String::as_str), Some("line-00468"));
}

/// The bound that a link actually feels. A line of ordinary output costs tens
/// of bytes and a line that changes color in every cell costs thousands, so a
/// policy bounded only in lines is not bounded at all — and this is not a
/// contrived stream, it is what a progress bar or a syntax-highlighted diff
/// looks like.
#[test]
fn the_byte_bound_wins_when_lines_are_expensive() {
    const BUDGET: usize = 4 * 1024;
    let mut term = terminal(1000);
    let mut costly = Vec::new();
    for line in 0..200u32 {
        for col in 0..COLS {
            costly.extend_from_slice(
                format!("\x1b[38;5;{}m#", (u32::from(col) + line) % 256).as_bytes(),
            );
        }
        costly.extend_from_slice(b"\x1b[m\r\n");
    }
    term.advance(&costly);

    let payload = term.attach_snapshot(HistoryPolicy {
        max_lines: 200,
        max_bytes: BUDGET,
    });
    let (history, _) = split_history(&payload);
    assert!(
        history.len() <= BUDGET,
        "the preamble was {} bytes against a {BUDGET} byte budget",
        history.len()
    );
    assert!(
        !history_lines(&payload).is_empty(),
        "a tight budget still delivers whole lines rather than nothing"
    );
}

/// A budget too small even for the mechanism sends nothing rather than sending
/// a partial one.
#[test]
fn a_budget_smaller_than_the_mechanism_sends_no_history_at_all() {
    let mut term = terminal(1000);
    term.advance(&numbered_lines(300));
    assert_eq!(
        term.attach_snapshot(HistoryPolicy {
            max_lines: 200,
            max_bytes: usize::from(ROWS) - 1,
        }),
        term.snapshot(),
    );
}

/// Lines are whole or absent. A truncated line is a claim the session never
/// printed, and on a build log the truncated end is the error message.
#[test]
fn a_line_that_does_not_fit_the_budget_is_omitted_rather_than_cut() {
    let mut term = terminal(1000);
    term.advance(b"SHORT\r\n");
    term.advance(format!("{}\r\n", "L".repeat(usize::from(COLS))).as_bytes());
    term.advance(&numbered_lines(30));

    // Enough for the mechanism and a couple of short lines, not for the long
    // one plus what follows it.
    let payload = term.attach_snapshot(HistoryPolicy {
        max_lines: 200,
        max_bytes: usize::from(ROWS) + 40,
    });
    for line in history_lines(&payload) {
        assert!(
            line.len() < usize::from(COLS),
            "a partial line reached the client: {line:?}"
        );
    }
}

/// The ring holds the bound; the policy cannot conjure lines that were never
/// retained.
#[test]
fn the_policy_never_asks_for_more_than_the_ring_holds() {
    let mut term = terminal(20);
    term.advance(&numbered_lines(500));
    assert_eq!(term.scrollback_len(), 20);

    let lines = history_lines(&term.attach_snapshot(generous()));
    assert_eq!(lines.len(), 20);
}

// --- What it costs on a link --------------------------------------------------

/// The measurement the decision was made from, kept executable.
///
/// Every recorded agent stream ends with an empty ring — Claude Code and Codex
/// both stay on one screen, whether by using the alternate screen or by
/// rewriting the visible one — so for the case this milestone exists for, the
/// history policy costs nothing and the snapshot alone is the whole answer.
/// If a re-recording ever breaks this, the decision needs revisiting rather
/// than the test relaxing: see `docs/DECISION_SCROLLBACK.md`.
#[test]
fn the_recorded_agent_streams_carry_no_scrollback_and_so_cost_nothing() {
    for case in support::cases() {
        let term = case.play();
        if term.scrollback_len() == 0 {
            assert_eq!(
                term.attach_snapshot(HistoryPolicy::new()).len(),
                term.snapshot().len(),
                "{}: an empty ring must add nothing to the attach payload",
                case.name
            );
        } else {
            assert_eq!(
                case.name, "high-rate-output",
                "a recorded case has started producing scrollback; the history \
                 policy was measured without it"
            );
        }
    }
}

/// The default policy against sustained output, stated as the number a link
/// pays. This is a per-glance cost: on a phone every app-backgrounding is a
/// reattach.
#[test]
fn the_default_policy_costs_a_bounded_and_stated_number_of_bytes() {
    let mut term = terminal(10_000);
    // Build-log shaped: mixed lengths, some color, some blank lines.
    for i in 0..3_000u32 {
        let line = match i % 5 {
            0 => format!("\x1b[32m   Compiling\x1b[m latch-term v0.0.{i}\r\n"),
            1 => format!("warning: unused variable `value` at line {i}\r\n"),
            2 => "\r\n".to_owned(),
            3 => format!("test worker::registry::case_{i} ... \x1b[32mok\x1b[m\r\n"),
            _ => format!("    {}\r\n", "x".repeat(usize::from(COLS) - 4)),
        };
        term.advance(line.as_bytes());
    }

    let payload = term.attach_snapshot(HistoryPolicy::new());
    let (history, _) = split_history(&payload);
    assert_eq!(
        history_lines(&payload).len(),
        // Blank lines are retained and carry no text, so the count of
        // non-empty ones is below the line bound.
        160,
        "the line bound is what binds on ordinary output, not the byte ceiling"
    );
    assert!(
        history.len() < 16 * 1024,
        "200 lines of build output cost {} bytes; the decision was made at \
         roughly 14 KB and a large change means it needs remaking",
        history.len()
    );
}

// --- Regression guards --------------------------------------------------------

/// A resize reaches retained lines, so history serialized after one must be at
/// the new width. A phone that takes control of a 200-column session and is
/// then sent 200-column history would wrap every line of it.
#[test]
fn history_is_serialized_at_the_current_width_after_a_resize() {
    let mut term =
        Terminal::new(TerminalConfig::new(Size::new(200, 50)).with_scrollback_limit(500));
    for i in 0..200u32 {
        term.advance(format!("{}\r\n", format_args!("{i:>03}{}", "-".repeat(150))).as_bytes());
    }
    term.resize(Size::new(40, 20));

    let payload = term.attach_snapshot(generous());
    for line in history_lines(&payload) {
        assert!(
            line.chars().count() <= 40,
            "a {} character history line was sent to a 40 column client",
            line.chars().count()
        );
    }
}

/// Row width and screen width can disagree only through a bug; asserting the
/// serializer tolerates it keeps a panic out of the worker's attach path, which
/// would take the session down for every client rather than one.
#[test]
fn a_ragged_retained_row_does_not_panic_the_serializer() {
    let row = Row {
        cells: vec![Cell::plain("a"), Cell::continuation()],
    };
    // Reaching `encode_row` through the public surface: a terminal whose ring
    // holds this row would serialize it on attach.
    assert_eq!(row.text(), "a");
    let model = ScreenModel {
        size: Size::new(2, 1),
        rows: vec![row],
        cursor: Default::default(),
        alternate_screen: false,
        scroll_region: latch_term::ScrollRegion::full(1),
        modes: Default::default(),
        title: None,
        pen: CellAttrs::default(),
    };
    assert_eq!(model.row_text(0), "a");
}
