#!/usr/bin/env bash
# Exercise install-cli.sh's publisher verification: a payload that is not
# signed by the Latch Team ID must be refused, and, when a release payload
# directory is given, that payload must pass.
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
installer="$repo_dir/scripts/install-cli.sh"
release_payload="${1:-}"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/latch-install-verify.XXXXXX")"
trap 'rm -rf -- "$work_dir"' EXIT

expect_refused() {
    local label="$1" directory="$2"
    if "$installer" --verify-payload "$directory" 2>/dev/null; then
        echo "FAIL: $label was accepted" >&2
        exit 1
    fi
    echo "ok: $label refused"
}

# Ad-hoc signed binaries: valid signatures with no publisher at all.
mkdir "$work_dir/ad-hoc"
for binary in latch latch-remote latchd; do
    cp /usr/bin/true "$work_dir/ad-hoc/$binary"
    codesign --force --sign - "$work_dir/ad-hoc/$binary" 2>/dev/null
done
expect_refused "ad-hoc signed payload" "$work_dir/ad-hoc"

# Apple-signed binaries: an Apple anchor, but not the Latch Team ID.
mkdir "$work_dir/apple"
for binary in latch latch-remote latchd; do
    cp /usr/bin/true "$work_dir/apple/$binary"
done
expect_refused "Apple-signed payload from another team" "$work_dir/apple"

expect_refused "missing payload" "$work_dir/missing"

if [[ -n "$release_payload" ]]; then
    "$installer" --verify-payload "$release_payload"
    echo "ok: release payload accepted"

    # One substituted binary is enough to refuse the whole payload.
    mkdir "$work_dir/mixed"
    cp -p "$release_payload/latch" "$release_payload/latch-remote" "$work_dir/mixed/"
    cp -p "$work_dir/ad-hoc/latchd" "$work_dir/mixed/latchd"
    expect_refused "release payload with an ad-hoc latchd" "$work_dir/mixed"

    # The pin, not just a valid Developer ID signature, is what admits the
    # release: the same payload under a different Team ID must be refused.
    sed 's/^latch_team_id=.*/latch_team_id="AAAAAAAAAA"/' "$installer" > "$work_dir/other-team.sh"
    if bash "$work_dir/other-team.sh" --verify-payload "$release_payload" 2>/dev/null; then
        echo "FAIL: release payload accepted under another Team ID" >&2
        exit 1
    fi
    echo "ok: release payload refused under another Team ID"
fi
