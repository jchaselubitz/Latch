#!/usr/bin/env bash
# Verify the coordinated three-binary CLI archive produced by release-cli.sh.
set -euo pipefail

archive="${1:?usage: scripts/test-release-archive.sh ARCHIVE TARGET}"
target="${2:?usage: scripts/test-release-archive.sh ARCHIVE TARGET}"
version="$(sed -nE 's/^version = "([0-9]+\.[0-9]+\.[0-9]+)"$/\1/p' Cargo.toml)"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/latch-release-test.XXXXXX")"
trap 'rm -rf -- "$work_dir"' EXIT

unzip -Z1 "$archive" | LC_ALL=C sort > "$work_dir/members"
printf '%s\n' latch latch-payload.json latch-remote latchd > "$work_dir/expected"
cmp "$work_dir/expected" "$work_dir/members"

ditto -x -k "$archive" "$work_dir/payload"
/usr/bin/python3 -c 'import json,sys; p=json.load(open(sys.argv[1])); assert p == {"formatVersion":1,"version":sys.argv[2],"target":sys.argv[3],"binaries":["latch","latch-remote","latchd"]}' \
  "$work_dir/payload/latch-payload.json" "$version" "$target"
"$work_dir/payload/latch" --version | grep -Fx "latch $version"
"$work_dir/payload/latch-remote" --version | grep -Fx "latch-remote $version"
"$work_dir/payload/latchd" version | grep -Fx "latchd $version protocol 1"
printf 'verified three-binary release payload %s for %s\n' "$version" "$target"
