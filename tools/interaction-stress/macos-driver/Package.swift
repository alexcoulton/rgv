// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "RgvMacosInputDriver",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "rgv-macos-input-driver", targets: ["RgvMacosInputDriver"]),
    ],
    targets: [
        .target(
            name: "RgvMacosInputDriverCore",
            path: "Sources/RgvMacosInputDriver",
            linkerSettings: [
                .linkedFramework("ApplicationServices"),
                .linkedFramework("AppKit"),
            ]
        ),
        .executableTarget(
            name: "RgvMacosInputDriver",
            dependencies: ["RgvMacosInputDriverCore"],
            path: "Sources/RgvMacosInputDriverEntry"
        ),
        .testTarget(
            name: "RgvMacosInputDriverTests",
            dependencies: ["RgvMacosInputDriverCore"]
        ),
    ]
)
