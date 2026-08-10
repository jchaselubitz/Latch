#!/bin/bash
set -euo pipefail

desktop_dir="$(cd "$(dirname "$0")" && pwd)"
repo_dir="$(cd "$desktop_dir/../.." && pwd)"

swift build --package-path "$desktop_dir" -c release
cargo build --manifest-path "$repo_dir/Cargo.toml" -p latch --release

swift_bin_dir="$(swift build --package-path "$desktop_dir" -c release --show-bin-path)"
app_dir="$desktop_dir/.build/release/Latch.app"
contents_dir="$app_dir/Contents"

rm -rf -- "$app_dir"
mkdir -p "$contents_dir/MacOS" "$contents_dir/Resources"
install -m 0755 "$swift_bin_dir/LatchDesktop" "$contents_dir/MacOS/LatchDesktop"
install -m 0755 "$repo_dir/target/release/latch" "$contents_dir/MacOS/latch"
install -m 0644 "$desktop_dir/Info.plist" "$contents_dir/Info.plist"

if [[ -n "${LATCH_CODESIGN_IDENTITY:-}" ]]; then
    codesign --force --options runtime --timestamp \
        --sign "$LATCH_CODESIGN_IDENTITY" "$contents_dir/MacOS/latch"
    codesign --force --deep --options runtime --timestamp \
        --sign "$LATCH_CODESIGN_IDENTITY" "$app_dir"
fi

echo "$app_dir"
