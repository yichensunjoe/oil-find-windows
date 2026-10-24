// swift-tools-version:5.10
import PackageDescription

// This company-intranet fork only builds the public, open-source targets.
let package = Package(
    name: "OilFind",
    platforms: [.macOS(.v14)],
    products: [.library(name: "OilFindCore", targets: ["OilFindCore"]), .library(name: "OilFindApp", targets: ["OilFindApp"]), .executable(name: "oilfind-cli", targets: ["oilfind-cli"])],
    targets: [
        .target(name: "COilFind", publicHeadersPath: "include"),
        .target(name: "OilFindCore", dependencies: ["COilFind"]),
        .target(name: "OilFindApp", dependencies: ["OilFindCore"], resources: [.copy("Resources")],
                swiftSettings: [.define("OILFIND_INTRANET_BUILD")]),
        .executableTarget(name: "OilFind", dependencies: ["OilFindApp"],
                          swiftSettings: [.define("OILFIND_FREE_BUILD")]),
        .executableTarget(name: "oilfind-cli", dependencies: ["OilFindCore"]),
        .testTarget(name: "OilFindCoreTests", dependencies: ["OilFindCore", "COilFind"]),
        .testTarget(name: "OilFindUITests", dependencies: ["OilFindApp", "OilFindCore"])
    ],
    swiftLanguageVersions: [.v5]
)
