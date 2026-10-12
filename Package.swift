// swift-tools-version: 5.9
import Foundation
import PackageDescription

// Updated by the release workflow after the binary artifact is built and tested.
private let releaseURL = "https://github.com/ensomi-labs/sonalign/releases/download/v0.1.1/SonalignFFI.xcframework.zip"
private let releaseChecksum = "3752bab9987caa10d52cb9418f07251b701bccef73ecb0c9a5dc39907ac83086"

let binary: Target
if let path = ProcessInfo.processInfo.environment["SONALIGN_XCFRAMEWORK_PATH"], !path.isEmpty {
    // SwiftPM requires a relative binary path; CI commonly supplies an absolute one.
    let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
    let artifact = URL(fileURLWithPath: path, relativeTo: root).standardizedFileURL.pathComponents
    let base = root.standardizedFileURL.pathComponents
    let shared = zip(base, artifact).prefix(while: { $0 == $1 }).count
    let relative = (Array(repeating: "..", count: base.count - shared) + artifact.dropFirst(shared)).joined(separator: "/")
    binary = .binaryTarget(name: "SonalignFFI", path: relative)
} else {
    binary = .binaryTarget(name: "SonalignFFI", url: releaseURL, checksum: releaseChecksum)
}

let package = Package(
    name: "Sonalign",
    platforms: [.macOS(.v12), .iOS(.v15)],
    products: [
        .library(name: "Sonalign", targets: ["Sonalign"]),
        .executable(name: "sonalign-example", targets: ["SonalignExample"]),
    ],
    targets: [
        binary,
        .target(name: "Sonalign", dependencies: ["SonalignFFI"], path: "swift/Sources/Sonalign"),
        .executableTarget(name: "SonalignExample", dependencies: ["Sonalign"], path: "swift/Examples/SyntheticAlignment"),
        .testTarget(name: "SonalignTests", dependencies: ["Sonalign"], path: "swift/Tests/SonalignTests"),
    ]
)
