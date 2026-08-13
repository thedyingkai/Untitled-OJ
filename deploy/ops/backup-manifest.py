#!/usr/bin/env python3
"""Create and verify OJOS full-stack backup manifests.

This utility intentionally uses only the Python standard library.  The tree
digest used by the manifest is deterministic: for every regular file, sorted
by its POSIX relative path, hash

    UTF8(path) NUL ASCII(size) NUL raw_sha256 LF

Directories are not part of the digest.  Symbolic links and other special
files are rejected instead of followed.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
import tarfile
from typing import Any, Iterable, Sequence


SCHEMA_VERSION = "ojos.dev/full-stack-backup/v2"
MANIFEST_NAME = "manifest.json"
CHECKSUMS_NAME = "SHA256SUMS"
DATABASES = ("orchestrator", "auth", "problem", "judge", "user")
TREE_INVENTORY_SCHEMA = "ojos.dev/tree-inventory/v1"
RETAINED_IDENTITY_SCHEMA = "ojos.dev/retained-volume-identity/v1"
PROBLEM_SERVICE_ID = "problem-service"
PROBLEM_VOLUME_LOGICAL_NAME = "problem-packages"
PROBLEM_VOLUME_TARGET = "/data/ojos/problems"
STANDARD_PROFILE_SHA256 = (
    "sha256:56c8ec1e421205dbebb97ad40cbda30bf468d198dd8c3fc50151e39465ea573f"
)
IDENTIFIER_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,159}$")
BUCKET_RE = re.compile(r"^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
SHA256_LINE_RE = re.compile(r"^([0-9a-f]{64}) ([ *])(.+)$")
MAX_MANIFEST_BYTES = 4 * 1024 * 1024
MAX_CHECKSUMS_BYTES = 16 * 1024 * 1024
MAX_INVENTORY_BYTES = 64 * 1024 * 1024


class ManifestError(ValueError):
    """A backup is invalid or unsafe."""


def _fail(message: str) -> None:
    raise ManifestError(message)


def _json_object(pairs: Sequence[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            _fail(f"duplicate JSON key: {key!r}")
        result[key] = value
    return result


def _reject_json_constant(value: str) -> None:
    _fail(f"invalid JSON constant: {value}")


def _load_json_text(text: str, label: str) -> Any:
    try:
        return json.loads(
            text,
            object_pairs_hook=_json_object,
            parse_constant=_reject_json_constant,
        )
    except ManifestError:
        raise
    except (json.JSONDecodeError, UnicodeError) as exc:
        raise ManifestError(f"{label} is not valid JSON: {exc}") from exc


def _read_limited_utf8(path: Path, limit: int, label: str) -> str:
    try:
        size = path.stat(follow_symlinks=False).st_size
    except OSError as exc:
        raise ManifestError(f"cannot stat {label}: {exc}") from exc
    if size > limit:
        _fail(f"{label} exceeds the {limit}-byte safety limit")
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        raise ManifestError(f"cannot read {label}: {exc}") from exc


def _expect_object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        _fail(f"{label} must be an object")
    return value


def _expect_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    actual = set(value)
    if actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        _fail(f"{label} keys do not match schema (missing={missing}, extra={extra})")


def _expect_bool(value: Any, label: str) -> bool:
    if type(value) is not bool:
        _fail(f"{label} must be a boolean")
    return value


def _expect_nonnegative_int(value: Any, label: str) -> int:
    if type(value) is not int or value < 0:
        _fail(f"{label} must be a non-negative integer")
    return value


def _expect_string(value: Any, label: str) -> str:
    if not isinstance(value, str):
        _fail(f"{label} must be a string")
    return value


def _expect_nullable_string(value: Any, label: str) -> str | None:
    if value is None:
        return None
    return _expect_string(value, label)


def _validate_identifier(value: str, label: str) -> str:
    if not IDENTIFIER_RE.fullmatch(value):
        _fail(f"{label} is invalid")
    return value


def _validate_sha256(value: Any, label: str) -> str:
    digest = _expect_string(value, label)
    if not SHA256_RE.fullmatch(digest):
        _fail(f"{label} must be a lowercase SHA-256 digest")
    return digest


def _validate_created_at(value: Any) -> str:
    timestamp = _expect_string(value, "created_at")
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", timestamp):
        _fail("created_at must be a second-precision UTC timestamp")
    try:
        dt.datetime.strptime(timestamp, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError as exc:
        raise ManifestError(f"created_at is invalid: {exc}") from exc
    return timestamp


def _portable_relative_path(raw: str, label: str, *, checksum_path: bool = False) -> str:
    if not isinstance(raw, str) or not raw:
        _fail(f"{label} must be a non-empty relative path")
    if checksum_path and raw.startswith("./"):
        raw = raw[2:]
    if not raw or raw.startswith(("/", "\\")) or "\\" in raw:
        _fail(f"{label} is absolute or non-portable: {raw!r}")
    if re.match(r"^[A-Za-z]:", raw):
        _fail(f"{label} is drive-qualified: {raw!r}")
    if any(ord(character) < 32 or ord(character) == 127 for character in raw):
        _fail(f"{label} contains a control character")
    path = PurePosixPath(raw)
    if raw.endswith("/") or any(part in ("", ".", "..") for part in path.parts):
        _fail(f"{label} is not canonical or escapes its root: {raw!r}")
    normalized = path.as_posix()
    if normalized != raw or normalized in (MANIFEST_NAME, CHECKSUMS_NAME):
        if normalized in (MANIFEST_NAME, CHECKSUMS_NAME):
            return normalized
        _fail(f"{label} is not canonical: {raw!r}")
    return normalized


def _tar_relative_path(raw: str, label: str) -> str:
    if not isinstance(raw, str) or not raw:
        _fail(f"{label} is empty")
    if raw.startswith(("/", "\\")) or "\\" in raw or re.match(r"^[A-Za-z]:", raw):
        _fail(f"{label} is absolute or non-portable: {raw!r}")
    if any(ord(character) < 32 or ord(character) == 127 for character in raw):
        _fail(f"{label} contains a control character")
    parts = list(PurePosixPath(raw).parts)
    while parts and parts[0] == ".":
        parts.pop(0)
    if any(part in ("", ".", "..") for part in parts):
        _fail(f"{label} escapes its root or is not canonical: {raw!r}")
    if not parts:
        return "."
    normalized = PurePosixPath(*parts).as_posix()
    _portable_relative_path(normalized, label)
    return normalized


def _require_real_directory(path: Path, label: str) -> Path:
    try:
        mode = path.lstat().st_mode
    except OSError as exc:
        raise ManifestError(f"cannot stat {label}: {exc}") from exc
    if stat.S_ISLNK(mode) or not stat.S_ISDIR(mode):
        _fail(f"{label} must be a real directory, not a link")
    return path


def _require_regular_file(path: Path, label: str) -> None:
    try:
        mode = path.lstat().st_mode
    except OSError as exc:
        raise ManifestError(f"cannot stat {label}: {exc}") from exc
    if stat.S_ISLNK(mode) or not stat.S_ISREG(mode):
        _fail(f"{label} must be a regular file, not a link or special file")


def _sha256_file(path: Path) -> tuple[int, str]:
    _require_regular_file(path, str(path))
    digest = hashlib.sha256()
    size = 0
    try:
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        with os.fdopen(descriptor, "rb") as handle:
            while True:
                chunk = handle.read(1024 * 1024)
                if not chunk:
                    break
                size += len(chunk)
                digest.update(chunk)
    except OSError as exc:
        raise ManifestError(f"cannot hash {path}: {exc}") from exc
    return size, digest.hexdigest()


def _summary(records: Iterable[tuple[str, int, str]]) -> dict[str, Any]:
    ordered = sorted(records, key=lambda record: record[0])
    digest = hashlib.sha256()
    total_bytes = 0
    seen: set[str] = set()
    for path, size, file_digest in ordered:
        if path in seen:
            _fail(f"duplicate regular file path in tree: {path}")
        seen.add(path)
        total_bytes += size
        digest.update(path.encode("utf-8"))
        digest.update(b"\0")
        digest.update(str(size).encode("ascii"))
        digest.update(b"\0")
        digest.update(bytes.fromhex(file_digest))
        digest.update(b"\n")
    return {
        "regular_files": len(ordered),
        "bytes": total_bytes,
        "sha256": digest.hexdigest(),
    }


def _walk_tree(root: Path) -> list[tuple[str, int, str]]:
    _require_real_directory(root, str(root))
    records: list[tuple[str, int, str]] = []

    def visit(directory: Path, prefix: PurePosixPath | None) -> None:
        try:
            entries = sorted(os.scandir(directory), key=lambda entry: entry.name)
        except OSError as exc:
            raise ManifestError(f"cannot scan {directory}: {exc}") from exc
        for entry in entries:
            relative = entry.name if prefix is None else (prefix / entry.name).as_posix()
            relative = _portable_relative_path(relative, f"tree path under {root}")
            try:
                metadata = entry.stat(follow_symlinks=False)
            except OSError as exc:
                raise ManifestError(f"cannot stat {entry.path}: {exc}") from exc
            mode = metadata.st_mode
            if stat.S_ISLNK(mode):
                _fail(f"tree contains a symbolic link: {relative}")
            if stat.S_ISDIR(mode):
                visit(Path(entry.path), PurePosixPath(relative))
            elif stat.S_ISREG(mode):
                size, file_digest = _sha256_file(Path(entry.path))
                records.append((relative, size, file_digest))
            else:
                _fail(f"tree contains a special file: {relative}")

    visit(root, None)
    return sorted(records, key=lambda record: record[0])


def tree_summary(root: Path) -> dict[str, Any]:
    return _summary(_walk_tree(root))


def tree_inventory(root: Path) -> dict[str, Any]:
    records = _walk_tree(root)
    return {
        "schema_version": TREE_INVENTORY_SCHEMA,
        "tree": _summary(records),
        "files": [
            {"path": path, "bytes": size, "sha256": digest}
            for path, size, digest in records
        ],
    }


def _validate_inventory(value: Any, label: str) -> dict[str, Any]:
    inventory = _expect_object(value, label)
    _expect_keys(inventory, {"schema_version", "tree", "files"}, label)
    if inventory["schema_version"] != TREE_INVENTORY_SCHEMA:
        _fail(f"{label}.schema_version is unsupported")
    tree = _validate_summary(inventory["tree"], f"{label}.tree")
    files = inventory["files"]
    if not isinstance(files, list):
        _fail(f"{label}.files must be an array")
    records: list[tuple[str, int, str]] = []
    normalized_files: list[dict[str, Any]] = []
    seen: set[str] = set()
    for index, raw in enumerate(files):
        record = _expect_object(raw, f"{label}.files[{index}]")
        _expect_keys(record, {"path", "bytes", "sha256"}, f"{label}.files[{index}]")
        path = _portable_relative_path(record["path"], f"{label}.files[{index}].path")
        if path in seen:
            _fail(f"{label} contains duplicate path: {path}")
        seen.add(path)
        size = _expect_nonnegative_int(record["bytes"], f"{label} {path}.bytes")
        digest = _validate_sha256(record["sha256"], f"{label} {path}.sha256")
        records.append((path, size, digest))
        normalized_files.append({"path": path, "bytes": size, "sha256": digest})
    if [record[0] for record in records] != sorted(seen):
        _fail(f"{label}.files must be sorted by path")
    if _summary(records) != tree:
        _fail(f"{label}.tree is not the digest of its exact file inventory")
    return {
        "schema_version": TREE_INVENTORY_SCHEMA,
        "tree": tree,
        "files": normalized_files,
    }


def _load_inventory(path: Path, label: str) -> dict[str, Any]:
    _require_regular_file(path, label)
    return _validate_inventory(
        _load_json_text(_read_limited_utf8(path, MAX_INVENTORY_BYTES, label), label),
        label,
    )


def _validate_retained_identity(value: Any, label: str) -> dict[str, Any]:
    identity = _expect_object(value, label)
    _expect_keys(
        identity,
        {"schema_version", "volume_name", "driver", "scope", "labels", "identity_sha256"},
        label,
    )
    if identity["schema_version"] != RETAINED_IDENTITY_SCHEMA:
        _fail(f"{label}.schema_version is unsupported")
    name = _expect_string(identity["volume_name"], f"{label}.volume_name")
    if not re.fullmatch(r"ojos-retain-[0-9a-f]{32}", name):
        _fail(f"{label}.volume_name is not an Agent-derived RETAIN volume name")
    if identity["driver"] != "local" or identity["scope"] != "local":
        _fail(f"{label} must use Docker's local driver and scope")
    labels = _expect_object(identity["labels"], f"{label}.labels")
    expected_label_keys = {
        "ojos.managed_by",
        "ojos.service_id",
        "ojos.runtime_profile_sha256",
        "ojos.volume_logical_name",
        "ojos.volume_lifecycle",
        "ojos.owner_instance_id",
        "ojos.volume_target",
    }
    _expect_keys(labels, expected_label_keys, f"{label}.labels")
    expected_values = {
        "ojos.managed_by": "orchestrator-agent",
        "ojos.service_id": PROBLEM_SERVICE_ID,
        "ojos.runtime_profile_sha256": STANDARD_PROFILE_SHA256,
        "ojos.volume_logical_name": PROBLEM_VOLUME_LOGICAL_NAME,
        "ojos.volume_lifecycle": "retain",
        "ojos.volume_target": PROBLEM_VOLUME_TARGET,
    }
    for key, expected in expected_values.items():
        if labels.get(key) != expected:
            _fail(f"{label}.labels[{key!r}] does not match the runtime contract")
    owner = _expect_string(labels["ojos.owner_instance_id"], f"{label}.owner_instance_id")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,255}", owner):
        _fail(f"{label}.owner_instance_id is invalid")
    digest = hashlib.sha256(
        f"{owner}\0{PROBLEM_SERVICE_ID}\0{PROBLEM_VOLUME_LOGICAL_NAME}".encode("utf-8")
    ).hexdigest()
    if name != f"ojos-retain-{digest[:32]}":
        _fail(f"{label}.volume_name does not match its stable owner identity")
    stable = {key: identity[key] for key in identity if key != "identity_sha256"}
    actual_identity_digest = hashlib.sha256(
        json.dumps(stable, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode(
            "utf-8"
        )
    ).hexdigest()
    if _validate_sha256(identity["identity_sha256"], f"{label}.identity_sha256") != actual_identity_digest:
        _fail(f"{label}.identity_sha256 does not bind the stable identity")
    return identity


def _load_retained_identity(path: Path, label: str) -> dict[str, Any]:
    _require_regular_file(path, label)
    return _validate_retained_identity(
        _load_json_text(_read_limited_utf8(path, MAX_MANIFEST_BYTES, label), label), label
    )


def _tar_records(archive: Path) -> list[tuple[str, int, str]]:
    _require_regular_file(archive, str(archive))
    records: list[tuple[str, int, str]] = []
    seen: set[str] = set()
    try:
        with tarfile.open(archive, mode="r:*") as bundle:
            for member in bundle:
                relative = _tar_relative_path(member.name, "tar member path")
                if relative in seen:
                    _fail(f"tar contains a duplicate member path: {relative}")
                seen.add(relative)
                if member.isdir():
                    continue
                if not member.isreg():
                    _fail(
                        "tar contains a link, device, FIFO, or other unsafe member: "
                        f"{member.name!r}"
                    )
                if relative == ".":
                    _fail("tar root member cannot be a regular file")
                stream = bundle.extractfile(member)
                if stream is None:
                    _fail(f"cannot read regular tar member: {member.name!r}")
                digest = hashlib.sha256()
                size = 0
                with stream:
                    while True:
                        chunk = stream.read(1024 * 1024)
                        if not chunk:
                            break
                        size += len(chunk)
                        digest.update(chunk)
                if size != member.size:
                    _fail(f"tar member size mismatch: {member.name!r}")
                records.append((relative, size, digest.hexdigest()))
    except ManifestError:
        raise
    except (OSError, tarfile.TarError) as exc:
        raise ManifestError(f"cannot read tar archive {archive}: {exc}") from exc
    return sorted(records, key=lambda record: record[0])


def tar_summary(archive: Path) -> dict[str, Any]:
    return _summary(_tar_records(archive))


def _inventory_records(inventory: dict[str, Any]) -> list[tuple[str, int, str]]:
    return [
        (record["path"], record["bytes"], record["sha256"])
        for record in inventory["files"]
    ]


def _compare_records(
    actual: list[tuple[str, int, str]], expected: list[tuple[str, int, str]], label: str
) -> None:
    if actual != expected:
        actual_paths = {record[0] for record in actual}
        expected_paths = {record[0] for record in expected}
        missing = sorted(expected_paths - actual_paths)
        extra = sorted(actual_paths - expected_paths)
        changed = sorted(
            path
            for path in actual_paths & expected_paths
            if next(record[1:] for record in actual if record[0] == path)
            != next(record[1:] for record in expected if record[0] == path)
        )
        _fail(
            f"{label} does not match exact inventory "
            f"(missing={missing}, extra={extra}, changed={changed})"
        )


def _validate_summary(value: Any, label: str) -> dict[str, Any]:
    summary = _expect_object(value, label)
    _expect_keys(summary, {"regular_files", "bytes", "sha256"}, label)
    return {
        "regular_files": _expect_nonnegative_int(
            summary["regular_files"], f"{label}.regular_files"
        ),
        "bytes": _expect_nonnegative_int(summary["bytes"], f"{label}.bytes"),
        "sha256": _validate_sha256(summary["sha256"], f"{label}.sha256"),
    }


def _compare_summary(actual: dict[str, Any], expected: Any, label: str) -> None:
    normalized = _validate_summary(expected, label)
    if actual != normalized:
        _fail(f"{label} does not match (expected={normalized}, actual={actual})")


def _record(path: str, root: Path) -> dict[str, Any]:
    normalized = _portable_relative_path(path, "payload path")
    size, digest = _sha256_file(root / Path(*PurePosixPath(normalized).parts))
    return {"path": normalized, "bytes": size, "sha256": digest}


def _all_regular_files(root: Path) -> list[tuple[str, int, str]]:
    return _walk_tree(root)


def _component_files(
    root: Path, components: dict[str, Any], *, verify_summaries: bool
) -> set[str]:
    expected: set[str] = set()

    postgres = _expect_object(components["postgres"], "components.postgres")
    _expect_keys(postgres, {"databases"}, "components.postgres")
    databases = postgres["databases"]
    if not isinstance(databases, list) or len(databases) != len(DATABASES):
        _fail("components.postgres.databases must declare exactly five databases")
    names: list[str] = []
    for index, raw_database in enumerate(databases):
        database = _expect_object(raw_database, f"components.postgres.databases[{index}]")
        _expect_keys(database, {"name", "dump", "dump_list"}, f"database[{index}]")
        name = _expect_string(database["name"], f"database[{index}].name")
        names.append(name)
        dump = _portable_relative_path(database["dump"], f"database[{index}].dump")
        dump_list = _portable_relative_path(
            database["dump_list"], f"database[{index}].dump_list"
        )
        if dump != f"postgres/{name}.dump" or dump_list != f"postgres/{name}.dump.list":
            _fail(f"database {name!r} uses non-canonical payload paths")
        expected.update((dump, dump_list))
    if tuple(names) != DATABASES:
        _fail(f"database declarations must be ordered as {list(DATABASES)}")

    redis = _expect_object(components["redis"], "components.redis")
    _expect_keys(redis, {"included", "excluded_reason", "rdb", "check"}, "components.redis")
    redis_included = _expect_bool(redis["included"], "components.redis.included")
    redis_reason = _expect_nullable_string(
        redis["excluded_reason"], "components.redis.excluded_reason"
    )
    redis_rdb = _expect_nullable_string(redis["rdb"], "components.redis.rdb")
    redis_check = _expect_nullable_string(redis["check"], "components.redis.check")
    if redis_included:
        if redis_reason is not None or redis_rdb != "redis/dump.rdb":
            _fail("included Redis must declare redis/dump.rdb and no exclusion reason")
        expected.add(redis_rdb)
        if redis_check is not None:
            redis_check = _portable_relative_path(redis_check, "components.redis.check")
            if redis_check != "redis/dump.rdb.check":
                _fail("Redis check uses a non-canonical payload path")
            expected.add(redis_check)
    elif not (
        redis_reason == "explicitly_excluded" and redis_rdb is None and redis_check is None
    ):
        _fail("excluded Redis must carry explicitly_excluded and no payload paths")

    storage = _expect_object(components["storage"], "components.storage")
    _expect_keys(storage, {"local", "minio"}, "components.storage")
    local = _expect_object(storage["local"], "components.storage.local")
    _expect_keys(local, {"included", "excluded_reason", "archive", "tree"}, "local storage")
    local_included = _expect_bool(local["included"], "components.storage.local.included")
    local_reason = _expect_nullable_string(
        local["excluded_reason"], "components.storage.local.excluded_reason"
    )
    local_archive = _expect_nullable_string(local["archive"], "components.storage.local.archive")
    if local_included:
        if local_reason is not None or local_archive != "storage/storage-root.tar.gz":
            _fail("included local storage must declare its canonical archive")
        local_tree = _validate_summary(local["tree"], "components.storage.local.tree")
        expected.add(local_archive)
        if verify_summaries:
            _compare_summary(tar_summary(root / local_archive), local_tree, "local archive tree")
    elif not (
        local_reason == "explicitly_excluded"
        and local_archive is None
        and local["tree"] is None
    ):
        _fail("excluded local storage must carry explicitly_excluded and no payload")

    minio = _expect_object(storage["minio"], "components.storage.minio")
    _expect_keys(minio, {"included", "excluded_reason", "buckets"}, "MinIO storage")
    minio_included = _expect_bool(minio["included"], "components.storage.minio.included")
    minio_reason = _expect_nullable_string(
        minio["excluded_reason"], "components.storage.minio.excluded_reason"
    )
    buckets = minio["buckets"]
    if not isinstance(buckets, list):
        _fail("components.storage.minio.buckets must be an array")
    if minio_included:
        if minio_reason is not None or not buckets:
            _fail("included MinIO must declare at least one bucket and no exclusion reason")
        bucket_names: list[str] = []
        bucket_root = root / "storage" / "minio"
        _require_real_directory(bucket_root, "MinIO payload root")
        for index, raw_bucket in enumerate(buckets):
            bucket = _expect_object(raw_bucket, f"MinIO bucket[{index}]")
            _expect_keys(bucket, {"name", "tree"}, f"MinIO bucket[{index}]")
            name = _expect_string(bucket["name"], f"MinIO bucket[{index}].name")
            if not BUCKET_RE.fullmatch(name):
                _fail(f"invalid MinIO bucket name: {name!r}")
            bucket_names.append(name)
            bucket_dir = bucket_root / name
            records = _walk_tree(bucket_dir)
            expected.update(f"storage/minio/{name}/{path}" for path, _, _ in records)
            if verify_summaries:
                _compare_summary(_summary(records), bucket["tree"], f"MinIO bucket {name} tree")
            else:
                _validate_summary(bucket["tree"], f"MinIO bucket {name} tree")
        if bucket_names != sorted(set(bucket_names)):
            _fail("MinIO bucket declarations must be unique and sorted")
        try:
            actual_bucket_entries = sorted(os.scandir(bucket_root), key=lambda entry: entry.name)
        except OSError as exc:
            raise ManifestError(f"cannot scan MinIO payload root: {exc}") from exc
        actual_bucket_names: list[str] = []
        for entry in actual_bucket_entries:
            if entry.is_symlink() or not entry.is_dir(follow_symlinks=False):
                _fail(f"MinIO payload root contains a non-directory entry: {entry.name!r}")
            actual_bucket_names.append(entry.name)
        if actual_bucket_names != bucket_names:
            _fail(
                "MinIO bucket directories do not match declarations "
                f"(declared={bucket_names}, actual={actual_bucket_names})"
            )
    elif not (minio_reason == "explicitly_excluded" and buckets == []):
        _fail("excluded MinIO must carry explicitly_excluded and no buckets")

    retained = _expect_object(
        components["problem_retained_volume"], "components.problem_retained_volume"
    )
    _expect_keys(
        retained,
        {
            "service_id",
            "logical_name",
            "target",
            "archive",
            "identity",
            "inventory",
            "identity_sha256",
            "tree",
        },
        "components.problem_retained_volume",
    )
    if (
        retained["service_id"] != PROBLEM_SERVICE_ID
        or retained["logical_name"] != PROBLEM_VOLUME_LOGICAL_NAME
        or retained["target"] != PROBLEM_VOLUME_TARGET
    ):
        _fail("Problem retained volume does not match the closed runtime contract")
    archive = _portable_relative_path(retained["archive"], "retained volume archive")
    identity_path = _portable_relative_path(retained["identity"], "retained volume identity")
    inventory_path = _portable_relative_path(
        retained["inventory"], "retained volume inventory"
    )
    if (
        archive != "retained/problem-packages.tar.gz"
        or identity_path != "retained/problem-packages.identity.json"
        or inventory_path != "retained/problem-packages.inventory.json"
    ):
        _fail("Problem retained volume uses non-canonical payload paths")
    identity = _load_retained_identity(root / identity_path, "retained volume identity")
    identity_digest = _validate_sha256(
        retained["identity_sha256"], "retained volume identity_sha256"
    )
    if identity["identity_sha256"] != identity_digest:
        _fail("retained volume component identity does not match its identity document")
    inventory = _load_inventory(root / inventory_path, "retained volume inventory")
    retained_tree = _validate_summary(retained["tree"], "retained volume tree")
    if inventory["tree"] != retained_tree:
        _fail("retained volume component tree does not match its exact inventory")
    if verify_summaries:
        archive_records = _tar_records(root / archive)
        _compare_summary(_summary(archive_records), retained_tree, "retained volume archive")
        _compare_records(
            archive_records,
            _inventory_records(inventory),
            "retained volume archive",
        )
    expected.update((archive, identity_path, inventory_path))

    return expected


def _parse_payload_files(value: Any) -> tuple[list[dict[str, Any]], dict[str, dict[str, Any]]]:
    if not isinstance(value, list):
        _fail("payload_files must be an array")
    normalized: list[dict[str, Any]] = []
    by_path: dict[str, dict[str, Any]] = {}
    for index, raw_record in enumerate(value):
        record = _expect_object(raw_record, f"payload_files[{index}]")
        _expect_keys(record, {"path", "bytes", "sha256"}, f"payload_files[{index}]")
        path = _portable_relative_path(record["path"], f"payload_files[{index}].path")
        if path in (MANIFEST_NAME, CHECKSUMS_NAME):
            _fail(f"payload_files cannot include control file {path}")
        if path in by_path:
            _fail(f"duplicate payload file path: {path}")
        parsed = {
            "path": path,
            "bytes": _expect_nonnegative_int(record["bytes"], f"payload {path}.bytes"),
            "sha256": _validate_sha256(record["sha256"], f"payload {path}.sha256"),
        }
        normalized.append(parsed)
        by_path[path] = parsed
    if [item["path"] for item in normalized] != sorted(by_path):
        _fail("payload_files must be sorted by path")
    return normalized, by_path


def _validate_manifest(value: Any) -> dict[str, Any]:
    manifest = _expect_object(value, "manifest")
    _expect_keys(
        manifest,
        {
            "schema_version",
            "created_at",
            "environment",
            "source_id",
            "fence_id_sha256",
            "components",
            "payload_files",
        },
        "manifest",
    )
    if manifest["schema_version"] != SCHEMA_VERSION:
        _fail(f"unsupported schema_version: {manifest['schema_version']!r}")
    _validate_created_at(manifest["created_at"])
    _validate_identifier(_expect_string(manifest["environment"], "environment"), "environment")
    _validate_identifier(_expect_string(manifest["source_id"], "source_id"), "source_id")
    _validate_sha256(manifest["fence_id_sha256"], "fence_id_sha256")
    components = _expect_object(manifest["components"], "components")
    _expect_keys(
        components,
        {"postgres", "redis", "storage", "problem_retained_volume"},
        "components",
    )
    _parse_payload_files(manifest["payload_files"])
    return manifest


def _load_manifest(root: Path) -> dict[str, Any]:
    path = root / MANIFEST_NAME
    _require_regular_file(path, MANIFEST_NAME)
    return _validate_manifest(
        _load_json_text(_read_limited_utf8(path, MAX_MANIFEST_BYTES, MANIFEST_NAME), MANIFEST_NAME)
    )


def _parse_checksums(root: Path) -> dict[str, str]:
    path = root / CHECKSUMS_NAME
    _require_regular_file(path, CHECKSUMS_NAME)
    text = _read_limited_utf8(path, MAX_CHECKSUMS_BYTES, CHECKSUMS_NAME)
    if "\r" in text or not text.endswith("\n"):
        _fail("SHA256SUMS must use LF lines and end with a newline")
    checksums: dict[str, str] = {}
    for line_number, line in enumerate(text[:-1].split("\n"), start=1):
        if not line:
            _fail(f"SHA256SUMS contains an empty line at {line_number}")
        if line.startswith("\\"):
            _fail("SHA256SUMS escaped filename syntax is not supported")
        match = SHA256_LINE_RE.fullmatch(line)
        if match is None:
            _fail(f"SHA256SUMS line {line_number} is malformed")
        digest, _, raw_path = match.groups()
        normalized = _portable_relative_path(
            raw_path, f"SHA256SUMS line {line_number} path", checksum_path=True
        )
        if normalized == CHECKSUMS_NAME:
            _fail("SHA256SUMS must not list itself")
        if normalized in checksums:
            _fail(f"SHA256SUMS contains duplicate path: {normalized}")
        checksums[normalized] = digest
    return checksums


def _actual_records(root: Path) -> dict[str, dict[str, Any]]:
    return {
        path: {"path": path, "bytes": size, "sha256": digest}
        for path, size, digest in _all_regular_files(root)
    }


def create_manifest(args: argparse.Namespace) -> None:
    root = _require_real_directory(Path(args.root), "backup root")
    environment = _validate_identifier(args.environment, "environment")
    source_id = _validate_identifier(args.source_id, "source_id")
    created_at = _validate_created_at(args.created_at)
    fence_digest = _validate_sha256(args.fence_id_sha256, "fence_id_sha256")
    redis_included = args.redis
    local_included = args.local_storage
    minio_included = args.minio
    retained_source = Path(args.problem_retained_volume_source)

    if (root / MANIFEST_NAME).exists() or (root / CHECKSUMS_NAME).exists():
        _fail("create requires a root without manifest.json or SHA256SUMS")

    databases = [
        {
            "name": name,
            "dump": f"postgres/{name}.dump",
            "dump_list": f"postgres/{name}.dump.list",
        }
        for name in DATABASES
    ]

    redis_check_path = root / "redis" / "dump.rdb.check"
    redis = {
        "included": redis_included,
        "excluded_reason": None if redis_included else "explicitly_excluded",
        "rdb": "redis/dump.rdb" if redis_included else None,
        "check": "redis/dump.rdb.check"
        if redis_included and redis_check_path.exists()
        else None,
    }

    if local_included:
        if not args.local_storage_source:
            _fail("--local-storage-source is required when local storage is included")
        source = Path(args.local_storage_source)
        source_summary = tree_summary(source)
        archive_summary = tar_summary(root / "storage" / "storage-root.tar.gz")
        if source_summary != archive_summary:
            _fail(
                "local storage archive does not match --local-storage-source "
                f"(source={source_summary}, archive={archive_summary})"
            )
        local = {
            "included": True,
            "excluded_reason": None,
            "archive": "storage/storage-root.tar.gz",
            "tree": source_summary,
        }
    else:
        if args.local_storage_source:
            _fail("--local-storage-source is only valid when local storage is included")
        local = {
            "included": False,
            "excluded_reason": "explicitly_excluded",
            "archive": None,
            "tree": None,
        }

    raw_buckets = _load_json_text(args.buckets_json, "--buckets-json")
    if not isinstance(raw_buckets, list) or any(not isinstance(item, str) for item in raw_buckets):
        _fail("--buckets-json must be an array of bucket-name strings")
    if raw_buckets != sorted(set(raw_buckets)):
        _fail("--buckets-json must be unique and sorted")
    for bucket in raw_buckets:
        if not BUCKET_RE.fullmatch(bucket):
            _fail(f"invalid MinIO bucket name: {bucket!r}")
    if minio_included and not raw_buckets:
        _fail("included MinIO requires at least one bucket")
    if not minio_included and raw_buckets:
        _fail("excluded MinIO cannot declare buckets")
    minio_buckets = []
    if minio_included:
        for bucket in raw_buckets:
            minio_buckets.append(
                {
                    "name": bucket,
                    "tree": tree_summary(root / "storage" / "minio" / bucket),
                }
            )
    minio = {
        "included": minio_included,
        "excluded_reason": None if minio_included else "explicitly_excluded",
        "buckets": minio_buckets,
    }

    retained_identity = _load_retained_identity(
        root / "retained" / "problem-packages.identity.json",
        "retained volume identity",
    )
    retained_inventory = _load_inventory(
        root / "retained" / "problem-packages.inventory.json",
        "retained volume inventory",
    )
    source_inventory = tree_inventory(retained_source)
    if source_inventory != retained_inventory:
        _fail("retained volume inventory changed or does not match the live source tree")
    archive_records = _tar_records(root / "retained" / "problem-packages.tar.gz")
    archive_summary = _summary(archive_records)
    if archive_summary != source_inventory["tree"]:
        _fail(
            "retained volume archive does not match the live source tree "
            f"(source={source_inventory['tree']}, archive={archive_summary})"
        )
    _compare_records(
        archive_records,
        _inventory_records(source_inventory),
        "retained volume archive",
    )
    retained_component = {
        "service_id": PROBLEM_SERVICE_ID,
        "logical_name": PROBLEM_VOLUME_LOGICAL_NAME,
        "target": PROBLEM_VOLUME_TARGET,
        "archive": "retained/problem-packages.tar.gz",
        "identity": "retained/problem-packages.identity.json",
        "inventory": "retained/problem-packages.inventory.json",
        "identity_sha256": retained_identity["identity_sha256"],
        "tree": source_inventory["tree"],
    }

    components = {
        "postgres": {"databases": databases},
        "redis": redis,
        "storage": {"local": local, "minio": minio},
        "problem_retained_volume": retained_component,
    }
    expected_files = _component_files(root, components, verify_summaries=True)
    actual_records = _actual_records(root)
    if set(actual_records) != expected_files:
        _fail(
            "payload files do not match component declarations "
            f"(missing={sorted(expected_files - set(actual_records))}, "
            f"extra={sorted(set(actual_records) - expected_files)})"
        )
    payload_files = [actual_records[path] for path in sorted(actual_records)]
    manifest = {
        "schema_version": SCHEMA_VERSION,
        "created_at": created_at,
        "environment": environment,
        "source_id": source_id,
        "fence_id_sha256": fence_digest,
        "components": components,
        "payload_files": payload_files,
    }
    _validate_manifest(manifest)
    encoded = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode("utf-8")
    temporary = root / f".{MANIFEST_NAME}.{os.getpid()}.tmp"
    try:
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, root / MANIFEST_NAME)
    except OSError as exc:
        try:
            temporary.unlink()
        except OSError:
            pass
        raise ManifestError(f"cannot write {MANIFEST_NAME}: {exc}") from exc


def verify_manifest(args: argparse.Namespace) -> None:
    root = _require_real_directory(Path(args.root), "backup root")
    manifest = _load_manifest(root)
    expected_environment = _validate_identifier(args.environment, "expected environment")
    if manifest["environment"] != expected_environment:
        _fail(
            f"backup environment mismatch: expected {expected_environment!r}, "
            f"found {manifest['environment']!r}"
        )
    if args.expected_source_id is not None:
        expected_source = _validate_identifier(args.expected_source_id, "expected source_id")
        if manifest["source_id"] != expected_source:
            _fail(
                f"backup source_id mismatch: expected {expected_source!r}, "
                f"found {manifest['source_id']!r}"
            )

    _, payload_by_path = _parse_payload_files(manifest["payload_files"])
    component_paths = _component_files(root, manifest["components"], verify_summaries=True)
    if component_paths != set(payload_by_path):
        _fail(
            "payload_files do not match component declarations "
            f"(missing={sorted(component_paths - set(payload_by_path))}, "
            f"extra={sorted(set(payload_by_path) - component_paths)})"
        )

    actual = _actual_records(root)
    expected_regular = set(payload_by_path) | {MANIFEST_NAME, CHECKSUMS_NAME}
    if set(actual) != expected_regular:
        _fail(
            "backup regular file set is not exact "
            f"(missing={sorted(expected_regular - set(actual))}, "
            f"extra={sorted(set(actual) - expected_regular)})"
        )
    for path, declared in payload_by_path.items():
        if actual[path] != declared:
            _fail(f"payload metadata does not match file contents: {path}")

    checksums = _parse_checksums(root)
    expected_checksums = set(payload_by_path) | {MANIFEST_NAME}
    if set(checksums) != expected_checksums:
        _fail(
            "SHA256SUMS file set is not exact "
            f"(missing={sorted(expected_checksums - set(checksums))}, "
            f"extra={sorted(set(checksums) - expected_checksums)})"
        )
    for path, declared_digest in checksums.items():
        if actual[path]["sha256"] != declared_digest:
            _fail(f"SHA256SUMS digest mismatch: {path}")


def _expected_summary_argument(raw: str) -> dict[str, Any]:
    return _validate_summary(_load_json_text(raw, "--expected-summary-json"), "expected summary")


def verify_tar_command(args: argparse.Namespace) -> None:
    actual = tar_summary(Path(args.archive))
    _compare_summary(actual, _expected_summary_argument(args.expected_summary_json), "tar tree")
    print(json.dumps(actual, sort_keys=True, separators=(",", ":")))


def verify_tree_command(args: argparse.Namespace) -> None:
    actual = tree_summary(Path(args.root))
    _compare_summary(actual, _expected_summary_argument(args.expected_summary_json), "tree")
    print(json.dumps(actual, sort_keys=True, separators=(",", ":")))


def verify_inventory_command(args: argparse.Namespace) -> None:
    expected = _load_inventory(Path(args.inventory), "expected inventory")
    actual_records = _walk_tree(Path(args.root))
    _compare_summary(_summary(actual_records), expected["tree"], "tree")
    _compare_records(actual_records, _inventory_records(expected), "tree")
    print(json.dumps(expected["tree"], sort_keys=True, separators=(",", ":")))


def inventory_command(args: argparse.Namespace) -> None:
    output = Path(args.output)
    if output.exists() or output.is_symlink():
        _fail("inventory output already exists")
    inventory = tree_inventory(Path(args.root))
    encoded = (json.dumps(inventory, indent=2, sort_keys=True) + "\n").encode("utf-8")
    try:
        descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
    except OSError as exc:
        raise ManifestError(f"cannot write inventory: {exc}") from exc


def _boolean(value: str) -> bool:
    if value == "true":
        return True
    if value == "false":
        return False
    raise argparse.ArgumentTypeError("expected exactly 'true' or 'false'")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    create = subparsers.add_parser("create", help="create manifest.json")
    create.add_argument("--root", required=True)
    create.add_argument("--environment", required=True)
    create.add_argument("--source-id", required=True)
    create.add_argument("--created-at", required=True)
    create.add_argument("--fence-id-sha256", required=True)
    create.add_argument("--redis", required=True, type=_boolean)
    create.add_argument("--local-storage", required=True, type=_boolean)
    create.add_argument("--local-storage-source")
    create.add_argument("--minio", required=True, type=_boolean)
    create.add_argument("--buckets-json", required=True)
    create.add_argument("--problem-retained-volume-source", required=True)
    create.set_defaults(handler=create_manifest)

    verify = subparsers.add_parser("verify", help="verify a complete backup directory")
    verify.add_argument("--root", required=True)
    verify.add_argument("--environment", required=True)
    verify.add_argument("--expected-source-id")
    verify.set_defaults(handler=verify_manifest)

    verify_tar = subparsers.add_parser("verify-tar", help="safely summarize a tar archive")
    verify_tar.add_argument("--archive", required=True)
    verify_tar.add_argument("--expected-summary-json", required=True)
    verify_tar.set_defaults(handler=verify_tar_command)

    verify_tree = subparsers.add_parser("verify-tree", help="safely summarize a directory tree")
    verify_tree.add_argument("--root", required=True)
    verify_tree.add_argument("--expected-summary-json", required=True)
    verify_tree.set_defaults(handler=verify_tree_command)

    inventory = subparsers.add_parser(
        "inventory", help="write a strict regular-file inventory for a directory tree"
    )
    inventory.add_argument("--root", required=True)
    inventory.add_argument("--output", required=True)
    inventory.set_defaults(handler=inventory_command)

    verify_inventory = subparsers.add_parser(
        "verify-inventory", help="verify a directory against an exact file inventory"
    )
    verify_inventory.add_argument("--root", required=True)
    verify_inventory.add_argument("--inventory", required=True)
    verify_inventory.set_defaults(handler=verify_inventory_command)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        args.handler(args)
    except ManifestError as exc:
        print(f"backup-manifest: ERROR: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
