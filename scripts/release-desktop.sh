#!/usr/bin/env bash
# Notarize the current, already-versioned Latch Desktop commit and publish it.
#
# This intentionally does not bump a version or create a commit. Run the
# version-bump workflow, review and commit it, then run this script.
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_dir"

load_env_value() {
    local name="$1"
    local env_file="$2"
    local value

    # Read only the explicitly supported key. Do not source .env: it may
    # contain arbitrary shell syntax and release credentials must never be
    # executed as code.
    value="$(sed -nE "s/^${name}=(.*)$/\\1/p" "$env_file" | tail -n 1)"
    case "$value" in
        \"*\") value="${value:1:${#value}-2}" ;;
        \'*\') value="${value:1:${#value}-2}" ;;
    esac
    printf '%s' "$value"
}

env_file="$repo_dir/.env"
if [[ -f "$env_file" ]]; then
    if [[ -z "${LATCH_CODESIGN_IDENTITY:-}" ]]; then
        export LATCH_CODESIGN_IDENTITY="$(load_env_value LATCH_CODESIGN_IDENTITY "$env_file")"
    fi
    if [[ -z "${LATCH_NOTARY_PROFILE:-}" ]]; then
        export LATCH_NOTARY_PROFILE="$(load_env_value LATCH_NOTARY_PROFILE "$env_file")"
    fi
fi

version="$(sed -nE 's/^version = "([0-9]+\.[0-9]+\.[0-9]+)"$/\1/p' Cargo.toml)"
if [[ -z "$version" ]]; then
    echo "Could not read the workspace version from Cargo.toml" >&2
    exit 1
fi

tag="v$version"
branch="$(git branch --show-current)"
if [[ -z "$branch" ]]; then
    echo "Refusing to release from a detached HEAD" >&2
    exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
    cat >&2 <<'EOF'
Refusing to release with uncommitted changes. Commit the reviewed version bump
and release changes first; this script will tag and push that commit.
EOF
    exit 1
fi

: "${LATCH_CODESIGN_IDENTITY:?Set a Developer ID Application identity (or add it to .env)}"
: "${LATCH_NOTARY_PROFILE:?Set the notarytool keychain profile (or add it to .env)}"

git fetch origin --tags
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    echo "Tag $tag already exists locally or on origin" >&2
    exit 1
fi

archive_path="$repo_dir/dist/Latch-$version-macos.zip"
LATCH_APP_ARCHIVE="$archive_path" \
    apps/LatchDesktop/build-app.sh

if [[ ! -f "$archive_path" ]]; then
    echo "Notarization completed but no release archive was produced: $archive_path" >&2
    exit 1
fi

git tag --annotate "$tag" --message "Latch $version"
git push origin "HEAD:refs/heads/$branch" "refs/tags/$tag"

printf 'Published %s and %s\n' "$tag" "$archive_path"
