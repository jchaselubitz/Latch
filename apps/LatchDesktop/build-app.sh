#!/bin/bash
set -euo pipefail

desktop_dir="$(cd "$(dirname "$0")" && pwd)"
repo_dir="$(cd "$desktop_dir/../.." && pwd)"

# SwiftUI executable packages require the full Xcode toolchain. With only the
# Command Line Tools selected, SwiftPM currently reports a misleading generic
# "Unknown error parsing property list" while initializing its build system.
if ! xcodebuild -version >/dev/null 2>&1; then
    cat >&2 <<'EOF'
Latch Desktop requires the full Xcode toolchain, but the active developer
directory is Command Line Tools (or Xcode is not installed).

Install Xcode, complete its first-launch setup, then select it with:
  sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
EOF
    exit 1
fi

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
    codesign --force --options runtime --timestamp \
        --sign "$LATCH_CODESIGN_IDENTITY" "$app_dir"
    codesign --verify --strict --verbose=2 "$contents_dir/MacOS/latch"
    codesign --verify --strict --verbose=2 "$app_dir"
fi

if [[ -n "${LATCH_NOTARY_PROFILE:-}" ]]; then
    if [[ -z "${LATCH_CODESIGN_IDENTITY:-}" ]]; then
        echo "LATCH_NOTARY_PROFILE requires LATCH_CODESIGN_IDENTITY" >&2
        exit 1
    fi

    # notarytool accepts archives, not bare app bundles.  Recreate the archive
    # after stapling so the distributed copy includes the notarization ticket.
    archive_path="${LATCH_APP_ARCHIVE:-$repo_dir/dist/Latch-macos.zip}"
    mkdir -p "$(dirname "$archive_path")"
    rm -f -- "$archive_path"
    ditto -c -k --keepParent "$app_dir" "$archive_path"
    xcrun notarytool submit "$archive_path" \
        --keychain-profile "$LATCH_NOTARY_PROFILE" --wait
    xcrun stapler staple "$app_dir"
    xcrun stapler validate "$app_dir"
    codesign --verify --strict --verbose=2 "$app_dir"
    spctl --assess --type execute --verbose=4 "$app_dir"
    rm -f -- "$archive_path"
    ditto -c -k --keepParent "$app_dir" "$archive_path"
    echo "$archive_path"
fi

echo "$app_dir"
