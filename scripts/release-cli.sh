#!/usr/bin/env bash
# Build one distributable, optionally notarized macOS Latch CLI archive.
#
# Usage: scripts/release-cli.sh [target]
#
# Set LATCH_CODESIGN_IDENTITY to sign the binary before it is archived. Set
# LATCH_NOTARY_PROFILE to submit the signed ZIP with notarytool. CI sets both
# after importing the Developer ID certificate into a temporary keychain.
set -euo pipefail

target="${1:-}"
if [[ -z "$target" ]]; then
  case "$(uname -m)" in
    arm64) target="aarch64-apple-darwin" ;;
    x86_64) target="x86_64-apple-darwin" ;;
    *) echo "Unsupported host architecture: $(uname -m)" >&2; exit 1 ;;
  esac
fi

case "$target" in
  aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) echo "Unsupported release target: $target" >&2; exit 1 ;;
esac

version="$(sed -nE 's/^version = "([0-9]+\.[0-9]+\.[0-9]+)"$/\1/p' Cargo.toml)"
if [[ -z "$version" ]]; then
  echo "Could not read the workspace version from Cargo.toml" >&2
  exit 1
fi

if [[ -n "${LATCH_RELEASE_TAG:-}" && "$LATCH_RELEASE_TAG" != "v$version" ]]; then
  echo "Release tag $LATCH_RELEASE_TAG does not match workspace version v$version" >&2
  exit 1
fi

output_dir="${LATCH_RELEASE_DIR:-dist}"
archive_name="latch-${version}-${target}.zip"
archive_path="$output_dir/$archive_name"
stage_dir="$output_dir/.stage-$target"

cargo build --locked --release --package latch --target "$target"

rm -rf "$stage_dir"
mkdir -p "$stage_dir"
cp "target/$target/release/latch" "$stage_dir/latch"

if [[ -n "${LATCH_CODESIGN_IDENTITY:-}" ]]; then
  codesign --force --options runtime --timestamp --sign "$LATCH_CODESIGN_IDENTITY" "$stage_dir/latch"
  codesign --verify --strict --verbose=2 "$stage_dir/latch"
fi

mkdir -p "$output_dir"
rm -f "$archive_path"
(cd "$stage_dir" && /usr/bin/zip -q -X "$archive_path" latch)

if [[ -n "${LATCH_NOTARY_PROFILE:-}" ]]; then
  : "${LATCH_CODESIGN_IDENTITY:?LATCH_NOTARY_PROFILE requires LATCH_CODESIGN_IDENTITY}"
  xcrun notarytool submit "$archive_path" --keychain-profile "$LATCH_NOTARY_PROFILE" --wait
fi

rm -rf "$stage_dir"

shasum -a 256 "$archive_path" > "$archive_path.sha256"
printf '%s\n' "$archive_path"
