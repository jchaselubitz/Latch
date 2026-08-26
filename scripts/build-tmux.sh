#!/usr/bin/env bash
# Build the patched Latch session kernel (`latch-tmux`).
#
# Usage: scripts/build-tmux.sh <output-binary> [work-dir]
#
# Downloads the pinned tmux source, verifies its checksum, applies every Latch
# patch listed in patches/tmux/manifest.json with zero fuzz, builds statically
# against libevent and utf8proc, then verifies the resulting binary advertises
# the exact Latch raw-attach capability. This is the single entry point used by
# local release builds, CI, and the Phase 0 conformance harness, so a kernel
# that ships and a kernel that is tested are built the same way.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
manifest="$repo_root/patches/tmux/manifest.json"

output="${1:-}"
if [[ -z "$output" ]]; then
  echo "usage: scripts/build-tmux.sh <output-binary> [work-dir]" >&2
  exit 1
fi
mkdir -p "$(dirname "$output")"
output="$(cd "$(dirname "$output")" && pwd -P)/$(basename "$output")"

work="${2:-$repo_root/dist/.tmux-build}"
mkdir -p "$work"
work="$(cd "$work" && pwd -P)"

read_manifest() {
  /usr/bin/env python3 -c 'import json,sys;d=json.load(open(sys.argv[1]))
for k in sys.argv[2].split("."): d = d[int(k)] if k.isdigit() else d[k]
print(d)' "$manifest" "$1"
}

tmux_version="$(read_manifest upstream.version)"
tmux_url="$(read_manifest upstream.url)"
tmux_sha256="$(read_manifest upstream.sha256)"
utf8proc_version="$(read_manifest utf8proc.version)"
utf8proc_url="$(read_manifest utf8proc.url)"
utf8proc_sha256="$(read_manifest utf8proc.sha256)"
capability="$(read_manifest latch.capability)"

fetch() {
  local url="$1" dest="$2" sha="$3"
  if [[ ! -f "$dest" ]]; then
    curl --fail --silent --show-error --location --proto '=https' "$url" -o "$dest.part"
    mv "$dest.part" "$dest"
  fi
  printf '%s  %s\n' "$sha" "$dest" | shasum -a 256 -c - >/dev/null
}

tmux_source="$work/tmux-$tmux_version.tar.gz"
utf8proc_source="$work/utf8proc-$utf8proc_version.tar.gz"
tmux_build="$work/tmux-src"
utf8proc_build="$work/utf8proc-src"

fetch "$tmux_url" "$tmux_source" "$tmux_sha256"
fetch "$utf8proc_url" "$utf8proc_source" "$utf8proc_sha256"

rm -rf "$tmux_build"
mkdir -p "$tmux_build"
tar -xzf "$tmux_source" -C "$tmux_build" --strip-components=1

# Apply every Latch patch. --no-backup-if-mismatch plus a zero fuzz factor makes
# a rebase-needed patch a hard build failure instead of a silently shifted hunk.
patch_count="$(/usr/bin/env python3 -c 'import json;print(len(json.load(open("'"$manifest"'"))["latch"]["patches"]))')"
for (( i = 0; i < patch_count; i++ )); do
  patch_file="$(read_manifest "latch.patches.$i.file")"
  patch_sha="$(read_manifest "latch.patches.$i.sha256")"
  patch_path="$repo_root/patches/tmux/$patch_file"
  printf '%s  %s\n' "$patch_sha" "$patch_path" | shasum -a 256 -c - >/dev/null
  ( cd "$tmux_build" && patch -p1 -F 0 --no-backup-if-mismatch < "$patch_path" )
done

rm -rf "$utf8proc_build"
mkdir -p "$utf8proc_build"
tar -xzf "$utf8proc_source" -C "$utf8proc_build" --strip-components=1
make -C "$utf8proc_build" >/dev/null

libevent_prefix="$(brew --prefix libevent)"

# macOS's <sys/queue.h> has no TAILQ_REPLACE, so tmux's configure clears
# HAVE_QUEUE_H and compat.h falls back to the bundled OpenBSD compat/queue.h --
# while <libproc.h>, libevent's <event.h>, and friends have already dragged the
# SDK header in. compat/queue.h carries its own include guard and no #undefs, so
# every macro the two headers share is redefined. The layouts are identical and
# tmux ships this way on every macOS build; the noise is not actionable.
#
# tmux already knows this: Makefile.am suppresses exactly these three classes on
# Darwin, but only under `if IS_DEBUG`, which a release configure never enters.
# Pass the same set through CFLAGS -- empty in a release build, and appended
# after AM_CFLAGS -- so the flags upstream considers correct for Darwin also
# apply to the kernel we ship. Anything outside these three classes still warns.
tmux_quiet_cflags="-Wno-macro-redefined -Wno-pointer-sign -Wno-deprecated-declarations"

(
  cd "$tmux_build"
  CFLAGS="$tmux_quiet_cflags" \
  PKG_CONFIG_PATH="$libevent_prefix/lib/pkgconfig" \
  LDFLAGS="-L$libevent_prefix/lib" \
  LIBEVENT_CORE_CFLAGS="-I$libevent_prefix/include" \
  LIBEVENT_CORE_LIBS="$libevent_prefix/lib/libevent_core.a" \
  LIBEVENT_CFLAGS="-I$libevent_prefix/include" \
  LIBEVENT_LIBS="$libevent_prefix/lib/libevent.a" \
  LIBUTF8PROC_CFLAGS="-I$utf8proc_build" \
  LIBUTF8PROC_LIBS="$utf8proc_build/libutf8proc.a" \
  ./configure --enable-utf8proc >/dev/null
  make -j"$(sysctl -n hw.ncpu)" >/dev/null
)

# Replace rather than overwrite. Writing over an existing binary keeps its
# inode, so on macOS the old code signature stays attached to new bytes and the
# kernel kills the process on exec -- which looks exactly like a build that
# produced a broken tmux.
rm -f "$output"
cp "$tmux_build/tmux" "$output"

# Capability verification, not a filename or version check. `-R` is the
# wire-level raw-attach flag: the patched kernel accepts it during client
# identification, upstream tmux rejects it as an unknown option.
"$output" -V | grep -Fx "tmux $tmux_version" >/dev/null || {
  echo "built kernel is not tmux $tmux_version" >&2; exit 1; }
"$output" -R -V >/dev/null 2>&1 || {
  echo "built kernel does not advertise $capability" >&2; exit 1; }
if otool -L "$output" 2>/dev/null | grep -Fq "$libevent_prefix"; then
  echo "built kernel still links to a Homebrew build dependency" >&2
  exit 1
fi

printf '%s\n' "$output"
