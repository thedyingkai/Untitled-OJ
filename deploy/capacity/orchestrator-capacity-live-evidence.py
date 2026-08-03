#!/usr/bin/env python3
"""Collect one fresh full-environment observation for the production gate.

This is the repository-owned command behind
``ORCHESTRATOR_GATE_ENVIRONMENT_ARGV_JSON``.  Provider-specific addresses and
SSH material live only in its protected Ansible inventory/extra-vars files.
Every child process is invoked as argv with ``shell=False`` and child output is
redacted; stdout contains exactly the non-secret evidence JSON contract.
"""

from __future__ import annotations

import argparse
import hashlib
import ipaddress
import json
import math
import os
import pathlib
import re
import subprocess
import sys
import tempfile
import time
import urllib.parse
from typing import Any, Sequence


MAX_OBSERVATION_SECONDS = 82
MAX_JSON_BYTES = 32 * 1024 * 1024
SHA40 = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
OCI_DIGEST = re.compile(r"^[^\s@]+@sha256:[0-9a-f]{64}$")
CONFIG_KEYS = {
    "schema_version",
    "candidate_sha",
    "fixture_image",
    "control_plane_image",
    "agent_image",
    "postgres_image",
    "docker_engine_image",
    "ansible_executable",
    "ansible_inventory",
    "ansible_extra_vars_file",
    "ansible_playbook",
    "engine_evidence_script",
    "runtime_evidence_script",
    "runtime_expected_manifest",
    "environment_script",
    "python_executable",
    "nodes_file",
    "fixture_file",
    "base_url",
    "ca_file",
    "token_argv_json",
    "restart_argv_json",
    "image_provenance_record",
    "helper_manifest",
    "applied_manifest",
}


class LiveEvidenceError(RuntimeError):
    pass


def bounded_bytes(path: pathlib.Path, maximum: int = MAX_JSON_BYTES) -> bytes:
    try:
        if path.is_symlink() or not path.is_file():
            raise LiveEvidenceError(f"protected input is not a regular file: {path}")
        with path.open("rb") as stream:
            raw = stream.read(maximum + 1)
    except OSError as error:
        raise LiveEvidenceError(f"cannot read protected input {path}") from error
    if len(raw) > maximum:
        raise LiveEvidenceError(f"protected input is oversized: {path}")
    return raw


def load_json(path: pathlib.Path) -> Any:
    try:
        return json.loads(bounded_bytes(path))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise LiveEvidenceError(f"protected JSON is invalid: {path}") from error


def require_path(value: Any, label: str) -> pathlib.Path:
    if not isinstance(value, str) or not value or "\0" in value:
        raise LiveEvidenceError(f"{label} must be a non-empty path")
    path = pathlib.Path(value)
    if not path.is_absolute():
        raise LiveEvidenceError(f"{label} must be absolute")
    return path


def parse_argv_json(value: Any, label: str = "helper argv") -> list[str]:
    if not isinstance(value, str):
        raise LiveEvidenceError(f"{label} must be a JSON string array")
    try:
        argv = json.loads(value)
    except json.JSONDecodeError as error:
        raise LiveEvidenceError(f"{label} is invalid JSON") from error
    if (
        not isinstance(argv, list)
        or not 1 <= len(argv) <= 32
        or any(not isinstance(item, str) or not item for item in argv)
    ):
        raise LiveEvidenceError(f"{label} must contain 1-32 argv strings")
    return argv


def normalize_https_origin(value: Any) -> str:
    if (
        not isinstance(value, str)
        or not value
        or value != value.strip()
        or any(character.isspace() for character in value)
    ):
        raise LiveEvidenceError("observer base_url must be a direct HTTPS origin")
    try:
        parsed = urllib.parse.urlsplit(value)
        port = parsed.port
    except ValueError as error:
        raise LiveEvidenceError("observer base_url has an invalid port or host") from error
    if (
        parsed.scheme.lower() != "https"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.path not in ("", "/")
        or parsed.query
        or parsed.fragment
        or not 1 <= (port or 443) <= 65_535
    ):
        raise LiveEvidenceError("observer base_url must be a direct HTTPS origin")
    hostname = parsed.hostname
    try:
        address = ipaddress.ip_address(hostname)
    except ValueError:
        if "%" in hostname:
            raise LiveEvidenceError("observer base_url host is invalid")
        try:
            canonical_host = hostname.encode("idna").decode("ascii").lower()
        except UnicodeError as error:
            raise LiveEvidenceError("observer base_url host is invalid") from error
        if not canonical_host or any(
            not label or len(label) > 63 for label in canonical_host.rstrip(".").split(".")
        ):
            raise LiveEvidenceError("observer base_url host is invalid")
    else:
        canonical_host = address.compressed.lower()
        if address.version == 6:
            canonical_host = f"[{canonical_host}]"
    suffix = "" if port in (None, 443) else f":{port}"
    return f"https://{canonical_host}{suffix}"


def validate_config(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != CONFIG_KEYS:
        raise LiveEvidenceError("observer config has missing or unexpected fields")
    if value.get("schema_version") != 1:
        raise LiveEvidenceError("observer config must use schema_version 1")
    candidate = value.get("candidate_sha")
    image = value.get("fixture_image")
    control_plane_image = value.get("control_plane_image")
    agent_image = value.get("agent_image")
    postgres_image = value.get("postgres_image")
    docker_engine_image = value.get("docker_engine_image")
    if not isinstance(candidate, str) or not SHA40.fullmatch(candidate):
        raise LiveEvidenceError("observer candidate_sha is invalid")
    if not isinstance(image, str) or not OCI_DIGEST.fullmatch(image):
        raise LiveEvidenceError("observer fixture_image is not digest pinned")
    if (
        not isinstance(control_plane_image, str)
        or not OCI_DIGEST.fullmatch(control_plane_image)
        or not isinstance(agent_image, str)
        or not OCI_DIGEST.fullmatch(agent_image)
        or not isinstance(postgres_image, str)
        or not OCI_DIGEST.fullmatch(postgres_image)
        or not isinstance(docker_engine_image, str)
        or not OCI_DIGEST.fullmatch(docker_engine_image)
    ):
        raise LiveEvidenceError("observer control-plane/Agent images are not digest pinned")
    value["base_url"] = normalize_https_origin(value.get("base_url"))
    for key in (
        "ansible_executable",
        "ansible_inventory",
        "ansible_extra_vars_file",
        "ansible_playbook",
        "engine_evidence_script",
        "runtime_evidence_script",
        "runtime_expected_manifest",
        "environment_script",
        "python_executable",
        "nodes_file",
        "fixture_file",
        "ca_file",
        "image_provenance_record",
        "helper_manifest",
        "applied_manifest",
    ):
        value[key] = str(require_path(value.get(key), key))
    parse_argv_json(value.get("token_argv_json"), "token_argv_json")
    parse_argv_json(value.get("restart_argv_json"), "restart_argv_json")
    return value


def remaining(deadline: float) -> float:
    value = deadline - time.monotonic()
    if value <= 0:
        raise LiveEvidenceError(
            f"full environment observation exceeded {MAX_OBSERVATION_SECONDS} seconds"
        )
    return value


def run_redacted(
    argv: Sequence[str],
    deadline: float,
    *,
    stdout: bool,
    runner: Any = subprocess.run,
) -> str:
    with tempfile.TemporaryFile() as redacted:
        try:
            result = runner(
                list(argv),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE if stdout else redacted,
                stderr=redacted,
                text=stdout,
                timeout=remaining(deadline),
                check=False,
                shell=False,
            )
        except subprocess.TimeoutExpired as error:
            raise LiveEvidenceError("protected observer child exceeded its deadline") from error
    if result.returncode != 0:
        raise LiveEvidenceError(
            f"protected observer child exited with {result.returncode}; output was redacted"
        )
    if not stdout:
        return ""
    if not isinstance(result.stdout, str) or len(result.stdout) > MAX_JSON_BYTES:
        raise LiveEvidenceError("environment preflight stdout is missing or oversized")
    return result.stdout


def sha256_bytes(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def sha256_values(values: Sequence[str]) -> str:
    digest = hashlib.sha256()
    for value in sorted(values):
        digest.update(value.encode("utf-8"))
        digest.update(b"\n")
    return digest.hexdigest()


def require_hash(value: Any, label: str) -> str:
    if not isinstance(value, str) or not SHA256.fullmatch(value):
        raise LiveEvidenceError(f"{label} is not a lowercase SHA-256")
    return value


def aggregate_identity(document: Any, config: dict[str, Any]) -> dict[str, Any]:
    if (
        not isinstance(document, dict)
        or document.get("schema_version") != 1
        or document.get("candidate_sha") != config["candidate_sha"]
        or document.get("fixture_image") != config["fixture_image"]
        or document.get("worker_count") != 10
        or document.get("engine_count") != 100
        or document.get("container_count") != 2_000
    ):
        raise LiveEvidenceError("Engine aggregate identity/cardinality is invalid")
    workers = document.get("workers")
    if not isinstance(workers, list) or len(workers) != 10:
        raise LiveEvidenceError("Engine aggregate does not contain 10 workers")
    nodes: list[str] = []
    deployments: list[str] = []
    containers: list[str] = []
    for worker in workers:
        if not isinstance(worker, dict) or not isinstance(worker.get("engines"), list):
            raise LiveEvidenceError("Engine aggregate worker is invalid")
        for engine in worker["engines"]:
            if not isinstance(engine, dict) or not isinstance(engine.get("containers"), list):
                raise LiveEvidenceError("Engine aggregate observation is invalid")
            node_id = engine.get("node_id")
            if not isinstance(node_id, str) or not node_id:
                raise LiveEvidenceError("Engine aggregate node identity is invalid")
            nodes.append(node_id)
            for container in engine["containers"]:
                if not isinstance(container, dict):
                    raise LiveEvidenceError("Engine aggregate container is invalid")
                deployment = container.get("deployment_id")
                container_id = container.get("container_id")
                if not isinstance(deployment, str) or not isinstance(container_id, str):
                    raise LiveEvidenceError("Engine aggregate resource identity is invalid")
                deployments.append(deployment)
                containers.append(f"{node_id}\0{container_id}")
    if len(nodes) != 100 or len(set(nodes)) != 100:
        raise LiveEvidenceError("Engine aggregate Node set is incomplete")
    if len(deployments) != 2_000 or len(set(deployments)) != 2_000:
        raise LiveEvidenceError("Engine aggregate Deployment set is incomplete")
    if len(containers) != 2_000 or len(set(containers)) != 2_000:
        raise LiveEvidenceError("Engine aggregate container set is incomplete")
    return {
        "node_ids_sha256": sha256_values(nodes),
        "deployment_ids_sha256": sha256_values(deployments),
        "container_ids_sha256": sha256_values(containers),
    }


def validate_runtime_aggregate(value: Any, config: dict[str, Any]) -> dict[str, Any]:
    manifest = load_json(pathlib.Path(config["runtime_expected_manifest"]))
    if (
        not isinstance(manifest, dict)
        or manifest.get("schema_version") != 2
        or manifest.get("candidate_sha") != config["candidate_sha"]
        or manifest.get("control_plane_origin") != config["base_url"]
        or manifest.get("control_plane", {}).get("image")
        != config["control_plane_image"]
        or manifest.get("postgres", {}).get("image") != config["postgres_image"]
        or manifest.get("agent", {}).get("image") != config["agent_image"]
        or manifest.get("engine", {}).get("image") != config["docker_engine_image"]
    ):
        raise LiveEvidenceError("expected runtime manifest identity is invalid")
    manifest_sha = sha256_bytes(
        json.dumps(
            manifest, ensure_ascii=False, sort_keys=True, separators=(",", ":")
        ).encode()
    )
    expected_keys = {
        "schema_version",
        "candidate_sha",
        "provision_manifest_sha256",
        "host_count",
        "host_identity_sha256",
        "hosts",
        "control_plane",
        "postgres",
        "restart_identity",
        "agents",
        "engines",
    }
    if (
        not isinstance(value, dict)
        or set(value) != expected_keys
        or value.get("schema_version") != 2
        or value.get("candidate_sha") != config["candidate_sha"]
        or value.get("provision_manifest_sha256") != manifest_sha
        or value.get("host_count") != 13
    ):
        raise LiveEvidenceError("runtime evidence identity/cardinality is invalid")
    require_hash(value.get("host_identity_sha256"), "runtime host identity")
    control_plane = value.get("control_plane")
    postgres = value.get("postgres")
    agents = value.get("agents")
    engines = value.get("engines")
    restart = value.get("restart_identity")
    if (
        not isinstance(control_plane, dict)
        or control_plane.get("image", {}).get("repo_digest")
        != config["control_plane_image"]
        or control_plane.get("configuration", {}).get("effective_sha256")
        != control_plane.get("configuration", {}).get("provisioned_sha256")
        or not isinstance(postgres, dict)
        or postgres.get("image", {}).get("repo_digest") != config["postgres_image"]
        or postgres.get("container", {}).get("state") != "RUNNING"
        or postgres.get("container", {}).get("health") != "HEALTHY"
        or postgres.get("configuration", {}).get("effective_sha256")
        != postgres.get("configuration", {}).get("provisioned_sha256")
        or control_plane.get("database_tls_identity", {}).get("peer_leaf_sha256")
        != postgres.get("server_leaf_sha256")
        or not isinstance(agents, dict)
        or agents.get("count") != 100
        or agents.get("running") != 100
        or agents.get("independent_mtls_identities") != 100
        or agents.get("independent_sqlite_ledgers") != 100
        or agents.get("control_plane_origin") != config["base_url"]
        or agents.get("image", {}).get("repo_digest") != config["agent_image"]
        or not isinstance(engines, dict)
        or engines.get("count") != 100
        or engines.get("running") != 100
        or engines.get("healthy") != 100
        or engines.get("inner_daemon_count") != 100
        or engines.get("container_count") != 2_000
        or engines.get("image", {}).get("repo_digest")
        != config["docker_engine_image"]
        or not isinstance(restart, dict)
        or restart.get("container_id")
        != control_plane.get("container", {}).get("container_id")
        or restart.get("started_at")
        != control_plane.get("container", {}).get("started_at")
        or restart.get("image_id") != control_plane.get("image", {}).get("image_id")
        or restart.get("repo_digest")
        != control_plane.get("image", {}).get("repo_digest")
    ):
        raise LiveEvidenceError("runtime evidence effective configuration is invalid")
    for section, fields in (
        (
            agents,
            (
                "node_ids_sha256",
                "container_ids_sha256",
                "started_at_sha256",
                "spiffe_ids_sha256",
                "certificate_fingerprints_sha256",
                "ledger_identities_sha256",
            ),
        ),
        (
            engines,
            (
                "outer_container_ids_sha256",
                "inner_daemon_ids_sha256",
                "socket_volumes_sha256",
                "data_volumes_sha256",
            ),
        ),
    ):
        for field in fields:
            require_hash(section.get(field), f"runtime {field}")
    return value


def verify_applied_manifest(
    config: dict[str, Any], config_path: pathlib.Path, program_path: pathlib.Path
) -> dict[str, str]:
    document = load_json(pathlib.Path(config["applied_manifest"]))
    expected_paths = {
        str(program_path),
        str(config_path),
        *(
            config[key]
            for key in (
                "ansible_inventory",
                "ansible_extra_vars_file",
                "ansible_playbook",
                "engine_evidence_script",
                "runtime_evidence_script",
                "runtime_expected_manifest",
                "environment_script",
                "nodes_file",
                "fixture_file",
                "ca_file",
                "image_provenance_record",
                "helper_manifest",
            )
        ),
    }
    if (
        not isinstance(document, dict)
        or set(document) != {"schema_version", "files"}
        or document.get("schema_version") != 1
        or not isinstance(document.get("files"), dict)
        or set(document["files"]) != expected_paths
    ):
        raise LiveEvidenceError("applied observer manifest does not cover the exact file set")
    observed: dict[str, str] = {}
    for path in sorted(expected_paths):
        digest = sha256_bytes(bounded_bytes(pathlib.Path(path)))
        if document["files"].get(path) != digest:
            raise LiveEvidenceError(f"applied observer file changed after provisioning: {path}")
        observed[path] = digest
    return observed


def verify_helper_manifest(config: dict[str, Any]) -> dict[str, str]:
    document = load_json(pathlib.Path(config["helper_manifest"]))
    if (
        not isinstance(document, dict)
        or set(document) != {"schema_version", "files"}
        or document.get("schema_version") != 1
        or not isinstance(document.get("files"), dict)
        or not document["files"]
    ):
        raise LiveEvidenceError("protected helper manifest is invalid")
    files: dict[str, str] = document["files"]
    required_executables = {
        parse_argv_json(config["token_argv_json"], "token_argv_json")[0],
        parse_argv_json(config["restart_argv_json"], "restart_argv_json")[0],
    }
    if not required_executables.issubset(files):
        raise LiveEvidenceError("protected helper manifest omits an invoked executable")
    observed: dict[str, str] = {}
    for path, expected in sorted(files.items()):
        if not isinstance(path, str) or not pathlib.Path(path).is_absolute():
            raise LiveEvidenceError("protected helper manifest path is invalid")
        require_hash(expected, f"protected helper {path}")
        candidate = pathlib.Path(path)
        digest = sha256_bytes(bounded_bytes(candidate))
        if digest != expected:
            raise LiveEvidenceError(f"protected helper changed after provisioning: {path}")
        observed[path] = digest
    for executable in required_executables:
        try:
            mode = pathlib.Path(executable).stat().st_mode
        except OSError as error:
            raise LiveEvidenceError(f"cannot stat protected helper {executable}") from error
        if os.name != "nt" and mode & 0o111 == 0:
            raise LiveEvidenceError(f"protected helper is not executable: {executable}")
    return observed


def verify_image_provenance(config: dict[str, Any]) -> dict[str, Any]:
    path = pathlib.Path(config["image_provenance_record"])
    raw = bounded_bytes(path)
    try:
        record = json.loads(raw)
    except json.JSONDecodeError as error:
        raise LiveEvidenceError("image provenance record is invalid JSON") from error
    expected_keys = {
        "schema_version",
        "candidate_sha",
        "repository",
        "source_workflow",
        "source_workflow_run_id",
        "source_workflow_run_attempt",
        "github_oidc_issuer",
        "control_plane",
        "agent",
        "capacity_fixture",
    }
    if (
        not isinstance(record, dict)
        or set(record) != expected_keys
        or record.get("schema_version") != 1
        or record.get("candidate_sha") != config["candidate_sha"]
        or record.get("source_workflow")
        != ".github/workflows/orchestrator-candidate-images.yml"
        or record.get("source_workflow_run_attempt") != 1
        or record.get("github_oidc_issuer")
        != "https://token.actions.githubusercontent.com"
        or not isinstance(record.get("repository"), str)
        or not record["repository"]
        or not isinstance(record.get("source_workflow_run_id"), str)
        or not re.fullmatch(r"[1-9][0-9]*", record["source_workflow_run_id"])
    ):
        raise LiveEvidenceError("image provenance record identity is invalid")
    for name, expected_reference in (
        ("control_plane", config["control_plane_image"]),
        ("agent", config["agent_image"]),
        ("capacity_fixture", config["fixture_image"]),
    ):
        subject = record.get(name)
        if (
            not isinstance(subject, dict)
            or set(subject) != {"reference", "digest"}
            or subject.get("reference") != expected_reference
            or subject.get("digest") != expected_reference.rsplit("@", 1)[1]
        ):
            raise LiveEvidenceError(f"image provenance {name} subject is invalid")
    return {
        "record_sha256": sha256_bytes(raw),
        "repository": record["repository"],
        "source_workflow": record["source_workflow"],
        "source_workflow_run_id": record["source_workflow_run_id"],
        "source_workflow_run_attempt": 1,
        "github_oidc_issuer": record["github_oidc_issuer"],
        "control_plane_reference": record["control_plane"]["reference"],
        "control_plane_digest": record["control_plane"]["digest"],
        "agent_reference": record["agent"]["reference"],
        "agent_digest": record["agent"]["digest"],
        "fixture_reference": record["capacity_fixture"]["reference"],
        "fixture_digest": record["capacity_fixture"]["digest"],
    }


def configuration_fingerprint(
    config: dict[str, Any],
    config_path: pathlib.Path,
    program_path: pathlib.Path,
    applied_files: dict[str, str],
) -> str:
    file_keys = (
        "ansible_inventory",
        "ansible_extra_vars_file",
        "ansible_playbook",
        "engine_evidence_script",
        "runtime_evidence_script",
        "runtime_expected_manifest",
        "environment_script",
        "nodes_file",
        "fixture_file",
        "ca_file",
        "image_provenance_record",
        "helper_manifest",
        "applied_manifest",
    )
    redacted = {
        "schema_version": 1,
        "candidate_sha": config["candidate_sha"],
        "fixture_image": config["fixture_image"],
        "control_plane_image": config["control_plane_image"],
        "agent_image": config["agent_image"],
        "postgres_image": config["postgres_image"],
        "docker_engine_image": config["docker_engine_image"],
        "base_origin_sha256": sha256_bytes(
            normalize_https_origin(config["base_url"]).encode()
        ),
        "token_argv_sha256": sha256_bytes(config["token_argv_json"].encode()),
        "restart_argv_sha256": sha256_bytes(
            json.dumps(
                parse_argv_json(config["restart_argv_json"], "restart_argv_json"),
                separators=(",", ":"),
            ).encode()
        ),
        "program_sha256": sha256_bytes(bounded_bytes(program_path)),
        "config_sha256": sha256_bytes(bounded_bytes(config_path)),
        "applied_files": applied_files,
        "protected_file_sha256": {
            key: sha256_bytes(bounded_bytes(pathlib.Path(config[key])))
            for key in file_keys
        },
    }
    canonical = json.dumps(redacted, sort_keys=True, separators=(",", ":")).encode()
    return sha256_bytes(canonical)


def collect(
    config: dict[str, Any], config_path: pathlib.Path, program_path: pathlib.Path
) -> dict[str, Any]:
    started_at = time.time()
    deadline = time.monotonic() + MAX_OBSERVATION_SECONDS
    applied_files = verify_applied_manifest(config, config_path, program_path)
    helper_files = verify_helper_manifest(config)
    provenance = verify_image_provenance(config)
    with tempfile.TemporaryDirectory(prefix="ojos-capacity-live-") as directory:
        output = pathlib.Path(directory)
        dynamic_vars = json.dumps(
            {
                "capacity_live_evidence_output_dir": str(output),
                "capacity_candidate_sha": config["candidate_sha"],
                "capacity_fixture_image": config["fixture_image"],
                "capacity_engine_evidence_script": config[
                    "engine_evidence_script"
                ],
                "capacity_runtime_evidence_script": config[
                    "runtime_evidence_script"
                ],
                "capacity_control_plane_image": config["control_plane_image"],
                "capacity_agent_image": config["agent_image"],
                "capacity_control_plane_api_url": config["base_url"],
                "capacity_runtime_expected_manifest_file": config[
                    "runtime_expected_manifest"
                ],
                "capacity_fixture_file": config["fixture_file"],
            },
            sort_keys=True,
            separators=(",", ":"),
        )
        run_redacted(
            (
                config["ansible_executable"],
                "--forks",
                "20",
                "--inventory",
                config["ansible_inventory"],
                config["ansible_playbook"],
                "--extra-vars",
                f"@{config['ansible_extra_vars_file']}",
                "--extra-vars",
                dynamic_vars,
            ),
            deadline,
            stdout=False,
        )
        aggregate_path = output / "engine-evidence.json"
        aggregate_raw = bounded_bytes(aggregate_path)
        try:
            aggregate = json.loads(aggregate_raw)
        except json.JSONDecodeError as error:
            raise LiveEvidenceError("Engine aggregate is invalid JSON") from error
        identities = aggregate_identity(aggregate, config)
        runtime_path = output / "runtime-evidence.json"
        runtime_raw = bounded_bytes(runtime_path)
        try:
            runtime = json.loads(runtime_raw)
        except json.JSONDecodeError as error:
            raise LiveEvidenceError("runtime evidence aggregate is invalid JSON") from error
        runtime = validate_runtime_aggregate(runtime, config)
        environment_stdout = run_redacted(
            (
                config["python_executable"],
                config["environment_script"],
                "--base-url",
                config["base_url"],
                "--ca-file",
                config["ca_file"],
                "--token-argv-json",
                config["token_argv_json"],
                "--candidate-sha",
                config["candidate_sha"],
                "--nodes-file",
                config["nodes_file"],
                "preflight",
                "--fixture-file",
                config["fixture_file"],
                "--engine-evidence-file",
                str(aggregate_path),
            ),
            deadline,
            stdout=True,
        )
        try:
            envelope = json.loads(environment_stdout)
        except json.JSONDecodeError as error:
            raise LiveEvidenceError("environment preflight stdout is invalid JSON") from error
        if (
            not isinstance(envelope, dict)
            or set(envelope) != {"status", "data"}
            or envelope.get("status") != "ok"
            or not isinstance(envelope.get("data"), dict)
        ):
            raise LiveEvidenceError("environment preflight did not return a strict success envelope")
        summary = envelope["data"]
        engine_summary = summary.get("engine_evidence")
        network = summary.get("network_evidence")
        if not isinstance(engine_summary, dict) or not isinstance(network, dict):
            raise LiveEvidenceError("environment preflight omitted Engine/network evidence")
        for field, expected in (
            ("endpoint_checks_total", 2_000),
            ("endpoint_checks_healthy", 2_000),
            ("endpoint_checks_failed", 0),
            ("link_probes_total", 8_000),
            ("link_probes_healthy", 8_000),
            ("link_probes_failed", 0),
            ("drift", 0),
        ):
            if network.get(field) != expected:
                raise LiveEvidenceError(f"network preflight {field} is not {expected}")
        endpoint_hash = require_hash(
            network.get("endpoint_ids_sha256"), "Endpoint identity hash"
        )
        link_hash = require_hash(network.get("link_ids_sha256"), "Link identity hash")
        checked_at = network.get("checked_at_epoch_seconds")
        if (
            isinstance(checked_at, bool)
            or not isinstance(checked_at, (int, float))
            or not math.isfinite(float(checked_at))
        ):
            raise LiveEvidenceError("network preflight timestamp is invalid")
        completed_at = time.time()
        applied_files_after = verify_applied_manifest(config, config_path, program_path)
        if applied_files_after != applied_files:
            raise LiveEvidenceError("observer files changed during an observation")
        if verify_helper_manifest(config) != helper_files:
            raise LiveEvidenceError("protected helpers changed during an observation")
        if verify_image_provenance(config) != provenance:
            raise LiveEvidenceError("image provenance changed during an observation")
        origin = normalize_https_origin(config["base_url"])
        restart_argv = json.dumps(
            parse_argv_json(config["restart_argv_json"], "restart_argv_json"),
            separators=(",", ":"),
        ).encode()
        topology_identity = {
            "topology_id": summary.get("topology_id"),
            "revision_id": summary.get("topology_revision_id"),
            "endpoint_ids_sha256": endpoint_hash,
            "link_ids_sha256": link_hash,
        }
        if any(
            not isinstance(topology_identity[field], str)
            or not topology_identity[field]
            for field in ("topology_id", "revision_id")
        ):
            raise LiveEvidenceError("environment preflight omitted topology identity")
        topology_identity_sha256 = sha256_bytes(
            json.dumps(
                topology_identity, sort_keys=True, separators=(",", ":")
            ).encode()
        )
        return {
            "schema_version": 1,
            "candidate_sha": config["candidate_sha"],
            "started_at_epoch_seconds": started_at,
            "completed_at_epoch_seconds": completed_at,
            "configuration_fingerprint_sha256": configuration_fingerprint(
                config, config_path, program_path, applied_files
            ),
            "observer_identity": {
                "program_sha256": applied_files[str(program_path)],
                "config_sha256": applied_files[str(config_path)],
                "applied_manifest_sha256": sha256_bytes(
                    bounded_bytes(pathlib.Path(config["applied_manifest"]))
                ),
                "helper_manifest_sha256": sha256_bytes(
                    bounded_bytes(pathlib.Path(config["helper_manifest"]))
                ),
                "helper_files_sha256": sha256_bytes(
                    json.dumps(
                        helper_files, sort_keys=True, separators=(",", ":")
                    ).encode()
                ),
                "ansible_playbook_sha256": applied_files[config["ansible_playbook"]],
            },
            "provenance_identity": provenance,
            "deployment_identity": {
                "control_plane_origin_sha256": sha256_bytes(origin.encode()),
                "restart_argv_sha256": sha256_bytes(restart_argv),
                "topology_id": topology_identity["topology_id"],
                "topology_revision_id": topology_identity["revision_id"],
                "topology_identity_sha256": topology_identity_sha256,
            },
            "engine_evidence": {
                "fixture_image": config["fixture_image"],
                "worker_count": 10,
                "engine_count": 100,
                "container_count": 2_000,
                "running_containers": 2_000,
                "healthy_containers": 2_000,
                "oldest_worker_observed_at_epoch_seconds": aggregate[
                    "collection_started_at_epoch_seconds"
                ],
                "newest_worker_observed_at_epoch_seconds": aggregate[
                    "collection_finished_at_epoch_seconds"
                ],
                "worker_collection_spread_seconds": aggregate[
                    "worker_collection_spread_seconds"
                ],
                "aggregate_sha256": sha256_bytes(aggregate_raw),
                **identities,
            },
            "network_evidence": {
                "checked_at_epoch_seconds": float(checked_at),
                "endpoint_checks_total": 2_000,
                "endpoint_checks_healthy": 2_000,
                "endpoint_checks_failed": 0,
                "link_probes_total": 8_000,
                "link_probes_healthy": 8_000,
                "link_probes_failed": 0,
                "drift": 0,
                "endpoint_ids_sha256": endpoint_hash,
                "link_ids_sha256": link_hash,
            },
            "runtime_evidence": runtime,
        }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", type=pathlib.Path, required=True)
    args = parser.parse_args()
    try:
        config = validate_config(load_json(args.config))
        observation = collect(
            config,
            args.config.absolute(),
            pathlib.Path(__file__).absolute(),
        )
        print(json.dumps(observation, sort_keys=True, separators=(",", ":")))
        return 0
    except (LiveEvidenceError, OSError, subprocess.SubprocessError) as error:
        print(f"capacity live evidence failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
