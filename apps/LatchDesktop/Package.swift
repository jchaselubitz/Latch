// swift-tools-version: 5.9

import PackageDescription

let package = Package(
    name: "LatchDesktop",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "LatchDesktop", targets: ["LatchDesktop"])
    ],
    targets: [
        .executableTarget(
            name: "LatchDesktop",
            path: "Sources/LatchDesktop"
        ),
        .testTarget(
            name: "LatchDesktopTests",
            dependencies: ["LatchDesktop"],
            path: "Tests/LatchDesktopTests"
        )
    ]
)
