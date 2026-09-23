#!/usr/bin/env python3
"""Release-manifest build/verify for Lerdr release bundles.

Mirrors the oracle's internal/release/manifest.go (schema 1): a staged
release tree is hashed file-by-file into `release-manifest.json`, and the
manifest is later re-verified against the extracted tree — offline, without
executing the bundle's binaries (cross-target safe).

The Rust `lerdr-relay` binary is growing `release-manifest`/`verify-release`
subcommands; once they land, package-release.sh prefers them and this file
becomes the fallback. Until then this is the authoritative manifest path —
the fields the installer consumes (`version`, `revision`, `target`,
`web_hash`) and the `files` map use the oracle's exact schema so the Go and
Rust bundle layouts stay interchangeable.

Usage:
    release-manifest.py build ROOT VERSION REVISION TARGET
    release-manifest.py verify ROOT --target T [--version V --revision R]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import stat
import sys
from pathlib import Path

MANIFEST_NAME = "release-manifest.json"
MANIFEST_SCHEMA = 1

# protocol.EncryptedWebSocketSubprotocol — the only transport this release
# advertises (E2EE v1 is retired).
ENCRYPTED_WS_SUBPROTOCOL = "herdr-e2ee-v2"

# Bundle contract for the Rust tarball: binary + docs + the operator-facing
# scripts/ wrappers (the Go bundle's relay/*.sh set). Kept in sync with
# package-release.sh's WRAPPER list.
REQUIRED_FILES = [
    "lerdr-relay",
    "README.md",
    "scripts/common.sh",
    "scripts/plugin-on-event.sh",
    "scripts/plugin-on-startup.sh",
    "scripts/setup-link.sh",
    "scripts/tailscale-serve.sh",
    "scripts/tailscale-service.sh",
]


def fail(message: str) -> "None":
    print(f"release-manifest: {message}", file=sys.stderr)
    raise SystemExit(1)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def clean_relative(name: str) -> str:
    """Oracle's cleanRelative: non-empty slash path that cannot escape root."""
    if not name or "\\" in name or name.startswith("/"):
        fail(f"invalid manifest path {name!r}")
    clean = os.path.normpath(name).replace(os.sep, "/")
    if clean in (".", "..") or clean.startswith("../"):
        fail(f"manifest path escapes release root: {name!r}")
    if clean != name:
        fail(f"manifest path is not canonical: {name!r}")
    return name


def hash_file_map(files: dict[str, str], prefix: str) -> str | None:
    """sha256 over sorted "name\\x00hash\\n" entries under prefix — manifest.go's
    hashFileMap. Returns None when no file carries the prefix (Go's omitempty
    then drops web_hash)."""
    keys = sorted(name for name in files if name.startswith(prefix))
    if not keys:
        return None
    digest = hashlib.sha256()
    for name in keys:
        digest.update(f"{name}\x00{files[name]}\n".encode())
    return digest.hexdigest()


def collect_files(root: Path) -> dict[str, str]:
    files: dict[str, str] = {}
    for dirpath, dirnames, filenames in os.walk(root):
        for filename in filenames:
            full = Path(dirpath) / filename
            relative = full.relative_to(root).as_posix()
            if relative == MANIFEST_NAME:
                continue
            if full.is_symlink():
                fail(f"release contains symlink {relative}")
            if not full.is_file():
                fail(f"release contains non-regular file {relative}")
            files[relative] = sha256_file(full)
        for dirname in dirnames:
            if (Path(dirpath) / dirname).is_symlink():
                relative = (Path(dirpath) / dirname).relative_to(root).as_posix()
                fail(f"release contains symlink {relative}")
    return files


def cmd_build(args: argparse.Namespace) -> None:
    root = Path(args.root).resolve()
    if not args.version.strip() or not args.revision.strip() or not args.target.strip():
        fail("version, revision, and target are required")
    files = collect_files(root)
    manifest = {
        "schema": MANIFEST_SCHEMA,
        "version": args.version,
        "revision": args.revision,
        "target": args.target,
        "app_transports": [ENCRYPTED_WS_SUBPROTOCOL],
        "relay_transports": [ENCRYPTED_WS_SUBPROTOCOL],
        "files": dict(sorted(files.items())),
    }
    web_hash = hash_file_map(files, "web/")
    if web_hash is not None:
        manifest["web_hash"] = web_hash
    data = json.dumps(manifest, indent=2) + "\n"
    # Atomic write — same temp+rename the oracle uses.
    temp = root / f".{MANIFEST_NAME}.{os.getpid()}"
    try:
        temp.write_text(data)
        os.chmod(temp, 0o644)
        os.replace(temp, root / MANIFEST_NAME)
    finally:
        temp.unlink(missing_ok=True)


def cmd_verify(args: argparse.Namespace) -> None:
    root = Path(args.root).resolve()
    manifest_path = root / MANIFEST_NAME
    try:
        manifest = json.loads(manifest_path.read_text())
    except OSError as error:
        fail(f"read release manifest: {error}")
    except json.JSONDecodeError as error:
        fail(f"parse release manifest: {error}")

    if manifest.get("schema") != MANIFEST_SCHEMA:
        fail(f"unsupported release manifest schema {manifest.get('schema')}")
    if not str(manifest.get("version", "")).strip() or not str(
        manifest.get("revision", "")
    ).strip():
        fail("release manifest version and revision are required")
    files = manifest.get("files")
    if not manifest.get("target") or not isinstance(files, dict) or not files:
        fail("release manifest target and files are required")
    if args.target and manifest["target"] != args.target:
        fail(f"release target {manifest['target']!r} does not match {args.target!r}")
    if args.version and manifest["version"] != args.version:
        fail(
            f"release version {manifest['version']!r} does not match {args.version!r}"
        )
    if args.revision and manifest["revision"] != args.revision:
        fail(
            f"release revision {manifest['revision']!r} does not match {args.revision!r}"
        )

    listed: set[str] = set()
    for name, expected in files.items():
        clean = clean_relative(name)
        if not (
            isinstance(expected, str)
            and len(expected) == 64
            and expected == expected.lower()
            and all(c in "0123456789abcdef" for c in expected)
        ):
            fail(f"invalid SHA-256 for {name}")
        full = root / clean
        if full.is_symlink() or not full.is_file():
            fail(f"verify {name}: not a regular file")
        if sha256_file(full) != expected:
            fail(f"hash mismatch for {name}")
        listed.add(clean)

    computed_web_hash = hash_file_map(files, "web/")
    # Unlike the Go verifier (which hard-requires a web/ bundle), the Rust
    # tarball ships no PWA — web_hash must simply be honest when present.
    if manifest.get("web_hash") != computed_web_hash and (
        manifest.get("web_hash") is not None or computed_web_hash is not None
    ):
        fail("release manifest web hash does not match its web files")

    actual = collect_files(root)
    for relative in actual:
        if relative not in listed:
            fail(f"release file is not listed in manifest: {relative}")

    for required in REQUIRED_FILES:
        if required not in listed:
            fail(f"release manifest is missing {required}")
    binary = root / "lerdr-relay"
    if not os.access(binary, os.X_OK) or not binary.is_file():
        fail("release relay binary is not executable")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    build = sub.add_parser("build", help="write release-manifest.json for a staged tree")
    build.add_argument("root")
    build.add_argument("version")
    build.add_argument("revision")
    build.add_argument("target", help="os/arch, e.g. linux/amd64")
    build.set_defaults(func=cmd_build)

    verify = sub.add_parser("verify", help="verify a staged/extracted release tree")
    verify.add_argument("root")
    verify.add_argument("--target", default="")
    verify.add_argument("--version", default="")
    verify.add_argument("--revision", default="")
    verify.set_defaults(func=cmd_verify)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
