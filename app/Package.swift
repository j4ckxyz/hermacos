// swift-tools-version: 6.2
import PackageDescription

// The Rust core is built first (scripts/build.sh) and linked as a static library; its UniFFI
// bindings are generated into Sources/HermesCore and Sources/hermes_coreFFI.
let package = Package(
    name: "Hermacos",
    platforms: [.macOS(.v26)],
    targets: [
        .target(name: "hermes_coreFFI", path: "Sources/hermes_coreFFI"),
        .target(
            name: "HermesCore",
            dependencies: ["hermes_coreFFI"],
            path: "Sources/HermesCore",
            swiftSettings: [.swiftLanguageMode(.v5)],
            linkerSettings: [
                .unsafeFlags(["-L", "\(Context.packageDirectory)/Vendor/lib"]),
                .linkedLibrary("hermes_core"),
                .linkedFramework("Security"),
                .linkedFramework("SystemConfiguration"),
                .linkedFramework("CoreFoundation"),
            ]
        ),
        .executableTarget(
            name: "Hermacos",
            dependencies: ["HermesCore"],
            path: "Sources/Hermacos",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
