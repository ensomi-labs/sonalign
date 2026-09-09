#!/usr/bin/env python3
"""Run Swift integration tests and link the example for every iOS XCFramework slice."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import plistlib
import subprocess


ROOT = Path(__file__).resolve().parents[1]


def run(*args: str, env: dict[str, str] | None = None) -> None:
    print("+ " + " ".join(args), flush=True)
    subprocess.run(args, cwd=ROOT, env=env, check=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--xcframework",
        default=os.environ.get("SONALIGN_XCFRAMEWORK_PATH", str(ROOT / ".build/dist/SonalignFFI.xcframework")),
    )
    args = parser.parse_args()
    framework = Path(args.xcframework).resolve()
    with (framework / "Info.plist").open("rb") as source:
        metadata = plistlib.load(source)
    env = dict(os.environ, SONALIGN_XCFRAMEWORK_PATH=str(framework))
    run("swift", "test", env=env)
    run("swift", "run", "sonalign-example", env=env)

    sources = sorted(str(path) for path in (ROOT / "swift/Sources/Sonalign").glob("*.swift"))
    example = ROOT / "swift/Examples/SyntheticAlignment/main.swift"
    linked = 0
    for library in metadata["AvailableLibraries"]:
        if library["SupportedPlatform"] != "ios":
            continue
        simulator = library.get("SupportedPlatformVariant") == "simulator"
        sdk_name = "iphonesimulator" if simulator else "iphoneos"
        sdk = subprocess.check_output(["xcrun", "--sdk", sdk_name, "--show-sdk-path"], text=True).strip()
        directory = framework / library["LibraryIdentifier"]
        headers = directory / library["HeadersPath"]
        for architecture in library["SupportedArchitectures"]:
            target = f"{architecture}-apple-ios15.0" + ("-simulator" if simulator else "")
            output = ROOT / ".build/swift-link-checks" / target
            output.mkdir(parents=True, exist_ok=True)
            common = ("-target", target, "-sdk", sdk, "-I", str(headers))
            run(
                "xcrun", "--sdk", sdk_name, "swiftc", *common, "-parse-as-library", "-module-name", "Sonalign",
                "-emit-library", "-static", "-emit-module",
                "-emit-module-path", str(output / "Sonalign.swiftmodule"),
                *sources, "-o", str(output / "libSonalign.a"),
            )
            run(
                "xcrun", "--sdk", sdk_name, "swiftc", *common, "-I", str(output), "-L", str(output), "-lSonalign",
                str(example), str(directory / library["LibraryPath"]),
                "-o", str(output / "sonalign-example"),
            )
            linked += 1
    if linked == 0:
        raise SystemExit("XCFramework contained no iOS slices to verify")
    print(f"Swift integration tests passed; linked {linked} iOS device/simulator examples.")


if __name__ == "__main__":
    main()
