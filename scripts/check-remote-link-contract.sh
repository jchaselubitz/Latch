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

# One device-name policy: the Mac refuses any proposed name the phone or the
# control plane would not produce, so all three must state the same allowlist
# and byte bound as the shared fixture.
python3 - "$root_dir" <<'PY'
import json, re, sys
root = sys.argv[1]
fixture = json.load(open(f"{root}/fixtures/remote-link/v1/device-names.json"))
punctuation = fixture["policy"]["allowedPunctuation"]
max_bytes = fixture["policy"]["maxBytes"]

def read(path):
    return open(f"{root}/{path}", encoding="utf-8").read()

def expect(path, pattern, value, what):
    match = re.search(pattern, read(path))
    if not match or match.group(1) != str(value):
        found = match.group(1) if match else "nothing"
        sys.exit(f"device-name policy drift: {path} {what} is {found!r}, fixture says {value!r}")

host = "crates/latch/src/cli/remote_access.rs"
phone = "apps/LatchMobile/Sources/LatchMobileKit/PairingModel.swift"
service = "services/control-plane/src/validation.ts"
expect(host, r'DEVICE_NAME_PUNCTUATION: &str = "([^"]*)"', punctuation, "punctuation")
expect(host, r"MAX_DEVICE_NAME_BYTES: usize = (\d+)", max_bytes, "byte bound")
expect(phone, r'enrollableName[\s\S]*?CharacterSet\(charactersIn: "([^"]*)"\)', punctuation, "punctuation")
expect(phone, r"maxEnrollableNameBytes = (\d+)", max_bytes, "byte bound")
expect(service, r"SAFE_LABEL = /\^\[\\p\{L\}\\p\{N\}([^\]]*)\]", punctuation, "punctuation")
PY
