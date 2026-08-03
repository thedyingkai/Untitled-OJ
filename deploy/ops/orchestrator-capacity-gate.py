#!/usr/bin/env python3
"""Black-box Orchestrator v1 capacity, recovery and soak gate.

The production profile verifies the published 100/2,000/10,000/50 scale.
Every count and duration is configurable so CI can run a small smoke against a
prepared environment while a self-hosted runner executes the 24-hour profile.
Only deployment.health Operation plan/apply/cancel is mutated against
deployments already present in the prepared environment; no deployment is
installed or removed by this harness.
"""

from __future__ import annotations

import argparse
import copy
import concurrent.futures
import datetime
import email.utils
import hashlib
import ipaddress
import json
import math
import os
import platform
import re
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Iterable, cast


REPORT_SCHEMA_VERSION = 2
TOKEN_REFRESH_WINDOW_SECONDS = 600
MAX_PRODUCTION_TOKEN_LIFETIME_SECONDS = 7_200
PRODUCTION_WARMUP_SECONDS = 600
PRODUCTION_SAMPLE_SECONDS = 30
PRODUCTION_OPERATION_INTERVAL_SECONDS = 300
PRODUCTION_MINIMUM_VALID_SAMPLES = 2_736
PRODUCTION_MAX_SAMPLE_GAP_SECONDS = 90
PRODUCTION_WORKFLOW_PATH = ".github/workflows/orchestrator-capacity.yml"
ENVIRONMENT_HELPER_TIMEOUT_SECONDS = 85
CLOCK_OFFSET_TOLERANCE_USEC = 1_000_000
MAX_LOCAL_CLOCK_SKEW_SECONDS = 30
HTTP_DATE_RESOLUTION_SECONDS = 1.0
COMMIT_SHA_PATTERN = re.compile(r"^[0-9a-f]{40}$")
SHA256_PATTERN = re.compile(r"^[0-9a-f]{64}$")
IMMUTABLE_OCI_PATTERN = re.compile(r"^[^\s@]+@sha256:[0-9a-f]{64}$")
RUNNER_NAME_PATTERN = re.compile(r"^[A-Za-z0-9_.-]{1,128}$")
RUNNER_SERVICE_UNIT_PATTERN = re.compile(
    r"^actions\.runner\.(?:[A-Za-z0-9_.:@-]|\\x[0-9A-Fa-f]{2})+\.service$"
)
RUNNER_INVOCATION_ID_PATTERN = re.compile(r"^[0-9a-f]{32}$")
LINUX_BOOT_ID_PATTERN = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
)
DOCKER_RFC3339_NANO_PATTERN = re.compile(
    r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?Z$"
)
RUNNER_SERVICE_CONTINUITY_FIELDS = (
    "unit",
    "boot_id",
    "control_group",
    "process_control_group",
    "active_enter_timestamp",
    "active_enter_monotonic_usec",
    "exec_main_start_timestamp",
    "exec_main_start_monotonic_usec",
    "invocation_id",
    "main_pid",
    "listener_pid",
    "listener_start_ticks",
    "listener_clock_ticks_per_second",
    "listener_control_group",
    "listener_executable",
    "listener_start_boottime_usec",
    "listener_ancestor_depth",
    "observer_pid",
    "observer_start_ticks",
)
RUNNER_SERVICE_PROPERTIES = (
    "Id",
    "LoadState",
    "ActiveState",
    "SubState",
    "ActiveEnterTimestamp",
    "ActiveEnterTimestampMonotonic",
    "ExecMainStartTimestamp",
    "ExecMainStartTimestampMonotonic",
    "InvocationID",
    "MainPID",
    "ControlGroup",
)
REPOSITORY_ENVIRONMENT_OBSERVER_ARGV = [
    "/usr/bin/python3",
    "/opt/actions-runner/environment-observer/orchestrator-capacity-live-evidence.py",
    "--config",
    "/opt/actions-runner/environment-observer/config.json",
]


@dataclass
class Sample:
    name: str
    status: int
    latency_ms: float
    ok: bool
    detail: str = ""


@dataclass
class GateReport:
    profile: str
    started_at: str
    expected: dict[str, int]
    observed: dict[str, int] = field(default_factory=dict)
    thresholds_ms: dict[str, float] = field(default_factory=dict)
    measurements_ms: dict[str, float] = field(default_factory=dict)
    process: dict[str, float] = field(default_factory=dict)
    evidence: dict[str, Any] = field(default_factory=dict)
    schema_version: int = REPORT_SCHEMA_VERSION
    identity: dict[str, Any] = field(default_factory=dict)
    configuration: dict[str, Any] = field(default_factory=dict)
    samples: list[dict[str, Any]] = field(default_factory=list)
    inventory_checks: list[dict[str, Any]] = field(default_factory=list)
    operation_rounds: list[dict[str, Any]] = field(default_factory=list)
    environment_checks: list[dict[str, Any]] = field(default_factory=list)
    logs: dict[str, Any] = field(default_factory=dict)
    failures: list[str] = field(default_factory=list)

    def __post_init__(self) -> None:
        self._checkpoint_lock = threading.RLock()


def parse_argv_json(value: str, label: str) -> list[str]:
    try:
        decoded = json.loads(value)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"{label} must be a JSON string array") from error
    if (
        not isinstance(decoded, list)
        or not decoded
        or len(decoded) > 32
        or any(
            not isinstance(item, str) or not item or len(item) > 4_096
            for item in decoded
        )
    ):
        raise RuntimeError(
            f"{label} must contain 1-32 non-empty string arguments"
        )
    return decoded


def normalize_https_origin(value: str) -> str:
    if (
        not value
        or value != value.strip()
        or any(character.isspace() for character in value)
    ):
        raise RuntimeError("production base URL must be a direct HTTPS origin")
    try:
        parsed = urllib.parse.urlsplit(value)
        port = parsed.port
    except ValueError as error:
        raise RuntimeError("production base URL has an invalid port or host") from error
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
        raise RuntimeError("production base URL must be a direct HTTPS origin")
    hostname = parsed.hostname
    try:
        address = ipaddress.ip_address(hostname)
    except ValueError:
        if "%" in hostname:
            raise RuntimeError("production base URL host is invalid")
        try:
            canonical_host = hostname.encode("idna").decode("ascii").lower()
        except UnicodeError as error:
            raise RuntimeError("production base URL host is invalid") from error
        if not canonical_host or any(
            not label or len(label) > 63
            for label in canonical_host.rstrip(".").split(".")
        ):
            raise RuntimeError("production base URL host is invalid")
    else:
        canonical_host = address.compressed.lower()
        if address.version == 6:
            canonical_host = f"[{canonical_host}]"
    suffix = "" if port in (None, 443) else f":{port}"
    return f"https://{canonical_host}{suffix}"


class TokenProvider:
    """Refresh an OIDC token using a runner-owned command without a shell."""

    def __init__(
        self,
        argv_json: str,
        *,
        max_lifetime_seconds: float | None = None,
        now: Callable[[], float] = time.time,
        runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    ) -> None:
        self.argv = parse_argv_json(argv_json, "token argv")
        self._now = now
        self._runner = runner
        self.max_lifetime_seconds = max_lifetime_seconds
        self._token = ""
        self._expires_at = 0.0
        self._lock = threading.Lock()
        self.refresh_count = 0

    @property
    def expires_at(self) -> float:
        return self._expires_at

    def token(self) -> str:
        with self._lock:
            now = self._now()
            if self._token and self._expires_at - now > TOKEN_REFRESH_WINDOW_SECONDS:
                return self._token
            completed = self._runner(
                self.argv,
                stdin=subprocess.DEVNULL,
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
                shell=False,
            )
            if completed.returncode != 0:
                raise RuntimeError(
                    f"token helper exited with {completed.returncode}; stderr was redacted"
                )
            try:
                payload = json.loads(completed.stdout)
            except json.JSONDecodeError as error:
                raise RuntimeError("token helper stdout must be one JSON object") from error
            if not isinstance(payload, dict) or set(payload) != {
                "access_token",
                "expires_at",
            }:
                raise RuntimeError(
                    "token helper stdout must contain exactly access_token and expires_at"
                )
            token = payload.get("access_token")
            expires_at = payload.get("expires_at")
            if not isinstance(token, str) or not token or len(token) > 65_536:
                raise RuntimeError("token helper access_token must be a non-empty string")
            if (
                not isinstance(expires_at, (int, float))
                or isinstance(expires_at, bool)
                or not math.isfinite(float(expires_at))
            ):
                raise RuntimeError("token helper expires_at must be a Unix timestamp")
            expires_at = float(expires_at)
            if expires_at - now <= TOKEN_REFRESH_WINDOW_SECONDS:
                raise RuntimeError(
                    "token helper returned a token expiring within the 10-minute refresh window"
                )
            if (
                self.max_lifetime_seconds is not None
                and expires_at - now > self.max_lifetime_seconds
            ):
                raise RuntimeError(
                    "production token helper returned a token valid for longer than 2 hours"
                )
            self._token = token
            self._expires_at = expires_at
            self.refresh_count += 1
            return token


ENVIRONMENT_EVIDENCE_KEYS = {
    "schema_version",
    "candidate_sha",
    "started_at_epoch_seconds",
    "completed_at_epoch_seconds",
    "configuration_fingerprint_sha256",
    "observer_identity",
    "provenance_identity",
    "deployment_identity",
    "engine_evidence",
    "network_evidence",
    "runtime_evidence",
}
OBSERVER_IDENTITY_KEYS = {
    "program_sha256",
    "config_sha256",
    "applied_manifest_sha256",
    "helper_manifest_sha256",
    "helper_files_sha256",
    "ansible_playbook_sha256",
}
PROVENANCE_IDENTITY_KEYS = {
    "record_sha256",
    "repository",
    "source_workflow",
    "source_workflow_run_id",
    "source_workflow_run_attempt",
    "github_oidc_issuer",
    "control_plane_reference",
    "control_plane_digest",
    "agent_reference",
    "agent_digest",
    "fixture_reference",
    "fixture_digest",
}
DEPLOYMENT_IDENTITY_KEYS = {
    "control_plane_origin_sha256",
    "restart_argv_sha256",
    "topology_id",
    "topology_revision_id",
    "topology_identity_sha256",
}
ENGINE_EVIDENCE_KEYS = {
    "fixture_image",
    "worker_count",
    "engine_count",
    "container_count",
    "running_containers",
    "healthy_containers",
    "oldest_worker_observed_at_epoch_seconds",
    "newest_worker_observed_at_epoch_seconds",
    "worker_collection_spread_seconds",
    "aggregate_sha256",
    "node_ids_sha256",
    "deployment_ids_sha256",
    "container_ids_sha256",
}
NETWORK_EVIDENCE_KEYS = {
    "checked_at_epoch_seconds",
    "endpoint_checks_total",
    "endpoint_checks_healthy",
    "endpoint_checks_failed",
    "link_probes_total",
    "link_probes_healthy",
    "link_probes_failed",
    "drift",
    "endpoint_ids_sha256",
    "link_ids_sha256",
}


def finite_timestamp(value: Any, label: str) -> float:
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(float(value))
        or float(value) <= 0
    ):
        raise RuntimeError(f"{label} must be a positive Unix timestamp")
    return float(value)


def sha256_value(value: Any, label: str) -> str:
    if not isinstance(value, str) or not SHA256_PATTERN.fullmatch(value):
        raise RuntimeError(f"{label} must be 64 lowercase hexadecimal characters")
    return value


def canonical_json_sha256(value: Any) -> str:
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()


def local_linux_machine_id_sha256(
    path: Path = Path("/etc/machine-id"),
) -> str:
    try:
        if not path.is_file() or path.is_symlink() or path.stat().st_size > 256:
            raise RuntimeError("runner /etc/machine-id is not a bounded regular file")
        machine_id = path.read_text(encoding="ascii").strip()
    except (OSError, UnicodeError) as error:
        raise RuntimeError("runner /etc/machine-id cannot be read") from error
    if not re.fullmatch(r"[0-9a-f]{32}", machine_id):
        raise RuntimeError("runner /etc/machine-id is not canonical")
    return hashlib.sha256(machine_id.encode()).hexdigest()


def docker_started_at_key(value: Any, label: str) -> tuple[int, int]:
    if not isinstance(value, str):
        raise RuntimeError(f"{label} must be Docker RFC3339Nano")
    match = DOCKER_RFC3339_NANO_PATTERN.fullmatch(value)
    if match is None:
        raise RuntimeError(f"{label} must be Docker RFC3339Nano")
    try:
        seconds = int(
            datetime.datetime.strptime(match.group(1), "%Y-%m-%dT%H:%M:%S")
            .replace(tzinfo=datetime.timezone.utc)
            .timestamp()
        )
    except ValueError as error:
        raise RuntimeError(f"{label} must be Docker RFC3339Nano") from error
    nanoseconds = int((match.group(2) or "0").ljust(9, "0"))
    return seconds, nanoseconds


def validate_runtime_evidence(
    value: Any,
    candidate_sha: str,
    *,
    control_plane_image: str | None,
    agent_image: str | None,
    expected_origin_sha256: str | None,
    runner_machine_id_sha256: str | None,
) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != {
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
    }:
        raise RuntimeError("environment runtime evidence fields are invalid")
    if (
        value.get("schema_version") != 2
        or value.get("candidate_sha") != candidate_sha
        or value.get("host_count") != 13
    ):
        raise RuntimeError("environment runtime evidence identity is invalid")
    sha256_value(value.get("provision_manifest_sha256"), "runtime provision manifest")
    sha256_value(value.get("host_identity_sha256"), "runtime host identity")
    hosts = value.get("hosts")
    expected_roles = {
        "control-plane",
        "postgres",
        "runner",
        *(f"worker-{ordinal:02d}" for ordinal in range(10)),
    }
    if not isinstance(hosts, list) or len(hosts) != 13:
        raise RuntimeError("runtime evidence must contain exactly 13 hosts")
    roles: set[str] = set()
    machines: set[str] = set()
    hosts_by_role: dict[str, dict[str, Any]] = {}
    for host in hosts:
        if not isinstance(host, dict) or set(host) != {
            "role",
            "machine_id_sha256",
            "boot_id",
        }:
            raise RuntimeError("runtime host identity fields are invalid")
        role = host.get("role")
        machine = sha256_value(host.get("machine_id_sha256"), "runtime machine ID")
        boot = host.get("boot_id")
        if (
            not isinstance(role, str)
            or not isinstance(boot, str)
            or not LINUX_BOOT_ID_PATTERN.fullmatch(boot)
        ):
            raise RuntimeError("runtime host identity is invalid")
        roles.add(role)
        machines.add(machine)
        hosts_by_role[role] = host
    if roles != expected_roles or len(machines) != 13:
        raise RuntimeError("runtime host roles or physical machine identities are not exact")
    if (
        runner_machine_id_sha256 is not None
        and hosts_by_role["runner"]["machine_id_sha256"]
        != runner_machine_id_sha256
    ):
        raise RuntimeError("runtime runner host is not the executing gate machine")

    control_plane = value.get("control_plane")
    if not isinstance(control_plane, dict):
        raise RuntimeError("control-plane runtime evidence is invalid")
    image = control_plane.get("image")
    container = control_plane.get("container")
    host = control_plane.get("host")
    configuration = control_plane.get("configuration")
    database_tls = control_plane.get("database_tls_identity")
    if (
        set(control_plane) != {
            "schema_version",
            "candidate_sha",
            "provision_manifest_sha256",
            "host",
            "image",
            "container",
            "configuration",
            "database_tls_identity",
        }
        or control_plane.get("schema_version") != 2
        or control_plane.get("candidate_sha") != candidate_sha
        or control_plane.get("provision_manifest_sha256")
        != value.get("provision_manifest_sha256")
        or not isinstance(host, dict)
        or host.get("role") != "control-plane"
        or not isinstance(image, dict)
        or set(image) != {"reference", "repo_digest", "image_id", "oci_revision"}
        or not isinstance(container, dict)
        or set(container) != {"container_id", "container_name", "started_at", "state"}
        or container.get("state") != "RUNNING"
        or not isinstance(container.get("container_id"), str)
        or not re.fullmatch(r"[0-9a-f]{64}", container["container_id"])
        or not isinstance(container.get("container_name"), str)
        or not container["container_name"]
        or not isinstance(container.get("started_at"), str)
        or not isinstance(image.get("image_id"), str)
        or not re.fullmatch(r"sha256:[0-9a-f]{64}", image["image_id"])
        or image.get("oci_revision") != candidate_sha
        or image.get("repo_digest") != image.get("reference")
        or not IMMUTABLE_OCI_PATTERN.fullmatch(str(image.get("reference", "")))
        or (control_plane_image is not None and image.get("reference") != control_plane_image)
        or not isinstance(configuration, dict)
        or set(configuration) != {
            "effective_sha256",
            "provisioned_sha256",
            "non_sensitive",
        }
        or configuration.get("effective_sha256")
        != configuration.get("provisioned_sha256")
        or not isinstance(configuration.get("non_sensitive"), dict)
        or not isinstance(database_tls, dict)
        or set(database_tls) != {
            "verified_hostname",
            "port",
            "peer_leaf_sha256",
            "root_certificates_sha256",
            "tls_version",
        }
    ):
        raise RuntimeError("control-plane runtime image/process identity is invalid")
    sha256_value(configuration.get("effective_sha256"), "control-plane configuration")
    sha256_value(database_tls.get("peer_leaf_sha256"), "PostgreSQL peer certificate")
    if (
        not isinstance(database_tls.get("verified_hostname"), str)
        or not database_tls["verified_hostname"]
        or not isinstance(database_tls.get("port"), int)
        or not 1 <= database_tls["port"] <= 65_535
        or not isinstance(database_tls.get("tls_version"), str)
        or not database_tls["tls_version"].startswith("TLSv1.")
        or not isinstance(database_tls.get("root_certificates_sha256"), list)
        or not database_tls["root_certificates_sha256"]
    ):
        raise RuntimeError("control-plane PostgreSQL TLS identity is invalid")
    for root_hash in database_tls["root_certificates_sha256"]:
        sha256_value(root_hash, "PostgreSQL root certificate")
    docker_started_at_key(container.get("started_at"), "control-plane StartedAt")

    postgres = value.get("postgres")
    postgres_image = postgres.get("image") if isinstance(postgres, dict) else None
    postgres_container = (
        postgres.get("container") if isinstance(postgres, dict) else None
    )
    postgres_configuration = (
        postgres.get("configuration") if isinstance(postgres, dict) else None
    )
    if (
        not isinstance(postgres, dict)
        or set(postgres) != {
            "schema_version",
            "candidate_sha",
            "provision_manifest_sha256",
            "host",
            "image",
            "container",
            "configuration",
            "server_leaf_sha256",
            "root_certificates_sha256",
            "settings",
        }
        or postgres.get("schema_version") != 2
        or postgres.get("candidate_sha") != candidate_sha
        or postgres.get("provision_manifest_sha256")
        != value.get("provision_manifest_sha256")
        or postgres.get("host", {}).get("role") != "postgres"
        or not isinstance(postgres_image, dict)
        or set(postgres_image)
        != {"reference", "repo_digest", "image_id", "oci_revision"}
        or postgres_image.get("reference") != postgres_image.get("repo_digest")
        or not IMMUTABLE_OCI_PATTERN.fullmatch(
            str(postgres_image.get("reference", ""))
        )
        or not re.fullmatch(
            r"sha256:[0-9a-f]{64}", str(postgres_image.get("image_id", ""))
        )
        or not isinstance(postgres_container, dict)
        or set(postgres_container)
        != {"container_id", "container_name", "started_at", "state", "health"}
        or postgres_container.get("state") != "RUNNING"
        or postgres_container.get("health") != "HEALTHY"
        or not re.fullmatch(
            r"[0-9a-f]{64}", str(postgres_container.get("container_id", ""))
        )
        or not isinstance(postgres_container.get("container_name"), str)
        or not postgres_container["container_name"]
        or not isinstance(postgres_configuration, dict)
        or set(postgres_configuration)
        != {"effective_sha256", "provisioned_sha256", "non_sensitive"}
        or postgres_configuration.get("effective_sha256")
        != postgres_configuration.get("provisioned_sha256")
        or not isinstance(postgres_configuration.get("non_sensitive"), dict)
        or not isinstance(postgres.get("settings"), dict)
        or postgres.get("server_leaf_sha256") != database_tls.get("peer_leaf_sha256")
        or postgres.get("root_certificates_sha256")
        != database_tls.get("root_certificates_sha256")
    ):
        raise RuntimeError("PostgreSQL runtime/TLS identity is invalid")
    sha256_value(postgres_configuration.get("effective_sha256"), "PostgreSQL configuration")
    sha256_value(postgres.get("server_leaf_sha256"), "PostgreSQL server certificate")
    docker_started_at_key(postgres_container.get("started_at"), "PostgreSQL StartedAt")

    agents = value.get("agents")
    agent_runtime_image = agents.get("image") if isinstance(agents, dict) else None
    if (
        not isinstance(agents, dict)
        or set(agents) != {
            "count",
            "running",
            "control_plane_origin",
            "image",
            "node_ids_sha256",
            "container_ids_sha256",
            "started_at_sha256",
            "spiffe_ids_sha256",
            "certificate_fingerprints_sha256",
            "ledger_identities_sha256",
            "independent_mtls_identities",
            "independent_sqlite_ledgers",
        }
        or agents.get("count") != 100
        or agents.get("running") != 100
        or agents.get("independent_mtls_identities") != 100
        or agents.get("independent_sqlite_ledgers") != 100
        or not isinstance(agents.get("control_plane_origin"), str)
        or (
            expected_origin_sha256 is not None
            and hashlib.sha256(agents["control_plane_origin"].encode()).hexdigest()
            != expected_origin_sha256
        )
        or not isinstance(agent_runtime_image, dict)
        or set(agent_runtime_image)
        != {"reference", "repo_digest", "image_ids", "oci_revision"}
        or agent_runtime_image.get("reference") != agent_runtime_image.get("repo_digest")
        or not IMMUTABLE_OCI_PATTERN.fullmatch(
            str(agent_runtime_image.get("reference", ""))
        )
        or (
            agent_image is not None
            and agent_runtime_image.get("reference") != agent_image
        )
        or agent_runtime_image.get("oci_revision") != candidate_sha
        or not isinstance(agent_runtime_image.get("image_ids"), list)
        or len(agent_runtime_image["image_ids"]) != 1
        or not re.fullmatch(
            r"sha256:[0-9a-f]{64}", str(agent_runtime_image["image_ids"][0])
        )
    ):
        raise RuntimeError("Agent runtime image/cardinality identity is invalid")
    for key in (
        "node_ids_sha256",
        "container_ids_sha256",
        "started_at_sha256",
        "spiffe_ids_sha256",
        "certificate_fingerprints_sha256",
        "ledger_identities_sha256",
    ):
        sha256_value(agents.get(key), f"runtime Agent {key}")

    engines = value.get("engines")
    engine_runtime_image = engines.get("image") if isinstance(engines, dict) else None
    if (
        not isinstance(engines, dict)
        or set(engines) != {
            "count",
            "running",
            "healthy",
            "inner_daemon_count",
            "container_count",
            "image",
            "outer_container_ids_sha256",
            "inner_daemon_ids_sha256",
            "socket_volumes_sha256",
            "data_volumes_sha256",
        }
        or engines.get("count") != 100
        or engines.get("running") != 100
        or engines.get("healthy") != 100
        or engines.get("inner_daemon_count") != 100
        or engines.get("container_count") != 2_000
        or not isinstance(engine_runtime_image, dict)
        or set(engine_runtime_image) != {"reference", "repo_digest", "image_ids"}
        or engine_runtime_image.get("reference")
        != engine_runtime_image.get("repo_digest")
        or not IMMUTABLE_OCI_PATTERN.fullmatch(
            str(engine_runtime_image.get("reference", ""))
        )
        or not isinstance(engine_runtime_image.get("image_ids"), list)
        or len(engine_runtime_image["image_ids"]) != 1
        or not re.fullmatch(
            r"sha256:[0-9a-f]{64}", str(engine_runtime_image["image_ids"][0])
        )
    ):
        raise RuntimeError("Docker Engine runtime/cardinality identity is invalid")
    for key in (
        "outer_container_ids_sha256",
        "inner_daemon_ids_sha256",
        "socket_volumes_sha256",
        "data_volumes_sha256",
    ):
        sha256_value(engines.get(key), f"runtime Engine {key}")

    restart = value.get("restart_identity")
    if (
        not isinstance(restart, dict)
        or set(restart)
        != {"container_id", "container_name", "started_at", "image_id", "repo_digest"}
        or restart.get("container_id") != container.get("container_id")
        or restart.get("container_name") != container.get("container_name")
        or restart.get("started_at") != container.get("started_at")
        or restart.get("image_id") != image.get("image_id")
        or restart.get("repo_digest") != image.get("repo_digest")
    ):
        raise RuntimeError("control-plane restart identity is inconsistent")
    return value


def validate_environment_observation(
    payload: Any,
    candidate_sha: str,
    *,
    local_started_at: float,
    local_completed_at: float,
    control_plane_image: str | None = None,
    agent_image: str | None = None,
    fixture_image: str | None = None,
    provenance_record_sha256: str | None = None,
    image_workflow_run_id: str | None = None,
    repository: str | None = None,
    expected_origin_sha256: str | None = None,
    expected_restart_argv_sha256: str | None = None,
    expected_observer_program_sha256: str | None = None,
    runner_machine_id_sha256: str | None = None,
) -> dict[str, Any]:
    if not isinstance(payload, dict) or set(payload) != ENVIRONMENT_EVIDENCE_KEYS:
        raise RuntimeError("environment helper stdout has unexpected top-level fields")
    if payload.get("schema_version") != 1:
        raise RuntimeError("environment helper must use schema_version 1")
    if payload.get("candidate_sha") != candidate_sha:
        raise RuntimeError("environment helper candidate does not match the gate commit")
    started_at = finite_timestamp(
        payload.get("started_at_epoch_seconds"), "environment collection start"
    )
    completed_at = finite_timestamp(
        payload.get("completed_at_epoch_seconds"), "environment collection completion"
    )
    if (
        started_at > completed_at
        or completed_at - started_at > ENVIRONMENT_HELPER_TIMEOUT_SECONDS
        or completed_at < local_started_at - MAX_LOCAL_CLOCK_SKEW_SECONDS
        or completed_at > local_completed_at + MAX_LOCAL_CLOCK_SKEW_SECONDS
    ):
        raise RuntimeError("environment helper collection window is stale or too long")
    configuration_fingerprint = sha256_value(
        payload.get("configuration_fingerprint_sha256"),
        "environment configuration fingerprint",
    )

    observer = payload.get("observer_identity")
    if not isinstance(observer, dict) or set(observer) != OBSERVER_IDENTITY_KEYS:
        raise RuntimeError("environment observer identity fields are invalid")
    for field_name in OBSERVER_IDENTITY_KEYS:
        sha256_value(observer.get(field_name), f"environment observer {field_name}")
    if (
        expected_observer_program_sha256 is not None
        and observer.get("program_sha256") != expected_observer_program_sha256
    ):
        raise RuntimeError("deployed environment observer does not match repository source")

    provenance = payload.get("provenance_identity")
    if not isinstance(provenance, dict) or set(provenance) != PROVENANCE_IDENTITY_KEYS:
        raise RuntimeError("environment provenance identity fields are invalid")
    for field_name in (
        "record_sha256",
        "control_plane_digest",
        "agent_digest",
        "fixture_digest",
    ):
        value = provenance.get(field_name)
        if field_name == "record_sha256":
            sha256_value(value, f"environment provenance {field_name}")
        elif not isinstance(value, str) or not re.fullmatch(
            r"sha256:[0-9a-f]{64}", value
        ):
            raise RuntimeError(f"environment provenance {field_name} is invalid")
    if (
        provenance.get("source_workflow")
        != ".github/workflows/orchestrator-candidate-images.yml"
        or provenance.get("source_workflow_run_attempt") != 1
        or provenance.get("github_oidc_issuer")
        != "https://token.actions.githubusercontent.com"
        or (
            provenance_record_sha256 is not None
            and provenance.get("record_sha256") != provenance_record_sha256
        )
        or (
            image_workflow_run_id is not None
            and provenance.get("source_workflow_run_id") != image_workflow_run_id
        )
        or (repository is not None and provenance.get("repository") != repository)
        or (
            control_plane_image is not None
            and provenance.get("control_plane_reference") != control_plane_image
        )
        or (
            agent_image is not None
            and provenance.get("agent_reference") != agent_image
        )
        or (
            fixture_image is not None
            and provenance.get("fixture_reference") != fixture_image
        )
    ):
        raise RuntimeError("environment provenance does not match verified workflow inputs")
    for reference_name, digest_name in (
        ("control_plane_reference", "control_plane_digest"),
        ("agent_reference", "agent_digest"),
        ("fixture_reference", "fixture_digest"),
    ):
        reference = provenance.get(reference_name)
        if (
            not isinstance(reference, str)
            or not IMMUTABLE_OCI_PATTERN.fullmatch(reference)
            or reference.rsplit("@", 1)[1] != provenance[digest_name]
        ):
            raise RuntimeError(f"environment provenance {reference_name} is invalid")

    deployment = payload.get("deployment_identity")
    if not isinstance(deployment, dict) or set(deployment) != DEPLOYMENT_IDENTITY_KEYS:
        raise RuntimeError("environment deployment identity fields are invalid")
    for field_name in (
        "control_plane_origin_sha256",
        "restart_argv_sha256",
        "topology_identity_sha256",
    ):
        sha256_value(deployment.get(field_name), f"environment deployment {field_name}")
    for field_name in ("topology_id", "topology_revision_id"):
        if not isinstance(deployment.get(field_name), str) or not deployment[field_name]:
            raise RuntimeError(f"environment deployment {field_name} is invalid")
    if (
        expected_origin_sha256 is not None
        and deployment.get("control_plane_origin_sha256") != expected_origin_sha256
    ) or (
        expected_restart_argv_sha256 is not None
        and deployment.get("restart_argv_sha256") != expected_restart_argv_sha256
    ):
        raise RuntimeError("environment deployment origin/restart identity is invalid")

    engine = payload.get("engine_evidence")
    if not isinstance(engine, dict) or set(engine) != ENGINE_EVIDENCE_KEYS:
        raise RuntimeError("environment helper Engine evidence fields are invalid")
    if not isinstance(engine.get("fixture_image"), str) or not IMMUTABLE_OCI_PATTERN.fullmatch(
        engine["fixture_image"]
    ):
        raise RuntimeError("environment helper fixture image is not digest pinned")
    if fixture_image is not None and engine["fixture_image"] != fixture_image:
        raise RuntimeError("environment helper fixture image is not the verified subject")
    for field_name, expected in (
        ("worker_count", 10),
        ("engine_count", 100),
        ("container_count", 2_000),
        ("running_containers", 2_000),
        ("healthy_containers", 2_000),
    ):
        if engine.get(field_name) != expected:
            raise RuntimeError(
                f"environment helper Engine {field_name} is not {expected}"
            )
    oldest = finite_timestamp(
        engine.get("oldest_worker_observed_at_epoch_seconds"),
        "oldest Engine observation",
    )
    newest = finite_timestamp(
        engine.get("newest_worker_observed_at_epoch_seconds"),
        "newest Engine observation",
    )
    spread = engine.get("worker_collection_spread_seconds")
    if (
        isinstance(spread, bool)
        or not isinstance(spread, (int, float))
        or float(spread) < 0
        or float(spread) > 90
        or abs((newest - oldest) - float(spread)) > 1.0
        or oldest < started_at - MAX_LOCAL_CLOCK_SKEW_SECONDS
        or newest > completed_at + MAX_LOCAL_CLOCK_SKEW_SECONDS
    ):
        raise RuntimeError("environment helper Engine observation window is invalid")
    for field_name in (
        "aggregate_sha256",
        "node_ids_sha256",
        "deployment_ids_sha256",
        "container_ids_sha256",
    ):
        sha256_value(
            engine.get(field_name), f"environment Engine {field_name}"
        )

    network = payload.get("network_evidence")
    if not isinstance(network, dict) or set(network) != NETWORK_EVIDENCE_KEYS:
        raise RuntimeError("environment helper network evidence fields are invalid")
    checked_at = finite_timestamp(
        network.get("checked_at_epoch_seconds"), "network observation"
    )
    if checked_at < started_at - MAX_LOCAL_CLOCK_SKEW_SECONDS or checked_at > (
        completed_at + MAX_LOCAL_CLOCK_SKEW_SECONDS
    ):
        raise RuntimeError("environment helper network observation is outside its window")
    for field_name, expected in (
        ("endpoint_checks_total", 2_000),
        ("endpoint_checks_healthy", 2_000),
        ("endpoint_checks_failed", 0),
        ("link_probes_total", 8_000),
        ("link_probes_healthy", 8_000),
        ("link_probes_failed", 0),
        ("drift", 0),
    ):
        if network.get(field_name) != expected:
            raise RuntimeError(
                f"environment helper network {field_name} is not {expected}"
            )
    for field_name in ("endpoint_ids_sha256", "link_ids_sha256"):
        sha256_value(
            network.get(field_name), f"environment network {field_name}"
        )

    validate_runtime_evidence(
        payload.get("runtime_evidence"),
        candidate_sha,
        control_plane_image=control_plane_image,
        agent_image=agent_image,
        expected_origin_sha256=expected_origin_sha256,
        runner_machine_id_sha256=runner_machine_id_sha256,
    )

    return {
        **payload,
        "started_at_epoch_seconds": started_at,
        "completed_at_epoch_seconds": completed_at,
        "configuration_fingerprint_sha256": configuration_fingerprint,
    }


class EnvironmentEvidenceProvider:
    """Run a protected full-environment observer through an argv-only boundary."""

    def __init__(
        self,
        argv_json: str,
        candidate_sha: str,
        *,
        control_plane_image: str | None = None,
        agent_image: str | None = None,
        fixture_image: str | None = None,
        provenance_record_sha256: str | None = None,
        image_workflow_run_id: str | None = None,
        repository: str | None = None,
        base_url: str | None = None,
        restart_argv_json: str | None = None,
        observer_program_sha256: str | None = None,
        runner_machine_id_sha256: str | None = None,
        runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
        now: Callable[[], float] = time.time,
    ) -> None:
        self.argv = parse_argv_json(argv_json, "environment evidence argv")
        self.candidate_sha = candidate_sha
        self._runner = runner
        self._now = now
        self.control_plane_image = control_plane_image
        self.agent_image = agent_image
        self.fixture_image = fixture_image
        self.provenance_record_sha256 = provenance_record_sha256
        self.image_workflow_run_id = image_workflow_run_id
        self.repository = repository
        self.expected_origin_sha256 = (
            hashlib.sha256(normalize_https_origin(base_url).encode()).hexdigest()
            if base_url
            else None
        )
        self.expected_restart_argv_sha256 = (
            hashlib.sha256(
                json.dumps(
                    parse_argv_json(restart_argv_json, "restart argv"),
                    separators=(",", ":"),
                ).encode()
            ).hexdigest()
            if restart_argv_json
            else None
        )
        self.observer_program_sha256 = observer_program_sha256
        self.runner_machine_id_sha256 = runner_machine_id_sha256
        self.observation_count = 0
        self._stable_identity: dict[str, Any] | None = None

    @staticmethod
    def identities(observation: dict[str, Any]) -> tuple[dict[str, Any], dict[str, str]]:
        engine = observation["engine_evidence"]
        network = observation["network_evidence"]
        runtime = observation["runtime_evidence"]
        control_plane = runtime["control_plane"]
        postgres = runtime["postgres"]
        agents = runtime["agents"]
        engines = runtime["engines"]
        stable = {
            "configuration_fingerprint_sha256": observation[
                "configuration_fingerprint_sha256"
            ],
            "observer_identity": observation["observer_identity"],
            "provenance_identity": observation["provenance_identity"],
            "deployment_identity": observation["deployment_identity"],
            "fixture_image": engine["fixture_image"],
            "node_ids_sha256": engine["node_ids_sha256"],
            "deployment_ids_sha256": engine["deployment_ids_sha256"],
            "container_ids_sha256": engine["container_ids_sha256"],
            "endpoint_ids_sha256": network["endpoint_ids_sha256"],
            "link_ids_sha256": network["link_ids_sha256"],
            "host_identity_sha256": runtime["host_identity_sha256"],
            "hosts": runtime["hosts"],
            "provision_manifest_sha256": runtime["provision_manifest_sha256"],
            "control_plane_host": control_plane["host"],
            "control_plane_image": control_plane["image"],
            "control_plane_container_name": control_plane["container"][
                "container_name"
            ],
            "control_plane_configuration": control_plane["configuration"],
            "control_plane_database_tls_identity": control_plane[
                "database_tls_identity"
            ],
            "postgres": postgres,
            "agent_control_plane_origin": agents["control_plane_origin"],
            "agent_image": agents["image"],
            "agent_node_ids_sha256": agents["node_ids_sha256"],
            "agent_container_ids_sha256": agents["container_ids_sha256"],
            "agent_started_at_sha256": agents["started_at_sha256"],
            "agent_spiffe_ids_sha256": agents["spiffe_ids_sha256"],
            "agent_certificate_fingerprints_sha256": agents[
                "certificate_fingerprints_sha256"
            ],
            "agent_ledger_identities_sha256": agents[
                "ledger_identities_sha256"
            ],
            "engine_image": engines["image"],
            "engine_outer_container_ids_sha256": engines[
                "outer_container_ids_sha256"
            ],
            "engine_inner_daemon_ids_sha256": engines[
                "inner_daemon_ids_sha256"
            ],
            "engine_socket_volumes_sha256": engines["socket_volumes_sha256"],
            "engine_data_volumes_sha256": engines["data_volumes_sha256"],
        }
        process = {
            "container_id": control_plane["container"]["container_id"],
            "started_at": control_plane["container"]["started_at"],
        }
        return stable, process

    def observe(
        self,
        *,
        establish_stable: bool = True,
        restart_previous: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        local_started_at = self._now()
        completed = self._runner(
            self.argv,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=ENVIRONMENT_HELPER_TIMEOUT_SECONDS,
            check=False,
            shell=False,
        )
        local_completed_at = self._now()
        if completed.returncode != 0:
            raise RuntimeError(
                f"environment evidence helper exited with {completed.returncode}; stderr was redacted"
            )
        if not isinstance(completed.stdout, str) or len(completed.stdout) > 1_048_576:
            raise RuntimeError("environment evidence helper stdout is missing or oversized")
        try:
            payload = json.loads(completed.stdout)
        except json.JSONDecodeError as error:
            raise RuntimeError(
                "environment evidence helper stdout must be one JSON object"
            ) from error
        observation = validate_environment_observation(
            payload,
            self.candidate_sha,
            local_started_at=local_started_at,
            local_completed_at=local_completed_at,
            control_plane_image=self.control_plane_image,
            agent_image=self.agent_image,
            fixture_image=self.fixture_image,
            provenance_record_sha256=self.provenance_record_sha256,
            image_workflow_run_id=self.image_workflow_run_id,
            repository=self.repository,
            expected_origin_sha256=self.expected_origin_sha256,
            expected_restart_argv_sha256=self.expected_restart_argv_sha256,
            expected_observer_program_sha256=self.observer_program_sha256,
            runner_machine_id_sha256=self.runner_machine_id_sha256,
        )
        stable_identity, process_identity = self.identities(observation)
        if restart_previous is not None:
            previous_stable, previous_process = self.identities(restart_previous)
            if stable_identity != previous_stable:
                raise RuntimeError("environment identity changed across controlled restart")
            if docker_started_at_key(
                process_identity["started_at"], "post-restart control-plane StartedAt"
            ) <= docker_started_at_key(
                previous_process["started_at"], "pre-restart control-plane StartedAt"
            ):
                raise RuntimeError(
                    "controlled restart did not change control-plane StartedAt"
                )
        if establish_stable:
            complete_identity = {**stable_identity, "control_plane_process": process_identity}
            if self._stable_identity is None:
                self._stable_identity = complete_identity
            elif complete_identity != self._stable_identity:
                raise RuntimeError("environment evidence identity changed during the gate")
        self.observation_count += 1
        return observation


class Client:
    def __init__(
        self,
        base_url: str,
        token: str,
        internal_token: str,
        ca_file: str,
        timeout: float,
        token_provider: TokenProvider | None = None,
    ):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.headers = {"Accept": "application/json"}
        self.static_token = token
        self.token_provider = token_provider
        if internal_token:
            self.headers["x-ojos-orchestrator-token"] = internal_token
        context = ssl.create_default_context(cafile=ca_file or None)
        self.opener = urllib.request.build_opener(urllib.request.HTTPSHandler(context=context))

    def call(
        self,
        method: str,
        path: str,
        body: Any = None,
        idem: str = "",
        *,
        request_headers: dict[str, str] | None = None,
        timeout_seconds: float | None = None,
        maximum_response_bytes: int | None = None,
    ) -> tuple[int, bytes, float, dict[str, str]]:
        headers = dict(self.headers)
        if request_headers:
            headers.update(request_headers)
        token = self.token_provider.token() if self.token_provider else self.static_token
        if token:
            headers["Authorization"] = f"Bearer {token}"
        payload = None
        if body is not None:
            payload = json.dumps(body, separators=(",", ":")).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if idem:
            headers["Idempotency-Key"] = idem
        request = urllib.request.Request(self.base_url + path, data=payload, headers=headers, method=method)
        started = time.perf_counter()
        timeout = self.timeout if timeout_seconds is None else timeout_seconds

        def read_response(response: Any) -> bytes:
            if maximum_response_bytes is None:
                return response.read()
            raw = response.read(maximum_response_bytes + 1)
            if len(raw) > maximum_response_bytes:
                raise RuntimeError(
                    f"{method} {path} response exceeded {maximum_response_bytes} bytes"
                )
            return raw

        try:
            with self.opener.open(request, timeout=timeout) as response:
                raw = read_response(response)
                status = response.status
                response_headers = {key.lower(): value for key, value in response.headers.items()}
        except urllib.error.HTTPError as error:
            raw = read_response(error)
            status = error.code
            response_headers = {key.lower(): value for key, value in error.headers.items()}
        elapsed = (time.perf_counter() - started) * 1_000
        return status, raw, elapsed, response_headers

    def json(self, method: str, path: str, body: Any = None, idem: str = "") -> tuple[int, Any, float, dict[str, str]]:
        status, raw, elapsed, headers = self.call(method, path, body, idem)
        try:
            value = json.loads(raw.decode("utf-8")) if raw else {}
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RuntimeError(f"{method} {path} returned invalid JSON ({status}): {error}") from error
        return status, value, elapsed, headers

    def paged(self, path: str, limit: int = 200) -> tuple[list[Any], list[float]]:
        items: list[Any] = []
        latencies: list[float] = []
        cursor = ""
        seen: set[str] = set()
        while True:
            separator = "&" if "?" in path else "?"
            suffix = f"{separator}limit={limit}"
            if cursor:
                suffix += "&cursor=" + urllib.parse.quote(cursor, safe="")
            status, body, elapsed, _ = self.json("GET", path + suffix)
            latencies.append(elapsed)
            if status != 200:
                raise RuntimeError(f"GET {path} failed with {status}: {problem_detail(body)}")
            data = body.get("data", body)
            page = data.get("items", []) if isinstance(data, dict) else []
            if not isinstance(page, list):
                raise RuntimeError(f"GET {path} response has no data.items array")
            items.extend(page)
            next_cursor = data.get("next_cursor") if isinstance(data, dict) else None
            if not next_cursor:
                return items, latencies
            cursor = str(next_cursor)
            if cursor in seen:
                raise RuntimeError(f"GET {path} repeated cursor {cursor!r}")
            seen.add(cursor)


def percentile(values: Iterable[float], quantile: float) -> float:
    ordered = sorted(values)
    if not ordered:
        return 0.0
    index = max(0, math.ceil(len(ordered) * quantile) - 1)
    return float(ordered[index])


def problem_detail(body: Any) -> str:
    if isinstance(body, dict):
        return str(body.get("detail") or body.get("message") or body.get("code") or body)[:500]
    return str(body)[:500]


def item_id(item: Any, *names: str) -> str:
    if not isinstance(item, dict):
        return ""
    for name in names:
        value = item.get(name)
        if isinstance(value, str) and value:
            return value
    for nested in ("heads", "operation", "deployment", "instance", "node"):
        value = item.get(nested)
        if isinstance(value, dict):
            found = item_id(value, *names)
            if found:
                return found
    return ""


def item_value(item: Any, *names: str) -> Any:
    if not isinstance(item, dict):
        return None
    for name in names:
        if name in item:
            return item[name]
    for nested in ("heads", "operation", "deployment", "instance", "node", "status"):
        value = item.get(nested)
        if isinstance(value, dict):
            found = item_value(value, *names)
            if found is not None:
                return found
    return None


TERMINAL_OPERATION_STATUSES = {
    "SUCCEEDED",
    "FAILED",
    "CANCELLED",
    "NEEDS_ATTENTION",
    "ROLLED_BACK",
}
MAX_OPERATION_SSE_BYTES = 1_048_576
MAX_OPERATION_EVENT_CURSOR_BYTES = 16_384
OPERATION_MUTATION_MEASUREMENTS = {
    "operation.plan": "mutation_plan",
    "operation.confirm": "mutation_confirm",
    "operation.apply": "mutation_apply",
    "operation.cancel": "mutation_cancel",
}


def operation_status(body: Any) -> str:
    """Return the formal v1 status from a direct or wrapped Operation response."""
    value = body.get("data", body) if isinstance(body, dict) else {}
    if isinstance(value, dict) and isinstance(value.get("operation"), dict):
        value = value["operation"]
    if not isinstance(value, dict):
        return ""
    status = value.get("status")
    return str(status).strip().upper() if isinstance(status, str) else ""


def decode_operation_event_cursor(value: str) -> dict[str, Any]:
    if (
        not value
        or len(value) > MAX_OPERATION_EVENT_CURSOR_BYTES * 2
        or len(value) % 2
        or any(character not in "0123456789abcdef" for character in value)
    ):
        raise RuntimeError("Operation SSE event id is not a canonical cursor")
    try:
        decoded = bytes.fromhex(value)
        cursor = json.loads(decoded)
    except (ValueError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RuntimeError("Operation SSE event id is not a valid cursor") from error
    if not isinstance(cursor, dict) or set(cursor) != {
        "operation_revision",
        "job_sequences",
    }:
        raise RuntimeError("Operation SSE cursor fields are invalid")
    operation_revision = cursor.get("operation_revision")
    job_sequences = cursor.get("job_sequences")
    if (
        isinstance(operation_revision, bool)
        or not isinstance(operation_revision, int)
        or operation_revision < 0
        or not isinstance(job_sequences, dict)
        or any(
            not isinstance(job_id, str)
            or not job_id
            or isinstance(sequence, bool)
            or not isinstance(sequence, int)
            or sequence < 0
            for job_id, sequence in job_sequences.items()
        )
    ):
        raise RuntimeError("Operation SSE cursor values are invalid")
    return {
        "operation_revision": operation_revision,
        "job_sequences": dict(job_sequences),
    }


def event_cursor_strictly_after(
    candidate: dict[str, Any], previous: dict[str, Any]
) -> bool:
    candidate_revision = candidate["operation_revision"]
    previous_revision = previous["operation_revision"]
    candidate_jobs = candidate["job_sequences"]
    previous_jobs = previous["job_sequences"]
    if candidate_revision < previous_revision or any(
        candidate_jobs.get(job_id, -1) < sequence
        for job_id, sequence in previous_jobs.items()
    ):
        return False
    return candidate_revision > previous_revision or any(
        sequence > previous_jobs.get(job_id, -1)
        for job_id, sequence in candidate_jobs.items()
    )


def parse_operation_sse(
    raw: bytes,
    operation_id: str,
    *,
    after_cursor: dict[str, Any] | None = None,
) -> list[dict[str, Any]]:
    if len(raw) > MAX_OPERATION_SSE_BYTES:
        raise RuntimeError("Operation SSE response is oversized")
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise RuntimeError("Operation SSE response is not UTF-8") from error
    text = text.replace("\r\n", "\n")
    if "\r" in text or "\0" in text:
        raise RuntimeError("Operation SSE response contains invalid control bytes")
    events: list[dict[str, Any]] = []
    previous = after_cursor
    for block in text.split("\n\n"):
        fields: dict[str, str] = {}
        for line in block.splitlines():
            if not line or line.startswith(":"):
                continue
            name, separator, value = line.partition(":")
            if not separator:
                raise RuntimeError("Operation SSE response contains a malformed field")
            value = value[1:] if value.startswith(" ") else value
            if name == "retry":
                if not value.isdigit():
                    raise RuntimeError("Operation SSE retry field is invalid")
                continue
            if name not in {"id", "event", "data"} or name in fields:
                raise RuntimeError("Operation SSE response has duplicate or unknown fields")
            fields[name] = value
        if not fields:
            continue
        if set(fields) != {"id", "event", "data"}:
            raise RuntimeError("Operation SSE event is incomplete")
        cursor = decode_operation_event_cursor(fields["id"])
        if previous is not None and not event_cursor_strictly_after(cursor, previous):
            raise RuntimeError("Operation SSE event cursor did not strictly advance")
        try:
            data = json.loads(fields["data"])
        except json.JSONDecodeError as error:
            raise RuntimeError("Operation SSE data is invalid JSON") from error
        if not isinstance(data, dict):
            raise RuntimeError("Operation SSE data must be an object")
        event_type = fields["event"]
        if event_type == "operation":
            operation = data.get("operation")
            if (
                not isinstance(operation, dict)
                or operation.get("operation_id") != operation_id
                or isinstance(operation.get("revision"), bool)
                or not isinstance(operation.get("revision"), int)
                or operation.get("revision") != cursor["operation_revision"]
            ):
                raise RuntimeError(
                    "Operation SSE data identity/revision does not match its cursor"
                )
        elif event_type != "job":
            raise RuntimeError("Operation SSE event type is invalid")
        events.append(
            {
                "id": fields["id"],
                "event": event_type,
                "data": data,
                "cursor": cursor,
            }
        )
        previous = cursor
    return events


def select_operation_targets(
    deployments: list[Any], count: int
) -> list[tuple[str, str, str]]:
    """Select real deployment/node/container targets at evenly spread indexes."""
    if count < 1:
        raise ValueError("concurrent Operation target count must be positive")
    candidates: list[tuple[str, str, str]] = []
    seen_deployments: set[str] = set()
    seen_nodes: set[str] = set()
    for item in deployments:
        deployment_id = item_id(item, "deployment_id")
        node_id = item_id(item, "node_id")
        container_id = item_id(item, "container_id")
        if (
            not deployment_id
            or not node_id
            or not container_id
            or deployment_id in seen_deployments
        ):
            continue
        seen_deployments.add(deployment_id)
        if node_id not in seen_nodes:
            seen_nodes.add(node_id)
            candidates.append((deployment_id, node_id, container_id))
    if len(candidates) < count:
        raise RuntimeError(
            "capacity gate needs at least "
            f"{count} distinct Nodes with real deployment_id and container_id values; "
            f"found {len(candidates)}"
        )
    if count == 1:
        return [candidates[len(candidates) // 2]]
    last = len(candidates) - 1
    indexes = [round(index * last / (count - 1)) for index in range(count)]
    return [candidates[index] for index in indexes]


def topology_resource_count(client: Client, topologies: list[Any]) -> tuple[int, list[float]]:
    total = 0
    latencies: list[float] = []
    for item in topologies:
        topology_id = item_id(item, "topology_id")
        if not topology_id:
            continue
        status, body, elapsed, _ = client.json("GET", f"/api/v1/topologies/{urllib.parse.quote(topology_id, safe='')}")
        latencies.append(elapsed)
        if status != 200:
            raise RuntimeError(f"topology {topology_id} read failed with {status}: {problem_detail(body)}")
        data = body.get("data", body)
        applied_revision_id = item_value(data, "applied_revision_id")
        if not isinstance(applied_revision_id, str) or not applied_revision_id:
            continue
        status, body, elapsed, _ = client.json(
            "GET",
            f"/api/v1/topologies/{urllib.parse.quote(topology_id, safe='')}/revisions/"
            f"{urllib.parse.quote(applied_revision_id, safe='')}",
        )
        latencies.append(elapsed)
        if status != 200:
            raise RuntimeError(
                f"topology {topology_id} applied revision read failed with {status}: "
                f"{problem_detail(body)}"
            )
        revision_data = body.get("data", body) if isinstance(body, dict) else {}
        revision = (
            revision_data.get("revision", revision_data)
            if isinstance(revision_data, dict)
            else {}
        )
        spec = revision.get("spec", revision) if isinstance(revision, dict) else {}
        endpoints = spec.get("endpoints", []) if isinstance(spec, dict) else []
        links = spec.get("links", []) if isinstance(spec, dict) else []
        total += len(endpoints) + len(links)
    return total, latencies


def operation_cycle(
    client: Client,
    run_id: str,
    index: int,
    deployment_id: str,
    node_id: str,
    container_id: str,
) -> tuple[list[Sample], str, float]:
    operation_seed = f"{run_id}-{index:04d}"
    plan = {
        "action": "deployment.health",
        "fields": {
            "target_node_id": node_id,
            "deployment_id": deployment_id,
            "payload": {"container_id": container_id},
        },
    }
    samples: list[Sample] = []
    status, body, latency, _ = client.json(
        "POST", "/api/v1/operations:plan", plan, f"capacity-plan-{operation_seed}"
    )
    samples.append(Sample("operation.plan", status, latency, status == 201, problem_detail(body)))
    operation_id = ""
    if status == 201:
        data = body.get("data", body)
        operation_id = item_id(data, "operation_id")
    if not operation_id:
        return samples, "", 0.0
    encoded = urllib.parse.quote(operation_id, safe="")
    status, body, latency, _ = client.json(
        "POST", f"/api/v1/operations/{encoded}:confirm", {}, f"capacity-confirm-{operation_seed}"
    )
    samples.append(Sample("operation.confirm", status, latency, status == 200, problem_detail(body)))
    if status != 200:
        return samples, operation_id, 0.0
    event_path = f"/api/v1/operations/{encoded}/events"
    baseline_status = 0
    baseline_latency = 0.0
    baseline_cursor_id = ""
    baseline_cursor: dict[str, Any] | None = None
    baseline_detail = ""
    try:
        baseline_status, baseline_raw, baseline_latency, baseline_headers = client.call(
            "GET",
            event_path,
            request_headers={"Accept": "text/event-stream"},
            timeout_seconds=1.0,
            maximum_response_bytes=MAX_OPERATION_SSE_BYTES,
        )
        if baseline_status != 200:
            raise RuntimeError(f"baseline SSE returned HTTP {baseline_status}")
        if not baseline_headers.get("content-type", "").lower().startswith(
            "text/event-stream"
        ):
            raise RuntimeError("baseline Operation events response is not SSE")
        baseline_events = parse_operation_sse(baseline_raw, operation_id)
        if not baseline_events or not any(
            event["event"] == "operation" for event in baseline_events
        ):
            raise RuntimeError("baseline Operation SSE omitted its Operation event cursor")
        baseline_cursor_id = baseline_events[-1]["id"]
        baseline_cursor = baseline_events[-1]["cursor"]
    except RuntimeError as error:
        baseline_detail = str(error)
    baseline_ok = baseline_cursor is not None
    samples.append(
        Sample(
            "operation.event_baseline",
            baseline_status,
            baseline_latency,
            baseline_ok,
            baseline_detail,
        )
    )

    event_latency = 0.0
    apply_started: float | None = None
    if baseline_ok:
        apply_started = time.perf_counter()
        status, body, latency, _ = client.json(
            "POST",
            f"/api/v1/operations/{encoded}:apply",
            {},
            f"capacity-apply-{operation_seed}",
        )
        samples.append(
            Sample(
                "operation.apply",
                status,
                latency,
                status == 202,
                problem_detail(body),
            )
        )
    else:
        status = 0
    if status == 202 and baseline_cursor is not None and apply_started is not None:
        deadline = time.monotonic() + 5
        current_cursor_id = baseline_cursor_id
        current_cursor = baseline_cursor
        event_status = 0
        event_detail = "no post-apply Operation event arrived before the deadline"
        while time.monotonic() < deadline:
            remaining_seconds = deadline - time.monotonic()
            try:
                event_status, raw, _, headers = client.call(
                    "GET",
                    event_path,
                    request_headers={
                        "Accept": "text/event-stream",
                        "Last-Event-ID": current_cursor_id,
                    },
                    timeout_seconds=max(0.05, min(1.0, remaining_seconds)),
                    maximum_response_bytes=MAX_OPERATION_SSE_BYTES,
                )
                if event_status != 200:
                    event_detail = f"post-apply SSE returned HTTP {event_status}"
                elif not headers.get("content-type", "").lower().startswith(
                    "text/event-stream"
                ):
                    event_detail = "post-apply Operation events response is not SSE"
                    break
                else:
                    events = parse_operation_sse(
                        raw,
                        operation_id,
                        after_cursor=current_cursor,
                    )
                    if events:
                        current_cursor_id = events[-1]["id"]
                        current_cursor = events[-1]["cursor"]
                    if any(
                        event["event"] == "operation"
                        and event["cursor"]["operation_revision"]
                        > baseline_cursor["operation_revision"]
                        for event in events
                    ):
                        event_latency = (time.perf_counter() - apply_started) * 1_000
                        event_detail = ""
                        break
            except RuntimeError as error:
                event_detail = str(error)
                break
            time.sleep(0.05)
        samples.append(
            Sample(
                "operation.events",
                event_status,
                event_latency,
                event_latency > 0,
                event_detail,
            )
        )
    cancel_status, cancel_body, cancel_latency, _ = client.json(
        "POST", f"/api/v1/operations/{encoded}:cancel", {}, f"capacity-cancel-{operation_seed}"
    )
    cancel_ok = cancel_status == 202
    cancel_detail = problem_detail(cancel_body)
    if cancel_status == 409:
        get_status, get_body, _, _ = client.json(
            "GET", f"/api/v1/operations/{encoded}"
        )
        observed_status = operation_status(get_body)
        cancel_ok = (
            get_status == 200
            and observed_status in TERMINAL_OPERATION_STATUSES
        )
        cancel_detail = (
            f"cancel raced with Operation status {observed_status or 'UNKNOWN'} "
            f"(GET {get_status}); {cancel_detail}"
        )
    samples.append(
        Sample(
            "operation.cancel",
            cancel_status,
            cancel_latency,
            cancel_ok,
            cancel_detail,
        )
    )
    return samples, operation_id, event_latency


def parse_prometheus(text: str) -> dict[str, float]:
    values: dict[str, float] = {}
    for line in text.splitlines():
        if not line or line.startswith("#") or " " not in line:
            continue
        name, raw = line.rsplit(" ", 1)
        try:
            values[name] = float(raw)
        except ValueError:
            continue
    return values


def metric_value(metrics: dict[str, float], prefix: str) -> float:
    for name, value in metrics.items():
        if name == prefix or name.startswith(prefix + "{"):
            return value
    return 0.0


def required_metric_value(metrics: dict[str, float], name: str) -> float:
    value = metrics.get(name)
    if value is None or not math.isfinite(value):
        raise RuntimeError(f"Prometheus snapshot omitted finite metric {name}")
    return value


def metrics(client: Client) -> dict[str, float]:
    status, raw, _, _ = client.call("GET", "/metrics")
    if status != 200:
        raise RuntimeError(f"GET /metrics failed with {status}")
    return parse_prometheus(raw.decode("utf-8"))


def ready_snapshot(client: Client) -> tuple[float, dict[str, Any]]:
    status, body, elapsed, headers = client.json("GET", "/api/v1/healthz/ready")
    if status != 200:
        retry = headers.get("retry-after", "missing")
        raise RuntimeError(f"readiness failed with {status} (Retry-After={retry}): {problem_detail(body)}")
    data = body.get("data", body) if isinstance(body, dict) else {}
    if not isinstance(data, dict):
        raise RuntimeError("readiness response has no data object")
    return elapsed, data


def check_ready(client: Client) -> float:
    elapsed, _ = ready_snapshot(client)
    return elapsed


def validate_server_build(
    readiness: dict[str, Any], source_commit: str, profile: str
) -> dict[str, str]:
    build = readiness.get("build")
    if not isinstance(build, dict):
        raise RuntimeError("readiness response omitted data.build")
    normalized: dict[str, str] = {}
    for name in ("version", "commit_sha", "profile", "target"):
        value = build.get(name)
        if not isinstance(value, str) or not value.strip():
            raise RuntimeError(f"readiness data.build.{name} must be a non-empty string")
        normalized[name] = value.strip()
    if not COMMIT_SHA_PATTERN.fullmatch(normalized["commit_sha"]):
        raise RuntimeError("readiness data.build.commit_sha must be 40 lowercase hex characters")
    if source_commit and normalized["commit_sha"] != source_commit:
        raise RuntimeError(
            "readiness build commit "
            f"{normalized['commit_sha']} does not match workflow source commit {source_commit}"
        )
    if profile == "production" and normalized["profile"] != "production":
        raise RuntimeError(
            "production capacity evidence requires readiness build.profile=production"
        )
    if profile == "production" and normalized["version"] != "1.0.0":
        raise RuntimeError("production capacity evidence requires build.version=1.0.0")
    if profile == "production" and not (
        "x86_64" in normalized["target"].lower()
        and "linux" in normalized["target"].lower()
    ):
        raise RuntimeError("production capacity evidence requires a Linux x86_64 build target")
    return normalized


def storage_pool(readiness: dict[str, Any]) -> tuple[float, float]:
    storage = readiness.get("storage")
    if not isinstance(storage, dict):
        return 0.0, 0.0
    connections = storage.get("pool_connections", 0)
    idle = storage.get("pool_idle_connections", 0)
    if not isinstance(connections, (int, float)) or isinstance(connections, bool):
        return 0.0, 0.0
    if not isinstance(idle, (int, float)) or isinstance(idle, bool):
        return 0.0, 0.0
    return float(connections), float(idle)


def inspect_inventory(
    client: Client,
    expected: dict[str, int],
    permanent_running_seconds: int,
) -> tuple[dict[str, Any], list[Any], list[float]]:
    """Read every published resource and prove its live production state."""
    nodes, node_reads = client.paged("/api/v1/nodes")
    deployments, deployment_reads = client.paged("/api/v1/deployments")
    topologies, topology_reads = client.paged("/api/v1/topologies")
    topology_resources, topology_detail_reads = topology_resource_count(client, topologies)

    def node_health(node: Any) -> tuple[bool, float, str]:
        node_id = item_id(node, "node_id")
        if not node_id:
            return False, 0.0, "<missing-node-id>"
        status, body, latency, _ = client.json(
            "GET", f"/api/v1/nodes/{urllib.parse.quote(node_id, safe='')}/health"
        )
        data = body.get("data", body) if isinstance(body, dict) else {}
        ready = status == 200 and isinstance(data, dict) and data.get("ready") is True
        return ready, latency, node_id

    health_results: list[tuple[bool, float, str]] = []
    if nodes:
        with concurrent.futures.ThreadPoolExecutor(max_workers=min(32, len(nodes))) as pool:
            health_results = list(pool.map(node_health, nodes))
    unhealthy_nodes = sorted(node_id for ready, _, node_id in health_results if not ready)
    running_deployments = sum(
        1
        for deployment in deployments
        if str(item_value(deployment, "observed_state") or "").upper() == "RUNNING"
    )

    topology_drift = 0
    topology_in_sync = 0
    topology_status_reads: list[float] = []
    unhealthy_topologies: list[str] = []
    for topology in topologies:
        topology_id = item_id(topology, "topology_id")
        if not topology_id:
            unhealthy_topologies.append("<missing-topology-id>")
            continue
        status, body, latency, _ = client.json(
            "GET",
            f"/api/v1/topologies/{urllib.parse.quote(topology_id, safe='')}/status",
        )
        topology_status_reads.append(latency)
        data = body.get("data", body) if isinstance(body, dict) else {}
        value = data.get("status", data) if isinstance(data, dict) else {}
        drift = value.get("drift", []) if isinstance(value, dict) else None
        state = str(value.get("state", "")) if isinstance(value, dict) else ""
        if status != 200 or not isinstance(drift, list):
            unhealthy_topologies.append(topology_id)
            continue
        topology_drift += len(drift)
        if state.upper() == "IN_SYNC" and not drift:
            topology_in_sync += 1
        else:
            unhealthy_topologies.append(topology_id)

    operations, operation_reads = client.paged("/api/v1/operations")
    now_ms = int(time.time() * 1_000)
    permanent_operations: list[str] = []
    for operation in operations:
        status = str(item_value(operation, "status") or "").upper()
        updated = item_value(operation, "updated_at_ms")
        operation_id = item_id(operation, "operation_id") or "<unknown>"
        if isinstance(updated, bool) or not isinstance(updated, int) or updated < 0:
            permanent_operations.append(f"{operation_id}:invalid-updated-at")
            continue
        if (
            status in {"RUNNING", "ENQUEUING", "CANCELLING"}
            and now_ms - updated > permanent_running_seconds * 1_000
        ):
            permanent_operations.append(operation_id)

    check = {
        "sampled_at_epoch_seconds": time.time(),
        "nodes_total": len(nodes),
        "nodes_ready": len(nodes) - len(unhealthy_nodes),
        "unhealthy_node_ids": unhealthy_nodes[:100],
        "deployments_total": len(deployments),
        "deployments_running": running_deployments,
        "topologies_total": len(topologies),
        "topologies_in_sync": topology_in_sync,
        "topology_resources": topology_resources,
        "topology_drift": topology_drift,
        "unhealthy_topology_ids": unhealthy_topologies[:100],
        "permanent_operations": permanent_operations[:100],
        "ok": (
            len(nodes) >= expected["nodes"]
            and len(nodes) == len(health_results)
            and not unhealthy_nodes
            and len(deployments) >= expected["deployments"]
            and running_deployments == len(deployments)
            and topology_resources >= expected["topology_resources"]
            and topology_in_sync == len(topologies)
            and not unhealthy_topologies
            and topology_drift == 0
            and not permanent_operations
        ),
    }
    latencies = (
        node_reads
        + deployment_reads
        + topology_reads
        + topology_detail_reads
        + [latency for _, latency, _ in health_results]
        + topology_status_reads
        + operation_reads
    )
    return check, deployments, latencies


def atomic_write(path: Path, contents: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.checkpoint-",
        suffix=".tmp",
        dir=path.parent,
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as output:
            output.write(contents)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        if os.name != "nt":
            flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
            directory_descriptor = os.open(path.parent, flags)
            try:
                os.fsync(directory_descriptor)
            finally:
                os.close(directory_descriptor)
    finally:
        temporary.unlink(missing_ok=True)


def report_output(report: GateReport) -> str:
    with report._checkpoint_lock:
        document = {
            key: copy.deepcopy(value)
            for key, value in report.__dict__.items()
            if not key.startswith("_")
        }
    return json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True)


class EvidenceWriter:
    def __init__(self, report: GateReport, report_path: Path) -> None:
        self.report = report
        self.report_path = report_path
        self.log_path = report_path.with_suffix(".events.ndjson")
        self.metrics_path = report_path.with_suffix(".metrics.ndjson")
        self.environment_path = report_path.with_suffix(".environment.ndjson")
        self.log_records = 0
        self.metric_records = 0
        self.environment_records = 0
        self._periodic_stop = threading.Event()
        self._periodic_thread: threading.Thread | None = None
        self._periodic_error: BaseException | None = None
        self._sidecar_lock = threading.Lock()
        self._checkpoint_io_lock = threading.Lock()
        self._generation_lock = threading.Lock()
        self._snapshot_generation = 0
        self._written_generation = 0
        self._checkpoint_clock: Callable[[], float] = time.monotonic
        with self.report._checkpoint_lock:
            self.report.logs = {
                "index": [
                    {
                        "kind": "capacity_events_ndjson",
                        "path": self.log_path.name,
                        "records": 0,
                    },
                    {
                        "kind": "prometheus_snapshots_ndjson",
                        "path": self.metrics_path.name,
                        "records": 0,
                    },
                    {
                        "kind": "environment_observations_ndjson",
                        "path": self.environment_path.name,
                        "records": 0,
                    },
                ]
            }

    @staticmethod
    def _append(path: Path, record: dict[str, Any]) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8", newline="\n") as output:
            output.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")
            output.flush()
            os.fsync(output.fileno())

    def event(self, kind: str, **fields: Any) -> None:
        record = {
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "kind": kind,
            **fields,
        }
        with self._sidecar_lock:
            self._append(self.log_path, record)
        with self.report._checkpoint_lock:
            self.log_records += 1
            self.report.logs["index"][0]["records"] = self.log_records

    def prometheus_snapshot(
        self,
        sequence: int,
        phase: str,
        sampled_at_epoch_seconds: float,
        sample_clock_seconds: float,
        snapshot: dict[str, float],
        storage: dict[str, float],
    ) -> int:
        with self._sidecar_lock:
            self._append(
                self.metrics_path,
                {
                    "sequence": sequence,
                    "phase": phase,
                    "sampled_at_epoch_seconds": sampled_at_epoch_seconds,
                    "sample_clock_seconds": sample_clock_seconds,
                    "metrics": dict(sorted(snapshot.items())),
                    "storage": dict(sorted(storage.items())),
                },
            )
        with self.report._checkpoint_lock:
            self.metric_records += 1
            self.report.logs["index"][1]["records"] = self.metric_records
            return self.metric_records

    def checkpoint(self, *, complete: bool = False) -> None:
        with self.report._checkpoint_lock:
            observed_at = time.time()
            observed_clock = self._checkpoint_clock()
            history = self.report.evidence.setdefault("checkpoint_history", [])
            history.append(
                {
                    "sequence": len(history) + 1,
                    "epoch_seconds": observed_at,
                    "clock_seconds": observed_clock,
                }
            )
            self.report.evidence["checkpoint_count"] = len(history)
            self.report.evidence["checkpointed_at"] = time.strftime(
                "%Y-%m-%dT%H:%M:%SZ", time.gmtime(observed_at)
            )
            if complete:
                completed_at = datetime.datetime.fromtimestamp(
                    observed_at, datetime.timezone.utc
                ).isoformat(timespec="microseconds").replace("+00:00", "Z")
                self.report.evidence["completed_at"] = completed_at
                self.report.evidence["checkpointed_at"] = completed_at
            document = {
                key: copy.deepcopy(value)
                for key, value in self.report.__dict__.items()
                if not key.startswith("_")
            }
            with self._generation_lock:
                self._snapshot_generation += 1
                generation = self._snapshot_generation
        contents = json.dumps(
            document, ensure_ascii=False, indent=2, sort_keys=True
        ) + "\n"
        with self._checkpoint_io_lock:
            with self._generation_lock:
                if generation <= self._written_generation:
                    return
            atomic_write(self.report_path, contents)
            with self._generation_lock:
                self._written_generation = generation

    def start_periodic_checkpoints(
        self,
        interval_seconds: float = 30.0,
        *,
        clock: Callable[[], float] = time.monotonic,
        clock_name: str = "CLOCK_MONOTONIC",
    ) -> None:
        if interval_seconds <= 0 or self._periodic_thread is not None:
            raise RuntimeError("periodic checkpoint interval/state is invalid")
        with self.report._checkpoint_lock:
            self.report.evidence["checkpoint_interval_seconds"] = interval_seconds
            self.report.evidence["checkpoint_clock"] = clock_name
        self._checkpoint_clock = clock

        def run() -> None:
            try:
                next_deadline = clock() + interval_seconds
                while True:
                    delay = max(0.0, next_deadline - clock())
                    if self._periodic_stop.wait(delay):
                        break
                    self.checkpoint()
                    next_deadline += interval_seconds
            except BaseException as error:  # surfaced synchronously on stop/checkpoint
                self._periodic_error = error
                self._periodic_stop.set()

        self._periodic_thread = threading.Thread(
            target=run,
            name="orchestrator-capacity-checkpoint",
            daemon=True,
        )
        self._periodic_thread.start()

    def stop_periodic_checkpoints(self) -> None:
        thread = self._periodic_thread
        if thread is None:
            return
        self._periodic_stop.set()
        thread.join()
        self._periodic_thread = None
        if self._periodic_error is not None:
            error = self._periodic_error
            self._periodic_error = None
            raise RuntimeError(f"periodic checkpoint failed: {error}") from error

    def environment_snapshot(
        self,
        phase: str,
        operation_round_index: int | None,
        observation: dict[str, Any],
    ) -> int:
        with self.report._checkpoint_lock:
            sequence = self.environment_records + 1
        with self._sidecar_lock:
            self._append(
                self.environment_path,
                {
                    "sequence": sequence,
                    "phase": phase,
                    "operation_round_index": operation_round_index,
                    "recorded_at_epoch_seconds": time.time(),
                    "observation": observation,
                },
            )
        with self.report._checkpoint_lock:
            self.environment_records = sequence
            self.report.logs["index"][2]["records"] = sequence
            return sequence

    def finalize(self) -> None:
        updates: list[dict[str, Any]] = []
        with self._sidecar_lock:
            for index, path in enumerate(
                (self.log_path, self.metrics_path, self.environment_path)
            ):
                if path.exists():
                    raw = path.read_bytes()
                    updates.append(
                        {
                            "index": index,
                            "bytes": len(raw),
                            "sha256": hashlib.sha256(raw).hexdigest(),
                        }
                    )
        with self.report._checkpoint_lock:
            for update in updates:
                self.report.logs["index"][update["index"]].update(
                    bytes=update["bytes"], sha256=update["sha256"]
                )
        self.checkpoint(complete=True)


def create_restart_probe(client: Client, run_id: str, node_id: str) -> str:
    """Persist an unapplied Operation whose identity must survive restart."""
    seed = f"restart-{run_id}"
    status, body, _, _ = client.json(
        "POST",
        "/api/v1/operations:plan",
        {
            "action": "deployment.health",
            "fields": {
                "target_node_id": node_id,
                "deployment_id": f"capacity-restart-probe-{run_id}",
                "payload": {"capacity_restart_probe": run_id},
            },
        },
        f"capacity-restart-plan-{seed}",
    )
    if status != 201:
        raise RuntimeError(
            f"restart probe Operation plan failed with {status}: {problem_detail(body)}"
        )
    data = body.get("data", body)
    operation_id = item_id(data, "operation_id")
    if not operation_id:
        raise RuntimeError("restart probe Operation response omitted operation_id")
    return operation_id


def operation_durability_snapshot(
    client: Client, operation_id: str
) -> dict[str, Any]:
    encoded = urllib.parse.quote(operation_id, safe="")
    status, body, _, _ = client.json("GET", f"/api/v1/operations/{encoded}")
    data = body.get("data", body) if isinstance(body, dict) else {}
    operation = data.get("operation", data) if isinstance(data, dict) else {}
    if status != 200 or not isinstance(operation, dict):
        raise RuntimeError(
            f"restart probe Operation {operation_id} could not be read: HTTP {status}"
        )
    revision = operation.get("revision")
    action = operation.get("action")
    operation_status_value = operation.get("status")
    if (
        operation.get("operation_id") != operation_id
        or isinstance(revision, bool)
        or not isinstance(revision, int)
        or revision <= 0
        or not isinstance(action, str)
        or not action
        or not isinstance(operation_status_value, str)
        or not operation_status_value
        or "request" not in operation
    ):
        raise RuntimeError("restart probe Operation document is incomplete")
    event_status, raw, _, headers = client.call(
        "GET",
        f"/api/v1/operations/{encoded}/events",
        request_headers={"Accept": "text/event-stream"},
        timeout_seconds=1.0,
        maximum_response_bytes=MAX_OPERATION_SSE_BYTES,
    )
    if event_status != 200 or not headers.get("content-type", "").lower().startswith(
        "text/event-stream"
    ):
        raise RuntimeError("restart probe Operation event cursor is unavailable")
    events = parse_operation_sse(raw, operation_id)
    operation_events = [event for event in events if event["event"] == "operation"]
    if not operation_events:
        raise RuntimeError("restart probe Operation event stream omitted its cursor")
    last = operation_events[-1]
    if last["cursor"]["operation_revision"] != revision:
        raise RuntimeError("restart probe Operation revision and event cursor disagree")
    return {
        "operation_id": operation_id,
        "status": operation_status_value,
        "action": action,
        "revision": revision,
        "request_sha256": canonical_json_sha256(operation["request"]),
        "operation_sha256": canonical_json_sha256(operation),
        "event_cursor": last["id"],
    }


def restart_argv(value: str) -> list[str]:
    return parse_argv_json(value, "restart argv")


def repository_observer_sha256() -> str:
    path = (
        Path(__file__).resolve().parents[1]
        / "capacity"
        / "orchestrator-capacity-live-evidence.py"
    )
    if path.is_symlink() or not path.is_file():
        raise RuntimeError("repository environment observer source is unavailable")
    return hashlib.sha256(path.read_bytes()).hexdigest()


def trigger_restart(
    client: Client,
    report: GateReport,
    operation_id: str,
    argv_json: str,
    deadline_seconds: float,
) -> None:
    """Run the runner-owned restart command and prove outage, recovery, and durability.

    The command is an argv array and is executed without a shell. Production
    runners normally point it at a narrowly scoped systemd/container helper.
    The target origin must be the single active control plane, not a load
    balancer that can hide the restart.
    """
    argv = restart_argv(argv_json)
    pre_restart_operation = operation_durability_snapshot(client, operation_id)
    started = time.monotonic()
    unavailable_at: float | None = None
    recovered_at: float | None = None
    exit_code: int | None = None
    with tempfile.TemporaryFile() as command_log:
        process = subprocess.Popen(
            argv,
            stdin=subprocess.DEVNULL,
            stdout=command_log,
            stderr=subprocess.STDOUT,
            shell=False,
        )
        deadline = started + deadline_seconds
        while time.monotonic() < deadline:
            exit_code = process.poll()
            ready = False
            try:
                check_ready(client)
                ready = True
            except Exception:
                if unavailable_at is None:
                    unavailable_at = time.monotonic()
            if exit_code is not None and unavailable_at is not None and ready:
                recovered_at = time.monotonic()
                break
            time.sleep(0.1)
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
            raise RuntimeError(
                f"restart command did not finish within {deadline_seconds:.0f}s"
            )
        exit_code = process.returncode
        if exit_code != 0:
            command_log.seek(0)
            detail = command_log.read(4_096).decode("utf-8", errors="replace")
            raise RuntimeError(
                f"restart command exited with {exit_code}: {detail.strip()}"
            )
    if unavailable_at is None:
        raise RuntimeError(
            "restart command completed without an observed readiness outage; "
            "the gate must target the single active control-plane origin"
        )
    if recovered_at is None:
        raise RuntimeError(
            f"control plane did not become ready within {deadline_seconds:.0f}s after restart"
        )

    post_restart_operation = operation_durability_snapshot(client, operation_id)
    if post_restart_operation != pre_restart_operation:
        raise RuntimeError(
            f"restart changed durable Operation {operation_id} state or event cursor"
        )
    recovery_ms = (recovered_at - unavailable_at) * 1_000
    with report._checkpoint_lock:
        report.measurements_ms["recovery"] = recovery_ms
        report.evidence.update(
            restart_triggered=True,
            restart_unavailable_observed=True,
            restart_exit_code=exit_code,
            restart_argv_count=len(argv),
            restart_probe_operation_id=operation_id,
            restart_probe_recovered=True,
            restart_probe_pre=pre_restart_operation,
            restart_probe_post=post_restart_operation,
            restart_recovery_ms=recovery_ms,
        )


def redacted_configuration(args: argparse.Namespace) -> dict[str, Any]:
    origin_value = (
        normalize_https_origin(args.base_url)
        if args.profile == "production"
        else args.base_url
    )
    origin = origin_value.encode("utf-8")
    redacted = {
        "base_origin_sha256": hashlib.sha256(origin).hexdigest(),
        "ca_configured": bool(args.ca_file),
        "authentication": "refreshing_oidc_helper"
        if args.token_argv_json
        else ("static" if args.token else "none"),
        "internal_token_configured": bool(args.internal_token),
        "environment_evidence": "protected_argv_helper"
        if args.environment_argv_json
        else "none",
        "nodes": args.nodes,
        "deployments": args.deployments,
        "topology_resources": args.topology_resources,
        "concurrent_operations": args.concurrent_operations,
        "soak_seconds": args.soak_seconds,
        "warmup_seconds": args.warmup_seconds,
        "sample_seconds": args.sample_seconds,
        "operation_interval_seconds": args.operation_interval_seconds,
    }
    if args.profile == "production":
        redacted.update(
            control_plane_image=args.control_plane_image,
            agent_image=args.agent_image,
            fixture_image=args.fixture_image,
            image_workflow_run_id=args.image_workflow_run_id,
            image_provenance_record_sha256=args.image_provenance_record_sha256,
            environment_observer_program_sha256=repository_observer_sha256(),
        )
    canonical = json.dumps(redacted, separators=(",", ":"), sort_keys=True).encode(
        "utf-8"
    )
    return {
        "redacted": redacted,
        "fingerprint_sha256": hashlib.sha256(canonical).hexdigest(),
    }


def linux_boottime_ns() -> int:
    clock = getattr(time, "CLOCK_BOOTTIME", None)
    clock_gettime_ns = cast(
        Callable[[int], int] | None,
        getattr(time, "clock_gettime_ns", None),
    )
    if clock is None or not callable(clock_gettime_ns):
        raise RuntimeError("production sampling requires Linux CLOCK_BOOTTIME")
    return clock_gettime_ns(clock)  # pylint: disable=not-callable


def linux_boottime_seconds() -> float:
    return linux_boottime_ns() / 1_000_000_000


def bracket_clock_sample(
    monotonic_ns: Callable[[], int],
    boottime_ns: Callable[[], int],
) -> dict[str, int]:
    """Bracket one BOOTTIME read between two MONOTONIC reads."""

    monotonic_lower_usec = monotonic_ns() // 1_000
    boottime_usec = boottime_ns() // 1_000
    monotonic_upper_usec = monotonic_ns() // 1_000
    if (
        monotonic_lower_usec < 0
        or boottime_usec < 0
        or monotonic_upper_usec < monotonic_lower_usec
    ):
        raise RuntimeError("clock bracket moved backwards or returned a negative value")
    return {
        "monotonic_lower_usec": monotonic_lower_usec,
        "monotonic_upper_usec": monotonic_upper_usec,
        "boottime_usec": boottime_usec,
        "offset_lower_usec": boottime_usec - monotonic_upper_usec,
        "offset_upper_usec": boottime_usec - monotonic_lower_usec,
    }


def intersect_clock_offsets(
    first: dict[str, int],
    second: dict[str, int],
    label: str,
) -> tuple[int, int]:
    """Return a conservative common BOOTTIME-MONOTONIC offset interval."""

    lower = max(first["offset_lower_usec"], second["offset_lower_usec"])
    upper = min(first["offset_upper_usec"], second["offset_upper_usec"])
    if lower > upper:
        if lower - upper > CLOCK_OFFSET_TOLERANCE_USEC:
            raise RuntimeError(f"runner host suspended {label}")
        midpoint = (lower + upper) // 2
        return midpoint, midpoint
    return lower, upper


def require_clock_offset_continuity(
    reference: dict[str, Any],
    current: dict[str, Any],
    label: str,
) -> None:
    reference_lower = int(reference["clock_offset_lower_usec"])
    reference_upper = int(reference["clock_offset_upper_usec"])
    current_lower = int(current["clock_offset_lower_usec"])
    current_upper = int(current["clock_offset_upper_usec"])
    if current_lower > reference_upper + CLOCK_OFFSET_TOLERANCE_USEC:
        raise RuntimeError(f"runner host suspended {label}")
    if current_upper < reference_lower - CLOCK_OFFSET_TOLERANCE_USEC:
        raise RuntimeError(f"runner host clock offset moved backwards {label}")


def parse_github_timestamp(value: Any, label: str) -> float:
    if not isinstance(value, str) or not value.endswith("Z"):
        raise RuntimeError(f"{label} must be an RFC 3339 UTC timestamp")
    try:
        parsed = datetime.datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise RuntimeError(f"{label} must be an RFC 3339 UTC timestamp") from error
    if parsed.tzinfo is None:
        raise RuntimeError(f"{label} must include a UTC offset")
    return parsed.timestamp()


def github_run_metadata(
    token: str,
    repository: str,
    run_id: str,
    run_attempt: str,
    workflow: str,
    source_commit: str,
    *,
    opener: Callable[..., Any] = urllib.request.urlopen,
    now: Callable[[], float] = time.time,
    monotonic_ns: Callable[[], int] = time.monotonic_ns,
    boottime_ns: Callable[[], int] = linux_boottime_ns,
) -> dict[str, Any]:
    """Read immutable dispatch metadata from the Actions API without exposing the token."""

    if not token or len(token) > 65_536:
        raise RuntimeError("GitHub Actions API token must be a non-empty bounded string")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise RuntimeError("GITHUB_REPOSITORY is not a canonical owner/repository name")
    if not run_id.isdigit() or not run_attempt.isdigit():
        raise RuntimeError("GitHub run id and attempt must be decimal integers")
    if run_attempt != "1":
        raise RuntimeError("production capacity evidence does not permit workflow reruns")
    url = f"https://api.github.com/repos/{repository}/actions/runs/{run_id}"
    request = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "User-Agent": "orchestrator-capacity-gate/1.0",
            "X-GitHub-Api-Version": "2022-11-28",
        },
        method="GET",
    )
    request_clock = bracket_clock_sample(monotonic_ns, boottime_ns)
    api_date: str | None = None
    try:
        with opener(request, timeout=15) as response:
            status = getattr(response, "status", None)
            if status is None:
                status = response.getcode()
            headers = getattr(response, "headers", None)
            if headers is not None:
                api_date = headers.get("Date")
            elif hasattr(response, "getheader"):
                api_date = response.getheader("Date")
            raw = response.read(1_048_577)
    except (OSError, urllib.error.URLError) as error:
        raise RuntimeError("could not read the current GitHub Actions run metadata") from error
    response_clock = bracket_clock_sample(monotonic_ns, boottime_ns)
    offset_lower, offset_upper = intersect_clock_offsets(
        request_clock, response_clock, "during the GitHub Actions API request"
    )
    if status != 200:
        raise RuntimeError(f"GitHub Actions run metadata returned HTTP {status}")
    if len(raw) > 1_048_576:
        raise RuntimeError("GitHub Actions run metadata exceeded one MiB")
    try:
        payload = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RuntimeError("GitHub Actions run metadata was not valid JSON") from error
    if not isinstance(payload, dict):
        raise RuntimeError("GitHub Actions run metadata must be an object")
    if not isinstance(api_date, str) or not api_date.strip():
        raise RuntimeError("GitHub Actions API response did not include a Date header")
    try:
        parsed_api_date = email.utils.parsedate_to_datetime(api_date)
    except (TypeError, ValueError) as error:
        raise RuntimeError("GitHub Actions API Date header is invalid") from error
    if (
        parsed_api_date.tzinfo is None
        or parsed_api_date.utcoffset() != datetime.timedelta(0)
    ):
        raise RuntimeError("GitHub Actions API Date header must be UTC")
    api_date_epoch_seconds = parsed_api_date.timestamp()
    local_received_at_epoch_seconds = float(now())
    if not math.isfinite(local_received_at_epoch_seconds):
        raise RuntimeError("local API receipt time must be finite")
    local_clock_skew_seconds = (
        local_received_at_epoch_seconds - api_date_epoch_seconds
    )
    if abs(local_clock_skew_seconds) > MAX_LOCAL_CLOCK_SKEW_SECONDS:
        raise RuntimeError(
            "local wall clock differs from the GitHub Actions API Date by more than 30 seconds"
        )
    expected = {
        "id": int(run_id),
        "run_attempt": int(run_attempt),
        "event": "workflow_dispatch",
        "head_sha": source_commit,
        "head_branch": "main",
        "name": workflow,
        "path": PRODUCTION_WORKFLOW_PATH,
    }
    for metadata_field, value in expected.items():
        if payload.get(metadata_field) != value:
            raise RuntimeError(
                f"GitHub Actions run metadata {metadata_field} does not match the current production run"
            )
    workflow_id = payload.get("workflow_id")
    if isinstance(workflow_id, bool) or not isinstance(workflow_id, int) or workflow_id <= 0:
        raise RuntimeError("GitHub Actions run metadata workflow_id is invalid")
    created_at = payload.get("created_at")
    created_at_epoch_seconds = parse_github_timestamp(
        created_at, "GitHub Actions run created_at"
    )
    if api_date_epoch_seconds + HTTP_DATE_RESOLUTION_SECONDS < created_at_epoch_seconds:
        raise RuntimeError("GitHub Actions API Date predates workflow creation")
    return {
        "api_verified": True,
        "event": payload["event"],
        "head_branch": payload["head_branch"],
        "path": payload["path"],
        "workflow_id": workflow_id,
        "created_at": created_at,
        "created_at_epoch_seconds": created_at_epoch_seconds,
        "api_date": api_date,
        "api_date_epoch_seconds": api_date_epoch_seconds,
        "api_local_received_at_epoch_seconds": local_received_at_epoch_seconds,
        "api_local_clock_skew_seconds": local_clock_skew_seconds,
        "api_request_monotonic_lower_usec": request_clock[
            "monotonic_lower_usec"
        ],
        "api_request_monotonic_upper_usec": request_clock[
            "monotonic_upper_usec"
        ],
        "api_request_boottime_usec": request_clock["boottime_usec"],
        "api_response_monotonic_lower_usec": response_clock[
            "monotonic_lower_usec"
        ],
        "api_response_monotonic_upper_usec": response_clock[
            "monotonic_upper_usec"
        ],
        "api_response_boottime_usec": response_clock["boottime_usec"],
        "api_clock_offset_lower_usec": offset_lower,
        "api_clock_offset_upper_usec": offset_upper,
    }


class RunnerServiceProbe:
    """Prove one Actions Runner.Listener remains live in the current runner unit."""

    def __init__(
        self,
        runner_name: str,
        *,
        expected_unit: str | None = None,
        cgroup_path: Path = Path("/proc/self/cgroup"),
        proc_root: Path = Path("/proc"),
        boot_id_path: Path = Path("/proc/sys/kernel/random/boot_id"),
        systemctl_path: Path = Path("/usr/bin/systemctl"),
        runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
        now: Callable[[], float] = time.time,
        monotonic_ns: Callable[[], int] = time.monotonic_ns,
        boottime_ns: Callable[[], int] = linux_boottime_ns,
        clock_ticks_per_second: int | None = None,
        process_id: int | None = None,
    ) -> None:
        if not RUNNER_NAME_PATTERN.fullmatch(runner_name):
            raise RuntimeError("RUNNER_NAME cannot be bound to a systemd service unit")
        if expected_unit is not None and (
            not RUNNER_SERVICE_UNIT_PATTERN.fullmatch(expected_unit)
            or not expected_unit.endswith(f".{runner_name}.service")
        ):
            raise RuntimeError(
                "expected runner service unit is malformed or does not match RUNNER_NAME"
            )
        self.runner_name = runner_name
        self.expected_unit = expected_unit
        self.cgroup_path = cgroup_path
        self.proc_root = proc_root
        self.boot_id_path = boot_id_path
        self.systemctl_path = systemctl_path
        self._runner = runner
        self._now = now
        self._monotonic_ns = monotonic_ns
        self._boottime_ns = boottime_ns
        if clock_ticks_per_second is None:
            sysconf = getattr(os, "sysconf", None)
            try:
                sysconf_callable = cast(Callable[[str], int], sysconf)
                clock_ticks_per_second = int(
                    sysconf_callable("SC_CLK_TCK")  # pylint: disable=not-callable
                )
            except (AttributeError, OSError, TypeError, ValueError):
                # This fallback only keeps unit tests/imports portable. Production is Linux.
                clock_ticks_per_second = 100
        if clock_ticks_per_second <= 0:
            raise RuntimeError("Linux clock ticks per second must be positive")
        self.clock_ticks_per_second = clock_ticks_per_second
        self.process_id = os.getpid() if process_id is None else process_id
        if self.process_id <= 0:
            raise RuntimeError("runner evidence process id must be positive")
        self._last: dict[str, Any] | None = None
        self._clock_baseline: dict[str, Any] | None = None
        self.observation_count = 0

    @staticmethod
    def _bounded_bytes(path: Path, label: str, limit: int = 65_536) -> bytes:
        try:
            with path.open("rb") as stream:
                raw = stream.read(limit + 1)
        except OSError as error:
            raise RuntimeError(f"could not read {label}") from error
        if len(raw) > limit:
            raise RuntimeError(f"{label} is oversized")
        return raw

    @classmethod
    def _cgroup_paths(cls, path: Path, label: str) -> set[str]:
        raw = cls._bounded_bytes(path, label).decode("utf-8")
        if not raw:
            raise RuntimeError(f"{label} is empty")
        paths: set[str] = set()
        for line in raw.splitlines():
            parts = line.split(":", 2)
            if len(parts) != 3 or not parts[2].startswith("/"):
                raise RuntimeError(f"{label} has an invalid record")
            paths.add(parts[2])
        if not paths:
            raise RuntimeError(f"{label} has no memberships")
        return paths

    def _current_service_cgroup(self) -> tuple[str, str]:
        try:
            paths = self._cgroup_paths(
                self.cgroup_path, "the current process cgroup"
            )
        except UnicodeDecodeError as error:
            raise RuntimeError("current process cgroup is not UTF-8") from error
        memberships: set[tuple[str, str]] = set()
        for path in paths:
            for component in path.split("/"):
                if component.startswith("actions.runner.") and component.endswith(
                    ".service"
                ):
                    if not RUNNER_SERVICE_UNIT_PATTERN.fullmatch(component):
                        raise RuntimeError("current process runner service unit is malformed")
                    memberships.add((component, path))
        if len(memberships) != 1:
            raise RuntimeError(
                "current process must have exactly one actions.runner service cgroup membership"
            )
        unit, process_control_group = next(iter(memberships))
        if self.expected_unit is not None and unit != self.expected_unit:
            raise RuntimeError(
                "current process runner service does not match the exact expected unit"
            )
        if not unit.endswith(f".{self.runner_name}.service"):
            raise RuntimeError(
                "current process runner service does not match RUNNER_NAME"
            )
        return unit, process_control_group

    @staticmethod
    def _parse_process_stat(raw: bytes, pid: int) -> tuple[str, int, int]:
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as error:
            raise RuntimeError(f"process {pid} stat is not UTF-8") from error
        open_paren = text.find("(")
        close_paren = text.rfind(")")
        if open_paren <= 0 or close_paren <= open_paren:
            raise RuntimeError(f"process {pid} stat is malformed")
        if text[:open_paren].strip() != str(pid):
            raise RuntimeError(f"process {pid} stat PID changed")
        fields = text[close_paren + 1 :].split()
        if (
            len(fields) <= 19
            or not fields[1].isdigit()
            or not fields[19].isdigit()
            or int(fields[19]) <= 0
        ):
            raise RuntimeError(f"process {pid} stat identity is malformed")
        return text[open_paren + 1 : close_paren], int(fields[1]), int(fields[19])

    def _listener_identity(self, control_group: str) -> dict[str, Any]:
        current_pid = self.process_id
        visited: set[int] = set()
        observer_start_ticks = 0
        for depth in range(64):
            if current_pid <= 0 or current_pid in visited:
                break
            visited.add(current_pid)
            entry = self.proc_root / str(current_pid)
            first_stat = self._bounded_bytes(
                entry / "stat", f"process {current_pid} stat"
            )
            _, parent_pid, start_ticks = self._parse_process_stat(
                first_stat, current_pid
            )
            if depth == 0:
                observer_start_ticks = start_ticks
            cmdline = self._bounded_bytes(
                entry / "cmdline", f"process {current_pid} command line"
            )
            if not cmdline:
                executable = ""
            else:
                try:
                    executable = cmdline.split(b"\0", 1)[0].decode("utf-8")
                except UnicodeDecodeError as error:
                    raise RuntimeError(
                        f"process {current_pid} command line is not UTF-8"
                    ) from error
            second_stat = self._bounded_bytes(
                entry / "stat", f"process {current_pid} stat"
            )
            if self._parse_process_stat(second_stat, current_pid) != (
                self._parse_process_stat(first_stat, current_pid)
            ):
                raise RuntimeError(
                    f"process {current_pid} identity changed while walking ancestors"
                )
            if not executable or re.split(r"[/\\]", executable)[-1] != "Runner.Listener":
                current_pid = parent_pid
                continue
            if depth == 0:
                raise RuntimeError("capacity gate cannot itself be Runner.Listener")
            try:
                cgroup_paths = self._cgroup_paths(
                    entry / "cgroup", f"Runner.Listener {current_pid} cgroup"
                )
            except UnicodeDecodeError as error:
                raise RuntimeError("Runner.Listener cgroup is not UTF-8") from error
            matching_groups = {
                path
                for path in cgroup_paths
                if path == control_group or path.startswith(control_group + "/")
            }
            if not matching_groups:
                raise RuntimeError(
                    "ancestor Runner.Listener is outside the current runner ControlGroup"
                )
            if len(matching_groups) != 1:
                raise RuntimeError(
                    "Runner.Listener has ambiguous systemd ControlGroup memberships"
                )
            return {
                "listener_pid": current_pid,
                "listener_start_ticks": start_ticks,
                "listener_clock_ticks_per_second": self.clock_ticks_per_second,
                "listener_control_group": next(iter(matching_groups)),
                "listener_executable": executable,
                "listener_start_boottime_usec": (
                    start_ticks * 1_000_000 // self.clock_ticks_per_second
                ),
                "listener_ancestor_depth": depth,
                "observer_pid": self.process_id,
                "observer_start_ticks": observer_start_ticks,
            }
        raise RuntimeError(
            "runner service must have exactly one live Runner.Listener ancestor"
        )

    def _boot_id(self) -> str:
        try:
            boot_id = self.boot_id_path.read_text(encoding="utf-8").strip()
        except OSError as error:
            raise RuntimeError("could not read the Linux boot id") from error
        if not LINUX_BOOT_ID_PATTERN.fullmatch(boot_id):
            raise RuntimeError("Linux boot id is missing or malformed")
        return boot_id

    @staticmethod
    def _positive_integer(value: str, label: str) -> int:
        if not value.isdigit() or int(value) <= 0:
            raise RuntimeError(f"runner service {label} must be a positive integer")
        return int(value)

    def observe(
        self, baseline: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        clock_before = bracket_clock_sample(self._monotonic_ns, self._boottime_ns)
        unit, process_control_group = self._current_service_cgroup()
        boot_id = self._boot_id()
        argv = [
            str(self.systemctl_path),
            "show",
            "--no-pager",
            *(f"--property={name}" for name in RUNNER_SERVICE_PROPERTIES),
            unit,
        ]
        completed = self._runner(
            argv,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
            shell=False,
        )
        if completed.returncode != 0:
            raise RuntimeError(
                f"systemctl could not inspect the current runner service (exit {completed.returncode})"
            )
        if not isinstance(completed.stdout, str) or len(completed.stdout) > 65_536:
            raise RuntimeError("runner service properties are missing or oversized")
        properties: dict[str, str] = {}
        for line in completed.stdout.splitlines():
            if not line:
                continue
            key, separator, value = line.partition("=")
            if not separator or key in properties:
                raise RuntimeError("runner service properties are malformed")
            properties[key] = value
        if set(properties) != set(RUNNER_SERVICE_PROPERTIES):
            raise RuntimeError("runner service properties are incomplete or unexpected")
        if properties["Id"] != unit:
            raise RuntimeError("systemctl returned a different runner service unit")
        control_group = properties["ControlGroup"]
        if (
            not control_group.startswith("/")
            or control_group == "/"
            or "//" in control_group
            or "/../" in f"{control_group}/"
        ):
            raise RuntimeError("runner service ControlGroup is invalid")
        if control_group.rstrip("/").rsplit("/", 1)[-1] != unit:
            raise RuntimeError("runner service ControlGroup does not identify its unit")
        if process_control_group != control_group and not process_control_group.startswith(
            control_group + "/"
        ):
            raise RuntimeError(
                "current process cgroup does not belong to systemd ControlGroup"
            )
        expected_states = {
            "LoadState": "loaded",
            "ActiveState": "active",
            "SubState": "running",
        }
        for state_field, value in expected_states.items():
            if properties[state_field] != value:
                raise RuntimeError(
                    f"runner service is not continuously active ({state_field}={properties[state_field]!r})"
                )
        for timestamp_field in ("ActiveEnterTimestamp", "ExecMainStartTimestamp"):
            if not properties[timestamp_field] or properties[timestamp_field].lower() in {
                "n/a",
                "[not set]",
            }:
                raise RuntimeError(
                    f"runner service {timestamp_field} is not available"
                )
        if not RUNNER_INVOCATION_ID_PATTERN.fullmatch(properties["InvocationID"]):
            raise RuntimeError("runner service InvocationID is invalid")

        active_enter = self._positive_integer(
            properties["ActiveEnterTimestampMonotonic"],
            "ActiveEnterTimestampMonotonic",
        )
        exec_start = self._positive_integer(
            properties["ExecMainStartTimestampMonotonic"],
            "ExecMainStartTimestampMonotonic",
        )
        main_pid = self._positive_integer(properties["MainPID"], "MainPID")
        listener = self._listener_identity(control_group)
        unit_after, process_control_group_after = self._current_service_cgroup()
        boot_id_after = self._boot_id()
        clock_after = bracket_clock_sample(self._monotonic_ns, self._boottime_ns)
        if (
            unit_after != unit
            or process_control_group_after != process_control_group
            or boot_id_after != boot_id
        ):
            raise RuntimeError("runner service cgroup or boot id changed during observation")
        if clock_after["monotonic_lower_usec"] < clock_before[
            "monotonic_upper_usec"
        ] or clock_after["boottime_usec"] < clock_before["boottime_usec"]:
            raise RuntimeError("runner service observation clock moved backwards")
        offset_lower, offset_upper = intersect_clock_offsets(
            clock_before, clock_after, "during service observation"
        )
        observed_at_epoch_seconds = float(self._now())
        if not math.isfinite(observed_at_epoch_seconds):
            raise RuntimeError("runner service observation time must be finite")
        service_start = max(active_enter, exec_start)
        if service_start > clock_before["monotonic_lower_usec"]:
            raise RuntimeError("runner service start timestamp is in the future")
        listener_start_boottime_usec = listener["listener_start_boottime_usec"]
        if listener_start_boottime_usec > clock_before["boottime_usec"]:
            raise RuntimeError("Runner.Listener start timestamp is in the future")
        active_uptime_seconds = (
            clock_after["monotonic_upper_usec"] - active_enter
        ) / 1_000_000
        process_uptime_seconds = (
            clock_after["monotonic_upper_usec"] - exec_start
        ) / 1_000_000
        active_age_lower_bound_seconds = (
            clock_before["monotonic_lower_usec"] - service_start
        ) / 1_000_000
        listener_age_lower_bound_seconds = (
            clock_before["boottime_usec"] - listener_start_boottime_usec
        ) / 1_000_000
        service_start_epoch_seconds = observed_at_epoch_seconds - (
            clock_after["monotonic_upper_usec"] - service_start
        ) / 1_000_000
        observation = {
            "schema_version": 1,
            "unit": unit,
            "boot_id": boot_id,
            "control_group": control_group,
            "process_control_group": process_control_group,
            "load_state": properties["LoadState"],
            "active_state": properties["ActiveState"],
            "sub_state": properties["SubState"],
            "active_enter_timestamp": properties["ActiveEnterTimestamp"],
            "active_enter_monotonic_usec": active_enter,
            "exec_main_start_timestamp": properties["ExecMainStartTimestamp"],
            "exec_main_start_monotonic_usec": exec_start,
            "invocation_id": properties["InvocationID"],
            "main_pid": main_pid,
            **listener,
            "observed_at_epoch_seconds": observed_at_epoch_seconds,
            "clock_before_monotonic_lower_usec": clock_before[
                "monotonic_lower_usec"
            ],
            "clock_before_monotonic_upper_usec": clock_before[
                "monotonic_upper_usec"
            ],
            "clock_before_boottime_usec": clock_before["boottime_usec"],
            "clock_after_monotonic_lower_usec": clock_after[
                "monotonic_lower_usec"
            ],
            "clock_after_monotonic_upper_usec": clock_after[
                "monotonic_upper_usec"
            ],
            "clock_after_boottime_usec": clock_after["boottime_usec"],
            "clock_offset_lower_usec": offset_lower,
            "clock_offset_upper_usec": offset_upper,
            "observed_monotonic_before_usec": clock_before[
                "monotonic_lower_usec"
            ],
            "observed_monotonic_after_usec": clock_after[
                "monotonic_upper_usec"
            ],
            "observed_monotonic_usec": clock_after["monotonic_upper_usec"],
            "observed_boottime_before_usec": clock_before["boottime_usec"],
            "observed_boottime_after_usec": clock_after["boottime_usec"],
            "observed_boottime_usec": clock_after["boottime_usec"],
            "active_enter_epoch_seconds": (
                observed_at_epoch_seconds - active_uptime_seconds
            ),
            "service_start_monotonic_usec": service_start,
            "service_start_epoch_seconds": service_start_epoch_seconds,
            "active_uptime_seconds": active_uptime_seconds,
            "main_process_uptime_seconds": process_uptime_seconds,
            "active_age_lower_bound_seconds": active_age_lower_bound_seconds,
            "listener_age_lower_bound_seconds": listener_age_lower_bound_seconds,
        }
        if baseline is not None:
            for field in RUNNER_SERVICE_CONTINUITY_FIELDS:
                if observation.get(field) != baseline.get(field):
                    raise RuntimeError(
                        f"runner service continuity changed at field {field}"
                    )
            require_clock_offset_continuity(
                baseline, observation, "since the baseline observation"
            )
        if self._last is not None:
            if observation["clock_before_monotonic_lower_usec"] < self._last[
                "clock_after_monotonic_upper_usec"
            ] or observation["clock_before_boottime_usec"] < self._last[
                "clock_after_boottime_usec"
            ]:
                raise RuntimeError("runner service observation clock moved backwards")
            if observation["active_uptime_seconds"] < self._last[
                "active_uptime_seconds"
            ]:
                raise RuntimeError("runner service active uptime moved backwards")
            if observation["listener_age_lower_bound_seconds"] < self._last[
                "listener_age_lower_bound_seconds"
            ]:
                raise RuntimeError("Runner.Listener age moved backwards")
        if self._clock_baseline is None:
            self._clock_baseline = observation
        else:
            require_clock_offset_continuity(
                self._clock_baseline,
                observation,
                "between service observations",
            )
        self._last = observation
        self.observation_count += 1
        return observation


def report_identity(
    args: argparse.Namespace,
    server_build: dict[str, str],
    runner_service: dict[str, Any] | None = None,
    github_run: dict[str, Any] | None = None,
) -> dict[str, Any]:
    workflow = {
        "repository": os.getenv("GITHUB_REPOSITORY", ""),
        "workflow": os.getenv("GITHUB_WORKFLOW", ""),
        "run_id": os.getenv("GITHUB_RUN_ID", ""),
        "run_attempt": os.getenv("GITHUB_RUN_ATTEMPT", ""),
        "job": os.getenv("GITHUB_JOB", ""),
        "ref": os.getenv("GITHUB_REF", ""),
        "sha": os.getenv("GITHUB_SHA", args.source_commit),
    }
    if github_run is not None:
        workflow.update(github_run)
    runner = {
        "name": os.getenv("RUNNER_NAME", ""),
        "os": os.getenv("RUNNER_OS", platform.system()),
        "arch": os.getenv("RUNNER_ARCH", platform.machine()),
        "environment": os.getenv("RUNNER_ENVIRONMENT", ""),
        "labels": [
            label.strip()
            for label in os.getenv("ORCHESTRATOR_GATE_RUNNER_LABELS", "").split(",")
            if label.strip()
        ],
        "expected_service_unit": os.getenv(
            "ORCHESTRATOR_GATE_EXPECTED_RUNNER_UNIT", ""
        ),
    }
    if runner_service is not None:
        runner["service"] = runner_service
    return {
        "source_commit": args.source_commit,
        "workflow": workflow,
        "runner": runner,
        "server_build": server_build,
        "oci_revision": args.oci_revision,
        "provenance_commit": args.provenance_commit,
        "image_provenance": {
            "control_plane_image": args.control_plane_image,
            "agent_image": args.agent_image,
            "fixture_image": args.fixture_image,
            "source_workflow_run_id": args.image_workflow_run_id,
            "record_sha256": args.image_provenance_record_sha256,
            "source_workflow": ".github/workflows/orchestrator-candidate-images.yml",
            "source_workflow_run_attempt": 1 if args.profile == "production" else 0,
        },
    }


def inventory_failures(check: dict[str, Any], expected: dict[str, int]) -> list[str]:
    failures: list[str] = []
    if check["nodes_total"] < expected["nodes"]:
        failures.append(
            f"nodes: observed {check['nodes_total']}, expected at least {expected['nodes']}"
        )
    if check["nodes_ready"] != check["nodes_total"]:
        failures.append(
            f"only {check['nodes_ready']}/{check['nodes_total']} Nodes were fully ready"
        )
    if check["deployments_total"] < expected["deployments"]:
        failures.append(
            "deployments: observed "
            f"{check['deployments_total']}, expected at least {expected['deployments']}"
        )
    if check["deployments_running"] != check["deployments_total"]:
        failures.append(
            f"only {check['deployments_running']}/{check['deployments_total']} Deployments were RUNNING"
        )
    if check["topology_resources"] < expected["topology_resources"]:
        failures.append(
            "topology_resources: observed "
            f"{check['topology_resources']}, expected at least {expected['topology_resources']}"
        )
    if check["topologies_in_sync"] != check["topologies_total"] or check["topology_drift"]:
        failures.append(
            f"Topology status was not fully IN_SYNC (drift={check['topology_drift']})"
        )
    if check["permanent_operations"]:
        failures.append(
            f"permanent active Operations detected: {check['permanent_operations']!r}"
        )
    return failures


def execute_operation_round(
    client: Client,
    report: GateReport,
    args: argparse.Namespace,
    targets: list[tuple[str, str, str]],
    run_id: str,
    round_index: int,
    phase: str,
    measurements: dict[str, list[float]],
    environment_identity: dict[str, Any] | None = None,
) -> bool:
    operation_samples: list[Sample] = []
    operation_ids: list[str] = []
    event_latencies: list[float] = []
    round_id = f"{run_id}-round-{round_index:04d}"
    started = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrent_operations) as pool:
        futures = [
            pool.submit(
                operation_cycle,
                client,
                round_id,
                index,
                deployment_id,
                node_id,
                container_id,
            )
            for index, (deployment_id, node_id, container_id) in enumerate(targets)
        ]
        for future in concurrent.futures.as_completed(futures):
            samples, operation_id, event_latency = future.result()
            operation_samples.extend(samples)
            if operation_id:
                operation_ids.append(operation_id)
            if event_latency:
                event_latencies.append(event_latency)
    failed = [sample for sample in operation_samples if not sample.ok]
    for sample in operation_samples:
        measurement_name = OPERATION_MUTATION_MEASUREMENTS.get(sample.name)
        if measurement_name is None:
            continue
        measurements["mutation"].append(sample.latency_ms)
        measurements[measurement_name].append(sample.latency_ms)
    measurements["event"].extend(event_latencies)
    target_deployments = sorted({target[0] for target in targets})
    target_nodes = sorted({target[1] for target in targets})
    target_containers = sorted({target[2] for target in targets})
    target_identities = [
        {
            "deployment_id": deployment_id,
            "node_id": node_id,
            "container_id": container_id,
        }
        for deployment_id, node_id, container_id in sorted(targets)
    ]
    unique_operation_ids = sorted(set(operation_ids))
    ok = (
        len(operation_ids) == args.concurrent_operations
        and len(unique_operation_ids) == len(operation_ids)
        and len(target_deployments) == args.concurrent_operations
        and len(target_nodes) == args.concurrent_operations
        and len(target_containers) == args.concurrent_operations
        and len(event_latencies) == len(operation_ids)
        and not failed
    )
    if getattr(args, "profile", "smoke") == "production" and phase == "soak":
        ok = ok and environment_identity is not None
    round_report = {
        "round": round_index,
        "phase": phase,
        "started_at_epoch_seconds": time.time() - (time.monotonic() - started),
        "elapsed_seconds": time.monotonic() - started,
        "requested_operations": args.concurrent_operations,
        "created_operations": len(operation_ids),
        "unique_created_operations": len(unique_operation_ids),
        "target_nodes": len(target_nodes),
        "target_deployments": len(target_deployments),
        "target_containers": len(target_containers),
        "target_identities": target_identities,
        "target_identities_sha256": canonical_json_sha256(target_identities),
        "operation_ids": unique_operation_ids,
        "operation_ids_sha256": canonical_json_sha256(unique_operation_ids),
        "event_streams_observed": len(event_latencies),
        "failed_requests": len(failed),
        "ok": ok,
    }
    if environment_identity is not None:
        round_report.update(environment_identity)
    with report._checkpoint_lock:
        report.operation_rounds.append(round_report)
        report.observed["concurrent_operations"] = min(
            report.observed.get(
                "concurrent_operations", args.concurrent_operations
            ),
            len(operation_ids),
        )
        if not ok:
            report.failures.append(
                "Operation round "
                f"{round_index} failed: created={len(operation_ids)}, "
                f"events={len(event_latencies)}, requests={len(failed)}"
            )
            for sample in failed[:10]:
                report.failures.append(
                    f"{sample.name} returned {sample.status}: {sample.detail}"
                )
    return ok


def finalize_latency_measurements(
    report: GateReport,
    measurements: dict[str, list[float]],
    *,
    read_threshold_ms: float,
    mutation_threshold_ms: float,
    event_threshold_ms: float,
) -> None:
    with report._checkpoint_lock:
        report.measurements_ms["read_p95"] = percentile(
            measurements["read"], 0.95
        )
        report.measurements_ms["mutation_accept_p95"] = percentile(
            measurements["mutation"], 0.95
        )
        report.measurements_ms["event_p95"] = percentile(
            measurements["event"], 0.95
        )
        for mutation_name in OPERATION_MUTATION_MEASUREMENTS.values():
            report.measurements_ms[f"{mutation_name}_p95"] = percentile(
                measurements[mutation_name], 0.95
            )
        for name, threshold in (
            ("read_p95", read_threshold_ms),
            ("mutation_accept_p95", mutation_threshold_ms),
            ("mutation_plan_p95", mutation_threshold_ms),
            ("mutation_confirm_p95", mutation_threshold_ms),
            ("mutation_apply_p95", mutation_threshold_ms),
            ("mutation_cancel_p95", mutation_threshold_ms),
            ("event_p95", event_threshold_ms),
        ):
            if report.measurements_ms[name] > threshold:
                report.failures.append(
                    f"{name} {report.measurements_ms[name]:.2f}ms exceeds {threshold:.2f}ms"
                )


def capture_sample(
    client: Client,
    report: GateReport,
    writer: EvidenceWriter,
    args: argparse.Namespace,
    phase: str,
    phase_started: float,
    runner_probe: RunnerServiceProbe | None = None,
    runner_service_baseline: dict[str, Any] | None = None,
    *,
    checkpoint: bool = True,
) -> dict[str, Any]:
    sample_clock = (
        linux_boottime_seconds if args.profile == "production" else time.monotonic
    )
    latency, readiness = ready_snapshot(client)
    build = validate_server_build(readiness, args.source_commit, args.profile)
    if build != report.identity.get("server_build"):
        raise RuntimeError("readiness build identity changed during the capacity run")
    snapshot = metrics(client)
    rss = required_metric_value(
        snapshot, "ojos_orchestrator_process_resident_memory_bytes"
    )
    threads = required_metric_value(snapshot, "ojos_orchestrator_process_threads")
    active = required_metric_value(
        snapshot, "ojos_orchestrator_http_active_requests"
    )
    job_metrics_error = required_metric_value(
        snapshot, "ojos_orchestrator_job_metrics_collection_error"
    )
    expired_job_leases = required_metric_value(
        snapshot, "ojos_orchestrator_expired_job_leases"
    )
    heartbeat_age = required_metric_value(
        snapshot, "ojos_orchestrator_oldest_leased_job_heartbeat_age_seconds"
    )
    anomaly_counters = {
        "expired_job_lease_transitions_total": required_metric_value(
            snapshot, "ojos_orchestrator_expired_job_lease_transitions_total"
        ),
        "operation_over_300_seconds_transitions_total": required_metric_value(
            snapshot,
            "ojos_orchestrator_operation_over_300_seconds_transitions_total",
        ),
        "operation_invalid_updated_at_transitions_total": required_metric_value(
            snapshot,
            "ojos_orchestrator_operation_invalid_updated_at_transitions_total",
        ),
        "observation_errors_total": required_metric_value(
            snapshot,
            "ojos_orchestrator_control_plane_anomaly_observation_errors_total",
        ),
        "process_starts_total": required_metric_value(
            snapshot, "ojos_orchestrator_control_plane_process_starts_total"
        ),
        "state_loaded": required_metric_value(
            snapshot, "ojos_orchestrator_control_plane_anomaly_state_loaded"
        ),
        "process_start_time_seconds": required_metric_value(
            snapshot, "ojos_orchestrator_process_start_time_seconds"
        ),
    }
    anomaly_values_valid = (
        anomaly_counters["state_loaded"] == 1
        and anomaly_counters["process_start_time_seconds"] > 0
        and all(
            value >= 0 and value.is_integer()
            for key, value in anomaly_counters.items()
            if key != "process_start_time_seconds"
        )
    )
    anomaly_window_unchanged = True
    if phase == "soak_boundary":
        with report._checkpoint_lock:
            if report.evidence.get("anomaly_counter_baseline") is None:
                report.evidence["anomaly_counter_baseline"] = dict(anomaly_counters)
            else:
                anomaly_window_unchanged = False
    elif phase == "soak":
        with report._checkpoint_lock:
            baseline = report.evidence.get("anomaly_counter_baseline")
            anomaly_window_unchanged = (
                isinstance(baseline, dict) and baseline == anomaly_counters
            )
    pool_connections, pool_idle = storage_pool(readiness)
    runner_service = None
    if runner_probe is not None:
        if runner_service_baseline is None:
            raise RuntimeError("runner service baseline is required for production samples")
        runner_service = runner_probe.observe(runner_service_baseline)
    valid = (
        rss > 0
        and threads > 0
        and job_metrics_error == 0
        and expired_job_leases == 0
        and heartbeat_age <= args.permanent_running_seconds
        and anomaly_values_valid
        and anomaly_window_unchanged
        and (args.profile != "production" or pool_connections > 0)
        and 0 <= pool_idle <= pool_connections
    )
    sequence = len(report.samples) + 1
    sampled_at_epoch_seconds = time.time()
    # This is the only clock used for sample continuity. Wall time remains in
    # the report for GitHub/checkpoint correlation, but it can step and must
    # never mask a suspended or otherwise delayed production runner.
    sample_clock_seconds = sample_clock()
    storage_snapshot = {
        "pool_connections": pool_connections,
        "pool_idle_connections": pool_idle,
    }
    snapshot_record = writer.prometheus_snapshot(
        sequence,
        phase,
        sampled_at_epoch_seconds,
        sample_clock_seconds,
        snapshot,
        storage_snapshot,
    )
    sample = {
        "sequence": sequence,
        "phase": phase,
        "sampled_at_epoch_seconds": sampled_at_epoch_seconds,
        "sample_clock_seconds": sample_clock_seconds,
        "phase_elapsed_seconds": sample_clock_seconds - phase_started,
        "ready_latency_ms": latency,
        "valid": valid,
        "metrics": {
            "snapshot_record": snapshot_record,
            "snapshot_kind": "prometheus_snapshots_ndjson",
        },
        "process": {
            "rss_bytes": rss,
            "threads": threads,
            "active_requests": active,
        },
        "storage": storage_snapshot,
        "jobs": {
            "collection_error": job_metrics_error,
            "expired_leases": expired_job_leases,
            "oldest_leased_heartbeat_age_seconds": heartbeat_age,
        },
        "anomalies": anomaly_counters,
    }
    if runner_service is not None:
        sample["runner_service"] = runner_service
    with report._checkpoint_lock:
        report.samples.append(sample)
    writer.event("prometheus_snapshot", sequence=sample["sequence"], phase=phase, valid=valid)
    if checkpoint:
        writer.checkpoint()
    if not valid:
        raise RuntimeError(
            "invalid production sample: RSS/threads/pool metrics missing, durable Job metrics failed, "
            "expired/stale Job leases were observed, or a durable anomaly counter changed"
        )
    return sample


def record_environment_observation(
    provider: EnvironmentEvidenceProvider,
    report: GateReport,
    writer: EvidenceWriter,
    *,
    phase: str,
    operation_round_index: int | None = None,
    establish_stable: bool = True,
    restart_previous: dict[str, Any] | None = None,
    checkpoint: bool = True,
) -> dict[str, Any]:
    observation = provider.observe(
        establish_stable=establish_stable,
        restart_previous=restart_previous,
    )
    sequence = writer.environment_snapshot(
        phase, operation_round_index, observation
    )
    engine = observation["engine_evidence"]
    network = observation["network_evidence"]
    observer = observation["observer_identity"]
    provenance = observation["provenance_identity"]
    deployment = observation["deployment_identity"]
    runtime = observation["runtime_evidence"]
    control_plane = runtime["control_plane"]
    postgres = runtime["postgres"]
    agents = runtime["agents"]
    engines = runtime["engines"]
    runner_host = next(host for host in runtime["hosts"] if host["role"] == "runner")
    check = {
        "sequence": sequence,
        "phase": phase,
        "operation_round_index": operation_round_index,
        "post_warmup_baseline": phase == "soak_boundary",
        "started_at_epoch_seconds": observation["started_at_epoch_seconds"],
        "completed_at_epoch_seconds": observation["completed_at_epoch_seconds"],
        "configuration_fingerprint_sha256": observation[
            "configuration_fingerprint_sha256"
        ],
        "observer_identity_sha256": hashlib.sha256(
            json.dumps(observer, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest(),
        "provenance_record_sha256": provenance["record_sha256"],
        "image_workflow_run_id": provenance["source_workflow_run_id"],
        "control_plane_image": provenance["control_plane_reference"],
        "agent_image": provenance["agent_reference"],
        "provenance_fixture_image": provenance["fixture_reference"],
        "control_plane_origin_sha256": deployment["control_plane_origin_sha256"],
        "restart_argv_sha256": deployment["restart_argv_sha256"],
        "topology_id": deployment["topology_id"],
        "topology_revision_id": deployment["topology_revision_id"],
        "topology_identity_sha256": deployment["topology_identity_sha256"],
        "runtime_provision_manifest_sha256": runtime[
            "provision_manifest_sha256"
        ],
        "runtime_host_identity_sha256": runtime["host_identity_sha256"],
        "runner_machine_id_sha256": runner_host["machine_id_sha256"],
        "control_plane_image_id": control_plane["image"]["image_id"],
        "control_plane_container_id": control_plane["container"]["container_id"],
        "control_plane_started_at": control_plane["container"]["started_at"],
        "control_plane_configuration_sha256": control_plane["configuration"][
            "effective_sha256"
        ],
        "postgres_image": postgres["image"]["repo_digest"],
        "postgres_image_id": postgres["image"]["image_id"],
        "postgres_container_id": postgres["container"]["container_id"],
        "postgres_started_at": postgres["container"]["started_at"],
        "postgres_configuration_sha256": postgres["configuration"][
            "effective_sha256"
        ],
        "postgres_server_leaf_sha256": postgres["server_leaf_sha256"],
        "agent_image_id": agents["image"]["image_ids"][0],
        "agent_node_ids_sha256": agents["node_ids_sha256"],
        "agent_container_ids_sha256": agents["container_ids_sha256"],
        "agent_started_at_sha256": agents["started_at_sha256"],
        "agent_spiffe_ids_sha256": agents["spiffe_ids_sha256"],
        "agent_certificate_fingerprints_sha256": agents[
            "certificate_fingerprints_sha256"
        ],
        "agent_ledger_identities_sha256": agents["ledger_identities_sha256"],
        "agent_independent_mtls_identities": agents[
            "independent_mtls_identities"
        ],
        "agent_independent_sqlite_ledgers": agents[
            "independent_sqlite_ledgers"
        ],
        "docker_engine_image": engines["image"]["repo_digest"],
        "docker_engine_image_id": engines["image"]["image_ids"][0],
        "engine_outer_container_ids_sha256": engines[
            "outer_container_ids_sha256"
        ],
        "engine_inner_daemon_ids_sha256": engines[
            "inner_daemon_ids_sha256"
        ],
        "engine_socket_volumes_sha256": engines["socket_volumes_sha256"],
        "engine_data_volumes_sha256": engines["data_volumes_sha256"],
        "fixture_image": engine["fixture_image"],
        "aggregate_sha256": engine["aggregate_sha256"],
        "node_ids_sha256": engine["node_ids_sha256"],
        "deployment_ids_sha256": engine["deployment_ids_sha256"],
        "container_ids_sha256": engine["container_ids_sha256"],
        "endpoint_ids_sha256": network["endpoint_ids_sha256"],
        "link_ids_sha256": network["link_ids_sha256"],
        "workers": engine["worker_count"],
        "engines": engine["engine_count"],
        "containers": engine["container_count"],
        "running_containers": engine["running_containers"],
        "healthy_containers": engine["healthy_containers"],
        "endpoint_checks_total": network["endpoint_checks_total"],
        "endpoint_checks_healthy": network["endpoint_checks_healthy"],
        "endpoint_checks_failed": network["endpoint_checks_failed"],
        "link_probes_total": network["link_probes_total"],
        "link_probes_healthy": network["link_probes_healthy"],
        "link_probes_failed": network["link_probes_failed"],
        "drift": network["drift"],
        "ok": True,
    }
    with report._checkpoint_lock:
        report.environment_checks.append(check)
        completed = [
            float(item["completed_at_epoch_seconds"])
            for item in report.environment_checks
            if item["phase"] not in {"pre_restart", "post_restart"}
        ]
        gaps = [right - left for left, right in zip(completed, completed[1:])]
        report.evidence.update(
            environment_observations=len(report.environment_checks),
            environment_first_record=1,
            environment_last_record=sequence,
            environment_configuration_fingerprint_sha256=check[
                "configuration_fingerprint_sha256"
            ],
            environment_max_observation_gap_seconds=max(gaps, default=0.0),
            environment_identity={
                field: check[field]
                for field in (
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
            },
        )
        if phase == "final":
            report.evidence["environment_final_record"] = sequence
    writer.event(
        "environment_observation",
        sequence=sequence,
        phase=phase,
        operation_round_index=operation_round_index,
        aggregate_sha256=engine["aggregate_sha256"],
    )
    if checkpoint:
        writer.checkpoint()
    return observation


def run_gate(args: argparse.Namespace) -> GateReport:
    gate_started = time.monotonic()
    token_provider = (
        TokenProvider(
            args.token_argv_json,
            max_lifetime_seconds=(
                MAX_PRODUCTION_TOKEN_LIFETIME_SECONDS
                if args.profile == "production"
                else None
            ),
        )
        if args.token_argv_json
        else None
    )
    environment_provider = (
        EnvironmentEvidenceProvider(
            args.environment_argv_json,
            args.source_commit,
            control_plane_image=args.control_plane_image or None,
            agent_image=args.agent_image or None,
            fixture_image=args.fixture_image or None,
            provenance_record_sha256=args.image_provenance_record_sha256 or None,
            image_workflow_run_id=args.image_workflow_run_id or None,
            repository=os.getenv("GITHUB_REPOSITORY") or None,
            base_url=args.base_url if args.profile == "production" else None,
            restart_argv_json=args.restart_argv_json or None,
            observer_program_sha256=(
                repository_observer_sha256()
                if args.profile == "production"
                else None
            ),
            runner_machine_id_sha256=(
                local_linux_machine_id_sha256()
                if args.profile == "production"
                else None
            ),
        )
        if args.environment_argv_json
        else None
    )
    client = Client(
        args.base_url,
        args.token,
        args.internal_token,
        args.ca_file,
        args.timeout,
        token_provider,
    )
    report = GateReport(
        profile=args.profile,
        started_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        expected={
            "nodes": args.nodes,
            "deployments": args.deployments,
            "topology_resources": args.topology_resources,
            "concurrent_operations": args.concurrent_operations,
        },
        thresholds_ms={
            "read_p95": args.read_p95_ms,
            "mutation_accept_p95": args.mutation_p95_ms,
            "event_p95": args.event_p95_ms,
            "recovery": args.recovery_seconds * 1_000,
        },
        evidence={
            "source_commit": args.source_commit,
            "soak_seconds_requested": args.soak_seconds,
            "warmup_seconds": args.warmup_seconds,
            "sample_seconds": args.sample_seconds,
            "operation_interval_seconds": args.operation_interval_seconds,
            "max_sample_gap_seconds": args.max_sample_gap_seconds,
            "minimum_valid_samples": args.minimum_valid_samples,
            "permanent_running_seconds": args.permanent_running_seconds,
            "max_rss_growth": args.max_rss_growth,
            "max_thread_growth": args.max_thread_growth,
            "max_pool_growth": args.max_pool_growth,
            "max_active_request_growth": args.max_active_request_growth,
            "sampling_clock": (
                "CLOCK_BOOTTIME" if args.profile == "production" else "CLOCK_MONOTONIC"
            ),
        },
    )
    report.configuration = redacted_configuration(args)
    writer = EvidenceWriter(report, Path(args.report))
    measurements: dict[str, list[float]] = {
        "read": [],
        "mutation": [],
        "event": [],
        **{
            measurement_name: []
            for measurement_name in OPERATION_MUTATION_MEASUREMENTS.values()
        },
    }
    run_id = args.run_id or f"{int(time.time())}-{uuid.uuid4().hex[:8]}"
    writer.start_periodic_checkpoints(
        30.0,
        clock=(
            linux_boottime_seconds
            if args.profile == "production"
            else time.monotonic
        ),
        clock_name=(
            "CLOCK_BOOTTIME"
            if args.profile == "production"
            else "CLOCK_MONOTONIC"
        ),
    )
    try:
        writer.event("gate_started", profile=args.profile, run_id=run_id)
        writer.checkpoint()
    except Exception:
        writer.stop_periodic_checkpoints()
        raise
    runner_probe: RunnerServiceProbe | None = None
    runner_service_baseline: dict[str, Any] | None = None
    try:
        github_run: dict[str, Any] | None = None
        if args.profile == "production":
            runner_probe = RunnerServiceProbe(
                os.environ["RUNNER_NAME"],
                expected_unit=os.environ["ORCHESTRATOR_GATE_EXPECTED_RUNNER_UNIT"],
            )
            runner_service_baseline = runner_probe.observe()
            github_run = github_run_metadata(
                os.environ["ORCHESTRATOR_GATE_GITHUB_TOKEN"],
                os.environ["GITHUB_REPOSITORY"],
                os.environ["GITHUB_RUN_ID"],
                os.environ["GITHUB_RUN_ATTEMPT"],
                os.environ["GITHUB_WORKFLOW"],
                args.source_commit,
            )
            dispatch_time = float(github_run["created_at_epoch_seconds"])
            api_request_monotonic = int(
                github_run["api_request_monotonic_lower_usec"]
            )
            api_request_boottime = int(github_run["api_request_boottime_usec"])
            if api_request_monotonic < runner_service_baseline[
                "clock_after_monotonic_upper_usec"
            ] or api_request_boottime < runner_service_baseline[
                "clock_after_boottime_usec"
            ]:
                raise RuntimeError(
                    "GitHub API clock sample predates the runner baseline"
                )
            require_clock_offset_continuity(
                runner_service_baseline,
                {
                    "clock_offset_lower_usec": github_run[
                        "api_clock_offset_lower_usec"
                    ],
                    "clock_offset_upper_usec": github_run[
                        "api_clock_offset_upper_usec"
                    ],
                },
                "between the baseline and GitHub API verification",
            )
            if runner_service_baseline["active_age_lower_bound_seconds"] < 3_600:
                raise RuntimeError(
                    "production runner service monotonic age lower bound is below one hour"
                )
            if runner_service_baseline["listener_age_lower_bound_seconds"] < 3_600:
                raise RuntimeError(
                    "production Runner.Listener age lower bound is below one hour"
                )
            service_age_at_api_request = (
                api_request_monotonic
                - runner_service_baseline["service_start_monotonic_usec"]
            ) / 1_000_000
            listener_age_at_api_request = (
                api_request_boottime
                - runner_service_baseline["listener_start_boottime_usec"]
            ) / 1_000_000
            api_date_upper_bound = (
                float(github_run["api_date_epoch_seconds"])
                + HTTP_DATE_RESOLUTION_SECONDS
            )
            active_before_dispatch = (
                service_age_at_api_request
                + dispatch_time
                - api_date_upper_bound
            )
            listener_active_before_dispatch = (
                listener_age_at_api_request
                + dispatch_time
                - api_date_upper_bound
            )
            runner_service_baseline[
                "active_at_api_request_lower_seconds"
            ] = service_age_at_api_request
            runner_service_baseline[
                "listener_age_at_api_request_lower_seconds"
            ] = listener_age_at_api_request
            runner_service_baseline[
                "active_before_dispatch_seconds"
            ] = active_before_dispatch
            runner_service_baseline[
                "listener_active_before_dispatch_seconds"
            ] = listener_active_before_dispatch
            if active_before_dispatch < 3_600:
                raise RuntimeError(
                    "production runner service must be continuously active for one hour before workflow dispatch"
                )
            if listener_active_before_dispatch < 3_600:
                raise RuntimeError(
                    "production Runner.Listener must be continuously active for one hour before workflow dispatch"
                )
        recovery_started = time.monotonic()
        while True:
            try:
                ready_latency, readiness = ready_snapshot(client)
                break
            except Exception as error:
                if time.monotonic() - recovery_started >= args.recovery_seconds:
                    raise RuntimeError(
                        f"control plane did not recover within {args.recovery_seconds}s: {error}"
                    ) from error
                time.sleep(1)
        server_build = validate_server_build(readiness, args.source_commit, args.profile)
        with report._checkpoint_lock:
            report.identity = report_identity(
                args, server_build, runner_service_baseline, github_run
            )
        if args.profile == "production":
            for label, value in (
                ("workflow SHA", report.identity["workflow"]["sha"]),
                ("OCI revision", args.oci_revision),
                ("provenance commit", args.provenance_commit),
            ):
                if value != args.source_commit:
                    raise RuntimeError(
                        f"{label} {value!r} does not match source commit {args.source_commit}"
                    )
        with report._checkpoint_lock:
            report.measurements_ms["ready_request"] = ready_latency
            report.measurements_ms["recovery"] = (
                time.monotonic() - recovery_started
            ) * 1_000

        inventory, deployments, reads = inspect_inventory(
            client, report.expected, args.permanent_running_seconds
        )
        inventory["phase"] = "qualification"
        with report._checkpoint_lock:
            report.inventory_checks.append(inventory)
        measurements["read"].extend(reads)
        with report._checkpoint_lock:
            report.observed.update(
                nodes=inventory["nodes_total"],
                deployments=inventory["deployments_total"],
                topology_resources=inventory["topology_resources"],
                concurrent_operations=args.concurrent_operations,
            )
            report.failures.extend(
                inventory_failures(inventory, report.expected)
            )
        writer.event("inventory_check", check=inventory)
        writer.checkpoint()
        if report.failures:
            raise RuntimeError("initial production inventory did not satisfy the capacity gate")

        targets = select_operation_targets(deployments, args.concurrent_operations)
        execute_operation_round(
            client,
            report,
            args,
            targets,
            run_id,
            0,
            "qualification",
            measurements,
        )
        writer.event("operation_round", **report.operation_rounds[-1])
        writer.checkpoint()
        if report.failures:
            raise RuntimeError("initial concurrent Operation qualification failed")

        node_id = targets[0][1]
        pre_restart_observation: dict[str, Any] | None = None
        if environment_provider is not None and args.restart_argv_json:
            pre_restart_observation = record_environment_observation(
                environment_provider,
                report,
                writer,
                phase="pre_restart",
                establish_stable=False,
            )
        if args.restart_argv_json:
            restart_probe = create_restart_probe(client, run_id, node_id)
            trigger_restart(
                client,
                report,
                restart_probe,
                args.restart_argv_json,
                args.recovery_seconds,
            )
            writer.event("control_plane_restart", recovery_ms=report.measurements_ms["recovery"])
            writer.checkpoint()
            if environment_provider is not None:
                if pre_restart_observation is None:
                    raise RuntimeError("controlled restart has no pre-restart runtime identity")
                post_restart = record_environment_observation(
                    environment_provider,
                    report,
                    writer,
                    phase="post_restart",
                    restart_previous=pre_restart_observation,
                )
                _, post_readiness = ready_snapshot(client)
                post_build = validate_server_build(
                    post_readiness, args.source_commit, args.profile
                )
                if post_build != server_build:
                    raise RuntimeError("server build identity changed across controlled restart")
                with report._checkpoint_lock:
                    report.evidence["restart_pre_control_plane"] = {
                        key: pre_restart_observation["runtime_evidence"][
                            "control_plane"
                        ]["container"][key]
                        for key in ("container_id", "started_at")
                    }
                    report.evidence["restart_post_control_plane"] = {
                        key: post_restart["runtime_evidence"]["control_plane"][
                            "container"
                        ][key]
                        for key in ("container_id", "started_at")
                    }

        if args.soak_seconds > 0:
            run_soak(
                client,
                report,
                writer,
                args,
                targets,
                run_id,
                measurements,
                runner_probe,
                runner_service_baseline,
                environment_provider,
            )
    except Exception as error:
        detail = str(error)
        with report._checkpoint_lock:
            if detail and detail not in report.failures:
                report.failures.append(detail)
        try:
            writer.event("gate_failed", detail=detail[:1_000])
        except Exception as event_error:
            with report._checkpoint_lock:
                report.failures.append(
                    f"capacity event evidence write failed: {event_error}"
                )

    if environment_provider is not None:
        try:
            record_environment_observation(
                environment_provider,
                report,
                writer,
                phase="final",
            )
        except Exception as error:
            detail = f"final environment evidence check failed: {error}"
            with report._checkpoint_lock:
                if detail not in report.failures:
                    report.failures.append(detail)
            try:
                writer.event("environment_observation_failed", detail=detail[:1_000])
            except Exception as event_error:
                with report._checkpoint_lock:
                    report.failures.append(
                        f"capacity event evidence write failed: {event_error}"
                    )

    if runner_probe is not None and runner_service_baseline is not None:
        try:
            runner_service_final = runner_probe.observe(runner_service_baseline)
            with report._checkpoint_lock:
                report.evidence["runner_service_final"] = runner_service_final
                report.evidence[
                    "runner_service_observations"
                ] = runner_probe.observation_count
            writer.event(
                "runner_service_final",
                observation_count=runner_probe.observation_count,
            )
        except Exception as error:
            detail = f"final runner service continuity check failed: {error}"
            with report._checkpoint_lock:
                if detail not in report.failures:
                    report.failures.append(detail)
            try:
                writer.event("runner_service_failed", detail=detail[:1_000])
            except Exception as event_error:
                with report._checkpoint_lock:
                    report.failures.append(
                        f"capacity event evidence write failed: {event_error}"
                    )

    try:
        writer.stop_periodic_checkpoints()
    except RuntimeError as error:
        with report._checkpoint_lock:
            report.failures.append(str(error))

    finalize_latency_measurements(
        report,
        measurements,
        read_threshold_ms=args.read_p95_ms,
        mutation_threshold_ms=args.mutation_p95_ms,
        event_threshold_ms=args.event_p95_ms,
    )
    if token_provider:
        with report._checkpoint_lock:
            if args.profile == "production" and token_provider.refresh_count < 2:
                report.failures.append(
                    "production gate did not prove OIDC token refresh"
                )
            report.evidence["token_refresh_count"] = token_provider.refresh_count
            report.evidence[
                "token_expires_at_epoch_seconds"
            ] = token_provider.expires_at
    with report._checkpoint_lock:
        report.evidence["gate_elapsed_seconds"] = time.monotonic() - gate_started
    try:
        writer.event("gate_completed", failures=len(report.failures))
    except Exception as error:
        with report._checkpoint_lock:
            report.failures.append(f"capacity event evidence write failed: {error}")
    writer.finalize()
    return report


def run_soak(
    client: Client,
    report: GateReport,
    writer: EvidenceWriter,
    args: argparse.Namespace,
    targets: list[tuple[str, str, str]],
    run_id: str,
    measurements: dict[str, list[float]],
    runner_probe: RunnerServiceProbe | None = None,
    runner_service_baseline: dict[str, Any] | None = None,
    environment_provider: EnvironmentEvidenceProvider | None = None,
) -> None:
    sample_clock = (
        linux_boottime_seconds if args.profile == "production" else time.monotonic
    )
    warmup_started = sample_clock()
    next_sample = warmup_started
    while sample_clock() - warmup_started < args.warmup_seconds:
        now = sample_clock()
        if now >= next_sample:
            capture_sample(
                client,
                report,
                writer,
                args,
                "warmup",
                warmup_started,
                runner_probe,
                runner_service_baseline,
            )
            next_sample += args.sample_seconds
            while next_sample <= sample_clock():
                next_sample += args.sample_seconds
        delay = min(next_sample, warmup_started + args.warmup_seconds) - sample_clock()
        if delay > 0:
            time.sleep(min(delay, 1.0))

    # Take a sample at the requested warmup boundary. The periodic loop's last
    # sample is normally at t=570s, and the live environment helper may consume
    # its full 85-second budget. Without this sample (and the samples below),
    # the helper could create an otherwise invisible 115-second metrics gap.
    capture_sample(
        client,
        report,
        writer,
        args,
        "warmup",
        warmup_started,
        runner_probe,
        runner_service_baseline,
    )

    # Freeze the anomaly/process baseline only after warmup and before any
    # soak Operation. The Prometheus and environment sidecars are cross-linked
    # in one explicit boundary record; the first soak sample may validate the
    # boundary but can never establish (and thereby absorb) its own baseline.
    boundary_started = sample_clock()
    boundary_environment: dict[str, Any] | None = None
    if environment_provider is not None:
        # The protected helper performs real 10-host/100-Engine inspection and
        # is deliberately allowed up to 85 seconds. Run it in parallel with
        # the normal metrics cadence so the warmup-to-boundary interval remains
        # observable and subject to the same global 90-second gap gate.
        with concurrent.futures.ThreadPoolExecutor(
            max_workers=1,
            thread_name_prefix="orchestrator-boundary-evidence",
        ) as pool:
            observation = pool.submit(
                record_environment_observation,
                environment_provider,
                report,
                writer,
                phase="soak_boundary",
                checkpoint=False,
            )
            next_helper_sample = sample_clock() + args.sample_seconds
            while not observation.done():
                remaining = next_helper_sample - sample_clock()
                if remaining > 0:
                    try:
                        observation.result(timeout=min(remaining, 1.0))
                    except concurrent.futures.TimeoutError:
                        continue
                    break
                capture_sample(
                    client,
                    report,
                    writer,
                    args,
                    "warmup",
                    warmup_started,
                    runner_probe,
                    runner_service_baseline,
                )
                next_helper_sample += args.sample_seconds
                while next_helper_sample <= sample_clock():
                    next_helper_sample += args.sample_seconds
            observation.result()
        with report._checkpoint_lock:
            boundary_environment = dict(report.environment_checks[-1])
    with report._checkpoint_lock:
        report.evidence["warmup_elapsed_seconds"] = sample_clock() - warmup_started
        report.evidence["warmup_samples"] = sum(
            1 for sample in report.samples if sample["phase"] == "warmup"
        )
    boundary_sample = capture_sample(
        client,
        report,
        writer,
        args,
        "soak_boundary",
        boundary_started,
        runner_probe,
        runner_service_baseline,
        checkpoint=False,
    )
    boundary_record = {
        "sample_sequence": boundary_sample["sequence"],
        "prometheus_snapshot_record": boundary_sample["metrics"]["snapshot_record"],
        "sampled_at_epoch_seconds": boundary_sample["sampled_at_epoch_seconds"],
        "sample_clock_seconds": boundary_sample["sample_clock_seconds"],
        "environment_record": (
            boundary_environment["sequence"] if boundary_environment is not None else None
        ),
        "environment_completed_at_epoch_seconds": (
            boundary_environment["completed_at_epoch_seconds"]
            if boundary_environment is not None
            else None
        ),
        "environment_aggregate_sha256": (
            boundary_environment["aggregate_sha256"]
            if boundary_environment is not None
            else None
        ),
        "anomalies": dict(boundary_sample["anomalies"]),
    }
    with report._checkpoint_lock:
        report.evidence["soak_boundary"] = boundary_record
    writer.event("soak_boundary", **boundary_record)
    writer.checkpoint()

    soak_started = sample_clock()
    next_sample = soak_started
    next_operation = soak_started
    round_index = 1
    boundary_process = boundary_sample["process"]
    boundary_storage = boundary_sample["storage"]
    baseline: dict[str, float] = {
        "rss": boundary_process["rss_bytes"],
        "threads": boundary_process["threads"],
        "active": boundary_process["active_requests"],
        "pool": boundary_storage["pool_connections"],
    }
    maxima = {
        "rss": float(boundary_process["rss_bytes"]),
        "threads": float(boundary_process["threads"]),
        "active": float(boundary_process["active_requests"]),
        "pool": float(boundary_storage["pool_connections"]),
        "pool_idle": float(boundary_storage["pool_idle_connections"]),
    }
    while sample_clock() - soak_started < args.soak_seconds:
        now = sample_clock()
        if now >= next_sample:
            sample = capture_sample(
                client,
                report,
                writer,
                args,
                "soak",
                soak_started,
                runner_probe,
                runner_service_baseline,
            )
            process = sample["process"]
            storage = sample["storage"]
            maxima["rss"] = max(maxima["rss"], process["rss_bytes"])
            maxima["threads"] = max(maxima["threads"], process["threads"])
            maxima["active"] = max(maxima["active"], process["active_requests"])
            maxima["pool"] = max(maxima["pool"], storage["pool_connections"])
            maxima["pool_idle"] = max(
                maxima["pool_idle"], storage["pool_idle_connections"]
            )
            next_sample += args.sample_seconds
            while next_sample <= sample_clock():
                next_sample += args.sample_seconds

        if now >= next_operation:
            round_phase_elapsed = sample_clock() - soak_started
            round_environment_identity: dict[str, Any] | None = None
            if environment_provider is not None:
                record_environment_observation(
                    environment_provider,
                    report,
                    writer,
                    phase="operation_round",
                    operation_round_index=round_index,
                )
                with report._checkpoint_lock:
                    environment_check = dict(report.environment_checks[-1])
                round_environment_identity = {
                    "environment_record": environment_check["sequence"],
                    "environment_engine_aggregate_sha256": environment_check[
                        "aggregate_sha256"
                    ],
                    "environment_node_ids_sha256": environment_check[
                        "node_ids_sha256"
                    ],
                    "environment_deployment_ids_sha256": environment_check[
                        "deployment_ids_sha256"
                    ],
                    "environment_container_ids_sha256": environment_check[
                        "container_ids_sha256"
                    ],
                }
            inventory, deployments, reads = inspect_inventory(
                client, report.expected, args.permanent_running_seconds
            )
            inventory["phase"] = "soak"
            inventory["phase_elapsed_seconds"] = round_phase_elapsed
            with report._checkpoint_lock:
                report.inventory_checks.append(inventory)
            measurements["read"].extend(reads)
            failures = inventory_failures(inventory, report.expected)
            with report._checkpoint_lock:
                report.failures.extend(failures)
            writer.event("inventory_check", check=inventory)
            if failures:
                writer.checkpoint()
                return
            refreshed_targets = select_operation_targets(
                deployments, args.concurrent_operations
            )
            if set(refreshed_targets) != set(targets):
                targets = refreshed_targets
            ok = execute_operation_round(
                client,
                report,
                args,
                targets,
                run_id,
                round_index,
                "soak",
                measurements,
                round_environment_identity,
            )
            with report._checkpoint_lock:
                report.operation_rounds[-1][
                    "phase_elapsed_seconds"
                ] = round_phase_elapsed
                operation_round = dict(report.operation_rounds[-1])
            writer.event("operation_round", **operation_round)
            writer.checkpoint()
            if not ok:
                return
            round_index += 1
            next_operation += args.operation_interval_seconds
            while next_operation <= sample_clock():
                next_operation += args.operation_interval_seconds

        deadline = min(
            next_sample,
            next_operation,
            soak_started + args.soak_seconds,
        )
        delay = deadline - sample_clock()
        if delay > 0:
            time.sleep(min(delay, 1.0))

    elapsed = sample_clock() - soak_started
    with report._checkpoint_lock:
        soak_samples = [
            sample for sample in report.samples if sample["phase"] == "soak"
        ]
    valid_samples = [sample for sample in soak_samples if sample.get("valid") is True]
    timestamps = [float(sample["sample_clock_seconds"]) for sample in valid_samples]
    gaps = [right - left for left, right in zip(timestamps, timestamps[1:])]
    max_gap = max(gaps, default=0.0)
    with report._checkpoint_lock:
        global_timestamps = [
            float(sample["sample_clock_seconds"])
            for sample in report.samples
            if sample.get("valid") is True
            and sample.get("phase") in {"warmup", "soak_boundary", "soak"}
        ]
    global_gaps = [
        right - left
        for left, right in zip(global_timestamps, global_timestamps[1:])
    ]
    max_global_gap = max(global_gaps, default=0.0)
    with report._checkpoint_lock:
        report.evidence.update(
            soak_elapsed_seconds=elapsed,
            soak_samples=len(soak_samples),
            valid_soak_samples=len(valid_samples),
            max_observed_sample_gap_seconds=max_gap,
            max_observed_global_sample_gap_seconds=max_global_gap,
            soak_operation_rounds=sum(
                1
                for round_ in report.operation_rounds
                if round_["phase"] == "soak"
            ),
        )
        if len(valid_samples) < args.minimum_valid_samples:
            report.failures.append(
                f"only {len(valid_samples)} valid soak samples were captured; "
                f"minimum is {args.minimum_valid_samples}"
            )
        if max_gap > args.max_sample_gap_seconds:
            report.failures.append(
                f"sample gap {max_gap:.2f}s exceeded {args.max_sample_gap_seconds:.2f}s"
            )
        if max_global_gap > args.max_sample_gap_seconds:
            report.failures.append(
                "global warmup/boundary/soak sample gap "
                f"{max_global_gap:.2f}s exceeded {args.max_sample_gap_seconds:.2f}s"
            )
    expected_rounds = max(
        1, math.floor(args.soak_seconds / args.operation_interval_seconds)
    )
    with report._checkpoint_lock:
        actual_rounds = report.evidence["soak_operation_rounds"]
        if actual_rounds < expected_rounds:
            report.failures.append(
                f"only {actual_rounds}/{expected_rounds} scheduled Operation rounds completed"
            )
        report.process.update(
            baseline_rss_bytes=baseline["rss"],
            max_rss_bytes=maxima["rss"],
            baseline_threads=baseline["threads"],
            max_threads=maxima["threads"],
            baseline_active_requests=baseline["active"],
            max_active_requests=maxima["active"],
            baseline_pool_connections=baseline["pool"],
            max_pool_connections=maxima["pool"],
            max_pool_idle_connections=maxima["pool_idle"],
        )
        if baseline["rss"] <= 0 or maxima["rss"] > baseline["rss"] * (
            1 + args.max_rss_growth
        ):
            report.failures.append(
                "RSS evidence is missing or exceeded the allowed growth"
            )
        if (
            baseline["threads"] <= 0
            or maxima["threads"]
            > baseline["threads"] + args.max_thread_growth
        ):
            report.failures.append(
                "thread evidence is missing or exceeded the allowed growth"
            )
        if (
            baseline["pool"] <= 0
            or maxima["pool"] > baseline["pool"] + args.max_pool_growth
        ):
            report.failures.append(
                "connection-pool evidence is missing or exceeded the allowed growth"
            )
        if maxima["active"] > baseline["active"] + args.max_active_request_growth:
            report.failures.append("active-request evidence exceeded the allowed growth")


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default=os.getenv("ORCHESTRATOR_GATE_BASE_URL", ""))
    parser.add_argument("--token", default=os.getenv("ORCHESTRATOR_GATE_OIDC_TOKEN", ""))
    parser.add_argument(
        "--token-argv-json",
        default=os.getenv("ORCHESTRATOR_GATE_TOKEN_ARGV_JSON", ""),
        help="runner-owned JSON argv that returns access_token/expires_at (no shell)",
    )
    parser.add_argument("--internal-token", default=os.getenv("ORCHESTRATOR_GATE_INTERNAL_TOKEN", ""))
    parser.add_argument("--ca-file", default=os.getenv("ORCHESTRATOR_GATE_CA_FILE", ""))
    parser.add_argument("--profile", choices=("production", "smoke"), default="production")
    parser.add_argument("--nodes", type=int)
    parser.add_argument("--deployments", type=int)
    parser.add_argument("--topology-resources", type=int)
    parser.add_argument("--concurrent-operations", type=int)
    parser.add_argument("--read-p95-ms", type=float, default=200)
    parser.add_argument("--mutation-p95-ms", type=float, default=500)
    parser.add_argument("--event-p95-ms", type=float, default=1_000)
    parser.add_argument("--recovery-seconds", type=float, default=60)
    parser.add_argument("--soak-seconds", type=int, default=0)
    parser.add_argument("--warmup-seconds", type=int)
    parser.add_argument("--sample-seconds", type=float)
    parser.add_argument("--operation-interval-seconds", type=float)
    parser.add_argument("--minimum-valid-samples", type=int)
    parser.add_argument("--max-sample-gap-seconds", type=float)
    parser.add_argument("--permanent-running-seconds", type=int, default=300)
    parser.add_argument("--max-rss-growth", type=float, default=0.10)
    parser.add_argument("--max-thread-growth", type=float, default=2)
    parser.add_argument("--max-pool-growth", type=float, default=2)
    parser.add_argument("--max-active-request-growth", type=float, default=2)
    parser.add_argument("--timeout", type=float, default=10)
    parser.add_argument("--run-id", default="")
    parser.add_argument("--source-commit", default=os.getenv("GITHUB_SHA", ""))
    parser.add_argument(
        "--oci-revision", default=os.getenv("ORCHESTRATOR_GATE_OCI_REVISION", "")
    )
    parser.add_argument(
        "--provenance-commit",
        default=os.getenv("ORCHESTRATOR_GATE_PROVENANCE_COMMIT", ""),
    )
    parser.add_argument(
        "--control-plane-image",
        default=os.getenv("ORCHESTRATOR_GATE_CONTROL_PLANE_IMAGE", ""),
    )
    parser.add_argument(
        "--agent-image", default=os.getenv("ORCHESTRATOR_GATE_AGENT_IMAGE", "")
    )
    parser.add_argument(
        "--fixture-image", default=os.getenv("ORCHESTRATOR_GATE_FIXTURE_IMAGE", "")
    )
    parser.add_argument(
        "--image-workflow-run-id",
        default=os.getenv("ORCHESTRATOR_GATE_IMAGE_WORKFLOW_RUN_ID", ""),
    )
    parser.add_argument(
        "--image-provenance-record-sha256",
        default=os.getenv("ORCHESTRATOR_GATE_IMAGE_PROVENANCE_RECORD_SHA256", ""),
    )
    parser.add_argument(
        "--restart-argv-json",
        default=os.getenv("ORCHESTRATOR_GATE_RESTART_ARGV_JSON", ""),
        help="runner-owned JSON argv for a real single-control-plane restart (no shell)",
    )
    parser.add_argument(
        "--environment-argv-json",
        default=os.getenv("ORCHESTRATOR_GATE_ENVIRONMENT_ARGV_JSON", ""),
        help="protected JSON argv for full Engine/container/network evidence (no shell)",
    )
    parser.add_argument("--report", default="artifacts/orchestrator-capacity-gate.json")
    args = parser.parse_args()
    defaults = {
        "production": (100, 2_000, 10_000, 50),
        "smoke": (0, 0, 0, 5),
    }[args.profile]
    for name, value in zip(("nodes", "deployments", "topology_resources", "concurrent_operations"), defaults):
        if getattr(args, name) is None:
            setattr(args, name, value)
    timing_defaults = {
        "production": (
            PRODUCTION_WARMUP_SECONDS,
            PRODUCTION_SAMPLE_SECONDS,
            PRODUCTION_OPERATION_INTERVAL_SECONDS,
            PRODUCTION_MINIMUM_VALID_SAMPLES,
            PRODUCTION_MAX_SAMPLE_GAP_SECONDS,
        ),
        "smoke": (0, 10.0, 300.0, 0, 90.0),
    }[args.profile]
    for name, value in zip(
        (
            "warmup_seconds",
            "sample_seconds",
            "operation_interval_seconds",
            "minimum_valid_samples",
            "max_sample_gap_seconds",
        ),
        timing_defaults,
    ):
        if getattr(args, name) is None:
            setattr(args, name, value)
    if not args.base_url:
        parser.error("--base-url or ORCHESTRATOR_GATE_BASE_URL is required")
    if args.concurrent_operations < 1:
        parser.error("--concurrent-operations must be positive")
    if args.sample_seconds <= 0 or args.operation_interval_seconds <= 0:
        parser.error("sample and Operation intervals must be positive")
    if args.warmup_seconds < 0 or args.soak_seconds < 0:
        parser.error("warmup and soak durations must not be negative")
    if args.profile == "production":
        try:
            args.base_url = normalize_https_origin(args.base_url)
        except RuntimeError as error:
            parser.error(str(error))
        if not args.restart_argv_json:
            parser.error(
                "production profile requires ORCHESTRATOR_GATE_RESTART_ARGV_JSON or --restart-argv-json"
            )
        if not args.token_argv_json:
            parser.error(
                "production profile requires ORCHESTRATOR_GATE_TOKEN_ARGV_JSON or --token-argv-json"
            )
        if not args.environment_argv_json:
            parser.error(
                "production profile requires ORCHESTRATOR_GATE_ENVIRONMENT_ARGV_JSON or --environment-argv-json"
            )
        if args.token or args.internal_token:
            parser.error("production profile forbids static OIDC and internal tokens")
        if not COMMIT_SHA_PATTERN.fullmatch(args.source_commit):
            parser.error("production --source-commit must be 40 lowercase hex characters")
        if os.getenv("GITHUB_SHA") != args.source_commit:
            parser.error("production GITHUB_SHA must match --source-commit")
        if os.getenv("GITHUB_REF") != "refs/heads/main":
            parser.error("production capacity evidence must run from refs/heads/main")
        if args.oci_revision != args.source_commit:
            parser.error("production OCI revision must match --source-commit")
        if args.provenance_commit != args.source_commit:
            parser.error("production provenance commit must match --source-commit")
        for name in ("control_plane_image", "agent_image", "fixture_image"):
            if not IMMUTABLE_OCI_PATTERN.fullmatch(getattr(args, name)):
                parser.error(f"production {name} must be an immutable OCI RepoDigest")
        if not re.fullmatch(r"[1-9][0-9]*", args.image_workflow_run_id):
            parser.error("production image workflow run ID must be a positive integer")
        if not SHA256_PATTERN.fullmatch(args.image_provenance_record_sha256):
            parser.error("production image provenance record SHA-256 is invalid")
        try:
            if parse_argv_json(
                args.environment_argv_json, "environment evidence argv"
            ) != REPOSITORY_ENVIRONMENT_OBSERVER_ARGV:
                parser.error(
                    "production environment observer argv must be the repository-owned fixed command"
                )
        except RuntimeError as error:
            parser.error(str(error))
        if args.soak_seconds < 86_400:
            parser.error("production soak must run for at least 86400 seconds")
        if args.warmup_seconds != PRODUCTION_WARMUP_SECONDS:
            parser.error("production warmup is fixed at 600 seconds")
        if args.sample_seconds != PRODUCTION_SAMPLE_SECONDS:
            parser.error("production sample interval is fixed at 30 seconds")
        if args.operation_interval_seconds != PRODUCTION_OPERATION_INTERVAL_SECONDS:
            parser.error("production Operation interval is fixed at 300 seconds")
        if args.minimum_valid_samples < PRODUCTION_MINIMUM_VALID_SAMPLES:
            parser.error("production requires at least 2736 valid samples")
        if args.max_sample_gap_seconds > PRODUCTION_MAX_SAMPLE_GAP_SECONDS:
            parser.error("production maximum sample gap cannot exceed 90 seconds")
        if args.permanent_running_seconds > 300:
            parser.error("production permanent-state limit cannot exceed 300 seconds")
        for name, minimum in zip(
            ("nodes", "deployments", "topology_resources", "concurrent_operations"),
            (100, 2_000, 10_000, 50),
        ):
            if getattr(args, name) < minimum:
                parser.error(f"production {name} cannot be below {minimum}")
        for name, maximum in (
            ("read_p95_ms", 200),
            ("mutation_p95_ms", 500),
            ("event_p95_ms", 1_000),
            ("recovery_seconds", 60),
            ("max_rss_growth", 0.10),
            ("max_thread_growth", 2),
            ("max_pool_growth", 2),
            ("max_active_request_growth", 2),
        ):
            if getattr(args, name) > maximum:
                parser.error(f"production {name} is weaker than {maximum}")
        for variable in (
            "GITHUB_REPOSITORY",
            "GITHUB_WORKFLOW",
            "GITHUB_RUN_ID",
            "GITHUB_RUN_ATTEMPT",
            "GITHUB_JOB",
            "GITHUB_SHA",
            "RUNNER_NAME",
            "RUNNER_OS",
            "RUNNER_ARCH",
            "RUNNER_ENVIRONMENT",
            "ORCHESTRATOR_GATE_RUNNER_LABELS",
            "ORCHESTRATOR_GATE_GITHUB_TOKEN",
            "ORCHESTRATOR_GATE_EXPECTED_RUNNER_UNIT",
        ):
            if not os.getenv(variable):
                parser.error(f"production evidence requires {variable}")
        expected_runner_unit = os.environ["ORCHESTRATOR_GATE_EXPECTED_RUNNER_UNIT"]
        if (
            not RUNNER_SERVICE_UNIT_PATTERN.fullmatch(expected_runner_unit)
            or not expected_runner_unit.endswith(
                f".{os.environ['RUNNER_NAME']}.service"
            )
        ):
            parser.error(
                "production expected runner service unit is malformed or mismatches RUNNER_NAME"
            )
        if os.getenv("GITHUB_RUN_ATTEMPT") != "1":
            parser.error(
                "production capacity evidence does not permit workflow reruns"
            )
    return args


def main() -> int:
    args = arguments()
    try:
        report = run_gate(args)
    except Exception as error:
        report = GateReport(
            profile=args.profile,
            started_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            expected={},
            evidence={
                "source_commit": args.source_commit,
                "soak_seconds_requested": args.soak_seconds,
            },
            failures=[str(error)],
        )
    output = report_output(report)
    print(output)
    path = Path(args.report)
    atomic_write(path, output + "\n")
    return 1 if report.failures else 0


if __name__ == "__main__":
    sys.exit(main())
