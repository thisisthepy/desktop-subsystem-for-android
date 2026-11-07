// swift-tools-version:5.9
// DSA Settings for macOS: the settings window, the menu bar item and the Dock icon of DSA.app.
//
// The app never speaks the local API itself. It runs the dsa command that ships beside it in
// DSA.app, which speaks the generated protocol to the per-user service socket and prints its
// answer as JSON, as the Windows settings app does. So every check, every saved value and every
// state comes from the service, and no codec is written here.
import PackageDescription

let package = Package(
    name: "DsaSettings",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "dsa-settings", targets: ["DsaSettings"]),
    ],
    targets: [
        // The view models and the dsa client: everything the pages show, with no UI in it.
        .target(name: "DsaSettingsCore", path: "Sources/DsaSettingsCore"),
        // The SwiftUI and AppKit app.
        .executableTarget(
            name: "DsaSettings",
            dependencies: ["DsaSettingsCore"],
            path: "Sources/DsaSettings"
        ),
        .testTarget(
            name: "DsaSettingsCoreTests",
            dependencies: ["DsaSettingsCore"],
            path: "Tests/DsaSettingsCoreTests"
        ),
    ]
)
