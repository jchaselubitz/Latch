#!/bin/sh
set -eu

root_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
schema="$root_dir/schemas/remote-link/v1/remote-link.schema.json"
copy="$root_dir/apps/LatchMobile/Contract/schemas/remote-link.schema.json"

python3 -m json.tool "$schema" >/dev/null
for fixture in "$root_dir"/fixtures/remote-link/v1/*.json; do
  python3 -m json.tool "$fixture" >/dev/null
done

if ! cmp -s "$schema" "$copy"; then
  echo "remote-link Apple contract is stale; copy schemas/remote-link/v1/remote-link.schema.json" >&2
  exit 1
fi
