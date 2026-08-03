#!/usr/bin/env python3
"""Create and verify the immutable 11-primary/22-file candidate payload."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import math
import os
import pathlib
import re
import tempfile
import time
from typing import Any


class CandidateError(RuntimeError):
    pass


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def primary_names(version: str) -> list[str]:
    windows = f"ojos-orchestrator-{version}-ga-windows-x64"
    linux = f"ojos-orchestrator-{version}-ga-linux-x86_64"
    return [
        f"ojos-orchestrator-{version}-windows-x64.msi",
        f"{windows}.zip",
        f"{windows}.spdx.json",
        f"{windows}.provenance.json",
        f"{windows}.SHA256SUMS",
        f"ojos-orchestrator-{version}-linux-x86_64.deb",
        f"ojos-orchestrator-{version}-linux-x86_64.AppImage",
        f"{linux}.tar.gz",
        f"{linux}.spdx.json",
        f"{linux}.provenance.json",
        f"{linux}.SHA256SUMS",
    ]


def validate_version(value: str) -> str:
    parts = value.removeprefix("v").split(".")
    if len(parts) != 3 or any(not part.isdigit() for part in parts):
        raise CandidateError("candidate version must be a three-part semantic version")
    return ".".join(parts)


def validate_sha(value: str) -> str:
    normalized = value.strip().lower()
    if value != normalized or not re.fullmatch(r"[0-9a-f]{40}", normalized):
        raise CandidateError("candidate SHA must be 40 lowercase hexadecimal characters")
    return normalized


def validate_run_id(value: Any, label: str) -> str:
    normalized = str(value)
    if not re.fullmatch(r"[1-9][0-9]*", normalized):
        raise CandidateError(f"{label} must be a positive decimal GitHub run id")
    return normalized


def validate_run_attempt(value: Any, label: str) -> int:
    normalized = str(value)
    if normalized != "1":
        raise CandidateError(f"{label} must be exactly 1; rerun artifacts are not candidates")
    return 1


def validate_repository(value: str) -> str:
    if not re.fullmatch(
        r"[A-Za-z0-9](?:[A-Za-z0-9_.-]{0,99})/[A-Za-z0-9](?:[A-Za-z0-9_.-]{0,99})",
        value,
    ) or any(part in {".", ".."} for part in value.split("/")):
        raise CandidateError("repository must be a canonical owner/name pair")
    return value


def validate_sha256(value: Any, label: str, *, prefixed: bool = False) -> str:
    normalized = str(value)
    pattern = r"sha256:[0-9a-f]{64}" if prefixed else r"[0-9a-f]{64}"
    if not re.fullmatch(pattern, normalized):
        raise CandidateError(f"{label} must be a canonical SHA-256 digest")
    return normalized


def docker_started_at_key(value: Any, label: str) -> tuple[int, int]:
    timestamp = str(value)
    match = re.fullmatch(
        r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?Z",
        timestamp,
    )
    if match is None:
        raise CandidateError(f"{label} must be Docker RFC3339Nano")
    try:
        seconds = int(
            datetime.datetime.strptime(match.group(1), "%Y-%m-%dT%H:%M:%S")
            .replace(tzinfo=datetime.timezone.utc)
            .timestamp()
        )
    except ValueError as error:
        raise CandidateError(f"{label} must be Docker RFC3339Nano") from error
    return seconds, int((match.group(2) or "0").ljust(9, "0"))


def validate_promotion_acceptance(
    artifact: dict[str, Any],
    *,
    candidate_sha: str,
    candidate_run_id: Any,
    candidate_run_attempt: Any,
    accepted_sha: str,
    accepted_run_id: Any,
    accepted_manifest_sha256: str,
    accepted_artifact_id: Any,
    accepted_artifact_digest: str,
    actual_artifact_archive_sha256: str | None = None,
    actual_manifest_sha256: str | None = None,
) -> tuple[str, str]:
    candidate_sha = validate_sha(candidate_sha)
    candidate_run_id = validate_run_id(candidate_run_id, "candidate_run_id")
    validate_run_attempt(candidate_run_attempt, "candidate_run_attempt")
    if validate_sha(accepted_sha) != candidate_sha:
        raise CandidateError("security acceptance belongs to another candidate SHA")
    if validate_run_id(accepted_run_id, "accepted_run_id") != candidate_run_id:
        raise CandidateError("security acceptance belongs to another candidate run")
    accepted_manifest_sha256 = validate_sha256(
        accepted_manifest_sha256, "accepted_manifest_sha256"
    )
    accepted_artifact_id = validate_run_id(accepted_artifact_id, "accepted_artifact_id")
    accepted_artifact_digest = validate_sha256(
        accepted_artifact_digest, "accepted_artifact_digest", prefixed=True
    )

    artifact_id = validate_run_id(artifact.get("id"), "artifact id")
    artifact_digest = validate_sha256(
        artifact.get("digest"), "artifact server digest", prefixed=True
    )
    workflow_run = artifact.get("workflow_run")
    if (
        artifact.get("name") != "orchestrator-v1-signed-candidate"
        or artifact.get("expired") is not False
        or artifact_id != accepted_artifact_id
        or artifact_digest != accepted_artifact_digest
        or not isinstance(workflow_run, dict)
        or str(workflow_run.get("id")) != candidate_run_id
        or workflow_run.get("head_sha") != candidate_sha
    ):
        raise CandidateError("candidate artifact identity does not match protected acceptance")
    if actual_manifest_sha256 is not None and (
        validate_sha256(actual_manifest_sha256, "actual_manifest_sha256")
        != accepted_manifest_sha256
    ):
        raise CandidateError("candidate manifest digest does not match protected acceptance")
    if actual_artifact_archive_sha256 is not None and (
        f"sha256:{validate_sha256(actual_artifact_archive_sha256, 'actual_artifact_archive_sha256')}"
        != artifact_digest
    ):
        raise CandidateError("downloaded candidate archive does not match its server digest")
    return artifact_id, artifact_digest


def validate_workflow_ref(value: str) -> str:
    if value != "refs/heads/main":
        raise CandidateError("candidate workflow ref must be refs/heads/main")
    return value


def expected_payload(version: str) -> list[str]:
    primary = primary_names(version)
    return sorted(primary + [f"{name}.sigstore.json" for name in primary])


def require_exact_payload(directory: pathlib.Path, version: str) -> list[str]:
    if not directory.is_dir():
        raise CandidateError(f"payload directory does not exist: {directory}")
    entries = sorted(directory.iterdir(), key=lambda path: path.name)
    actual = [path.name for path in entries]
    expected = expected_payload(version)
    if actual != expected:
        missing = sorted(set(expected) - set(actual))
        extra = sorted(set(actual) - set(expected))
        raise CandidateError(f"payload is not the exact 22-file set; missing={missing}, extra={extra}")
    for path in entries:
        if not path.is_file() or path.is_symlink():
            raise CandidateError(f"payload entry is not a regular file: {path.name}")
        if path.stat().st_size <= 0:
            raise CandidateError(f"payload file is empty: {path.name}")
    return primary_names(version)


def require_manifest_outside_payload(
    payload_directory: pathlib.Path, manifest_path: pathlib.Path
) -> None:
    payload = payload_directory.resolve()
    manifest = manifest_path.resolve()
    if manifest == payload or manifest.is_relative_to(payload):
        raise CandidateError("candidate manifest must not be part of the 22-file payload")


def load_object(path: pathlib.Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8-sig"))
    except (OSError, json.JSONDecodeError) as error:
        raise CandidateError(f"cannot read {label}: {error}") from error
    if not isinstance(value, dict):
        raise CandidateError(f"{label} must be a JSON object")
    return value


def provenance_subject_names(version: str, platform: str) -> list[str]:
    base = f"ojos-orchestrator-{version}-ga-{platform}"
    if platform == "windows-x64":
        return [
            f"{base}.zip",
            f"ojos-orchestrator-{version}-windows-x64.msi",
            f"{base}.spdx.json",
        ]
    if platform == "linux-x86_64":
        return [
            f"{base}.tar.gz",
            f"ojos-orchestrator-{version}-linux-x86_64.deb",
            f"ojos-orchestrator-{version}-linux-x86_64.AppImage",
            f"{base}.spdx.json",
        ]
    raise CandidateError(f"unsupported candidate platform: {platform}")


def validate_checksum_manifest(
    payload_dir: pathlib.Path, version: str, platform: str
) -> None:
    base = f"ojos-orchestrator-{version}-ga-{platform}"
    checksum_path = payload_dir / f"{base}.SHA256SUMS"
    expected_names = provenance_subject_names(version, platform) + [
        f"{base}.provenance.json"
    ]
    records: dict[str, str] = {}
    try:
        lines = checksum_path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError) as error:
        raise CandidateError(f"cannot read {checksum_path.name}: {error}") from error
    for line in lines:
        match = re.fullmatch(r"([0-9a-f]{64}) [ *]([^/\\]+)", line)
        if match is None or match.group(2) in records:
            raise CandidateError(f"checksum manifest has an invalid or duplicate line: {line!r}")
        records[match.group(2)] = match.group(1)
    if set(records) != set(expected_names):
        raise CandidateError(
            f"{checksum_path.name} does not cover exactly the platform primary files"
        )
    for name in expected_names:
        if records[name] != sha256(payload_dir / name):
            raise CandidateError(f"checksum manifest digest mismatch for {name}")


def validate_provenance(
    payload_dir: pathlib.Path,
    version: str,
    platform: str,
    candidate_sha: str,
    candidate_run_id: str,
    repository: str,
    workflow_ref: str,
) -> None:
    base = f"ojos-orchestrator-{version}-ga-{platform}"
    statement = load_object(payload_dir / f"{base}.provenance.json", "provenance")
    if statement.get("_type") != "https://in-toto.io/Statement/v1" or statement.get(
        "predicateType"
    ) != "https://slsa.dev/provenance/v1":
        raise CandidateError(f"{platform} provenance is not an in-toto SLSA v1 statement")
    raw_subjects = statement.get("subject")
    if not isinstance(raw_subjects, list):
        raise CandidateError(f"{platform} provenance subject must be an array")
    subjects: dict[str, str] = {}
    for raw in raw_subjects:
        if not isinstance(raw, dict) or not isinstance(raw.get("digest"), dict):
            raise CandidateError(f"{platform} provenance contains an invalid subject")
        name = raw.get("name")
        digest = raw["digest"].get("sha256")
        if (
            not isinstance(name, str)
            or name in subjects
            or not re.fullmatch(r"[0-9a-f]{64}", str(digest))
        ):
            raise CandidateError(f"{platform} provenance contains an invalid subject")
        subjects[name] = str(digest)
    expected_subjects = provenance_subject_names(version, platform)
    if set(subjects) != set(expected_subjects):
        raise CandidateError(f"{platform} provenance subject set is not exact")
    for name in expected_subjects:
        if subjects[name] != sha256(payload_dir / name):
            raise CandidateError(f"{platform} provenance digest mismatch for {name}")

    predicate = statement.get("predicate")
    definition = predicate.get("buildDefinition") if isinstance(predicate, dict) else None
    details = predicate.get("runDetails") if isinstance(predicate, dict) else None
    if not isinstance(definition, dict) or not isinstance(details, dict):
        raise CandidateError(f"{platform} provenance omits build or run details")
    if definition.get("buildType") != "https://github.com/ojos/orchestrator/release-v1":
        raise CandidateError(f"{platform} provenance build type is invalid")
    if definition.get("externalParameters") != {
        "version": version,
        "channel": "ga",
        "platform": platform,
    }:
        raise CandidateError(f"{platform} provenance external parameters are invalid")
    dependencies = definition.get("resolvedDependencies")
    expected_dependency = {
        "uri": f"git+https://github.com/{repository}",
        "digest": {"gitCommit": candidate_sha},
    }
    if dependencies != [expected_dependency]:
        raise CandidateError(f"{platform} provenance is not bound to the candidate commit")
    expected_builder = f"{repository}/.github/workflows/release.yml@{workflow_ref}"
    if details.get("builder") != {"id": expected_builder}:
        raise CandidateError(f"{platform} provenance builder identity is invalid")
    metadata = details.get("metadata")
    if not isinstance(metadata, dict) or str(metadata.get("invocationId")) != candidate_run_id:
        raise CandidateError(f"{platform} provenance run id is invalid")


def validate_payload_metadata(
    payload_dir: pathlib.Path,
    version: str,
    candidate_sha: str,
    candidate_run_id: str,
    repository: str,
    workflow_ref: str,
) -> None:
    for platform in ("windows-x64", "linux-x86_64"):
        validate_provenance(
            payload_dir,
            version,
            platform,
            candidate_sha,
            candidate_run_id,
            repository,
            workflow_ref,
        )
        validate_checksum_manifest(payload_dir, version, platform)


def validate_authenticode_evidence(
    evidence: dict[str, Any], candidate_sha: str, expected_publisher_subject: str
) -> None:
    if evidence.get("schema_version") != 2 or evidence.get("candidate_sha") != candidate_sha:
        raise CandidateError("Authenticode evidence belongs to another commit")
    if evidence.get("expected_publisher_subject") != expected_publisher_subject:
        raise CandidateError("Authenticode evidence publisher policy is inconsistent")
    if evidence.get("timestamp_policy") != {
        "ojos_publisher": "RFC3161/SHA256",
        "retained_microsoft": "verify-original-and-report-protocol",
    }:
        raise CandidateError("Authenticode evidence does not require RFC3161/SHA256")
    raw_files = evidence.get("files")
    if not isinstance(raw_files, list) or len(raw_files) != 13:
        raise CandidateError("Authenticode evidence must contain the exact 13 packaged locations")
    records: dict[str, dict[str, Any]] = {}
    for raw in raw_files:
        if not isinstance(raw, dict) or not isinstance(raw.get("location"), str):
            raise CandidateError("Authenticode evidence contains a malformed file record")
        location = raw["location"]
        if location in records:
            raise CandidateError(f"duplicate Authenticode evidence location: {location}")
        if raw.get("file_name") != pathlib.PurePosixPath(location).name:
            raise CandidateError(f"Authenticode evidence filename mismatch at {location}")
        if raw.get("status") != "Valid" or not re.fullmatch(
            r"[0-9a-f]{64}", str(raw.get("sha256", ""))
        ):
            raise CandidateError(f"invalid Authenticode signature evidence at {location}")
        if not re.fullmatch(
            r"[0-9a-f]{64}", str(raw.get("signtool_output_sha256", ""))
        ) or raw.get("signtool_policy") != "pa/all/v":
            raise CandidateError(f"signtool verification evidence is missing at {location}")
        protocol = raw.get("timestamp_protocol")
        if protocol == "RFC3161":
            imprint_length = raw.get("timestamp_message_imprint_length")
            imprint = raw.get("timestamp_message_imprint")
            if (
                not all(
                    isinstance(raw.get(name), str) and raw[name]
                    for name in ("timestamp_subject", "timestamp_thumbprint")
                )
                or raw.get("timestamp_content_type_oid")
                != "1.2.840.113549.1.9.16.1.4"
                or not re.fullmatch(r"[0-9]+(?:\.[0-9]+)+", str(raw.get("timestamp_digest_oid", "")))
                or not isinstance(raw.get("timestamp_digest_algorithm"), str)
                or not raw["timestamp_digest_algorithm"]
                or not isinstance(imprint_length, int)
                or imprint_length <= 0
                or not re.fullmatch(rf"[0-9a-f]{{{imprint_length * 2}}}", str(imprint or ""))
                or raw.get("timestamp_token_signature_valid") is not True
                or raw.get("timestamp_parent_signature_digest_verified") is not True
            ):
                raise CandidateError(f"structured RFC3161 timestamp evidence is invalid at {location}")
        elif protocol == "AuthenticodeLegacy":
            if not all(
                isinstance(raw.get(name), str) and raw[name]
                for name in ("timestamp_subject", "timestamp_thumbprint")
            ) or any(
                raw.get(name) is not None
                for name in (
                    "timestamp_content_type_oid",
                    "timestamp_digest_oid",
                    "timestamp_message_imprint",
                    "timestamp_token_signature_valid",
                    "timestamp_parent_signature_digest_verified",
                )
            ) or (
                raw.get("timestamp_digest_algorithm") != "UNKNOWN"
                or raw.get("timestamp_message_imprint_length") != 0
            ):
                raise CandidateError(f"legacy timestamp evidence is not honest at {location}")
        elif protocol == "None":
            if any(
                raw.get(name) is not None
                for name in (
                    "timestamp_subject",
                    "timestamp_thumbprint",
                    "timestamp_content_type_oid",
                    "timestamp_digest_oid",
                    "timestamp_message_imprint",
                    "timestamp_token_signature_valid",
                    "timestamp_parent_signature_digest_verified",
                )
            ) or (
                raw.get("timestamp_digest_algorithm") != "NONE"
                or raw.get("timestamp_message_imprint_length") != 0
            ):
                raise CandidateError(f"absent timestamp evidence is not honest at {location}")
        else:
            raise CandidateError(f"timestamp protocol evidence is missing at {location}")
        records[location] = raw

    executables = (
        "ojos-orchestrator-daemon.exe",
        "ojos-orchestrator-tui.exe",
        "ojos-orchestrator-agent.exe",
        "ojos-orchestrator-desktop.exe",
    )
    publisher_locations = {
        *(f"build/{name}" for name in executables),
        *(f"portable/{name}" for name in executables),
        "msi/ojos-orchestrator-desktop.exe",
    }
    installer_locations = [
        location
        for location in records
        if location.startswith("installer/") and location.lower().endswith(".msi")
    ]
    if len(installer_locations) != 1:
        raise CandidateError("Authenticode evidence must contain exactly one signed MSI")
    publisher_locations.add(installer_locations[0])
    microsoft_locations = {
        "build/WebView2Loader.dll",
        "portable/WebView2Loader.dll",
        "msi/WebView2Loader.dll",
    }
    if set(records) != publisher_locations | microsoft_locations:
        raise CandidateError("Authenticode evidence packaged location set is not exact")
    for location in publisher_locations:
        record = records[location]
        if record.get("publisher_subject") != expected_publisher_subject or not (
            isinstance(record.get("publisher_thumbprint"), str)
            and record["publisher_thumbprint"]
        ) or (
            record.get("timestamp_protocol") != "RFC3161"
            or record.get("timestamp_digest_oid") != "2.16.840.1.101.3.4.2.1"
            or record.get("timestamp_digest_algorithm") != "SHA256"
            or record.get("timestamp_message_imprint_length") != 32
        ):
            raise CandidateError(f"publisher or signtool evidence mismatch at {location}")
    for location in microsoft_locations:
        record = records[location]
        if (
            record.get("retained_vendor_signature") != "Microsoft"
            or "Microsoft Corporation" not in str(record.get("publisher_subject", ""))
            or record.get("publisher_subject") == expected_publisher_subject
        ):
            raise CandidateError(f"retained Microsoft signature evidence mismatch at {location}")

    for name in executables:
        hashes = {
            records[f"{location}/{name}"]["sha256"]
            for location in ("build", "portable")
        }
        if len(hashes) != 1:
            raise CandidateError(f"portable packaging changed signed Windows file {name}")

    for name in ("ojos-orchestrator-desktop.exe", "WebView2Loader.dll"):
        hashes = {
            records[f"{location}/{name}"]["sha256"]
            for location in ("build", "portable", "msi")
        }
        if len(hashes) != 1:
            raise CandidateError(f"packaging changed signed Windows file {name}")


def validate_capacity_environment_evidence(
    capacity: dict[str, Any], evidence: dict[str, Any]
) -> None:
    raw_operation_rounds = capacity.get("operation_rounds")
    checks = capacity.get("environment_checks")
    operation_rounds = (
        [
            item
            for item in raw_operation_rounds
            if isinstance(item, dict) and item.get("phase") == "soak"
        ]
        if isinstance(raw_operation_rounds, list)
        else None
    )
    if (
        not operation_rounds
        or not isinstance(checks, list)
        or len(checks) != len(operation_rounds) + 4
    ):
        raise CandidateError(
            "capacity environment evidence must cover pre/post-restart, the soak boundary, every Operation round, and final"
        )

    identity_fields = (
        "fixture_image",
        "node_ids_sha256",
        "deployment_ids_sha256",
        "container_ids_sha256",
        "endpoint_ids_sha256",
        "link_ids_sha256",
        "observer_identity_sha256",
        "provenance_record_sha256",
        "image_workflow_run_id",
        "control_plane_image",
        "agent_image",
        "provenance_fixture_image",
        "control_plane_origin_sha256",
        "restart_argv_sha256",
        "topology_id",
        "topology_revision_id",
        "topology_identity_sha256",
        "runtime_provision_manifest_sha256",
        "runtime_host_identity_sha256",
        "runner_machine_id_sha256",
        "control_plane_image_id",
        "control_plane_container_id",
        "control_plane_started_at",
        "control_plane_configuration_sha256",
        "postgres_image",
        "postgres_image_id",
        "postgres_container_id",
        "postgres_started_at",
        "postgres_configuration_sha256",
        "postgres_server_leaf_sha256",
        "agent_image_id",
        "agent_node_ids_sha256",
        "agent_container_ids_sha256",
        "agent_started_at_sha256",
        "agent_spiffe_ids_sha256",
        "agent_certificate_fingerprints_sha256",
        "agent_ledger_identities_sha256",
        "agent_independent_mtls_identities",
        "agent_independent_sqlite_ledgers",
        "docker_engine_image",
        "docker_engine_image_id",
        "engine_outer_container_ids_sha256",
        "engine_inner_daemon_ids_sha256",
        "engine_socket_volumes_sha256",
        "engine_data_volumes_sha256",
    )
    digest_fields = (
        "configuration_fingerprint_sha256",
        "node_ids_sha256",
        "deployment_ids_sha256",
        "container_ids_sha256",
        "endpoint_ids_sha256",
        "link_ids_sha256",
        "observer_identity_sha256",
        "provenance_record_sha256",
        "control_plane_origin_sha256",
        "restart_argv_sha256",
        "topology_identity_sha256",
        "runtime_provision_manifest_sha256",
        "runtime_host_identity_sha256",
        "runner_machine_id_sha256",
        "control_plane_configuration_sha256",
        "postgres_container_id",
        "postgres_configuration_sha256",
        "postgres_server_leaf_sha256",
        "agent_node_ids_sha256",
        "agent_container_ids_sha256",
        "agent_started_at_sha256",
        "agent_spiffe_ids_sha256",
        "agent_certificate_fingerprints_sha256",
        "agent_ledger_identities_sha256",
        "engine_outer_container_ids_sha256",
        "engine_inner_daemon_ids_sha256",
        "engine_socket_volumes_sha256",
        "engine_data_volumes_sha256",
    )
    stable_identity: dict[str, Any] | None = None
    stable_process: dict[str, Any] | None = None
    completed_at: list[float] = []
    for position, check in enumerate(checks):
        if not isinstance(check, dict):
            raise CandidateError("capacity environment evidence contains a malformed check")
        if position == 0:
            expected_phase, expected_round = "pre_restart", None
        elif position == 1:
            expected_phase, expected_round = "post_restart", None
        elif position == 2:
            expected_phase, expected_round = "soak_boundary", None
        elif position == len(checks) - 1:
            expected_phase, expected_round = "final", None
        else:
            expected_phase, expected_round = "operation_round", position - 2
            operation_round = operation_rounds[position - 3]
            if not isinstance(operation_round, dict) or operation_round.get("round") != position - 2:
                raise CandidateError("capacity Operation rounds are not complete or ordered")
        expected_baseline = position == 2
        if (
            check.get("sequence") != position + 1
            or check.get("phase") != expected_phase
            or check.get("operation_round_index") != expected_round
            or check.get("post_warmup_baseline") is not expected_baseline
            or check.get("ok") is not True
        ):
            raise CandidateError("capacity environment observation phases are incomplete or unordered")
        for field, expected in (
            ("workers", 10),
            ("engines", 100),
            ("containers", 2_000),
            ("running_containers", 2_000),
            ("healthy_containers", 2_000),
            ("endpoint_checks_total", 2_000),
            ("endpoint_checks_healthy", 2_000),
            ("endpoint_checks_failed", 0),
            ("link_probes_total", 8_000),
            ("link_probes_healthy", 8_000),
            ("link_probes_failed", 0),
            ("drift", 0),
        ):
            if check.get(field) != expected:
                raise CandidateError(f"capacity environment {field} is not {expected}")
        fixture_image = check.get("fixture_image")
        if not isinstance(fixture_image, str) or not re.fullmatch(
            r"[^\s@]+@sha256:[0-9a-f]{64}", fixture_image
        ):
            raise CandidateError("capacity environment fixture image is not digest pinned")
        for image_field in (
            "control_plane_image",
            "agent_image",
            "provenance_fixture_image",
            "postgres_image",
            "docker_engine_image",
        ):
            if not isinstance(check.get(image_field), str) or not re.fullmatch(
                r"[^\s@]+@sha256:[0-9a-f]{64}", check[image_field]
            ):
                raise CandidateError(f"capacity environment {image_field} is not digest pinned")
        for image_id_field in (
            "control_plane_image_id",
            "postgres_image_id",
            "agent_image_id",
            "docker_engine_image_id",
        ):
            if not isinstance(check.get(image_id_field), str) or not re.fullmatch(
                r"sha256:[0-9a-f]{64}", check[image_id_field]
            ):
                raise CandidateError(f"capacity environment {image_id_field} is invalid")
        for count_field in (
            "agent_independent_mtls_identities",
            "agent_independent_sqlite_ledgers",
        ):
            if check.get(count_field) != 100:
                raise CandidateError(f"capacity environment {count_field} is not 100")
        docker_started_at_key(
            check.get("postgres_started_at"), "capacity PostgreSQL StartedAt"
        )
        for field in digest_fields:
            validate_sha256(check.get(field), f"capacity environment {field}")
        validate_sha256(check.get("aggregate_sha256"), "capacity environment aggregate")
        started = check.get("started_at_epoch_seconds")
        completed = check.get("completed_at_epoch_seconds")
        if (
            isinstance(started, bool)
            or not isinstance(started, (int, float))
            or isinstance(completed, bool)
            or not isinstance(completed, (int, float))
            or not math.isfinite(float(started))
            or not math.isfinite(float(completed))
            or float(started) <= 0
            or float(completed) < float(started)
            or float(completed) - float(started) > 85
            or (completed_at and float(completed) < completed_at[-1])
        ):
            raise CandidateError("capacity environment observation timestamps are invalid")
        completed_at.append(float(completed))
        current_identity = {
            "configuration_fingerprint_sha256": check["configuration_fingerprint_sha256"],
            **{
                field: check[field]
                for field in identity_fields
                if field not in {"control_plane_container_id", "control_plane_started_at"}
            },
        }
        if stable_identity is None:
            stable_identity = current_identity
        elif current_identity != stable_identity:
            raise CandidateError("capacity environment identity changed during the production gate")
        process = {
            "container_id": check.get("control_plane_container_id"),
            "started_at": check.get("control_plane_started_at"),
        }
        if not isinstance(process["container_id"], str) or not re.fullmatch(
            r"[0-9a-f]{64}", process["container_id"]
        ) or not isinstance(process["started_at"], str) or not process["started_at"]:
            raise CandidateError("capacity control-plane process identity is malformed")
        process_started_at = docker_started_at_key(
            process["started_at"], "capacity control-plane StartedAt"
        )
        if position == 1:
            if process_started_at <= docker_started_at_key(
                checks[0].get("control_plane_started_at"),
                "capacity pre-restart StartedAt",
            ):
                raise CandidateError("capacity restart did not advance control-plane StartedAt")
            stable_process = process
        elif position >= 2 and process != stable_process:
            raise CandidateError("capacity control-plane process changed after the restart")

    if stable_identity is None:
        raise CandidateError("capacity environment evidence has no stable identity")
    coverage = completed_at[2:]
    max_gap = max(
        (right - left for left, right in zip(coverage, coverage[1:])),
        default=0.0,
    )
    reported_gap = evidence.get("environment_max_observation_gap_seconds")
    expected_identity = {
        **{
            field: stable_identity[field]
            for field in identity_fields
            if field not in {"control_plane_container_id", "control_plane_started_at"}
        },
        "control_plane_container_id": stable_process["container_id"],
        "control_plane_started_at": stable_process["started_at"],
    }
    if (
        evidence.get("environment_observations") != len(checks)
        or evidence.get("environment_first_record") != 1
        or evidence.get("environment_last_record") != len(checks)
        or evidence.get("environment_final_record") != len(checks)
        or evidence.get("environment_configuration_fingerprint_sha256")
        != stable_identity["configuration_fingerprint_sha256"]
        or evidence.get("environment_identity") != expected_identity
        or isinstance(reported_gap, bool)
        or not isinstance(reported_gap, (int, float))
        or not math.isclose(float(reported_gap), max_gap, rel_tol=1e-9, abs_tol=1e-6)
    ):
        raise CandidateError("capacity environment evidence summary is inconsistent")

    logs = capacity.get("logs")
    index = logs.get("index") if isinstance(logs, dict) else None
    environment_entries = (
        [
            item
            for item in index
            if isinstance(item, dict)
            and item.get("kind") == "environment_observations_ndjson"
        ]
        if isinstance(index, list)
        else []
    )
    if len(environment_entries) != 1:
        raise CandidateError("capacity report has no unique environment observation sidecar")
    sidecar = environment_entries[0]
    if (
        sidecar.get("records") != len(checks)
        or not isinstance(sidecar.get("path"), str)
        or not re.fullmatch(r"[^/\\]+\.environment\.ndjson", sidecar["path"])
        or not isinstance(sidecar.get("bytes"), int)
        or sidecar["bytes"] <= 0
    ):
        raise CandidateError("capacity environment observation sidecar index is inconsistent")
    validate_sha256(sidecar.get("sha256"), "capacity environment sidecar")


def capacity_rfc3339_epoch(value: Any, label: str) -> float:
    if not isinstance(value, str) or not value.endswith("Z"):
        raise CandidateError(f"{label} is not canonical UTC RFC3339")
    try:
        parsed = datetime.datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise CandidateError(f"{label} is not canonical UTC RFC3339") from error
    return parsed.timestamp()


def validate_capacity_checkpoint_history(
    capacity: dict[str, Any], evidence: dict[str, Any]
) -> None:
    history = evidence.get("checkpoint_history")
    if (
        evidence.get("checkpoint_interval_seconds") != 30
        or evidence.get("checkpoint_clock") != "CLOCK_BOOTTIME"
        or not isinstance(history, list)
        or not history
        or evidence.get("checkpoint_count") != len(history)
    ):
        raise CandidateError("capacity checkpoint history identity is incomplete")
    epochs: list[float] = []
    clocks: list[float] = []
    for position, checkpoint in enumerate(history):
        if (
            not isinstance(checkpoint, dict)
            or set(checkpoint) != {"sequence", "epoch_seconds", "clock_seconds"}
            or checkpoint.get("sequence") != position + 1
        ):
            raise CandidateError("capacity checkpoint history is malformed or unordered")
        epoch = checkpoint.get("epoch_seconds")
        clock = checkpoint.get("clock_seconds")
        if (
            isinstance(epoch, bool)
            or not isinstance(epoch, (int, float))
            or not math.isfinite(float(epoch))
            or isinstance(clock, bool)
            or not isinstance(clock, (int, float))
            or not math.isfinite(float(clock))
        ):
            raise CandidateError("capacity checkpoint timestamps are invalid")
        epochs.append(float(epoch))
        clocks.append(float(clock))
    if any(right <= left for left, right in zip(epochs, epochs[1:])) or any(
        right <= left for left, right in zip(clocks, clocks[1:])
    ):
        raise CandidateError("capacity checkpoint timestamps are not strictly increasing")
    if any(right - left > 35 for left, right in zip(clocks, clocks[1:])):
        raise CandidateError("capacity checkpoint history contains a gap over 35 seconds")
    started_at = capacity_rfc3339_epoch(capacity.get("started_at"), "capacity start")
    completed_at = capacity_rfc3339_epoch(
        evidence.get("completed_at"), "capacity completion"
    )
    checkpointed_at = capacity_rfc3339_epoch(
        evidence.get("checkpointed_at"), "capacity final checkpoint"
    )
    if (
        epochs[0] < started_at - 1
        or epochs[0] > started_at + 35
        or epochs[-1] < completed_at
        or epochs[-1] > completed_at + 35
        or abs(checkpointed_at - math.floor(epochs[-1])) > 1
        or len(history) < math.floor((completed_at - started_at) / 35) + 1
    ):
        raise CandidateError("capacity checkpoint history does not cover the full gate")


def validate_capacity_evidence(
    capacity: dict[str, Any],
    candidate_sha: str,
    capacity_run_id: str,
    repository: str,
    workflow_ref: str,
) -> dict[str, Any]:
    if (
        capacity.get("schema_version") != 2
        or capacity.get("profile") != "production"
        or capacity.get("failures") != []
    ):
        raise CandidateError("capacity evidence is not a successful production v2 report")
    identity = capacity.get("identity")
    if not isinstance(identity, dict):
        raise CandidateError("capacity evidence has no candidate identity")
    for name in ("source_commit", "oci_revision", "provenance_commit"):
        if identity.get(name) != candidate_sha:
            raise CandidateError(f"capacity evidence {name} belongs to another commit")
    workflow = identity.get("workflow")
    if not isinstance(workflow, dict) or (
        str(workflow.get("run_id")) != capacity_run_id
        or workflow.get("run_attempt") != "1"
        or workflow.get("repository") != repository
        or workflow.get("ref") != workflow_ref
        or workflow.get("sha") != candidate_sha
        or workflow.get("workflow") != "Orchestrator capacity and soak gate"
        or workflow.get("job") != "production-soak"
    ):
        raise CandidateError("capacity workflow identity is not exact")
    build = identity.get("server_build")
    target = str(build.get("target", "")).lower() if isinstance(build, dict) else ""
    if not isinstance(build, dict) or (
        build.get("version") != "1.0.0"
        or build.get("commit_sha") != candidate_sha
        or build.get("profile") != "production"
        or "linux" not in target
        or not ("x86_64" in target or "amd64" in target)
    ):
        raise CandidateError("capacity server build belongs to another candidate")
    image_provenance = identity.get("image_provenance")
    if not isinstance(image_provenance, dict) or set(image_provenance) != {
        "control_plane_image",
        "agent_image",
        "fixture_image",
        "source_workflow_run_id",
        "record_sha256",
        "source_workflow",
        "source_workflow_run_attempt",
    }:
        raise CandidateError("capacity image provenance identity is incomplete")
    for field in ("control_plane_image", "agent_image", "fixture_image"):
        if not isinstance(image_provenance.get(field), str) or not re.fullmatch(
            r"[^\s@]+@sha256:[0-9a-f]{64}", image_provenance[field]
        ):
            raise CandidateError(f"capacity image provenance {field} is invalid")
    image_run_id = str(image_provenance.get("source_workflow_run_id", ""))
    if (
        not image_run_id.isdigit()
        or int(image_run_id) <= 0
        or image_provenance.get("source_workflow")
        != ".github/workflows/orchestrator-candidate-images.yml"
        or image_provenance.get("source_workflow_run_attempt") != 1
    ):
        raise CandidateError("capacity image provenance workflow identity is invalid")
    validate_sha256(
        image_provenance.get("record_sha256"),
        "capacity image provenance record",
    )
    evidence = capacity.get("evidence")
    if not isinstance(evidence, dict) or evidence.get("source_commit") != candidate_sha:
        raise CandidateError("capacity evidence summary belongs to another commit")
    token_refresh_count = evidence.get("token_refresh_count")
    if (
        isinstance(token_refresh_count, bool)
        or not isinstance(token_refresh_count, int)
        or token_refresh_count < 2
    ):
        raise CandidateError("capacity evidence did not prove OIDC token refresh")
    validate_capacity_checkpoint_history(capacity, evidence)
    validate_capacity_environment_evidence(capacity, evidence)
    environment_identity = evidence.get("environment_identity")
    if not isinstance(environment_identity, dict) or (
        environment_identity.get("control_plane_image")
        != image_provenance["control_plane_image"]
        or environment_identity.get("agent_image")
        != image_provenance["agent_image"]
        or environment_identity.get("fixture_image")
        != image_provenance["fixture_image"]
        or environment_identity.get("provenance_fixture_image")
        != image_provenance["fixture_image"]
        or environment_identity.get("image_workflow_run_id") != image_run_id
        or environment_identity.get("provenance_record_sha256")
        != image_provenance["record_sha256"]
    ):
        raise CandidateError(
            "capacity environment does not match independently verified image provenance"
        )
    return identity


def atomic_json(path: pathlib.Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = pathlib.Path(name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as stream:
            json.dump(value, stream, indent=2, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def create(args: argparse.Namespace) -> None:
    version = validate_version(args.version)
    candidate_sha = validate_sha(args.candidate_sha)
    candidate_run_id = validate_run_id(args.candidate_run_id, "candidate_run_id")
    candidate_run_attempt = validate_run_attempt(
        args.candidate_run_attempt, "candidate_run_attempt"
    )
    capacity_run_id = validate_run_id(args.capacity_run_id, "capacity_run_id")
    repository = validate_repository(args.repository)
    workflow_ref = validate_workflow_ref(args.workflow_ref)
    require_manifest_outside_payload(args.payload_dir, args.output)
    primary = require_exact_payload(args.payload_dir, version)
    validate_payload_metadata(
        args.payload_dir,
        version,
        candidate_sha,
        candidate_run_id,
        repository,
        workflow_ref,
    )
    authenticode = load_object(args.authenticode_evidence, "Authenticode evidence")
    publisher_subject = authenticode.get("expected_publisher_subject")
    if not isinstance(publisher_subject, str) or not publisher_subject:
        raise CandidateError("Authenticode evidence has no protected publisher subject")
    validate_authenticode_evidence(authenticode, candidate_sha, publisher_subject)
    capacity = load_object(args.capacity_evidence, "capacity evidence")
    identity = validate_capacity_evidence(
        capacity,
        candidate_sha,
        capacity_run_id,
        repository,
        workflow_ref,
    )
    assets = []
    for name in primary:
        artifact = args.payload_dir / name
        bundle = args.payload_dir / f"{name}.sigstore.json"
        assets.append(
            {
                "name": name,
                "sha256": sha256(artifact),
                "size_bytes": artifact.stat().st_size,
                "sigstore_bundle": {
                    "name": bundle.name,
                    "sha256": sha256(bundle),
                    "size_bytes": bundle.stat().st_size,
                },
            }
        )
    manifest = {
        "schema_version": 2,
        "status": "SECURITY_ACCEPTANCE_PENDING",
        "published": False,
        "version": version,
        "candidate_sha": candidate_sha,
        "candidate_workflow_run_id": candidate_run_id,
        "candidate_workflow_run_attempt": candidate_run_attempt,
        "capacity_workflow_run_id": capacity_run_id,
        "repository": repository,
        "workflow_ref": workflow_ref,
        "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "server_build": identity.get("server_build"),
        "authenticode_evidence": {
            "sha256": sha256(args.authenticode_evidence),
            "publisher_subject": publisher_subject,
            "timestamp_policy": authenticode.get("timestamp_policy"),
        },
        "capacity_evidence": {
            "sha256": sha256(args.capacity_evidence),
            "report_schema_version": capacity.get("schema_version"),
        },
        "payload": {
            "primary_count": 11,
            "sigstore_bundle_count": 11,
            "release_file_count": 22,
            "assets": assets,
        },
    }
    atomic_json(args.output, manifest)


def verify(args: argparse.Namespace) -> None:
    manifest = load_object(args.manifest, "candidate manifest")
    version = validate_version(str(manifest.get("version", "")))
    expected_sha = validate_sha(args.expected_sha)
    expected_run_id = validate_run_id(args.expected_run_id, "expected_run_id")
    expected_run_attempt = validate_run_attempt(
        args.expected_run_attempt, "expected_run_attempt"
    )
    expected_repository = validate_repository(args.expected_repository)
    expected_workflow_ref = validate_workflow_ref(args.expected_workflow_ref)
    require_manifest_outside_payload(args.payload_dir, args.manifest)
    primary = require_exact_payload(args.payload_dir, version)
    if manifest.get("schema_version") != 2 or manifest.get("candidate_sha") != expected_sha:
        raise CandidateError("candidate manifest identity does not match the expected commit")
    if str(manifest.get("candidate_workflow_run_id")) != expected_run_id:
        raise CandidateError("candidate manifest does not match the selected workflow run")
    if manifest.get("candidate_workflow_run_attempt") != expected_run_attempt:
        raise CandidateError("candidate manifest does not describe the first workflow attempt")
    if (
        manifest.get("published") is not False
        or manifest.get("status") != "SECURITY_ACCEPTANCE_PENDING"
    ):
        raise CandidateError("candidate manifest must describe an unpublished security-pending candidate")
    if (
        manifest.get("repository") != expected_repository
        or manifest.get("workflow_ref") != expected_workflow_ref
    ):
        raise CandidateError("candidate manifest repository/workflow identity is invalid")
    validate_run_id(manifest.get("capacity_workflow_run_id"), "capacity_workflow_run_id")
    server_build = manifest.get("server_build")
    target = (
        str(server_build.get("target", "")).lower()
        if isinstance(server_build, dict)
        else ""
    )
    if not isinstance(server_build, dict) or (
        server_build.get("version") != "1.0.0"
        or server_build.get("commit_sha") != expected_sha
        or server_build.get("profile") != "production"
        or "linux" not in target
        or not ("x86_64" in target or "amd64" in target)
    ):
        raise CandidateError("candidate manifest server build identity is invalid")
    validate_payload_metadata(
        args.payload_dir,
        version,
        expected_sha,
        expected_run_id,
        expected_repository,
        expected_workflow_ref,
    )
    payload = manifest.get("payload")
    if not isinstance(payload, dict) or (
        payload.get("primary_count"),
        payload.get("sigstore_bundle_count"),
        payload.get("release_file_count"),
    ) != (11, 11, 22):
        raise CandidateError("candidate manifest payload cardinality is invalid")
    records = payload.get("assets")
    if (
        not isinstance(records, list)
        or any(not isinstance(item, dict) for item in records)
        or [item.get("name") for item in records] != primary
    ):
        raise CandidateError("candidate manifest primary asset order/set is invalid")
    for record in records:
        name = record["name"]
        artifact = args.payload_dir / name
        bundle_record = record.get("sigstore_bundle")
        bundle = args.payload_dir / f"{name}.sigstore.json"
        if (
            record.get("sha256") != sha256(artifact)
            or record.get("size_bytes") != artifact.stat().st_size
            or not isinstance(bundle_record, dict)
            or bundle_record.get("name") != bundle.name
            or bundle_record.get("sha256") != sha256(bundle)
            or bundle_record.get("size_bytes") != bundle.stat().st_size
        ):
            raise CandidateError(f"candidate manifest digest/size mismatch for {name}")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    listing = subparsers.add_parser("list-assets")
    listing.add_argument("--version", default="1.0.0")
    creation = subparsers.add_parser("create")
    creation.add_argument("--version", default="1.0.0")
    creation.add_argument("--candidate-sha", required=True)
    creation.add_argument("--candidate-run-id", required=True)
    creation.add_argument("--candidate-run-attempt", required=True)
    creation.add_argument("--capacity-run-id", required=True)
    creation.add_argument("--repository", required=True)
    creation.add_argument("--workflow-ref", required=True)
    creation.add_argument("--payload-dir", type=pathlib.Path, required=True)
    creation.add_argument("--authenticode-evidence", type=pathlib.Path, required=True)
    creation.add_argument("--capacity-evidence", type=pathlib.Path, required=True)
    creation.add_argument("--output", type=pathlib.Path, required=True)
    verification = subparsers.add_parser("verify")
    verification.add_argument("--payload-dir", type=pathlib.Path, required=True)
    verification.add_argument("--manifest", type=pathlib.Path, required=True)
    verification.add_argument("--expected-sha", required=True)
    verification.add_argument("--expected-run-id", required=True)
    verification.add_argument("--expected-run-attempt", required=True)
    verification.add_argument("--expected-repository", required=True)
    verification.add_argument(
        "--expected-workflow-ref", default="refs/heads/main"
    )
    promotion = subparsers.add_parser("verify-promotion-acceptance")
    promotion.add_argument("--artifact-json", type=pathlib.Path, required=True)
    promotion.add_argument("--candidate-sha", required=True)
    promotion.add_argument("--candidate-run-id", required=True)
    promotion.add_argument("--candidate-run-attempt", required=True)
    promotion.add_argument("--accepted-sha", required=True)
    promotion.add_argument("--accepted-run-id", required=True)
    promotion.add_argument("--accepted-manifest-sha256", required=True)
    promotion.add_argument("--accepted-artifact-id", required=True)
    promotion.add_argument("--accepted-artifact-digest", required=True)
    promotion.add_argument("--actual-manifest-sha256")
    promotion.add_argument("--actual-artifact-archive-sha256")
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        if args.command == "list-assets":
            print("\n".join(primary_names(validate_version(args.version))))
        elif args.command == "create":
            create(args)
            print(args.output)
        elif args.command == "verify":
            verify(args)
            print("candidate payload verification passed")
        else:
            artifact = load_object(args.artifact_json, "candidate artifact metadata")
            artifact_id, artifact_digest = validate_promotion_acceptance(
                artifact,
                candidate_sha=args.candidate_sha,
                candidate_run_id=args.candidate_run_id,
                candidate_run_attempt=args.candidate_run_attempt,
                accepted_sha=args.accepted_sha,
                accepted_run_id=args.accepted_run_id,
                accepted_manifest_sha256=args.accepted_manifest_sha256,
                accepted_artifact_id=args.accepted_artifact_id,
                accepted_artifact_digest=args.accepted_artifact_digest,
                actual_artifact_archive_sha256=args.actual_artifact_archive_sha256,
                actual_manifest_sha256=args.actual_manifest_sha256,
            )
            print(json.dumps({"artifact_id": artifact_id, "digest": artifact_digest}))
        return 0
    except CandidateError as error:
        print(f"candidate verification failed: {error}", file=os.sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
