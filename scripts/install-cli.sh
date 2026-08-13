#!/usr/bin/env bash
# Install the newest notarized Latch CLI release into ~/.local/bin.
set -euo pipefail

repository="jchaselubitz/Latch"
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
codesign --verify --strict "$work_dir/extracted/latch"
codesign --verify --strict "$work_dir/extracted/latch-tmux"
"$work_dir/extracted/latch-tmux" -V | grep -Fx "tmux 3.7b"

mkdir -p "$HOME/.local/bin"
install -m 0755 "$work_dir/extracted/latch-tmux" "$HOME/.local/bin/latch-tmux"
install -m 0755 "$work_dir/extracted/latch" "$HOME/.local/bin/latch"
printf 'Installed Latch %s payload at %s\n' "$version" "$HOME/.local/bin"
printf 'If latch is not on PATH, add $HOME/.local/bin to your shell configuration.\n'
