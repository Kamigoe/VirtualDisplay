// swift-tools-version:6.0
import PackageDescription

let package = Package(
    name: "vdsend",
    platforms: [.macOS(.v14)],
    targets: [
        // Declarations for CoreGraphics' private CGVirtualDisplay API (no implementation of our own).
        .target(
            name: "CGVirtualDisplayPrivate",
            path: "Sources/CGVirtualDisplayPrivate",
            linkerSettings: [.linkedFramework("CoreGraphics")]
        ),
        .executableTarget(
            name: "vdsend",
            dependencies: ["CGVirtualDisplayPrivate"],
            path: "Sources/vdsend",
            swiftSettings: [.swiftLanguageMode(.v5)],
            linkerSettings: [
                .linkedFramework("ScreenCaptureKit"),
                .linkedFramework("VideoToolbox"),
                .linkedFramework("CoreMedia"),
                .linkedFramework("Network"),
            ]
        ),
    ]
)
