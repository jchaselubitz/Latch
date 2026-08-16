#!/usr/bin/env bash
# Reproducibly builds the owned Rust transport XCFramework and generated Swift API.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
cd "$repo_root"

framework_dir="${LATCH_TRANSPORT_FRAMEWORK_DIR:-apps/LatchMobile/Native/LatchTransportFFI.xcframework}"
generated_dir="${LATCH_TRANSPORT_GENERATED_DIR:-apps/LatchMobile/Sources/LatchTransportNative/Generated}"
scratch_dir="${TMPDIR:-/tmp}/latch-transport-xcframework"
deployment_target="${LATCH_TRANSPORT_IOS_DEPLOYMENT_TARGET:-17.0}"

required_targets=(
  aarch64-apple-ios
  aarch64-apple-ios-sim
  x86_64-apple-ios
  aarch64-apple-darwin
  x86_64-apple-darwin
)
for target in "${required_targets[@]}"; do
  if ! rustup target list --installed | grep -Fxq "$target"; then
    echo "Missing Rust target $target. Install it with: rustup target add $target" >&2
    exit 1
  fi
done

rm -rf "$scratch_dir/generated" "$scratch_dir/headers" "$scratch_dir/simulator" "$scratch_dir/macos"
mkdir -p "$scratch_dir/generated" "$scratch_dir/headers" "$scratch_dir/simulator" "$scratch_dir/macos"

cargo build --locked --release --package latch-transport-ffi
# Bindgen is a host-only build tool; optimizing it adds minutes to a cold CI
# build without changing a byte of generated Swift or C headers.
cargo run --locked --package latch-transport-bindgen -- \
  generate --library target/release/liblatch_transport_ffi.dylib \
  --language swift --out-dir "$scratch_dir/generated"

# `xcodebuild -create-xcframework` copies a headers directory verbatim, while
# Clang only auto-discovers a module map named `module.modulemap`. UniFFI emits
# a crate-qualified filename for non-framework consumers, so stage the C
# surface under the canonical name instead of leaving `canImport(...)` false
# in the generated Swift at app-build time.
cp "$scratch_dir/generated/latch_transport_ffiFFI.h" "$scratch_dir/headers/"
cp "$scratch_dir/generated/latch_transport_ffiFFI.modulemap" \
  "$scratch_dir/headers/module.modulemap"

build_pids=()
for target in "${required_targets[@]}"; do
  if [[ "$target" == *-apple-ios* ]]; then
    IPHONEOS_DEPLOYMENT_TARGET="$deployment_target" \
      CARGO_TARGET_DIR="$scratch_dir/cargo-$target" \
      cargo build --locked --release --package latch-transport-ffi --target "$target" &
  else
    CARGO_TARGET_DIR="$scratch_dir/cargo-$target" \
      cargo build --locked --release --package latch-transport-ffi --target "$target" &
  fi
  build_pids+=("$!")
done
for build_pid in "${build_pids[@]}"; do
  wait "$build_pid"
done

lipo -create \
  "$scratch_dir/cargo-aarch64-apple-ios-sim/aarch64-apple-ios-sim/release/liblatch_transport_ffi.a" \
  "$scratch_dir/cargo-x86_64-apple-ios/x86_64-apple-ios/release/liblatch_transport_ffi.a" \
  -output "$scratch_dir/simulator/liblatch_transport_ffi.a"

# SwiftPM validates every local binary target before selecting the test target.
# A universal macOS slice keeps `swift test` working after CI has generated the
# iOS framework, while exercising the same generated Swift boundary on host.
lipo -create \
  "$scratch_dir/cargo-aarch64-apple-darwin/aarch64-apple-darwin/release/liblatch_transport_ffi.a" \
  "$scratch_dir/cargo-x86_64-apple-darwin/x86_64-apple-darwin/release/liblatch_transport_ffi.a" \
  -output "$scratch_dir/macos/liblatch_transport_ffi.a"

rm -rf "$framework_dir" "$generated_dir"
mkdir -p "$(dirname "$framework_dir")" "$generated_dir"
cp "$scratch_dir/generated/latch_transport_ffi.swift" "$generated_dir/LatchTransportFFI.swift"

xcodebuild -create-xcframework \
  -library "$scratch_dir/cargo-aarch64-apple-ios/aarch64-apple-ios/release/liblatch_transport_ffi.a" \
  -headers "$scratch_dir/headers" \
  -library "$scratch_dir/simulator/liblatch_transport_ffi.a" \
  -headers "$scratch_dir/headers" \
  -library "$scratch_dir/macos/liblatch_transport_ffi.a" \
  -headers "$scratch_dir/headers" \
  -output "$framework_dir"

echo "Built $framework_dir"
