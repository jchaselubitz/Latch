#!/usr/bin/env bash
# Set the workspace version to 0.YYMMDDHHMM.0 using the current UTC time.
# Usage: ./scripts/bump-minor-version.sh

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$repo_root/Cargo.toml"
timestamp="$(date -u +%y%m%d%H%M)"
next_version="0.${timestamp}.0"

if ! grep -Eq '^version = "[0-9]+\.[0-9]+\.[0-9]+"$' "$manifest"; then
  echo "Could not find the workspace version in $manifest" >&2
  exit 1
fi

if grep -Fq "version = \"$next_version\"" "$manifest"; then
  echo "Refusing to reuse version $next_version; run again after the minute changes." >&2
  exit 1
fi

perl -0pi -e "s/^version = \"[0-9]+\\.[0-9]+\\.[0-9]+\"$/version = \"$next_version\"/m" "$manifest"

echo "Updated workspace version to $next_version"
