//! The snapshot round trip.
//!
//! ```text
//! feed input.bin -> snapshot() -> replay into a freshly reset emulator
//!                -> assert the two screens are identical
//! ```
//!
//! This is the single most important test in the product. Criterion 4 of the
//! success criteria is that reattaching to a running Claude Code session
//! reproduces the screen identically to what a continuously attached client
//! shows, and every mobile reattach is exactly this operation: backgrounding an
//! app is a detach, foregrounding it is a reattach, so a phone runs this path
//! dozens of times an hour.
//!
//! "Identical" means the whole [`ScreenModel`]: every cell with its rendition,
//! the cursor, the alternate-screen flag, the scroll region, the modes, the
//! title, and the pen new output will be written with. A snapshot that gets the
//! visible text right and the pen wrong looks correct until the next byte
//! arrives.

mod support;

use latch_term::{Screen, ScreenModel, Size, Terminal, TerminalConfig};
use proptest::prelude::*;
use support::{assert_screens_identical, cases, replay, Case};

/// Streams a case, snapshots, replays into a fresh terminal, and returns both
/// screens.
fn round_trip(case: &Case) -> (ScreenModel, ScreenModel) {
    let term = case.play();
    let snapshot = term.snapshot();
    let restored = replay(&snapshot, term.size());
    (term.model(), restored.model())
}

#[test]
fn every_case_round_trips_through_its_snapshot() {
    for case in cases() {
        let (before, after) = round_trip(&case);
        assert_screens_identical(
            &format!("{}: {}", case.name, case.description),
            &before,
            &after,
        );
    }
}

/// The snapshot must reconstruct from a *reset* terminal, not from one that
/// happens to be freshly constructed. A worker resyncing a slow client sends
/// the same bytes to a client whose terminal already holds another session's
/// output, and anything the snapshot leaves implicit will be inherited from
/// there instead.
#[test]
fn snapshot_reconstructs_from_a_dirtied_terminal_after_reset() {
    for case in cases() {
        let term = case.play();
        let snapshot = term.snapshot();

        let mut dirty = Terminal::new(TerminalConfig::new(term.size()));
        dirty.advance(b"\x1b[?1049h\x1b[31;44m\x1b[?25l\x1b[?2004h");
        dirty.advance("leftovers from another session\r\nand another line".as_bytes());
        dirty.advance(b"\x1b[5;10r\x1b[1m");
        dirty.reset();
        dirty.advance(&snapshot);

        assert_screens_identical(
            &format!("{}: replayed into a reset terminal", case.name),
            &term.model(),
            &dirty.model(),
        );
    }
}

/// A resize is one of the two cases a byte-tail replay gets wrong: the tail was
/// produced at the old size, and replaying it at the new one lays the text out
/// somewhere the program never put it.
#[test]
fn round_trip_holds_after_a_resize() {
    let sizes = [Size::new(72, 18), Size::new(140, 45), Size::new(40, 12)];
    for case in cases() {
        for size in sizes {
            let mut term = case.play();
            term.resize(size);
            let snapshot = term.snapshot();
            let restored = replay(&snapshot, size);
            assert_screens_identical(
                &format!(
                    "{}: resized to {}x{} before snapshot",
                    case.name, size.cols, size.rows
                ),
                &term.model(),
                &restored.model(),
            );
        }
    }
}

/// The cases that carry a mid-stream resize exercise the harder shape: the
/// resize happened while the program was drawing, and the program reacted to
/// it.
#[test]
fn round_trip_holds_for_cases_resized_mid_stream() {
    let resized: Vec<Case> = cases()
        .into_iter()
        .filter(|case| !case.resizes.is_empty())
        .collect();
    assert!(
        !resized.is_empty(),
        "fixtures/vt/ must carry at least one case with a mid-stream resize"
    );
    for case in resized {
        let (before, after) = round_trip(&case);
        assert_screens_identical(
            &format!("{}: resized mid-stream", case.name),
            &before,
            &after,
        );
    }
}

/// The other case a byte-tail replay gets wrong, and the one that matters most
/// in practice: Claude Code and Codex spend their whole lives here.
#[test]
fn round_trip_holds_while_the_alternate_screen_is_active() {
    let mut checked = 0;
    for case in cases() {
        let term = case.play();
        if !term.alternate_screen_active() {
            continue;
        }
        checked += 1;
        let snapshot = term.snapshot();
        let restored = replay(&snapshot, term.size());
        assert!(
            restored.alternate_screen_active(),
            "{}: the snapshot did not restore the alternate screen",
            case.name
        );
        assert_screens_identical(
            &format!("{}: alternate screen active", case.name),
            &term.model(),
            &restored.model(),
        );
    }
    assert!(
        checked > 0,
        "fixtures/vt/ must carry at least one case that ends on the alternate screen"
    );
}

/// Leaving the alternate screen must restore the primary screen's contents, and
/// a snapshot taken afterwards describes the primary screen.
#[test]
fn round_trip_holds_after_leaving_the_alternate_screen() {
    let case = support::case("alternate-screen-enter-exit");
    let term = case.play();
    assert!(
        !term.alternate_screen_active(),
        "the case ends back on the primary screen"
    );
    let restored = replay(&term.snapshot(), term.size());
    assert!(!restored.alternate_screen_active());
    assert_screens_identical(
        "alternate-screen-enter-exit",
        &term.model(),
        &restored.model(),
    );
}

/// A detach happens at an arbitrary moment, not at a convenient boundary. The
/// round trip has to hold at every point in a stream, including halfway through
/// a redraw.
#[test]
fn round_trip_holds_at_arbitrary_points_mid_stream() {
    for case in cases() {
        let len = case.input.len();
        if len == 0 {
            continue;
        }
        let cuts = [len / 7, len / 3, len / 2, (len * 4) / 5, len - 1];
        for cut in cuts {
            let mut term = Terminal::new(TerminalConfig::new(case.size));
            term.advance(&case.input[..cut]);
            let snapshot = term.snapshot();
            let restored = replay(&snapshot, term.size());
            assert_screens_identical(
                &format!("{}: snapshot taken after {cut} of {len} bytes", case.name),
                &term.model(),
                &restored.model(),
            );
        }
    }
}

/// Re-snapshotting a restored screen must produce the same bytes. If it does
/// not, the snapshot is lossy in a way the model comparison happens not to see,
/// and the loss compounds across a session's reattaches.
#[test]
fn snapshot_of_a_restored_screen_is_identical() {
    for case in cases() {
        let term = case.play();
        let first = term.snapshot();
        let restored = replay(&first, term.size());
        let second = restored.snapshot();
        assert_eq!(
            first, second,
            "{}: snapshotting a restored screen produced different bytes",
            case.name
        );
    }
}

/// A PTY read boundary falls wherever it falls — in the middle of an escape
/// sequence, a code point, or a grapheme cluster. The screen must depend only
/// on the bytes, never on how they were delivered.
#[test]
fn screens_do_not_depend_on_chunk_boundaries() {
    for case in cases() {
        let whole = case.play().model();
        for chunk in [1usize, 2, 3, 7, 64, 997] {
            let chunked = case.play_chunked(chunk).model();
            assert_screens_identical(
                &format!("{}: fed in {chunk}-byte chunks", case.name),
                &whole,
                &chunked,
            );
        }
    }
}

/// The snapshot is bytes on a wire, so it must be bytes a terminal can accept:
/// a snapshot that only works because the receiving emulator is the same
/// implementation is not a snapshot, it is a memory dump.
#[test]
fn snapshot_is_not_empty_for_a_screen_with_content() {
    let case = support::case("unicode-wide-combining-emoji");
    let term = case.play();
    assert!(
        !term.snapshot().is_empty(),
        "a screen with content must not snapshot to nothing"
    );
}

proptest! {
    /// The emulator is fed by a child process that can emit anything, including
    /// a wedged program spraying binary. A panic here kills the worker, and the
    /// worker owns the agent's PTY.
    #[test]
    fn arbitrary_bytes_never_panic_the_emulator(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let mut term = Terminal::new(TerminalConfig::new(Size::new(80, 24)));
        term.advance(&bytes);
        let _ = term.model();
        let _ = term.snapshot();
    }

    /// The round trip is a property of the emulator, not of the fixture set.
    /// Bytes weighted towards escape sequences reach states no recorded case
    /// happens to contain.
    #[test]
    fn arbitrary_streams_round_trip(chunks in proptest::collection::vec(escape_ish(), 0..64)) {
        let mut term = Terminal::new(TerminalConfig::new(Size::new(40, 12)));
        for chunk in &chunks {
            term.advance(chunk);
        }
        let restored = replay(&term.snapshot(), term.size());
        prop_assert_eq!(term.model(), restored.model());
    }
}

/// Byte strings biased towards real terminal traffic: escape sequences, mode
/// changes, and text, rather than uniform noise that mostly prints.
fn escape_ish() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(b"\x1b[?1049h".to_vec()),
        Just(b"\x1b[?1049l".to_vec()),
        Just(b"\x1b[?25l".to_vec()),
        Just(b"\x1b[?25h".to_vec()),
        Just(b"\x1b[?2004h".to_vec()),
        Just(b"\x1b[?2004l".to_vec()),
        Just(b"\x1b[2J".to_vec()),
        Just(b"\x1b[H".to_vec()),
        Just(b"\x1b[m".to_vec()),
        Just("\u{4f60}\u{597d}".as_bytes().to_vec()),
        Just("e\u{301}".as_bytes().to_vec()),
        (1u16..40, 1u16..14).prop_map(|(row, col)| format!("\x1b[{row};{col}H").into_bytes()),
        (1u16..14, 1u16..14).prop_map(|(top, bottom)| format!("\x1b[{top};{bottom}r").into_bytes()),
        (0u8..108).prop_map(|sgr| format!("\x1b[{sgr}m").into_bytes()),
        "[ -~]{0,40}".prop_map(String::into_bytes),
        proptest::collection::vec(any::<u8>(), 0..32),
    ]
}
