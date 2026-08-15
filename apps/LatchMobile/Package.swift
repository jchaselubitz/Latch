// swift-tools-version: 5.9

import PackageDescription

// LatchMobileKit holds everything the phone app does that is not a view: the
// generated wire contract, the gateway client, the event stream, and the
// transcript reducer. Keeping it a plain library means `swift test` exercises
// the client without a simulator, and the Xcode app target consumes it as a
// local package rather than compiling a second copy of the sources.
let package = Package(
    name: "LatchMobile",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "LatchMobileKit", targets: ["LatchMobileKit"])
    ],
    targets: [
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
)
