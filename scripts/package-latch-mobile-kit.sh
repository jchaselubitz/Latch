#!/usr/bin/env bash
# Build a self-contained Swift package with the native transport from the same inputs.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
cd "$repo_root"

if [[ $# != 1 ]]; then
  echo "Usage: $0 OUTPUT_DIRECTORY" >&2
  exit 2
fi
mkdir -p "$1"
output_dir="$(cd "$1" && pwd -P)"
scratch_root="${TMPDIR:-/tmp}"
work_dir="$(mktemp -d "$scratch_root/latch-mobile-package.XXXXXXXX")"
trap 'rm -rf "$work_dir"' EXIT

package_dir="$work_dir/LatchMobile"
mobile_dir="apps/LatchMobile"
manifest="$mobile_dir/Tools/Distribution/Package.swift"
mkdir -p "$package_dir/Sources"
cp "$manifest" "$package_dir/Package.swift"
cp LICENSE "$package_dir/"

# Keep the native build inputs separate from the delivered Swift package. Hash
# the tracked Rust workspace, including the lockfile, and the actual builder.
native_inputs() {
  git ls-files -z Cargo.toml Cargo.lock rust-toolchain.toml crates \
    scripts/build-latch-transport-xcframework.sh |
    xargs -0 shasum -a 256
}
native_inputs > "$package_dir/NativeInputs.sha256"

for module in LatchMobileKit LatchNotificationKit LatchTransportNative; do
  # SwiftPM's generated UniFFI source is copied in by the native builder.
  git ls-files --cached --others --exclude-standard -z "$mobile_dir/Sources/$module" |
    while IFS= read -r -d '' source; do
      destination="$package_dir/${source#"$mobile_dir/"}"
      mkdir -p "$(dirname "$destination")"
      cp "$source" "$destination"
    done
done

export LATCH_TRANSPORT_IOS_DEPLOYMENT_TARGET=17.0
framework="$package_dir/Native/LatchTransportFFI.xcframework"
LATCH_TRANSPORT_BUILD_DIR="${LATCH_TRANSPORT_BUILD_DIR:-$work_dir/native-build}" \
  CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$work_dir/native-host}" \
  LATCH_TRANSPORT_FRAMEWORK_DIR="$framework" \
  LATCH_TRANSPORT_GENERATED_DIR="$package_dir/Sources/LatchTransportNative/Generated" \
  scripts/build-latch-transport-xcframework.sh >&2
native_inputs > "$work_dir/native-after.sha256"
cmp "$package_dir/NativeInputs.sha256" "$work_dir/native-after.sha256"

{
  printf 'source_base_commit=%s\n' "$(git rev-parse HEAD)"
  printf 'package_builder_sha256=%s\n' "$(shasum -a 256 scripts/package-latch-mobile-kit.sh | cut -d ' ' -f 1)"
  printf 'ios_deployment_target=%s\n' "$LATCH_TRANSPORT_IOS_DEPLOYMENT_TARGET"
  printf 'macos_sdk=%s\n' "$(xcrun --sdk macosx --show-sdk-version)"
  printf 'ios_sdk=%s\n' "$(xcrun --sdk iphoneos --show-sdk-version)"
  printf 'ios_simulator_sdk=%s\n' "$(xcrun --sdk iphonesimulator --show-sdk-version)"
  rustc -Vv
  cargo -V
  xcodebuild -version
} > "$package_dir/BuildProvenance.txt"

archive="$work_dir/LatchMobile.zip"
(
  cd "$work_dir"
  export LC_ALL=C TZ=UTC
  cd LatchMobile
  find Native -type f | sort |
    while IFS= read -r entry; do shasum -a 256 "$entry"; done > Native.sha256
  find Package.swift LICENSE Sources -type f | sort |
    while IFS= read -r entry; do shasum -a 256 "$entry"; done > Sources.sha256
  cd ..
  find LatchMobile -exec touch -t 200001010000 {} +
  find LatchMobile -type f | sort | zip -X -q "$archive" -@
)
checksum="$(shasum -a 256 "$archive" | cut -d ' ' -f 1)"
archive_name="LatchMobile-$checksum.zip"
mv "$archive" "$output_dir/$archive_name"
printf '%s  %s\n' "$checksum" "$archive_name" > "$output_dir/$archive_name.sha256"
printf '%s\n' "$output_dir/$archive_name"
