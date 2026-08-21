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
    targets: targets
)
