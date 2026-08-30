bump-minor:
    ./scripts/bump-minor-version.sh

test-install:
    cargo install --path crates/latch --root /tmp/latch-test --force
    /tmp/latch-test/bin/latch --help

release-cli target="":
    ./scripts/release-cli.sh {{target}}

release-desktop:
    ./scripts/release-desktop.sh

build-debug:
    cargo build --package latch

build-release:
    cargo build --package latch --release

test:
    cargo test --workspace

# The kernel's adversarial suite plus lints: run before merging anything
# under crates/latchd. See docs/LATCHD_SECURITY.md.
security-latchd:
    cargo clippy -p latchd --all-targets -- -D warnings
    cargo test -p latchd

check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    just check-web

# The SDK packages. `npm ci` needs a lockfile, so `npm install` is the
# first-run path; both are cheap next to the Rust build.
check-web:
    npm install --no-audit --no-fund
    npm run typecheck
    npm run build
    npm test
