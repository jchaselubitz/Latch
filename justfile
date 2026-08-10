bump-minor:
    ./scripts/bump-minor-version.sh

test-install:
    cargo install --path crates/latch --root /tmp/latch-test --force
    /tmp/latch-test/bin/latch --help

release-cli target="":
    ./scripts/release-cli.sh {{target}}

build-debug:
    cargo build --package latch

test:
    cargo test --workspace

check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
