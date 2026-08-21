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

# --- Conversation v2 is a clean break ---------------------------------------
#
# The retired v1 TypeScript SDK was not a compatibility layer. Keeping one
# source module or workspace around makes it easy to accidentally publish the
# old event/cursor/send contract again.
for retired_path in \
    packages/chat-react \
    packages/harness-schema \
    examples/remote-sdk-react \
    packages/client/src/events.ts \
    packages/client/src/send.ts \
    packages/client/src/compatibility.ts; do
    if [ -e "$retired_path" ]; then
        report "$retired_path is a retired v1 conversation surface"
    fi
done

if [ -d packages/client ] &&
    grep -rInE 'subscribeEvents|supportsGatewayEndpoint|LatchSendError|/v1/' \
        packages/client/src --include='*.ts' 2>/dev/null; then
    report '@latch/client exposes a retired v1 conversation API'
fi

# --- Cloud services stay independent deployables ------------------------------
#
# services/ holds deployable cloud services. Each ships on its own, so it must
# not be pulled into the root workspace build and must not depend on a
# local-plane package: a control-plane deploy that needs packages/ or crates/
# has quietly stopped being independent.
if [ -d services ]; then
    if grep -qE '"services/' package.json; then
        report 'root package.json lists a services/ workspace; cloud services deploy independently'
    fi

    for manifest in services/*/package.json; do
        [ -e "$manifest" ] || continue
        if grep -q '"@latch/[a-z-]*": *"' "$manifest"; then
            report "$manifest depends on a local-plane @latch/ package"
        fi
    done

    # The relay is a separate deployable with a different scaling profile. The
    # control plane authorizes admission; it must not start forwarding frames.
    if [ -d services/control-plane ] &&
        grep -rIlE 'WebSocketServer|createWebSocketStream|forwardFrame' services/control-plane/src 2>/dev/null; then
        report 'services/control-plane implements relay transport; the relay is a separate deployable'
    fi
fi

if [ "$fail" -eq 0 ]; then
    echo 'boundaries ok'
fi
exit "$fail"
