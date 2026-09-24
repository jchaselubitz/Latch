//! Security regressions for the public `latch` CLI boundary.
//!
//! The detailed proxy and gateway checks live with their implementation in
//! `src/cli`. This integration target mirrors `latchd/tests/security.rs`: it
//! keeps the externally observable loopback-only invariant available as an
//! independently named CI gate.

use std::process::Command;

#[test]
fn latch_serve_refuses_a_non_loopback_listener() {
    let output = Command::new(env!("CARGO_BIN_EXE_latch"))
        .args(["serve", "--bind", "0.0.0.0:0"])
        .output()
        .expect("run latch serve");

    assert!(
        !output.status.success(),
        "latch serve must never expose its plaintext authority listener"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("only listens on loopback"), "{stderr}");
}
