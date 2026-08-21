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

# A release build must start with a fresh app bundle.  This keeps a bundle from
# an earlier local build from being mistaken for the application being released
# if packaging is interrupted or retried.
app_dir="$repo_dir/apps/LatchDesktop/.build/release/Latch.app"
if [[ -e "$app_dir" ]]; then
    rm -rf -- "$app_dir"
fi

LATCH_APP_ARCHIVE="$archive_path" \
    apps/LatchDesktop/build-app.sh

if [[ ! -f "$archive_path" ]]; then
    echo "Notarization completed but no release archive was produced: $archive_path" >&2
    exit 1
fi

git tag --annotate "$tag" --message "Latch $version"
git push origin "HEAD:refs/heads/$branch" "refs/tags/$tag"

# Pushing the tag starts the CLI release workflow, and that workflow is what
# creates the GitHub Release. The desktop archive is built here rather than in
# CI — notarization needs the Developer ID identity on this machine — so it has
# to be attached once the release exists. Without this the app's in-place
# updater has nothing to find.
attach_desktop_archive() {
    if ! command -v gh >/dev/null 2>&1; then
        cat >&2 <<EOF
The GitHub CLI is not installed, so $archive_path was not attached to $tag.
Attach it once the release workflow finishes:
  gh release download $tag --pattern checksums.txt --dir /tmp/latch-release
  shasum -a 256 "$archive_path" | sed 's|  .*/|  |' >> /tmp/latch-release/checksums.txt
  gh release upload $tag "$archive_path" /tmp/latch-release/checksums.txt --clobber
EOF
        return 1
    fi

    printf 'Waiting for the release workflow to create %s…\n' "$tag"
    local waited=0
    until gh release view "$tag" >/dev/null 2>&1; do
        if (( waited >= 1800 )); then
            cat >&2 <<EOF
Release $tag did not appear within 30 minutes. Check the release workflow, then
attach the desktop archive by hand:
  gh release download $tag --pattern checksums.txt --dir /tmp/latch-release
  shasum -a 256 "$archive_path" | sed 's|  .*/|  |' >> /tmp/latch-release/checksums.txt
  gh release upload $tag "$archive_path" /tmp/latch-release/checksums.txt --clobber
EOF
            return 1
        fi
        sleep 15
        waited=$((waited + 15))
    done

    local checksum_dir
    checksum_dir="$(mktemp -d "${TMPDIR:-/tmp}/latch-release.XXXXXX")"
    trap 'rm -rf -- "$checksum_dir"' RETURN
    gh release download "$tag" --pattern checksums.txt --dir "$checksum_dir"
    awk -v archive="$(basename "$archive_path")" \
        '$2 != archive { print }' "$checksum_dir/checksums.txt" > "$checksum_dir/checksums.next"
    shasum -a 256 "$archive_path" | sed 's|  .*/|  |' >> "$checksum_dir/checksums.next"
    mv "$checksum_dir/checksums.next" "$checksum_dir/checksums.txt"
    gh release upload "$tag" "$archive_path" "$checksum_dir/checksums.txt" --clobber
}

if attach_desktop_archive; then
    printf 'Published %s with %s\n' "$tag" "$archive_path"
else
    printf 'Published %s; %s still needs to be attached\n' "$tag" "$archive_path" >&2
    exit 1
fi
