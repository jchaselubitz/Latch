// swift-tools-version: 5.9

import PackageDescription
import Foundation

// LatchMobileKit holds everything the phone app does that is not a view: the
// generated wire contract, gateway client, transport, and compatibility rules.
// Keeping it a plain library means `swift test` exercises
// the client without a simulator, and the Xcode app target consumes it as a
// local package rather than compiling a second copy of the sources.
let nativeFrameworkPath = "Native/LatchTransportFFI.xcframework"
let hasNativeFramework = FileManager.default.fileExists(
    atPath: URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .appendingPathComponent(nativeFrameworkPath)
        .path
)

var targets: [Target] = [
    .target(
        name: "LatchMobileKit",
        path: "Sources/LatchMobileKit"
    ),
    .testTarget(
        name: "LatchMobileKitTests",
        dependencies: ["LatchMobileKit"],
        path: "Tests/LatchMobileKitTests"
    ),
    // The emulator's own test target, deliberately separate from the kit's.
    // LatchMobileKit must never see SwiftTerm — only this target and the app's
    // `SwiftTermSurface` may name it — and a shared test target would put the
    // emulator on the kit's compile path.
    .testTarget(
        name: "TerminalEmulatorTests",
        // The kit is here for one reason: phase 5 has to check that the flags
        // the emulator reports and the encoding table the kit owns agree about
        // a real Claude Code stream. The architecture rule is that
        // `LatchMobileKit` must never *see* the emulator, and it still does not
        // — the dependency runs one way, in a test target that exists to hold
        // the two against each other.
        dependencies: [
            "LatchMobileKit",
            .product(name: "SwiftTerm", package: "SwiftTerm")
        ],
        path: "Tests/TerminalEmulatorTests"
    )
]
if hasNativeFramework {
    targets.append(.binaryTarget(name: "LatchTransportFFI", path: nativeFrameworkPath))
    targets.append(
        .target(
            name: "LatchTransportNative",
            dependencies: ["LatchMobileKit", "LatchTransportFFI"],
            path: "Sources/LatchTransportNative"
        )
    )
}

var products: [Product] = [
    .library(name: "LatchMobileKit", targets: ["LatchMobileKit"])
]
if hasNativeFramework {
    products.append(.library(name: "LatchTransportNative", targets: ["LatchTransportNative"]))
}

let package = Package(
    name: "LatchMobile",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: products,
    dependencies: [
        // The terminal emulator, admitted by the `fixtures/vt` measurement in
        // `Tests/TerminalEmulatorTests`. The app links it through its own
        // package reference in LatchMobile.xcodeproj so that `SwiftTermSurface`
        // can compile; SPM resolves both references to one version.
        //
        // Two things this dependency needs from the machine, both discovered
        // by building it rather than by reading about it:
        //
        // 1. SwiftTerm ships a Metal renderer and processes `Shaders.metal` as
        //    a resource, so it does not build at all without the Metal
        //    toolchain: `xcodebuild -downloadComponent MetalToolchain`.
        // 2. The version is held below 1.19 deliberately. 1.19 added a
        //    build-tool plugin (`SwiftTermBuildInfoPlugin`) whose generator
        //    Xcode 27 builds for the run destination instead of for the host,
        //    so `xcodebuild` fails looking for an iOS binary in the host
        //    products directory. Nothing in Latch uses what 1.19 and 1.20 add,
        //    and 1.18 passes the same `fixtures/vt` gate, so the pin costs
        //    nothing today. Lift it once Xcode resolves the plugin's host
        //    build — and re-run TerminalEmulatorTests when you do.
        .package(url: "https://github.com/migueldeicaza/SwiftTerm.git", .upToNextMinor(from: "1.18.0"))
    ],
    targets: targets
)
