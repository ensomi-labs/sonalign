#!/usr/bin/env python3
"""Release guards and immutable GitHub assets for the dispatch workflow."""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import urllib.error
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "ensomi-labs/sonalign"


def run(*args: str) -> None:
    print("+ " + " ".join(args), flush=True)
    subprocess.run(args, cwd=ROOT, check=True)


def git(*args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def output(name: str, value: str) -> None:
    print(f"{name}={value}", flush=True)
    if destination := os.environ.get("GITHUB_OUTPUT"):
        with open(destination, "a", encoding="utf-8") as stream:
            stream.write(f"{name}={value}\n")


def fetch_json(url: str, *, github: bool = False) -> dict | None:
    headers = {"User-Agent": "sonalign-release (https://github.com/ensomi-labs/sonalign)"}
    if github:
        headers["Accept"] = "application/vnd.github+json"
        if token := os.environ.get("GH_TOKEN"):
            headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise


def validate(version: str, *, require_tag: bool = False, resume: bool = False) -> str:
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise SystemExit("Use a version such as 0.1.0 or 0.2.0-rc.1, without the v prefix.")
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
        cwd=ROOT, text=True,
    ))
    package = next(package for package in metadata["packages"]
                   if Path(package["manifest_path"]).resolve() == ROOT / "Cargo.toml")
    if package["name"] != "sonalign" or package["version"] != version:
        raise SystemExit("The requested version must match sonalign's Cargo.toml version.")
    if os.environ.get("GITHUB_REPOSITORY", REPOSITORY) != REPOSITORY:
        raise SystemExit(f"Publishing is restricted to {REPOSITORY}.")
    if git("status", "--porcelain", "--untracked-files=no"):
        raise SystemExit("Release source must have no tracked changes.")
    commit = git("rev-parse", "HEAD")
    tag = f"v{version}"
    tags = git("ls-remote", "--tags", "origin", f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}")
    if tags:
        # Annotated tags expose their peeled commit on the second line.
        tagged_commit = tags.splitlines()[-1].split()[0]
        if tagged_commit != commit:
            if resume:
                run("git", "fetch", "--no-tags", "origin", f"refs/tags/{tag}")
                parents = git("rev-list", "--parents", "-n", "1", tagged_commit).split()[1:]
                changed = git("diff", "--name-only", commit, tagged_commit).splitlines()
                if parents == [commit] and changed == ["Package.swift"]:
                    prepared = subprocess.check_output(
                        ["git", "show", f"{tagged_commit}:Package.swift"], cwd=ROOT, text=True,
                    )
                    original = subprocess.check_output(
                        ["git", "show", f"{commit}:Package.swift"], cwd=ROOT, text=True,
                    )
                    checksum = re.search(r'private let releaseChecksum = "([0-9a-f]{64})"', prepared)
                    if checksum and render_manifest(original, version, checksum[1]) == prepared:
                        run("git", "checkout", "--detach", tagged_commit)
                        return validate(version, require_tag=True)
            raise SystemExit(f"{tag} already points at a different commit; do not move release tags.")
    elif require_tag:
        raise SystemExit(f"{tag} must exist and point at this commit before publishing.")
    return commit


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def render_manifest(original: str, version: str, checksum: str) -> str:
    replacements = {
        "releaseURL": f"https://github.com/{REPOSITORY}/releases/download/v{version}/SonalignFFI.xcframework.zip",
        "releaseChecksum": checksum,
    }
    updated = original
    for name, value in replacements.items():
        updated, count = re.subn(
            rf'(private let {name} = ")[^"\n]*(")',
            lambda match: match[1] + value + match[2], updated,
        )
        if count != 1:
            raise SystemExit(f"Expected exactly one private let {name} declaration in Package.swift.")
    return updated


def update_manifest(version: str, checksum: str) -> bool:
    manifest = ROOT / "Package.swift"
    original = manifest.read_text()
    updated = render_manifest(original, version, checksum)
    if updated == original:
        return False
    manifest.write_text(updated)
    return True


def finalize(version: str, assets: Path) -> None:
    validate(version, resume=True)
    tag = f"v{version}"
    archives = sorted(assets.glob("*.zip"))
    xcframework = assets / "SonalignFFI.xcframework.zip"
    required = (
        "SonalignFFI.xcframework.zip",
        "sonalign-*-apple-darwin.zip",
        "sonalign-*-unknown-linux-gnu.zip",
        "sonalign-*-pc-windows-msvc.zip",
        "sonalign-aarch64-linux-android-static.zip",
    )
    for pattern in required:
        if not any(assets.glob(pattern)):
            raise SystemExit(f"Missing release artifact: {pattern}")
    checksums = assets / "SHA256SUMS"
    checksums.write_text("".join(f"{digest(path)}  {path.name}\n" for path in archives))
    archives.append(checksums)
    tag_exists = bool(git("ls-remote", "--tags", "origin", f"refs/tags/{tag}"))
    if update_manifest(version, digest(xcframework)):
        if tag_exists:
            run("git", "restore", "--", "Package.swift")
            raise SystemExit("The existing tag's Swift checksum differs from this build; assets cannot be replaced.")
        run("git", "add", "--", "Package.swift")
        run("git", "commit", "-m", f"Pin Swift binary for {tag}")
    if not tag_exists:
        run("git", "tag", tag)
        # Fail if main moved during the build; never force-push either reference.
        run("git", "push", "--atomic", "origin", "HEAD:refs/heads/main", f"refs/tags/{tag}")
    validate(version, require_tag=True)
    commit = git("rev-parse", "HEAD")
    release = fetch_json(f"https://api.github.com/repos/{REPOSITORY}/releases/tags/{tag}", github=True)
    with tempfile.TemporaryDirectory(prefix="sonalign-release-") as temp:
        temporary = Path(temp)
        if release is None:
            notes = temporary / "notes.md"
            notes.write_text(
                f"Sonalign {version}\n\n"
                "- Rust crate: `cargo add sonalign`\n"
                f"- Swift package: add `https://github.com/{REPOSITORY}` at `{tag}`.\n"
                "- The XCFramework ZIP contains iOS device, iOS simulator, and universal macOS libraries.\n"
                "- Native ZIPs contain the C headers, libraries, and replay CLI.\n"
                "- Android arm64 is provided as a static library with C headers.\n\n"
                "Artifact SHA-256 digests are in `SHA256SUMS`.\n"
            )
            run("gh", "release", "create", tag, "--repo", REPOSITORY, "--verify-tag", "--draft",
                "--title", f"Sonalign {tag}", "--notes-file", str(notes))
            release = {"assets": [], "draft": True}
        existing = {asset["name"] for asset in release["assets"]}
        # Check all existing assets before uploading any missing ones.
        for path in archives:
            if path.name in existing:
                run("gh", "release", "download", tag, "--repo", REPOSITORY,
                    "--pattern", path.name, "--dir", str(temporary))
                if digest(temporary / path.name) != digest(path):
                    raise SystemExit(f"Existing release asset {path.name} differs; refusing to overwrite it.")
        missing = [str(path) for path in archives if path.name not in existing]
        if missing:
            run("gh", "release", "upload", tag, *missing, "--repo", REPOSITORY)
        if release["draft"]:
            run("gh", "release", "edit", tag, "--repo", REPOSITORY, "--draft=false")
    output("commit", commit)


def crate_status(version: str) -> None:
    commit = validate(version, require_tag=True)
    published = fetch_json(f"https://crates.io/api/v1/crates/sonalign/{version}")
    if published is None:
        output("published", "false")
        return
    request = urllib.request.Request(
        f"https://static.crates.io/crates/sonalign/sonalign-{version}.crate",
        headers={"User-Agent": "sonalign-release (https://github.com/ensomi-labs/sonalign)"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        archive = response.read()
    if hashlib.sha256(archive).hexdigest() != published["version"]["checksum"]:
        raise SystemExit("Published crate download does not match the crates.io checksum.")
    verify_crate(archive, version, commit)
    output("published", "true")


def verify_crate(archive: bytes, version: str, commit: str) -> None:
    expected = set(subprocess.check_output(
        ["cargo", "package", "--list", "--locked"], cwd=ROOT, text=True,
    ).splitlines())
    # Cargo consumers use the normalized manifest, not Cargo.toml.orig. Generate
    # that file with the release's pinned Cargo version before accepting a retry.
    with tempfile.TemporaryDirectory(prefix="sonalign-package-check-") as temporary:
        run("cargo", "package", "--no-verify", "--locked", "--target-dir", temporary)
        package = Path(temporary) / "package" / f"sonalign-{version}.crate"
        with tarfile.open(package, mode="r:gz") as local:
            normalized_manifest = local.extractfile(f"sonalign-{version}/Cargo.toml").read()
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as source:
        prefix = f"sonalign-{version}/"
        files = {}
        for entry in source.getmembers():
            if entry.isdir():
                continue
            if not entry.isfile() or not entry.name.startswith(prefix):
                raise SystemExit("Published crate contains an unexpected archive entry.")
            name = entry.name.removeprefix(prefix)
            if name in files:
                raise SystemExit("Published crate contains duplicate source entries.")
            files[name] = source.extractfile(entry).read()
        if set(files) != expected:
            raise SystemExit("Published crate file list differs from this checkout's package.")
        vcs = json.loads(files[".cargo_vcs_info.json"])
        if vcs["git"]["sha1"] != commit or vcs["git"].get("dirty", False) is not False:
            raise SystemExit("Published crate does not record this clean release tag commit.")
        if vcs.get("path_in_vcs", "") != "":
            raise SystemExit("Published crate does not come from the standalone repository root.")
        if files["Cargo.toml"] != normalized_manifest:
            raise SystemExit("Published crate's effective Cargo.toml differs from this checkout's package.")
        for name, contents in files.items():
            if name in ("Cargo.toml", ".cargo_vcs_info.json"):
                continue
            path = ROOT / ("Cargo.toml" if name == "Cargo.toml.orig" else name)
            if not path.is_file() or path.read_bytes() != contents:
                raise SystemExit(f"Published crate source differs from this checkout: {name}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("validate", "finalize", "crate-status"))
    parser.add_argument("version")
    parser.add_argument("--assets", type=Path, default=ROOT / ".build" / "dist")
    args = parser.parse_args()
    if args.command == "validate":
        output("commit", validate(args.version, resume=True))
    elif args.command == "finalize":
        finalize(args.version, args.assets.resolve())
    else:
        crate_status(args.version)


if __name__ == "__main__":
    main()
