// swift-tools-version: 5.9

import PackageDescription

let package = Package(
    name: "LatchMobile",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "LatchMobileKit", targets: ["LatchMobileKit"]),
        .library(name: "LatchNotificationKit", targets: ["LatchNotificationKit"]),
        .library(name: "LatchTransportNative", targets: ["LatchTransportNative"]),
    ],
    targets: [
        .target(name: "LatchNotificationKit"),
        .target(name: "LatchMobileKit", dependencies: ["LatchNotificationKit"]),
        .target(name: "LatchTransportNative", dependencies: ["LatchMobileKit", "LatchTransportFFI"]),
        .binaryTarget(name: "LatchTransportFFI", path: "Native/LatchTransportFFI.xcframework"),
    ]
)
