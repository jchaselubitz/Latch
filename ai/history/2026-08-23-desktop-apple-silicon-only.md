# Desktop app is Apple Silicon only

Latch Desktop no longer ships a universal (arm64 + x86_64) binary.

`apps/LatchDesktop/build-app.sh` builds and packages only `arm64`. The Intel
CLI archives are unchanged.

Xcode 27's macOS 27 SDK deprecates x86_64; dropping that slice removes the
warning and halves the desktop compile. Existing Intel Macs can still install
the standalone CLI.
