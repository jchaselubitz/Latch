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
# `-verify_arch` takes one architecture and one input at a time.  Invoking it
# twice makes both required universal slices explicit without asking lipo to
# parse multiple input files.
lipo "$contents_dir/MacOS/LatchDesktop" -verify_arch arm64
lipo "$contents_dir/MacOS/LatchDesktop" -verify_arch x86_64
chmod 0755 "$contents_dir/MacOS/LatchDesktop"
install -m 0644 "$desktop_dir/Info.plist" "$contents_dir/Info.plist"

# Build a complete, native macOS icon set from the approved transparent Latch
# logo.  Supplying every standard representation keeps the Dock, Finder, and
# high-density displays sharp instead of asking macOS to scale a single image.
iconset_dir="$desktop_dir/.build/Latch.iconset"
rm -rf -- "$iconset_dir"
mkdir -p "$iconset_dir"
icon_source="$desktop_dir/Assets/latch-logo-l-beveled-transparent-v1.png"
for icon_size in 16 32 128 256 512; do
    sips --resampleHeightWidth "$icon_size" "$icon_size" "$icon_source" \
        --out "$iconset_dir/icon_${icon_size}x${icon_size}.png" >/dev/null
done
for icon_size in 16 32 128 256 512; do
    doubled_size=$((icon_size * 2))
    sips --resampleHeightWidth "$doubled_size" "$doubled_size" "$icon_source" \
        --out "$iconset_dir/icon_${icon_size}x${icon_size}@2x.png" >/dev/null
done
iconutil --convert icns "$iconset_dir" --output "$contents_dir/Resources/Latch.icns"
rm -rf -- "$iconset_dir"

# These monochrome images are marked as a template at runtime so macOS adapts
# them automatically to light and dark menu bars.
install -m 0644 "$desktop_dir/Assets/latch-menubar-template.png" \
    "$contents_dir/Resources/latch-menubar-template.png"
install -m 0644 "$desktop_dir/Assets/latch-menubar-template@2x.png" \
    "$contents_dir/Resources/latch-menubar-template@2x.png"

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
        --entitlements "$desktop_dir/LatchDesktop.entitlements" \
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
