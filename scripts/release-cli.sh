#!/usr/bin/env bash
# Build one distributable macOS Latch CLI archive.
#
# Usage: scripts/release-cli.sh [target]
#
# Set LATCH_CODESIGN_IDENTITY to sign the binary before it is archived. CI sets
# this after importing the Developer ID certificate into a temporary keychain.
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
archive_name="latch-${version}-${target}.tar.gz"
archive_path="$output_dir/$archive_name"
stage_dir="$output_dir/.stage-$target"

cargo build --locked --release --package latch --target "$target"

rm -rf "$stage_dir"
mkdir -p "$stage_dir"
cp "target/$target/release/latch" "$stage_dir/latch"

if [[ -n "${LATCH_CODESIGN_IDENTITY:-}" ]]; then
  codesign --force --options runtime --timestamp --sign "$LATCH_CODESIGN_IDENTITY" "$stage_dir/latch"
  codesign --verify --deep --strict --verbose=2 "$stage_dir/latch"
fi

mkdir -p "$output_dir"
tar -C "$stage_dir" -czf "$archive_path" latch
rm -rf "$stage_dir"

shasum -a 256 "$archive_path" > "$archive_path.sha256"
printf '%s\n' "$archive_path"
