#!/usr/bin/env python3
"""Validate the Agent-owned Problem RETAIN Docker volume identity.

The helper consumes the unmodified JSON returned by ``docker volume inspect``.
It deliberately projects only the closed set of labels written by the Agent;
node-local mount paths are printed for the caller but are never persisted in a
portable backup.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
from typing import Any, Sequence


IDENTITY_SCHEMA = "ojos.dev/retained-volume-identity/v1"
STANDARD_PROFILE_SHA256 = (
    "sha256:56c8ec1e421205dbebb97ad40cbda30bf468d198dd8c3fc50151e39465ea573f"
)
EXPECTED_SERVICE_ID = "problem-service"
EXPECTED_LOGICAL_NAME = "problem-packages"
EXPECTED_TARGET = "/data/ojos/problems"
EXPECTED_LABELS = {
    "ojos.managed_by": "orchestrator-agent",
    "ojos.service_id": EXPECTED_SERVICE_ID,
    "ojos.runtime_profile_sha256": STANDARD_PROFILE_SHA256,
    "ojos.volume_logical_name": EXPECTED_LOGICAL_NAME,
    "ojos.volume_lifecycle": "retain",
    "ojos.volume_target": EXPECTED_TARGET,
}
OWNER_LABEL = "ojos.owner_instance_id"
STABLE_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,255}$")


class IdentityError(ValueError):
    pass


def fail(message: str) -> None:
    raise IdentityError(message)


def object_no_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            fail(f"duplicate JSON key: {key!r}")
        value[key] = item
    return value


def reject_constant(value: str) -> None:
    fail(f"invalid JSON constant: {value}")


def load_inspect(path: Path) -> dict[str, Any]:
    try:
        if path.is_symlink() or not path.is_file():
            fail("inspect input must be a regular file, not a link")
        if path.stat().st_size > 1024 * 1024:
            fail("inspect input exceeds the 1 MiB safety limit")
        raw = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=object_no_duplicates,
            parse_constant=reject_constant,
        )
    except IdentityError:
        raise
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise IdentityError(f"cannot read Docker volume inspect JSON: {exc}") from exc
    if not isinstance(raw, list) or len(raw) != 1 or not isinstance(raw[0], dict):
        fail("Docker volume inspect JSON must be a one-element object array")
    return raw[0]


def canonical_digest(value: dict[str, Any]) -> str:
    encoded = json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def expected_volume_name(owner_instance_id: str) -> str:
    digest = hashlib.sha256(
        (
            owner_instance_id
            + "\0"
            + EXPECTED_SERVICE_ID
            + "\0"
            + EXPECTED_LOGICAL_NAME
        ).encode("utf-8")
    ).hexdigest()
    return "ojos-retain-" + digest[:32]


def real_directory(path: str, label: str) -> str:
    if (
        not isinstance(path, str)
        or not os.path.isabs(path)
        or "\n" in path
        or "\r" in path
    ):
        fail(f"{label} must be a one-line absolute path")
    candidate = Path(path)
    try:
        mode = candidate.lstat().st_mode
    except OSError as exc:
        raise IdentityError(f"cannot stat {label}: {exc}") from exc
    if stat.S_ISLNK(mode) or not stat.S_ISDIR(mode):
        fail(f"{label} must be a real directory, not a link")
    resolved = os.path.realpath(path)
    if os.path.normcase(resolved) != os.path.normcase(os.path.normpath(path)):
        fail(f"{label} must be canonical and contain no symlinked path components")
    return resolved


def validate(args: argparse.Namespace) -> None:
    if not STABLE_RE.fullmatch(args.owner_instance_id):
        fail("expected owner_instance_id is invalid")
    inspected = load_inspect(Path(args.inspect))
    name = inspected.get("Name")
    expected_name = expected_volume_name(args.owner_instance_id)
    if name != expected_name:
        fail(f"volume name does not match the Agent-derived identity: {name!r}")
    if inspected.get("Driver") != "local" or inspected.get("Scope") != "local":
        fail("retained volume must use Docker's local driver and local scope")
    if inspected.get("Options") not in (None, {}):
        fail("retained volume must not carry driver options")
    labels = inspected.get("Labels")
    expected_labels = dict(EXPECTED_LABELS)
    expected_labels[OWNER_LABEL] = args.owner_instance_id
    if labels != expected_labels:
        fail("Docker volume labels do not exactly match the Agent ownership contract")
    mountpoint = real_directory(inspected.get("Mountpoint"), "Docker volume Mountpoint")
    if args.root is not None:
        root = real_directory(args.root, "retained volume root")
        if root != mountpoint:
            fail("retained volume root does not match Docker's inspected Mountpoint")

    stable = {
        "schema_version": IDENTITY_SCHEMA,
        "volume_name": expected_name,
        "driver": "local",
        "scope": "local",
        "labels": expected_labels,
    }
    identity = dict(stable)
    identity["identity_sha256"] = canonical_digest(stable)
    encoded = json.dumps(identity, indent=2, sort_keys=True) + "\n"
    if args.output:
        output = Path(args.output)
        try:
            if output.exists() or output.is_symlink():
                fail("identity output already exists")
            descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
                handle.write(encoded)
                handle.flush()
                os.fsync(handle.fileno())
        except IdentityError:
            raise
        except OSError as exc:
            raise IdentityError(f"cannot write identity output: {exc}") from exc
    if args.print_mountpoint:
        print(mountpoint)
    elif not args.output:
        print(encoded, end="")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inspect", required=True)
    parser.add_argument("--owner-instance-id", required=True)
    parser.add_argument("--root")
    parser.add_argument("--output")
    parser.add_argument("--print-mountpoint", action="store_true")
    parser.set_defaults(handler=validate)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        args.handler(args)
    except IdentityError as exc:
        print(f"retained-volume: ERROR: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
