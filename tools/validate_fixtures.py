#!/usr/bin/env python3
"""Validate fixture vector suites against fixtures/README.md contract.

Usage: python3 tools/validate_fixtures.py [fixtures_dir]
Exit 0 when every *.json under fixtures/ conforms. This is the CI floor —
Rust/Kotlin harnesses add semantic assertions on top.
"""
import json
import re
import sys
from pathlib import Path

REQUIRED_ENVELOPE = {"format_version", "suite", "source", "vectors"}
REQUIRED_SOURCE = {"repo", "commit", "package", "generator"}
SUITE_RE = re.compile(r"^[a-z]+(\.[a-z0-9_]+)+$")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
FORBIDDEN_TOP = {"generated_at", "timestamp"}

KNOWN_SUITES = {
    "crypto.handshake.credential",
    "crypto.handshake.invitation",
    "crypto.frames.json",
    "crypto.frames.binary",
    "crypto.failures",
    "pane.delta",
    "pane.sendbuffer",
    "pane.lease",
    "ansi.spans",
    "questions.interaction",
    "protocol.envelope",
}
KNOWN_SUITE_PREFIXES = ("conversation.page.",)


def validate_file(path: Path, errors: list[str]) -> None:
    rel = path.name
    try:
        doc = json.loads(path.read_text(encoding="utf-8"))
    except Exception as exc:  # noqa: BLE001 - report any parse failure
        errors.append(f"{rel}: invalid JSON: {exc}")
        return

    missing = REQUIRED_ENVELOPE - doc.keys()
    if missing:
        errors.append(f"{rel}: missing envelope keys {sorted(missing)}")
        return

    if doc["format_version"] != 1:
        errors.append(f"{rel}: format_version must be 1")

    suite = doc["suite"]
    if not SUITE_RE.match(suite):
        errors.append(f"{rel}: bad suite name {suite!r}")
    elif suite not in KNOWN_SUITES and not suite.startswith(KNOWN_SUITE_PREFIXES):
        errors.append(f"{rel}: unknown suite {suite!r} — extend README table")

    src = doc.get("source", {})
    miss_src = REQUIRED_SOURCE - src.keys()
    if miss_src:
        errors.append(f"{rel}: source missing {sorted(miss_src)}")
    elif not SHA_RE.match(str(src.get("commit", ""))):
        errors.append(f"{rel}: source.commit must be a full sha")

    bad_keys = FORBIDDEN_TOP & doc.keys()
    if bad_keys:
        errors.append(f"{rel}: forbidden keys {sorted(bad_keys)} — must be deterministic")

    vectors = doc["vectors"]
    if not isinstance(vectors, list) or not vectors:
        errors.append(f"{rel}: vectors must be a non-empty list")
        return
    names = [v.get("name") for v in vectors if isinstance(v, dict)]
    if len(names) != len(vectors) or any(not isinstance(n, str) or not n for n in names):
        errors.append(f"{rel}: every vector needs a non-empty 'name'")
    elif len(set(names)) != len(names):
        errors.append(f"{rel}: duplicate vector names: "
                      f"{sorted(n for n in names if names.count(n) > 1)}")


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else "fixtures")
    errors: list[str] = []
    files = sorted(root.rglob("*.json"))
    readme = root / "README.md"
    if not readme.exists():
        errors.append("fixtures/README.md missing")
    for f in files:
        validate_file(f, errors)
    if not files:
        print("no fixture files found — nothing to validate (ok pre-generation)")
    for e in errors:
        print(f"ERROR {e}")
    print(f"validated {len(files)} suite file(s), {len(errors)} error(s)")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
