// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "ExtenderApp",
    platforms: [
        .macOS(.v13)
    ],
    products: [
        .executable(name: "ExtenderApp", targets: ["ExtenderApp"]),
    ],
    targets: [
        .target(
            name: "ExtenderBridge",
            path: "Sources/ExtenderBridge"
        ),
        .executableTarget(
            name: "ExtenderApp",
            dependencies: ["ExtenderBridge"],
            path: "Sources/ExtenderApp",
            resources: [.copy("../../Resources")]
        ),
    ]
)
