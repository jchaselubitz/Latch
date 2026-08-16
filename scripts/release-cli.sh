#!/usr/bin/env bash
# Build the distributable Latch payload: CLI plus pinned private tmux.
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
tmux_version="3.7b"
tmux_sha256="87f2e99e3b685973f2ca002ffd6ed7e51a5744f7009daae5a15670b6d532db96"
tmux_source="$output_dir/tmux-$tmux_version.tar.gz"
tmux_build="$output_dir/.tmux-$target"
utf8proc_version="2.11.3"
utf8proc_sha256="abfed50b6d4da51345713661370290f4f4747263ee73dc90356299dfc7990c78"
utf8proc_source="$output_dir/utf8proc-$utf8proc_version.tar.gz"
utf8proc_build="$output_dir/.utf8proc-$target"

cargo build --locked --release --package latch --package latch-remote --target "$target"

rm -rf "$stage_dir"
mkdir -p "$stage_dir"
cp "target/$target/release/latch" "$stage_dir/latch"
cp "target/$target/release/latch-remote" "$stage_dir/latch-remote"

curl --fail --silent --show-error --location --proto '=https' \
  "https://github.com/tmux/tmux/releases/download/$tmux_version/tmux-$tmux_version.tar.gz" \
  -o "$tmux_source"
printf '%s  %s\n' "$tmux_sha256" "$tmux_source" | shasum -a 256 -c -
rm -rf "$tmux_build"
mkdir -p "$tmux_build"
tar -xzf "$tmux_source" -C "$tmux_build" --strip-components=1

libevent_prefix="$(brew --prefix libevent)"
curl --fail --silent --show-error --location --proto '=https' \
  "https://github.com/JuliaStrings/utf8proc/archive/refs/tags/v$utf8proc_version.tar.gz" \
  -o "$utf8proc_source"
printf '%s  %s\n' "$utf8proc_sha256" "$utf8proc_source" | shasum -a 256 -c -
rm -rf "$utf8proc_build"
mkdir -p "$utf8proc_build"
tar -xzf "$utf8proc_source" -C "$utf8proc_build" --strip-components=1
make -C "$utf8proc_build"
utf8proc_build="$(cd "$utf8proc_build" && pwd -P)"
utf8proc_include="$utf8proc_build"
utf8proc_library="$utf8proc_build/libutf8proc.a"
(
  cd "$tmux_build"
  PKG_CONFIG_PATH="$libevent_prefix/lib/pkgconfig" \
  LDFLAGS="-L$libevent_prefix/lib" \
  LIBEVENT_CORE_CFLAGS="-I$libevent_prefix/include" \
  LIBEVENT_CORE_LIBS="$libevent_prefix/lib/libevent_core.a" \
  LIBEVENT_CFLAGS="-I$libevent_prefix/include" \
  LIBEVENT_LIBS="$libevent_prefix/lib/libevent.a" \
  LIBUTF8PROC_CFLAGS="-I$utf8proc_include" \
  LIBUTF8PROC_LIBS="$utf8proc_library" \
  ./configure --enable-utf8proc
  make -j"$(sysctl -n hw.ncpu)"
)
cp "$tmux_build/tmux" "$stage_dir/latch-tmux"
"$stage_dir/latch-tmux" -V | grep -Fx "tmux $tmux_version"
if otool -L "$stage_dir/latch-tmux" | grep -Fq "$libevent_prefix"; then
  echo "Vendored tmux still links to a Homebrew build dependency" >&2
  exit 1
fi

if [[ -n "${LATCH_CODESIGN_IDENTITY:-}" ]]; then
  codesign --force --options runtime --timestamp --sign "$LATCH_CODESIGN_IDENTITY" "$stage_dir/latch"
  codesign --force --options runtime --timestamp --sign "$LATCH_CODESIGN_IDENTITY" "$stage_dir/latch-remote"
  codesign --force --options runtime --timestamp --sign "$LATCH_CODESIGN_IDENTITY" "$stage_dir/latch-tmux"
  codesign --verify --strict --verbose=2 "$stage_dir/latch"
  codesign --verify --strict --verbose=2 "$stage_dir/latch-remote"
  codesign --verify --strict --verbose=2 "$stage_dir/latch-tmux"
fi

mkdir -p "$output_dir"
archive_path="$(cd "$output_dir" && pwd -P)/$archive_name"
rm -f "$archive_path"
(cd "$stage_dir" && /usr/bin/zip -q -X "$archive_path" latch latch-remote latch-tmux)

if [[ -n "${LATCH_NOTARY_PROFILE:-}" ]]; then
  : "${LATCH_CODESIGN_IDENTITY:?LATCH_NOTARY_PROFILE requires LATCH_CODESIGN_IDENTITY}"
  xcrun notarytool submit "$archive_path" --keychain-profile "$LATCH_NOTARY_PROFILE" --wait
fi

rm -rf "$stage_dir" "$tmux_build" "$tmux_source" "$utf8proc_build" "$utf8proc_source"

checksum="$(shasum -a 256 "$archive_path" | awk '{print $1}')"
printf '%s  %s\n' "$checksum" "$archive_name" > "$archive_path.sha256"
printf '%s\n' "$archive_path"
