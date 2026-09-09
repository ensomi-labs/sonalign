#!/usr/bin/env python3
"""Build Sonalign with the Rust/Xcode tools on PATH (Python 3.10+)."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import zipfile


ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / ".build"
DIST = BUILD / "dist"
TARGET = BUILD / "cargo"
APPLE_TARGETS = (
    "aarch64-apple-ios",
    "aarch64-apple-ios-sim",
    "x86_64-apple-ios",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
)


def run(*args: str, env: dict[str, str] | None = None) -> None:
    print("+ " + " ".join(args), flush=True)
    subprocess.run(args, cwd=ROOT, env=env, check=True)


def reset_directory(path: Path) -> None:
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True)


def archive(directory: Path) -> Path:
    """Keep the root directory; normalize ZIP metadata for a stable checksum."""
    shutil.copy2(ROOT / "LICENSE", directory / "LICENSE")
    dependency_notices(directory)
    destination = directory.with_suffix(directory.suffix + ".zip")
    with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as output:
        for path in sorted(directory.rglob("*")):
            if not path.is_file():
                continue
            info = zipfile.ZipInfo(path.relative_to(directory.parent).as_posix())
            info.create_system = 3
            mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
            info.external_attr = (0o100000 | mode) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            output.writestr(info, path.read_bytes(), compresslevel=9)
    checksum = hashlib.sha256(destination.read_bytes()).hexdigest()
    print(f"{checksum}  {destination.name}", flush=True)
    return destination


def dependency_notices(directory: Path) -> None:
    notices = directory / "THIRD-PARTY-LICENSES"
    reset_directory(notices)
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1"], cwd=ROOT, text=True,
    ))
    summary = ["Cargo dependencies, including build-time dependencies:\n"]
    for package in sorted(metadata["packages"], key=lambda package: package["id"]):
        if package["name"] == "sonalign":
            continue
        source = Path(package["manifest_path"]).parent
        destination = notices / f"{package['name']}-{package['version']}"
        destination.mkdir()
        files = {path for path in source.iterdir() if path.is_file()
                 and path.name.upper().startswith(("LICENSE", "UNLICENSE", "COPYING", "NOTICE"))}
        if package.get("license_file"):
            files.add(source / package["license_file"])
        if not files:
            raise SystemExit(f"No license text found for {package['name']} {package['version']}.")
        for path in sorted(files):
            shutil.copy2(path, destination / path.name)
        summary.append(f"{package['name']} {package['version']}: {package['license']}\n")
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    rust_docs = sysroot / "share" / "doc" / "rust"
    rust_notices = notices / "rust-standard-library"
    rust_notices.mkdir()
    shutil.copy2(rust_docs / "COPYRIGHT-library.html", rust_notices / "COPYRIGHT-library.html")
    if (rust_docs / "licenses").is_dir():
        shutil.copytree(rust_docs / "licenses", rust_notices / "licenses")
    summary.append("\nRust standard library notices: THIRD-PARTY-LICENSES/rust-standard-library/\n")
    (directory / "THIRD-PARTY-NOTICES.txt").write_text("".join(summary), encoding="utf-8")


def headers(destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    for name in ("sonalign.h", "module.modulemap"):
        shutil.copy2(ROOT / "include" / name, destination / name)


def static_library(target: str, env: dict[str, str] | None = None) -> Path:
    run(
        "cargo", "rustc", "--locked", "--release", "--lib",
        "--crate-type", "staticlib", "--target", target,
        "--target-dir", str(TARGET), env=env,
    )
    name = "sonalign.lib" if "msvc" in target else "libsonalign.a"
    return TARGET / target / "release" / name


def host() -> None:
    details = subprocess.check_output(["rustc", "-vV"], text=True)
    target = next(line.removeprefix("host: ") for line in details.splitlines()
                  if line.startswith("host: "))
    run(
        "cargo", "build", "--locked", "--release", "--lib",
        "--bin", "sonalign-replay", "--target", target,
        "--target-dir", str(TARGET),
    )
    package = DIST / f"sonalign-{target}"
    reset_directory(package)
    headers(package / "include")
    libraries = package / "lib"
    libraries.mkdir()
    binaries = package / "bin"
    binaries.mkdir()
    products = TARGET / target / "release"
    if "windows" in target:
        names = ("sonalign.lib", "sonalign.dll", "sonalign.dll.lib")
        executable = "sonalign-replay.exe"
    elif "apple" in target:
        names = ("libsonalign.a", "libsonalign.dylib")
        executable = "sonalign-replay"
    else:
        names = ("libsonalign.a", "libsonalign.so")
        executable = "sonalign-replay"
    for name in names:
        shutil.copy2(products / name, libraries / name)
    if "apple" in target:
        dylib = str(libraries / "libsonalign.dylib")
        run("xcrun", "install_name_tool", "-id", "@rpath/libsonalign.dylib", dylib)
        run("codesign", "--force", "--sign", "-", dylib)
    shutil.copy2(products / executable, binaries / executable)
    archive(package)


def apple() -> None:
    if platform.system() != "Darwin":
        raise SystemExit("--apple requires macOS with Xcode installed and selected.")
    run("rustup", "target", "add", *APPLE_TARGETS)
    env = dict(os.environ, MACOSX_DEPLOYMENT_TARGET="12.0", IPHONEOS_DEPLOYMENT_TARGET="15.0")
    libraries = {target: static_library(target, env) for target in APPLE_TARGETS}
    staging = BUILD / "apple"
    reset_directory(staging)
    universal = {}
    # Never combine device and simulator archives: they are different platforms.
    for name, targets in (
        ("ios-simulator", ("aarch64-apple-ios-sim", "x86_64-apple-ios")),
        ("macos", ("aarch64-apple-darwin", "x86_64-apple-darwin")),
    ):
        output = staging / name / "libsonalign.a"
        output.parent.mkdir()
        run("xcrun", "lipo", "-create", *(str(libraries[t]) for t in targets), "-output", str(output))
        universal[name] = output
    destination = DIST / "SonalignFFI.xcframework"
    if destination.exists():
        shutil.rmtree(destination)
    run(
        "xcodebuild", "-create-xcframework",
        "-library", str(libraries["aarch64-apple-ios"]), "-headers", str(ROOT / "include"),
        "-library", str(universal["ios-simulator"]), "-headers", str(ROOT / "include"),
        "-library", str(universal["macos"]), "-headers", str(ROOT / "include"),
        "-output", str(destination),
    )
    archive(destination)


def portable_static(target: str) -> None:
    run("rustup", "target", "add", target)
    library = static_library(target)
    package = DIST / f"sonalign-{target}-static"
    reset_directory(package)
    headers(package / "include")
    (package / "lib").mkdir()
    shutil.copy2(library, package / "lib" / library.name)
    archive(package)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", action="store_true", help="Host static/shared library and CLI (default)")
    parser.add_argument("--apple", action="store_true", help="Universal Apple XCFramework and SwiftPM ZIP")
    parser.add_argument("--android", action="store_true", help="Android arm64 static library and headers")
    parser.add_argument("--windows", action="store_true", help="Windows x86_64 MSVC static library and headers")
    args = parser.parse_args()
    DIST.mkdir(parents=True, exist_ok=True)
    if args.host or not any((args.apple, args.android, args.windows)):
        host()
    if args.apple:
        apple()
    if args.android:
        portable_static("aarch64-linux-android")
    if args.windows:
        portable_static("x86_64-pc-windows-msvc")


if __name__ == "__main__":
    main()
