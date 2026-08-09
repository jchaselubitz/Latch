#!/usr/bin/env bash
#
# Enforces the dependency rules in docs/ARCHITECTURE_RULES.md.
#
# These are the constraints that are cheap to check mechanically and expensive
# to discover late: a leaf crate that quietly grew a dependency on the binary,
# or an Overlord type that made its way into the local plane.

set -euo pipefail

cd "$(dirname "$0")/.."

fail=0
report() {
    printf 'boundary violation: %s\n' "$1" >&2
    fail=1
}

# --- Leaf crates stay leaves -------------------------------------------------
#
# latch-protocol and latch-term may depend on third-party crates, but not on
# each other and not on the binary.
for leaf in latch-protocol latch-term; do
    manifest="crates/$leaf/Cargo.toml"
    for forbidden in latch-protocol latch-term 'latch '; do
        [ "$(printf '%s' "$forbidden" | tr -d ' ')" = "$leaf" ] && continue
        if grep -Eq "^[[:space:]]*${forbidden%% }[[:space:]]*[.=]" "$manifest"; then
            report "$manifest depends on ${forbidden%% }; leaf crates must not"
        fi
    done
done

# --- No Overlord in the local plane ------------------------------------------
#
# Latch is useful without Overlord. Integration flows the other way: Overlord
# calls the public latch CLI. Comments and doc text may name Overlord — code
# may not import it.
if grep -rInE '^[[:space:]]*(use|extern crate)[[:space:]]+[a-z_]*overlord' crates/ 2>/dev/null; then
    report 'crates/ imports an Overlord module; nothing under crates/ may depend on Overlord'
fi

if grep -rIn --include=Cargo.toml -E '^[[:space:]]*[a-z-]*overlord' crates/ 2>/dev/null; then
    report 'a crate manifest declares an Overlord dependency'
fi

# --- No Node.js in the local plane -------------------------------------------
#
# Every terminal window pays CLI startup cost (decision D1/D2), so the local
# plane is Rust only. TypeScript lives in packages/.
if [ -f package.json ] && [ ! -d packages ]; then
    report 'a root package.json exists without packages/; the local plane must stay Node-free'
fi

if [ "$fail" -eq 0 ]; then
    echo 'boundaries ok'
fi
exit "$fail"
