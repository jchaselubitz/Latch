//! The bounded journal.
//!
//! An agent session left running overnight emits gigabytes. A log that grows
//! without limit fills the user's disk with terminal output nobody asked to
//! keep, so the cap is the feature and the bytes are the detail.
//!
//! The cap is enforced oldest-first because the recent tail is the part anyone
//! wants — a journal that discarded the newest bytes would satisfy the bound and
//! be useless. And it is owner-only for the same reason the socket is: it holds
//! whatever the child printed, which for an agent session includes whatever the
//! agent was shown.

mod support;

use latch::worker::journal::{
    Journal, JournalConfig, JournalError, DEFAULT_JOURNAL_CAP_BYTES, MIN_JOURNAL_CAP_BYTES,
};
use latch::worker::manifest::{TerminalSize, WorkerTuning};
use latch::worker::paths::FILE_MODE;
use support::{mode_of, sh_manifest, wait_for, Harness, SETTLE};

fn scratch() -> tempfile::TempDir {
    support::relax_umask();
    tempfile::tempdir().expect("cannot create a scratch directory")
}

#[test]
fn the_default_cap_is_ten_megabytes() {
    assert_eq!(
        DEFAULT_JOURNAL_CAP_BYTES,
        10 * 1024 * 1024,
        "the default cap is documented in the implementation plan; it is part of the contract, \
         not an implementation detail to drift"
    );
    assert_eq!(
        JournalConfig::default().cap_bytes,
        DEFAULT_JOURNAL_CAP_BYTES
    );
    assert_eq!(
        WorkerTuning::default().journal_cap_bytes,
        DEFAULT_JOURNAL_CAP_BYTES
    );
}

#[test]
fn the_journal_holds_the_newest_bytes_and_discards_the_oldest() {
    let dir = scratch();
    let path = dir.path().join("journal");
    let cap = MIN_JOURNAL_CAP_BYTES;
    let mut journal =
        Journal::open(&path, JournalConfig { cap_bytes: cap }).expect("the journal should open");

    // Written as recognizable records so the assertion is about *which* bytes
    // survived, not merely how many.
    let record_count = 400usize;
    for n in 0..record_count {
        journal
            .append(format!("[{n:06}]").as_bytes())
            .expect("appending should succeed");
    }
    journal.sync().expect("the journal should flush");

    assert!(
        journal.len_bytes() <= cap,
        "the journal must never exceed its cap: held {} of {cap}",
        journal.len_bytes()
    );
    assert!(
        journal.dropped_bytes() > 0,
        "writing far past the cap must be reported as dropped history, not silently shortened"
    );

    let held = journal.read_all().expect("the journal should be readable");
    let text = String::from_utf8_lossy(&held);
    assert!(
        text.contains(&format!("[{:06}]", record_count - 1)),
        "the newest record must survive; the tail is the part anyone wants"
    );
    assert!(
        !text.contains("[000000]"),
        "the oldest record must be the one discarded"
    );

    let on_disk = std::fs::metadata(&path)
        .expect("the journal file exists")
        .len();
    assert!(
        on_disk <= cap,
        "the cap bounds the file, not just the accounting: {on_disk} bytes on disk"
    );
}

#[test]
fn a_journal_larger_than_a_lowered_cap_is_trimmed_when_it_is_opened() {
    let dir = scratch();
    let path = dir.path().join("journal");
    std::fs::write(&path, vec![b'x'; (MIN_JOURNAL_CAP_BYTES * 4) as usize])
        .expect("cannot pre-write a journal");

    let journal = Journal::open(
        &path,
        JournalConfig {
            cap_bytes: MIN_JOURNAL_CAP_BYTES,
        },
    )
    .expect("the journal should open");

    assert!(
        journal.len_bytes() <= MIN_JOURNAL_CAP_BYTES,
        "a cap lowered between runs must take effect at open, not at whatever append \
         happens to notice"
    );
}

#[test]
fn an_unusable_cap_is_refused_rather_than_rounded() {
    let dir = scratch();
    let err = Journal::open(&dir.path().join("journal"), JournalConfig { cap_bytes: 0 })
        .expect_err("a zero cap must be refused");
    assert!(
        matches!(err, JournalError::CapTooSmall { .. }),
        "a cap of zero would make every append a full truncation: {err}"
    );
}

#[test]
fn the_journal_file_is_owner_only() {
    let dir = scratch();
    let path = dir.path().join("journal");
    let mut journal =
        Journal::open(&path, JournalConfig::default()).expect("the journal should open");
    journal.append(b"hello").expect("appending should succeed");
    journal.sync().expect("the journal should flush");

    assert_eq!(
        mode_of(&path),
        FILE_MODE,
        "terminal output is private; the journal is 0600 whatever the umask says"
    );
}

#[test]
fn the_tail_can_be_read_without_reading_everything() {
    let dir = scratch();
    let mut journal = Journal::open(&dir.path().join("journal"), JournalConfig::default())
        .expect("the journal should open");
    journal
        .append(b"old-old-old-NEWEST")
        .expect("appending should succeed");
    journal.sync().expect("the journal should flush");

    let tail = journal.read_tail(6).expect("the tail should be readable");
    assert_eq!(
        tail,
        b"NEWEST".to_vec(),
        "a tail read returns the end of the journal, which is what a reattaching client wants"
    );
}

#[test]
fn a_live_session_journals_what_its_child_printed() {
    let harness = Harness::new();
    let session = harness.spawn(support::tuned(
        sh_manifest(
            "echo JOURNALLED-MARKER; while :; do sleep 0.2; done",
            TerminalSize::new(200, 50),
        ),
        WorkerTuning::default(),
    ));

    wait_for("the marker to reach the journal", SETTLE, || {
        std::fs::read(session.paths().journal())
            .map(|bytes| String::from_utf8_lossy(&bytes).contains("JOURNALLED-MARKER"))
            .unwrap_or(false)
    });

    assert_eq!(
        mode_of(&session.paths().journal()),
        FILE_MODE,
        "the journal a live worker created is owner-only too"
    );
}
