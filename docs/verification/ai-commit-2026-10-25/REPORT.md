# AI commit-message verification — coo:1215.0vh2

25 October 2026. Local verification is complete within the available configuration.
**Live Gemini, signed helper Keychain access, and physical-phone acceptance remain
unverified. This is not an unconditional release sign-off.** No Gemini key was
configured or transmitted. No changes were staged or committed in the Latch checkout;
Git mutations below used disposable repositories only.

## Defects fixed

The new mobile wire test reproduced a schema mismatch: synthesized Swift Codable
omitted nil `branch` and `head`, while the draft schema requires explicit nulls.
This affects unborn and detached sources. The schema-driven generator now emits
explicit encoders for required nullable fields, including draft expected/source,
response body, failure correlation and omission paths. Regenerated Swift and its
manifest pass drift checks. The wire regression checks actual URLSession JSON.

Provider parsing also enforces its 32 KiB input bound directly, in addition to the
existing streaming transport bound, and rejects a missing `body` key instead of
silently treating it as null. An explicit null remains valid; the malformed-output
fixture covers the missing-field case. Cancellation/deadline handling was extracted
without changing its 65-second production deadline so tests can prove pending
work is dropped and admission is released. A collector boundary callback is empty
in production and permits deterministic external-writer race fixtures.

## Automated evidence

| Check | Result |
| --- | --- |
| Full mobile package, final generated contract | **PASS:** 451 tests, 1 skipped, 0 failures; includes 9 draft model/wire tests |
| Full relevant Git suite, final collector | **PASS:** 128 tests |
| Focused draft/config/provider/HTTP/proxy/capture tests | **PASS:** 21 tests |
| Paired proxy/device authority suite | **PASS:** 15 tests, including active draft-stream revocation |
| Required serial daemon parity gate | **PASS:** 16 tests |
| Rust workspace excluding terminal package, with parallel-flaky parity case run separately | **PASS:** library 421 passed/2 ignored; remaining integration, transport, remote-helper, FFI and daemon targets pass |
| Full unfiltered Rust workspace | **FAIL:** unrelated terminal snapshot property test; a later run also encountered a parallel daemon exit-reason race, described below |
| Remaining terminal scrollback and VT fidelity targets | **PASS:** 10 and 14 tests |
| Workspace Clippy with warnings denied; workspace formatting | **PASS** |
| Rust workspace doc tests | **PASS** |
| Canonical and mobile contract generation checks; architecture and Remote Link policy checks | **PASS** |
| TypeScript workspace typecheck and client tests | **PASS:** 19 tests; locked dependencies installed with `npm ci --ignore-scripts` |
| Root production npm audit | **PASS:** 0 vulnerabilities |
| Rust dependency audit | **FAIL:** existing crypto dependency advisory plus unsound/unmaintained warnings, detailed below |
| Normal iOS app, generic simulator arm64/x86_64 build after contract fix | **PASS** |
| Real built gateway / disposable Git HTTP script | **PASS:** 79 requests, [recorded results](live-gateway.json) |

The final collector race test was added after the full workspace run. Its focused
21-test run and the complete 128-test Git suite were rerun afterward; Clippy and
contract/static checks also pass on the final source.

### Coverage

- Staged-only patches, partial staging, untracked exclusion, unborn and empty
  indexes, intent-to-add, conflicts, linked worktrees, split and sparse indexes, SHA-256,
  rename/delete/mode/binary/submodule metadata.
- Uneven file sizes, distributed patch budgets, full-patch scan limits and file
  count limits; sensitive-path and credential omissions with anonymous paths;
  refusal when no safe context remains. Filtering remains conservative and
  incomplete, as disclosed in the UI.
- Deterministic branch/HEAD/index changes between copying and validating the
  captured index; private-index isolation; changes during provider generation;
  pre-disclosure stale refusal and post-provider stale rejection.
- Byte-for-byte Git directory preservation, including index, refs and objects,
  after context collection with hostile external diff/textconv/fsmonitor settings
  and a pre-commit hook. No hook marker is created.
- Invalid/missing provider envelopes, blocked/truncated candidates, tool calls,
  code fences, invalid subjects/bodies, detected credentials, oversized output.
  Prompt policy treats staged data as untrusted and forbids invented changes.
  This does not prove semantic truthfulness of arbitrary model output.
- Local fake HTTP transport verifies the API-key header and request shape,
  redirect refusal, response bounds, invalid-key/429 responses, timeout and
  disconnected provider errors. Production destination, TLS, proxy/redirect
  policy remain fixed. No live TLS/provider inference is made from this fixture.
- Control authorization at HTTP and paired proxy boundaries, revoked active
  streams, malformed/oversized requests, cancellation authorization and device
  scoping, per-device/global/window/table limits, dropped pending work at timeout
  and cancellation, and admission cleanup.
- Mobile newer-typing protection, repeated taps, cancellation then regeneration,
  dismissed late responses, mismatched response/request identity, root/branch/
  HEAD/index changes, failed refresh, offline and revoked access, typed failures,
  original source retention through successive manual reviews, older gateway
  refusal before sending and support independent of readiness.
- Real HTTP manual commits, stale-index protection, hooks that try to stage
  unreviewed content, duplicate request receipts, receipt lookup, branch/update/
  push regressions. The HTTP harness also checks its gateway log for fixture
  source, repository root and bearer-token leakage. Provider/config errors are
  sanitized; raw requests and provider errors have no feature logging call sites.

The transport fixture does not exhaust every HTTP framing variant. every credential format, every possible writer interleaving, provider semantic
truthfulness and interactive accessibility are not claimed exhaustively verified.

### Reproduction commands

Run loopback/PTY-dependent commands with ordinary local development permissions.
All Git test repositories are disposable.

```sh
cargo build --locked -p latchd -p latch -p latch-remote
cargo test --locked --workspace --all-targets
cargo test --locked -p latch --test latchd_kernel_e2e -- --test-threads=1
cargo test --locked --workspace --all-targets --exclude latch-term -- --skip a_pane_that_exits_releases_its_surface_with_session_exited
cargo test --locked -p latch --lib cli::serve::git::
cargo test --locked -p latch --lib draft_
cargo test --locked -p latch --lib remote_access::devices::tests
cargo test --locked -p latch-term --test vt_fidelity --test scrollback
cargo test --locked --workspace --doc
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
python3 scripts/generate-remote-access-types.py --check
python3 apps/LatchMobile/Tools/generate-contract.py --check --upstream .
./scripts/check-boundaries.sh
./scripts/check-remote-link-contract.sh
npm ci --ignore-scripts
npm run typecheck
npm test
swift test --package-path apps/LatchMobile --build-system native --scratch-path /private/tmp/latch-ai-native-tests --disable-sandbox
xcodebuild -project apps/LatchMobile/App/LatchMobile.xcodeproj -scheme LatchMobile -destination 'generic/platform=iOS Simulator' -derivedDataPath /private/tmp/latch-ai-mobile-build CODE_SIGNING_ALLOWED=NO build
python3 docs/verification/ai-commit-2026-10-25/live_gateway.py
```

The HTTP script expects no saved Gemini credential. It refuses a configured
machine rather than replacing a credential. Its model override uses a syntactic
fixture identifier in an isolated Latch home: this proves local reload only,
**not** that Google supports that identifier. The gateway is stopped afterward.
`--keep` retains a disposable gateway intentionally; runtime bearer material is
written only to its private temporary fixture, never to repository evidence.

## Unrelated failures and infrastructure

`latch-term`'s `snapshot_roundtrip::arbitrary_streams_round_trip` fails on a
generated terminal stream involving scroll-region/cursor state. The full run
reported 11 other snapshot tests passing. This terminal implementation was not
changed for AI drafting. The broad run therefore cannot be reported green.

The later parallel workspace invocation failed
`a_pane_that_exits_releases_its_surface_with_session_exited` at
`crates/latch/tests/latchd_kernel_e2e.rs:706`. The repository's explicitly required
serial parity command passes all 16 cases. The remaining workspace run skipped
that one already-serial-verified case and passed; this is not a claim that the
parallel exit-reason race was fixed.

Initial sandboxed HTTP/PTY tests failed on permissions. The sandboxed mobile suite
crashed in `AppModelRecoveryTests` after a loopback-dependent assertion. A normal
permission rerun then stalled in SwiftBuild's `clang-stat-cache` for the macOS SDK.
The native SwiftPM backend completed the full suite; the default backend is not
claimed passing. It emits a deprecation notice and existing concurrency warnings.
The normal Xcode iOS build passes. The existing transport XCFramework was used;
it was not rebuilt because this feature changes no FFI or transport implementation.
No Linux or Rust-1.85 toolchain run was performed locally.

The refreshed Rust advisory database flags `ctutils 0.4.3`
([RUSTSEC-2026-0385](https://rustsec.org/advisories/RUSTSEC-2026-0385.html)); patched
versions start at 0.4.4. `cargo tree -i ctutils` traces it through existing HPKE and
WebSocket transport crypto dependencies, rather than the Gemini HTTP client.
The audit also warns about Tokio 1.53.1
([RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386.html), patched
in 1.53.3 or the listed backport range) and unmaintained `basic-toml 0.1.10`
([RUSTSEC-2026-0374](https://rustsec.org/advisories/RUSTSEC-2026-0374.html)). No
dependency lockfile was changed by this verification objective. These existing
workspace dependency findings need separate remediation; this report does not
claim the repository's dependency-security gate passes. Initial sandbox audit
attempts lacked advisory-cache/network access; normal-permission reruns produced
these findings. Root npm production audit reports zero vulnerabilities; separate
service production lockfiles were not audited in this feature verification.

## Simulator, accessibility and physical device evidence

iOS 27 / iPhone 17 Pro simulator. A separate private copy of the app used a
distinct `dev.cooperativ.latch.ai-verification` bundle identifier and
[fake-data harness](simulator_harness.swift.txt). The real production Commit sheet
was rendered with a supplied draft model; the copy bypassed pairing and made no
provider or Git mutation request. No harness entrypoint was added to the repository
app, and its normal build was rerun afterward.

- [Standard light rendering](generated.png): populated editable summary/body,
  Regenerate, review notice and a separate explicit Commit action.
- [Largest accessibility text, dark rendering](stale-ax5.png): large editable text
  wraps readably; the screen uses scrolling with the action in list content.
  The capture shows only the visible portion, not all controls below the fold.

Desktop UI automation stalled. No claim is made for interactive scrolling,
button taps, first-use disclosure interaction, VoiceOver focus/announcements,
keyboard interruption, swipe dismissal, iPad layout or physical-device operation.
Lifecycle/typing behavior is established by automated model tests, not screenshots.
Xcode enumerated paired iPhone/iPad devices, but no app was installed on them.

## Live provider / operational release limitations

The built CLI's noninteractive `commit-message status` reports **unconfigured**.
The installed `/Users/jake/.local/bin/latch` rejects `commit-message` as an unknown
subcommand, so it predates this feature. No user credential was set, replaced,
removed, copied from another application or printed. The helper's selected CLI
execution chain is source-inspected; actual credential access by a signed selected
release executable under the Desktop helper is not established.

Consequently, credential-scoped availability of `gemini-3.5-flash-lite`, a real
supported override, invalid live credentials, Google TLS/network behavior, key
replacement/removal ACLs and the full phone → Mac → Gemini → editable draft →
explicit disposable commit flow remain acceptance gaps. No fallback model was used.

Before release, select a feature-capable CLI in Latch Desktop, configure the
already-authorized Gemini project using that exact executable and the documented
hidden/stdin setup, then verify its noninteractive readiness through the actual
helper. With a disposable staged repository, explicitly generate on the phone,
review/edit, commit, and confirm unstaged content remains excluded. Verify the
default model against that project's provider response, then set an actually
supported model override on the Mac and repeat without updating mobile. Exercise
key replacement/removal and VoiceOver/dismissal on that same operational setup.
See [setup and disclosure documentation](../../AI_COMMIT_MESSAGES.md).
