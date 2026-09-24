#!/usr/bin/env bash
# Install the newest notarized Latch CLI release into ~/.local/bin.
set -euo pipefail

repository="jchaselubitz/Latch"
# The Apple Developer Team ID that signs every Latch release. It is public (it
# appears in `codesign -dvv` of any shipped binary); the APPLE_TEAM_ID secret in
# .github/workflows/release-cli.yml is the source of truth for the value. A
# checksum only proves the archive matches the release it was downloaded
# with, so this pin is what ties the binaries to the Latch publisher.
latch_team_id="X84RPB4674"
latch_binaries=(latch latch-remote latchd)

# Refuses a binary unless it is signed by the Latch Developer ID certificate
# and Gatekeeper accepts it as notarized. `spctl --type execute` rejects every
# bare command-line tool as "not an app", so the assessment uses the `open`
# type with the primary-signature context, which is the Gatekeeper check that
# applies to standalone Mach-O binaries. The requirement must be passed with
# --test-requirement: `--requirement` abbreviates the signing-time
# --requirements option, which `--verify` silently ignores.
verify_publisher() {
    local binary="$1"
    codesign --verify --strict \
        --test-requirement="=anchor apple generic and certificate leaf[subject.OU] = \"$latch_team_id\"" \
        "$binary" &&
        spctl --assess --type open --context context:primary-signature "$binary"
}

verify_payload() {
    local directory="$1" binary
    for binary in "${latch_binaries[@]}"; do
        if ! verify_publisher "$directory/$binary"; then
            echo "$binary is not signed and notarized by the Latch publisher (Team ID $latch_team_id); refusing to install." >&2
            return 1
        fi
    done
}

# CI runs this against freshly built release assets before publishing them.
if [[ "${1:-}" == "--verify-payload" ]]; then
    verify_payload "${2:?usage: install-cli.sh --verify-payload DIRECTORY}"
    exit
fi

case "$(uname -m)" in
    arm64) target="aarch64-apple-darwin" ;;
    x86_64) target="x86_64-apple-darwin" ;;
    *) echo "Latch supports Apple Silicon and Intel Macs." >&2; exit 1 ;;
esac

tag="$(curl -fsSL "https://api.github.com/repos/$repository/releases/latest" |
    sed -nE 's/^[[:space:]]*"tag_name":[[:space:]]*"([^"]+)",?$/\1/p' | head -n 1)"
if [[ ! "$tag" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
    echo "Could not determine the newest Latch release." >&2
    exit 1
fi

version="${BASH_REMATCH[1]}"
archive="latch-${version}-${target}.zip"
release_base="https://github.com/$repository/releases/download/$tag"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/latch-install.XXXXXX")"
trap 'rm -rf -- "$work_dir"' EXIT

curl -fL "$release_base/$archive" -o "$work_dir/$archive"
curl -fL "$release_base/checksums.txt" -o "$work_dir/checksums.txt"
awk -v archive="$archive" '$2 == archive { print }' "$work_dir/checksums.txt" > "$work_dir/archive.sha256"
if [[ ! -s "$work_dir/archive.sha256" ]]; then
    echo "The release checksum does not list $archive." >&2
    exit 1
fi
(cd "$work_dir" && shasum -a 256 -c archive.sha256)
ditto -x -k "$work_dir/$archive" "$work_dir/extracted"
/usr/bin/python3 -c 'import json,sys; p=json.load(open(sys.argv[1])); expected=["latch","latch-remote","latchd"]; assert p == {"formatVersion":1,"version":sys.argv[2],"target":sys.argv[3],"binaries":expected}' \
    "$work_dir/extracted/latch-payload.json" "$version" "$target"
verify_payload "$work_dir/extracted"
"$work_dir/extracted/latch" --version | grep -F " $version"
"$work_dir/extracted/latch-remote" --version | grep -F " $version"
"$work_dir/extracted/latchd" version | grep -Fx "latchd $version protocol 1"
mkdir -p "$HOME/.local/bin"
install -m 0755 "$work_dir/extracted/latch-remote" "$HOME/.local/bin/latch-remote"
install -m 0755 "$work_dir/extracted/latchd" "$HOME/.local/bin/latchd"
install -m 0755 "$work_dir/extracted/latch" "$HOME/.local/bin/latch"
printf 'Installed Latch %s payload at %s\n' "$version" "$HOME/.local/bin"
printf 'If latch is not on PATH, add $HOME/.local/bin to your shell configuration.\n'
