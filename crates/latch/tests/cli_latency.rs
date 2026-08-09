//! Startup latency budgets.
//!
//! Every terminal window pays CLI startup cost when the iTerm profile points at
//! `latch`. Single-digit milliseconds is the product requirement (decision D1);
//! these budgets fail the build if a heavy dependency or a synchronous scan
//! sneaks onto the hot path.

mod support;

use std::process::Command;
use std::time::{Duration, Instant};

use support::{latch_cmd, worker_binary, Harness, EMPTY_LIST_BUDGET, VERSION_BUDGET};

#[test]
fn version_returns_within_budget() {
    // Warm the page cache, then measure. Cold-start is OS-dependent and not the
    // product claim — the claim is that once the binary is warm, every new
    // window feels instant.
    let _ = Command::new(worker_binary())
        .arg("--version")
        .output()
        .unwrap();

    let mut samples = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        let output = Command::new(worker_binary())
            .arg("--version")
            .output()
            .unwrap();
        let elapsed = started.elapsed();
        assert!(output.status.success(), "latch --version must succeed");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains("latch") || text.contains(env!("CARGO_PKG_VERSION")),
            "version output should name the product: {text}"
        );
        samples.push(elapsed);
    }

    let best = samples.iter().min().copied().unwrap();
    assert!(
        best <= VERSION_BUDGET,
        "latch --version best-of-5 was {best:?}, budget is {VERSION_BUDGET:?}; \
         every new terminal window pays this cost"
    );
}

#[test]
fn empty_list_returns_within_budget() {
    let harness = Harness::new();
    // Warm.
    let _ = latch_cmd(&harness, &["list", "--json"]);

    let mut samples = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        let output = latch_cmd(&harness, &["list", "--json"]);
        let elapsed = started.elapsed();
        samples.push((elapsed, output));
    }

    let (best, output) = samples
        .into_iter()
        .min_by_key(|(elapsed, _)| *elapsed)
        .unwrap();

    // Until list is implemented the command fails fast with "lands in M1",
    // which still has to be within budget — a slow failure is still a hitch on
    // every window that runs `latch list`.
    assert!(
        best <= EMPTY_LIST_BUDGET,
        "latch list against an empty home was {best:?}, budget is {EMPTY_LIST_BUDGET:?}; \
         stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn version_does_not_touch_the_latch_home() {
    // `--version` must not create `~/.latch` or scan sessions. Point HOME at a
    // path that does not exist and require success anyway.
    let missing = tempfile::tempdir().unwrap().path().join("no-such-home");
    let output = Command::new(worker_binary())
        .arg("--version")
        .env("HOME", &missing)
        .env_remove(latch::worker::paths::HOME_ENV)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "latch --version must not require a home directory; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !missing.exists(),
        "latch --version must not create a home directory as a side effect"
    );
}

#[allow(dead_code)]
fn _duration() -> Duration {
    Duration::from_millis(1)
}
