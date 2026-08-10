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

# Build both slices explicitly so the one desktop download runs natively on
# Apple Silicon and Intel. The CLI is a separate release asset and is never
# copied into the application bundle. SwiftPM uses one default build directory
# regardless of --arch, so the second build otherwise replaces the first slice
# and lipo receives two copies of the host architecture.
arm64_build_dir="$desktop_dir/.build/arm64-release"
x86_64_build_dir="$desktop_dir/.build/x86_64-release"
rm -rf -- "$arm64_build_dir" "$x86_64_build_dir"

swift build --package-path "$desktop_dir" -c release --arch arm64 --scratch-path "$arm64_build_dir"
swift build --package-path "$desktop_dir" -c release --arch x86_64 --scratch-path "$x86_64_build_dir"

arm64_bin_dir="$(swift build --package-path "$desktop_dir" -c release --arch arm64 --scratch-path "$arm64_build_dir" --show-bin-path)"
x86_64_bin_dir="$(swift build --package-path "$desktop_dir" -c release --arch x86_64 --scratch-path "$x86_64_build_dir" --show-bin-path)"
app_dir="$desktop_dir/.build/release/Latch.app"
contents_dir="$app_dir/Contents"

rm -rf -- "$app_dir"
mkdir -p "$contents_dir/MacOS" "$contents_dir/Resources"
lipo -create \
    "$arm64_bin_dir/LatchDesktop" \
    "$x86_64_bin_dir/LatchDesktop" \
    -output "$contents_dir/MacOS/LatchDesktop"
lipo -verify_arch arm64 x86_64 "$contents_dir/MacOS/LatchDesktop"
chmod 0755 "$contents_dir/MacOS/LatchDesktop"
install -m 0644 "$desktop_dir/Info.plist" "$contents_dir/Info.plist"

# The app's in-place updater compares CFBundleShortVersionString with the
# newest published release, so a bundle that ships the placeholder version in
# the source plist would offer itself an update forever. Stamp the workspace
# version in before signing — Info.plist is covered by the signature.
version="$(sed -nE 's/^version = "([0-9]+\.[0-9]+\.[0-9]+)"$/\1/p' "$repo_dir/Cargo.toml")"
if [[ -z "$version" ]]; then
    echo "Could not read the workspace version from Cargo.toml" >&2
    exit 1
fi
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$contents_dir/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $version" "$contents_dir/Info.plist"

if [[ -n "${LATCH_CODESIGN_IDENTITY:-}" ]]; then
    codesign --force --options runtime --timestamp \
        --sign "$LATCH_CODESIGN_IDENTITY" "$app_dir"
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
