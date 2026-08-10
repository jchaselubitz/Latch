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

check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
