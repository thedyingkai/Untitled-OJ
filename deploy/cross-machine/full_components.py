"""Production-equivalent A/B implementation for ``cross_machine_e2e.py``.

This module is intentionally imported only by the expensive ``--full-components``
lane.  It never has a fixture fallback: every accepted run starts the real
Orchestrator, enrolls the real mTLS Agent, installs the real Rust Judge Worker
through Store, and drives the real Problem -> Redis -> Judge -> Worker flow.

All subprocesses use explicit argv through the parent harness.  Temporary
Docker build contexts are generated under the gate's private temporary
directory; no shell script is generated or executed.
"""

from __future__ import annotations

import base64
import copy
import hashlib
import json
import math
import os
import re
import secrets
import shutil
import ssl
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence


class FullGateError(RuntimeError):
    pass


IMAGE_BUNDLE_SAVE_TIMEOUT_ENV = "OJOS_CROSS_MACHINE_IMAGE_BUNDLE_SAVE_TIMEOUT_SECONDS"
IMAGE_BUNDLE_LOAD_TIMEOUT_ENV = "OJOS_CROSS_MACHINE_IMAGE_BUNDLE_LOAD_TIMEOUT_SECONDS"
DEFAULT_IMAGE_BUNDLE_SAVE_TIMEOUT_SECONDS = 3600.0
DEFAULT_IMAGE_BUNDLE_LOAD_TIMEOUT_SECONDS = 7200.0
MAX_IMAGE_BUNDLE_TIMEOUT_SECONDS = 21600.0
DOCKER_BUILD_MAX_ATTEMPTS = 4
DOCKER_BUILD_RETRY_DELAYS_SECONDS = (2.0, 5.0, 15.0)
CONTROL_PLANE_HEALTHCHECK_URL = (
    "https://127.0.0.1:8090/api/v1/healthz/ready"
)
CONTROL_PLANE_HEALTHCHECK_CA_CERT = "/opt/ojos-pki/ca.pem"
OPERATION_TIMEOUT_LOG_API_PAGE_SIZE = 500
OPERATION_TIMEOUT_LOG_API_MAX_PAGES = 4
OPERATION_TIMEOUT_ORCHESTRATOR_LOG_TAIL_LINES = 4_000
OPERATION_TIMEOUT_ORCHESTRATOR_LOG_MAX_CHARS = 256 * 1024
OPERATION_TIMEOUT_CORRELATED_LOG_MAX_CHARS = 128 * 1024
OPERATION_TIMEOUT_DIAGNOSTIC_ERROR_MAX_CHARS = 4_000
PROJECTION_INTEGRITY_CONVERGENCE_TIMEOUT_SECONDS = 120.0
PROJECTION_INTEGRITY_CONVERGENCE_POLL_SECONDS = 1.0
STANDARD_WORKLOAD_UID = 65532
STANDARD_WORKLOAD_GID = 65532
AGENT_DOCKER_SOCKET_GID = 10004
AGENT_CONFIG_USER = f"{STANDARD_WORKLOAD_UID}:{STANDARD_WORKLOAD_GID}"
AGENT_PRIVATE_DIRECTORY_MODE = "0700"
AGENT_PRIVATE_FILE_MODE = "0600"
A_AGENT_STATE_ROOT = "/var/lib/ojos-agent-a"
A_WORKLOAD_EXPORT_ROOT = "/var/lib/ojos-workload-export-a"
B_AGENT_STATE_ROOT = "/var/lib/ojos-agent"
B_WORKLOAD_EXPORT_ROOT = "/var/lib/ojos-workload-export"
AUTH_BOOTSTRAP_SECRET_HOST_DIRECTORY = "/var/lib/ojos-auth-bootstrap"
AUTH_BOOTSTRAP_SECRET_HOST_FILE = (
    AUTH_BOOTSTRAP_SECRET_HOST_DIRECTORY + "/initial-admin"
)
AUTH_BOOTSTRAP_SECRET_CONTAINER_FILE = "/run/secrets/ojos-auth-admin-bootstrap"
WORKLOAD_KEY_ID = "workload-1"
WORKLOAD_ISSUER = "ojos-auth/workload"
WORKLOAD_AUDIENCE = "ojos-gateway"


_TRANSIENT_DOCKER_BUILD_FAILURES: tuple[tuple[str, re.Pattern[str]], ...] = (
    (
        "tls-syscall",
        re.compile(r"\bssl_error_syscall\b", re.IGNORECASE),
    ),
    (
        "tls-handshake-timeout",
        re.compile(r"\b(?:net/http:\s*)?tls handshake timeout\b", re.IGNORECASE),
    ),
    (
        "tcp-transport",
        re.compile(
            r"\b(?:dial|read|write) tcp\b.{0,500}"
            r"(?:i/o timeout|connection reset by peer|connection timed out|unexpected eof)",
            re.IGNORECASE | re.DOTALL,
        ),
    ),
    (
        "name-resolution",
        re.compile(
            r"\btemporary failure in name resolution\b",
            re.IGNORECASE,
        ),
    ),
    (
        "registry-layer-short-read",
        re.compile(
            r"(?:failed to (?:copy|pull)|failed to fetch (?:anonymous|oauth) token|"
            r"failed to fetch https?://\S+|pulling (?:fs )?layer|"
            r"download(?:ing)? (?:layer|blob)|(?:blob|layer) sha256:[0-9a-f]+|"
            r"failed to resolve source metadata for \S+).{0,800}"
            r"(?:short read|unexpected eof)",
            re.IGNORECASE | re.DOTALL,
        ),
    ),
    (
        "registry-request-eof",
        re.compile(
            r"failed to (?:do )?request.{0,800}"
            r"(?:unexpected eof|connection reset by peer|i/o timeout)",
            re.IGNORECASE | re.DOTALL,
        ),
    ),
    (
        "registry-byte-count-eof",
        re.compile(
            r"failed to read expected number of bytes.{0,200}unexpected eof",
            re.IGNORECASE | re.DOTALL,
        ),
    ),
    (
        "registry-http-overload",
        re.compile(
            r"(?:https?://|registry|failed to fetch anonymous token|unexpected status from)"
            r".{0,500}"
            r"(?:429 too many requests|50[234] (?:bad gateway|service unavailable|gateway timeout))",
            re.IGNORECASE | re.DOTALL,
        ),
    ),
)


def _configured_timeout_seconds(name: str, default: float) -> float:
    raw = os.environ.get(name)
    if raw is None or not raw.strip():
        return default
    try:
        value = float(raw)
    except ValueError as exc:
        raise FullGateError(f"{name} must be a number of seconds") from exc
    if not math.isfinite(value) or value <= 0 or value > MAX_IMAGE_BUNDLE_TIMEOUT_SECONDS:
        raise FullGateError(
            f"{name} must be greater than zero and no more than "
            f"{MAX_IMAGE_BUNDLE_TIMEOUT_SECONDS:g} seconds"
        )
    return value


def _transient_docker_build_failure_kind(error: BaseException | str) -> str | None:
    detail = str(error)
    for kind, pattern in _TRANSIENT_DOCKER_BUILD_FAILURES:
        if pattern.search(detail):
            return kind
    return None


def _canonical(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


_PROJECTION_ROUTE_FIELDS = (
    "binding_id",
    "requirement_name",
    "consumer_deployment_id",
    "consumer_service_id",
    "consumer_node_id",
    "credential_generation",
    "api_id",
    "provider_deployment_id",
    "provider_service_id",
    "provider_node_id",
    "provider_endpoint",
    "upstream_base",
    "provider_path",
    "virtual_path",
    "auth_mode",
    "provider_auth_mode",
    "permission",
    "methods",
    "timeout_ms",
)
_PROJECTION_GRANT_FIELDS = (
    "binding_id",
    "requirement_name",
    "consumer_deployment_id",
    "consumer_service_id",
    "consumer_node_id",
    "credential_generation",
    "api_id",
    "permission",
)


def _effective_projection_sha256(routes: Any, grants: Any) -> str:
    """Independently reproduce the Go/Rust effective projection digest."""

    def normalize(items: Any, fields: tuple[str, ...], kind: str) -> list[dict[str, Any]]:
        if not isinstance(items, list):
            raise FullGateError(f"effective projection {kind} must be an array")
        normalized: list[dict[str, Any]] = []
        binding_ids: set[str] = set()
        for index, item in enumerate(items):
            if not isinstance(item, Mapping):
                raise FullGateError(
                    f"effective projection {kind} item {index} is not an object"
                )
            missing = [field for field in fields if field not in item]
            unknown = sorted(set(item) - set(fields))
            binding_id = item.get("binding_id")
            if missing or unknown or not isinstance(binding_id, str) or not binding_id:
                raise FullGateError(
                    f"effective projection {kind} item {index} is non-canonical: "
                    f"missing={missing}, unknown={unknown}"
                )
            if binding_id in binding_ids:
                raise FullGateError(
                    f"effective projection {kind} has duplicate binding_id {binding_id}"
                )
            binding_ids.add(binding_id)
            normalized.append({field: item[field] for field in fields})
        normalized.sort(key=lambda item: item["binding_id"])
        return normalized

    payload = {
        "routes": normalize(routes, _PROJECTION_ROUTE_FIELDS, "routes"),
        "grants": normalize(grants, _PROJECTION_GRANT_FIELDS, "grants"),
    }
    encoded = json.dumps(
        payload, ensure_ascii=False, separators=(",", ":"), sort_keys=False
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _bounded_diagnostic_text(value: str, limit: int) -> tuple[str, bool, int]:
    """Keep both ends of a diagnostic window instead of losing its first error."""

    if limit < 256:
        raise FullGateError("diagnostic text limit must be at least 256 characters")
    length = len(value)
    if length <= limit:
        return value, False, length
    marker = f"\n[... truncated {length - limit} diagnostic characters ...]\n"
    available = max(0, limit - len(marker))
    head = available * 3 // 5
    tail = available - head
    return value[:head] + marker + value[-tail:], True, length


def _redact_diagnostic_value(value: Any) -> Any:
    """Remove credential-shaped fields and strings from failure-only evidence."""

    exact = {
        "authorization",
        "cookie",
        "set_cookie",
        "token",
        "access_token",
        "refresh_token",
        "id_token",
        "jwt",
        "password",
        "secret",
        "private_key",
        "lease_token",
    }
    suffixes = ("_token", "_secret", "_password", "_private_key")
    if isinstance(value, Mapping):
        redacted: dict[str, Any] = {}
        for raw_key, child in value.items():
            key = str(raw_key)
            normalized = key.casefold().replace("-", "_")
            if normalized in exact or normalized.endswith(suffixes):
                redacted[key + "_redacted"] = child not in (None, False, "", [], {})
            else:
                redacted[key] = _redact_diagnostic_value(child)
        return redacted
    if isinstance(value, list):
        return [_redact_diagnostic_value(item) for item in value]
    if isinstance(value, tuple):
        return [_redact_diagnostic_value(item) for item in value]
    if not isinstance(value, str):
        return value
    value = re.sub(
        r"(?i)(?:^|(?<=\s))Bearer\s+[A-Za-z0-9._~+/=-]+",
        "Bearer [redacted]",
        value,
    )
    value = re.sub(
        r"(?<![A-Za-z0-9_-])eyJ[A-Za-z0-9_-]{5,}\."
        r"[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}(?![A-Za-z0-9_-])",
        "[redacted-jwt]",
        value,
    )
    value = re.sub(
        r"-----BEGIN (?:RSA |EC )?PRIVATE KEY-----.*?"
        r"-----END (?:RSA |EC )?PRIVATE KEY-----",
        "[redacted-private-key]",
        value,
        flags=re.DOTALL,
    )
    return value


def _rfc3339_timestamp(value: Any) -> bool:
    return isinstance(value, str) and re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T"
        r"[0-9]{2}:[0-9]{2}:[0-9]{2}"
        r"(?:\.[0-9]{1,9})?(?:Z|[+-][0-9]{2}:[0-9]{2})",
        value,
    ) is not None


def _single_line_sql(value: str) -> str:
    """Normalize inline SQL for the no-shell command runner."""
    normalized = " ".join(line.strip() for line in value.splitlines() if line.strip())
    if not normalized:
        raise FullGateError("inline SQL command must not be empty")
    return normalized


def _sha256(value: bytes | str) -> str:
    if isinstance(value, str):
        value = value.encode("utf-8")
    return "sha256:" + hashlib.sha256(value).hexdigest()


def _canonical_sha256_digest(value: Any, label: str) -> str:
    """Normalize a service/database digest at the public evidence boundary."""

    if not isinstance(value, str):
        raise FullGateError(f"{label} must be a SHA-256 digest")
    if re.fullmatch(r"[0-9a-f]{64}", value):
        return "sha256:" + value
    if re.fullmatch(r"sha256:[0-9a-f]{64}", value):
        return value
    raise FullGateError(f"{label} must be a lowercase SHA-256 digest")


def _normalize_actual_flow_digest_evidence(
    problem_evidence: Mapping[str, Any], submission_digest: Any
) -> tuple[dict[str, Any], dict[str, Any], str]:
    """Return canonical problem/projection/submission digest evidence."""

    problem = problem_evidence.get("problem")
    projection = problem_evidence.get("projection")
    if not isinstance(problem, Mapping) or not isinstance(projection, Mapping):
        raise FullGateError("actual flow digest evidence is missing problem or projection")
    normalized_problem = copy.deepcopy(dict(problem))
    normalized_projection = copy.deepcopy(dict(projection))
    normalized_problem["package_sha256"] = _canonical_sha256_digest(
        normalized_problem.get("package_sha256"), "Problem package"
    )
    normalized_projection["package_sha256"] = _canonical_sha256_digest(
        normalized_projection.get("package_sha256"), "Judge projection package"
    )
    normalized_submission = _canonical_sha256_digest(
        submission_digest, "Submission package"
    )
    return normalized_problem, normalized_projection, normalized_submission


def _preprovisioned_dependency_contract(
    *,
    service_id: str,
    version: str,
    service_type: str,
    protocol: str,
    port: int,
    health_path: str,
) -> dict[str, Any]:
    """Return the v2 image release for an A-side bootstrap dependency.

    Store management mode (Managed or External) is selected by the install
    request, not by release metadata.  These releases therefore remain honest
    digest-pinned image releases while the live gate registers the already
    running instances as External after a real protocol health probe.
    """

    return {
        "schema_version": 2,
        "service_name": service_id,
        "version": version,
        "description": f"Production-equivalent preprovisioned {service_id} dependency.",
        "service_type": service_type,
        "source": {"kind": "local", "url": f"local://services/{service_id}", "checksum": ""},
        "runtime": {
            "kind": "image",
            "image": "",
            "binary": "",
            "system_service": "",
        },
        "frontend": {
            "enabled": False,
            "route_prefix": "",
            "remote_entry": "",
            "menu_items": [],
        },
        "backend": {"protocol": protocol, "port": port, "health_path": health_path},
        "migrations": [],
        "permissions": [],
        "routes": [],
        "provides": {"apis": []},
        "requires": {"apis": []},
        "events": {"publishes": [], "subscribes": []},
        "runtime_contract": {
            "id": "standard-container-v1",
            "sha256": "sha256:56c8ec1e421205dbebb97ad40cbda30bf468d198dd8c3fc50151e39465ea573f",
            "binding_directory": "/run/ojos/service",
            "identity_mode": "workload",
            "credential_delivery": "file",
            "restart_on_change": False,
        },
        "redis": [],
        "storage": [],
        "dependencies": [],
        "config_schema": {},
        "secrets": [],
        "observability": {"metrics": True, "jaeger": True},
    }


def _audit_generated_catalog(
    output: Path,
    *,
    expected_catalog_id: str,
    expected_os: str,
    expected_arch: str,
    expected_contracts: Mapping[tuple[str, str], Mapping[str, Any]] | None = None,
) -> list[dict[str, Any]]:
    """Fail before publication if signed Catalog and metadata diverge.

    Store repeats these checks at import time.  Running them immediately after
    generation keeps a malformed source out of the catalog-server image and
    audits every service entry, rather than discovering one mismatch per costly
    dual-Engine run.
    """

    catalog_path = output / "catalog.json"
    metadata_paths = sorted((output / "metadata").glob("*.release.json"))
    if not metadata_paths:
        raise FullGateError(f"Catalog {expected_catalog_id} contains no metadata documents")
    try:
        catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise FullGateError(f"Catalog {expected_catalog_id} audit could not read JSON: {exc}") from exc

    modules = catalog.get("modules")
    if catalog.get("id") != expected_catalog_id or not isinstance(modules, list) or not modules:
        raise FullGateError(f"Catalog {expected_catalog_id} audit expected its ID and modules")
    module_versions: dict[str, list[str]] = {}
    for module in modules:
        if not isinstance(module, Mapping) or not isinstance(module.get("id"), str):
            raise FullGateError(f"Catalog {expected_catalog_id} contains an invalid module")
        releases = module.get("releases")
        if not isinstance(releases, list) or not releases:
            raise FullGateError(
                f"Catalog {expected_catalog_id} module {module['id']} has no releases"
            )
        module_versions[str(module["id"])] = [
            str(release.get("version", ""))
            for release in releases
            if isinstance(release, Mapping)
        ]

    expected_platform = {"os": expected_os, "arch": expected_arch}
    referenced_metadata: set[Path] = set()
    audits: list[dict[str, Any]] = []
    for module in modules:
        service_id = str(module["id"])
        for release in module["releases"]:
            metadata_url = release.get("metadata", {}).get("url")
            metadata_name = Path(str(metadata_url)).name
            metadata_path = output / "metadata" / metadata_name
            if not metadata_path.is_file():
                raise FullGateError(
                    f"Catalog {expected_catalog_id} metadata is missing for "
                    f"{service_id}@{release.get('version')}"
                )
            referenced_metadata.add(metadata_path.resolve())
            metadata_bytes = metadata_path.read_bytes()
            metadata = json.loads(metadata_bytes)
            version = metadata.get("version")
            if metadata.get("service_name") != service_id or release.get("version") != version:
                raise FullGateError(
                    f"Catalog {expected_catalog_id} module/version differs from release metadata"
                )
            expected_checksum = _sha256(metadata_bytes)
            if release.get("metadata", {}).get("sha256") != expected_checksum:
                raise FullGateError(
                    f"Catalog {expected_catalog_id} metadata checksum differs for {service_id}@{version}"
                )

            catalog_dependencies = {
                dependency.get("module_id"): dependency.get("requirement")
                for dependency in release.get("dependencies", [])
                if isinstance(dependency, Mapping)
            }
            metadata_dependencies_raw = metadata.get("dependencies", [])
            if not isinstance(metadata_dependencies_raw, list) or not all(
                isinstance(dependency, str) for dependency in metadata_dependencies_raw
            ):
                raise FullGateError(
                    f"Catalog {expected_catalog_id} metadata dependencies are invalid"
                )
            metadata_dependencies = set(metadata_dependencies_raw)
            if set(catalog_dependencies) != metadata_dependencies:
                raise FullGateError(
                    f"Catalog {expected_catalog_id} dependency sets differ for {service_id}@{version}: "
                    f"catalog={sorted(catalog_dependencies)}, metadata={sorted(metadata_dependencies)}"
                )
            for dependency, requirement in catalog_dependencies.items():
                dependency_versions = module_versions.get(str(dependency), [])
                if len(dependency_versions) != 1 or requirement != "=" + dependency_versions[0]:
                    raise FullGateError(
                        f"Catalog {expected_catalog_id} dependency version differs for "
                        f"{service_id}@{version} -> {dependency}"
                    )

            platforms = release.get("platforms")
            if platforms != [expected_platform]:
                raise FullGateError(
                    f"Catalog {expected_catalog_id} platform differs: expected "
                    f"{expected_platform}, got {platforms}"
                )
            runtime = metadata.get("runtime")
            metadata_image = runtime.get("image") if isinstance(runtime, Mapping) else None
            if release.get("oci_image") != metadata_image:
                raise FullGateError(
                    f"Catalog {expected_catalog_id} OCI image differs from release metadata"
                )
            audits.append(
                {
                    "catalog_id": expected_catalog_id,
                    "service_id": service_id,
                    "version": version,
                    "dependencies": sorted(metadata_dependencies),
                    "platform": expected_platform,
                    "oci_image": metadata_image,
                }
            )
    available_metadata = {path.resolve() for path in metadata_paths}
    if referenced_metadata != available_metadata:
        raise FullGateError(
            f"Catalog {expected_catalog_id} metadata file set differs from signed releases"
        )
    audits = sorted(audits, key=lambda item: (str(item["service_id"]), str(item["version"])))
    if expected_contracts is not None:
        observed = {
            (str(item["service_id"]), str(item["version"])): item for item in audits
        }
        if set(observed) != set(expected_contracts):
            raise FullGateError(
                f"Catalog {expected_catalog_id} release identity set differs: "
                f"expected={sorted(expected_contracts)}, observed={sorted(observed)}"
            )
        for identity, expected in expected_contracts.items():
            actual = observed[identity]
            if (
                actual["dependencies"] != sorted(expected.get("dependencies", []))
                or actual["oci_image"] != expected.get("oci_image")
            ):
                raise FullGateError(
                    f"Catalog {expected_catalog_id} expected contract differs for "
                    f"{identity[0]}@{identity[1]}"
                )
    return audits


def _orchestrator_postgres_database_url(password: str) -> str:
    """Return the TLS-required URL accepted by ``tokio-postgres``.

    Unlike libpq, ``tokio-postgres`` only accepts ``disable``, ``prefer`` and
    ``require`` as sslmode values.  The Orchestrator supplies its private CA
    separately through ``ORCHESTRATOR_POSTGRES_CA_CERT``; the rustls connector
    still verifies both the certificate chain and the ``postgres-a`` hostname.
    """

    return (
        f"postgresql://postgres:{password}@postgres-a:5432/ojos_orchestrator"
        "?sslmode=require"
    )


def _standalone_node_enrollment(
    node_id: str,
    host_ip: str,
    labels: Mapping[str, Any],
) -> dict[str, Any]:
    """Build the public enrollment request for an independent runtime host."""

    return {
        "node_id": node_id,
        "host_ip": host_ip,
        "role": "standalone",
        "labels": copy.deepcopy(dict(labels)),
        "ttl_seconds": 600,
    }


def _json_from_last_line(output: str) -> dict[str, Any]:
    for line in reversed(output.splitlines()):
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            return value
    raise FullGateError(f"command produced no JSON object: {output[-2000:]!r}")


def _binding_names(bindings: Any) -> set[str]:
    if not isinstance(bindings, list):
        return set()
    return {
        str(binding.get("requirement_name", ""))
        for binding in bindings
        if isinstance(binding, Mapping)
        and str(binding.get("state", "")).upper() == "ACTIVE"
        and str(binding.get("desired_state", "")).upper() == "ACTIVE"
    }


def _judge_sandbox_security_options_are_exact(value: Any) -> bool:
    """Accept only the requested AppArmor option plus Docker's privileged normalization."""

    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        return False
    options = set(value)
    return len(options) == len(value) and options in (
        {"apparmor=unconfined"},
        {"apparmor=unconfined", "label=disable"},
    )


def _judge_sandbox_host_mounts_are_exact(value: Any) -> bool:
    """Match the fixed profile while honoring Docker's omitted-false encoding."""

    if (
        not isinstance(value, list)
        or len(value) != 5
        or any(not isinstance(item, Mapping) for item in value)
    ):
        return False

    by_target: dict[str, Mapping[str, Any]] = {}
    for item in value:
        target = item.get("Target")
        if not isinstance(target, str) or not target or target in by_target:
            return False
        by_target[target] = item

    expected_targets = {
        "/var/lib/ojos-worker/work",
        "/var/lib/ojos-worker/cache",
        "/sys/fs/cgroup",
        "/tmp",
        "/run/ojos/service",
    }
    if set(by_target) != expected_targets:
        return False

    def has_default_false(item: Mapping[str, Any], key: str) -> bool:
        # Docker Engine omits false boolean fields from Mount inspect JSON.
        return key not in item or item[key] is False

    def has_no_options(item: Mapping[str, Any], key: str) -> bool:
        return key not in item or item[key] is None

    def is_agent_path(value: Any) -> bool:
        return isinstance(value, str) and value.startswith("/") and value != "/"

    def is_rprivate_bind(item: Mapping[str, Any], *, read_only: bool) -> bool:
        bind = item.get("BindOptions")
        if not isinstance(bind, Mapping):
            return False
        if set(bind) - {
            "Propagation",
            "NonRecursive",
            "CreateMountpoint",
            "ReadOnlyNonRecursive",
            "ReadOnlyForceRecursive",
        }:
            return False
        if str(bind.get("Propagation", "")).lower() != "rprivate":
            return False
        if not all(
            has_default_false(bind, key)
            for key in (
                "NonRecursive",
                "CreateMountpoint",
                "ReadOnlyNonRecursive",
                "ReadOnlyForceRecursive",
            )
        ):
            return False
        if read_only:
            if item.get("ReadOnly") is not True:
                return False
        elif not has_default_false(item, "ReadOnly"):
            return False
        return (
            item.get("Type") == "bind"
            and has_no_options(item, "VolumeOptions")
            and has_no_options(item, "TmpfsOptions")
        )

    scratch = by_target["/var/lib/ojos-worker/work"]
    if not is_agent_path(scratch.get("Source")) or not is_rprivate_bind(
        scratch, read_only=False
    ):
        return False

    cgroup = by_target["/sys/fs/cgroup"]
    if cgroup.get("Source") != "/sys/fs/cgroup" or not is_rprivate_bind(
        cgroup, read_only=False
    ):
        return False

    service_context = by_target["/run/ojos/service"]
    if not is_agent_path(service_context.get("Source")) or not is_rprivate_bind(
        service_context, read_only=True
    ):
        return False

    cache = by_target["/var/lib/ojos-worker/cache"]
    volume = cache.get("VolumeOptions")
    if (
        cache.get("Type") != "volume"
        or not isinstance(cache.get("Source"), str)
        or not cache.get("Source")
        or not has_default_false(cache, "ReadOnly")
        or not has_no_options(cache, "BindOptions")
        or not has_no_options(cache, "TmpfsOptions")
        or not isinstance(volume, Mapping)
        or set(volume) - {"NoCopy", "Labels", "DriverConfig", "Subpath"}
        or volume.get("NoCopy") is not True
        or volume.get("Labels") not in (None, {})
        or volume.get("DriverConfig") is not None
        or volume.get("Subpath") not in (None, "")
    ):
        return False

    tmpfs = by_target["/tmp"]
    tmpfs_options = tmpfs.get("TmpfsOptions")
    if (
        tmpfs.get("Type") != "tmpfs"
        or tmpfs.get("Source") not in (None, "")
        or not has_default_false(tmpfs, "ReadOnly")
        or not has_no_options(tmpfs, "BindOptions")
        or not has_no_options(tmpfs, "VolumeOptions")
        or not isinstance(tmpfs_options, Mapping)
        or set(tmpfs_options) - {"SizeBytes", "Mode", "Options"}
        or tmpfs_options.get("SizeBytes") != 268435456
        or tmpfs_options.get("Mode") != 0o1777
        or tmpfs_options.get("Options") not in (None, [])
    ):
        return False

    return True


def _rebind_topology_requirement(
    spec: Mapping[str, Any],
    *,
    consumer_deployment_id: str,
    requirement_name: str,
    old_provider_deployment_id: str,
    new_provider_deployment_id: str,
    require_same_service: bool = True,
) -> tuple[dict[str, Any], dict[str, str]]:
    """Move one explicit API requirement to another deployed provider.

    This is deliberately a semantic rebind: both the Link target and the
    binding's provider deployment change.  Cosmetic Topology metadata must not
    be used to manufacture a ServiceContext generation change.
    """

    rebound = copy.deepcopy(spec)
    if not isinstance(rebound, dict):
        raise FullGateError("Topology rebind requires an object spec")
    endpoints = rebound.get("endpoints")
    links = rebound.get("links")
    if not isinstance(endpoints, list) or not isinstance(links, list):
        raise FullGateError("Topology rebind requires endpoint and Link arrays")

    def endpoint_for(deployment_id: str, role: str) -> dict[str, Any]:
        matches = [
            endpoint
            for endpoint in endpoints
            if isinstance(endpoint, dict)
            and isinstance(endpoint.get("config"), Mapping)
            and endpoint["config"].get("deployment_id") == deployment_id
        ]
        if len(matches) != 1:
            raise FullGateError(
                f"Topology rebind expected one {role} endpoint for {deployment_id}, "
                f"found {len(matches)}"
            )
        return matches[0]

    consumer = endpoint_for(consumer_deployment_id, "consumer")
    old_provider = endpoint_for(old_provider_deployment_id, "old provider")
    new_provider = endpoint_for(new_provider_deployment_id, "new provider")
    if old_provider_deployment_id == new_provider_deployment_id:
        raise FullGateError("Topology rebind requires a different provider deployment")
    if (
        require_same_service
        and old_provider.get("service_id") != new_provider.get("service_id")
    ):
        raise FullGateError("Topology rebind providers do not implement the same service")

    consumer_endpoint = str(consumer.get("endpoint", ""))
    old_provider_endpoint = str(old_provider.get("endpoint", ""))
    new_provider_endpoint = str(new_provider.get("endpoint", ""))
    if not consumer_endpoint or not old_provider_endpoint or not new_provider_endpoint:
        raise FullGateError("Topology rebind resolved an empty endpoint")
    if old_provider_endpoint == new_provider_endpoint:
        raise FullGateError("Topology rebind requires a different provider endpoint")

    matches: list[tuple[int, int]] = []
    for link_index, link in enumerate(links):
        if not isinstance(link, dict) or link.get("source_endpoint") != consumer_endpoint:
            continue
        selections = link.get("api_bindings", [])
        if not isinstance(selections, list):
            raise FullGateError("Topology Link api_bindings must be an array")
        for selection_index, selection in enumerate(selections):
            if (
                isinstance(selection, Mapping)
                and selection.get("requirement") == requirement_name
            ):
                matches.append((link_index, selection_index))
    if len(matches) != 1:
        raise FullGateError(
            f"Topology rebind expected one {requirement_name} selection, found {len(matches)}"
        )

    link_index, selection_index = matches[0]
    source_link = links[link_index]
    selections = source_link["api_bindings"]
    selection = selections[selection_index]
    if source_link.get("target_endpoint") != old_provider_endpoint:
        raise FullGateError(
            f"Topology rebind {requirement_name} Link does not target the expected old provider"
        )
    if selection.get("provider_deployment_id") != old_provider_deployment_id:
        raise FullGateError(
            f"Topology rebind {requirement_name} selection does not name the expected old provider"
        )

    moved_selection = copy.deepcopy(selection)
    moved_selection["provider_deployment_id"] = new_provider_deployment_id
    existing_target_links = [
        (index, link)
        for index, link in enumerate(links)
        if isinstance(link, dict)
        and index != link_index
        and link.get("source_endpoint") == consumer_endpoint
        and link.get("target_endpoint") == new_provider_endpoint
    ]
    if len(existing_target_links) > 1:
        raise FullGateError("Topology rebind found duplicate Links to the new provider")

    if len(selections) == 1 and not existing_target_links:
        source_link["target_endpoint"] = new_provider_endpoint
        source_link["api_bindings"] = [moved_selection]
    else:
        del selections[selection_index]
        if existing_target_links:
            _, target_link = existing_target_links[0]
            for field in ("protocol", "auth_mode", "scope", "enabled"):
                if target_link.get(field) != source_link.get(field):
                    raise FullGateError(
                        f"Topology rebind cannot merge Links with different {field}"
                    )
            target_selections = target_link.get("api_bindings")
            if not isinstance(target_selections, list):
                raise FullGateError("Topology target Link api_bindings must be an array")
            target_selections.append(moved_selection)
        else:
            target_link = copy.deepcopy(source_link)
            target_link["target_endpoint"] = new_provider_endpoint
            target_link["api_bindings"] = [moved_selection]
            links.insert(link_index + 1, target_link)
        if not selections and source_link.get("scope") == "api-binding":
            links.remove(source_link)

    return rebound, {
        "consumer_deployment_id": consumer_deployment_id,
        "consumer_endpoint": consumer_endpoint,
        "requirement_name": requirement_name,
        "api_id": str(moved_selection.get("api_id", "")),
        "old_provider_deployment_id": old_provider_deployment_id,
        "old_provider_endpoint": old_provider_endpoint,
        "new_provider_deployment_id": new_provider_deployment_id,
        "new_provider_endpoint": new_provider_endpoint,
    }


class JsonClient:
    def __init__(
        self,
        origin: str,
        ca_file: Path,
        *,
        default_headers: Mapping[str, str] | None = None,
    ) -> None:
        self.origin = origin.rstrip("/")
        context = ssl.create_default_context(cafile=str(ca_file))
        self.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=context)
        )
        self.default_headers = dict(default_headers or {})

    def request(
        self,
        method: str,
        path: str,
        value: Any | None = None,
        *,
        headers: Mapping[str, str] | None = None,
        expected: Iterable[int] = (200,),
        timeout: float = 60,
        status_only: bool = False,
    ) -> tuple[dict[str, Any], dict[str, str], int]:
        body = None if value is None else _canonical(value).encode("utf-8")
        request_headers = dict(self.default_headers)
        request_headers.update(headers or {})
        if body is not None:
            request_headers.setdefault("content-type", "application/json")
        request = urllib.request.Request(
            self.origin + "/" + path.lstrip("/"),
            data=body,
            method=method,
            headers=request_headers,
        )
        try:
            response = self.opener.open(request, timeout=timeout)
        except urllib.error.HTTPError as error:
            status = error.code
            response_headers = {
                key.lower(): val for key, val in error.headers.items()
            }
            raw = b"" if status_only else error.read()
            error.close()
            if status not in set(expected):
                if status_only:
                    raise FullGateError(
                        f"{method} {path} returned {status}, expected {sorted(expected)}"
                    ) from error
                detail = raw.decode("utf-8", "replace")[-4000:]
                raise FullGateError(
                    f"{method} {path} returned {status}, expected {sorted(expected)}: {detail}"
                ) from error
        else:
            with response:
                status = response.status
                response_headers = {
                    key.lower(): val for key, val in response.headers.items()
                }
                raw = b"" if status_only else response.read()
        if status not in set(expected):
            raise FullGateError(
                f"{method} {path} returned {status}, expected {sorted(expected)}"
            )
        if status_only:
            return {}, response_headers, status
        if not raw:
            decoded: Any = {}
        else:
            try:
                decoded = json.loads(raw)
            except json.JSONDecodeError as error:
                raise FullGateError(f"{method} {path} did not return JSON") from error
        if not isinstance(decoded, dict):
            raise FullGateError(f"{method} {path} returned a non-object JSON document")
        return decoded, response_headers, status


class FullComponentsScenario:
    """One uninterrupted actual-service chain spanning Engines A and B."""

    A_NETWORK = "172.28.0.0/24"
    B_SERVICE_NETWORK = "172.30.0.0/24"
    B_AGENT_NETWORK = "172.31.0.0/24"
    A_IPS = {
        "postgres": "172.28.0.10",
        "redis": "172.28.0.11",
        "minio": "172.28.0.12",
        "storage": "172.28.0.13",
        "auth": "172.28.0.14",
        "problem": "172.28.0.15",
        "judge": "172.28.0.16",
        "gateway": "172.28.0.17",
        "orchestrator": "172.28.0.18",
        "gateway_tls": "172.28.0.19",
        "oidc": "172.28.0.20",
        "catalog": "172.28.0.21",
        "registry": "172.28.0.22",
        "echo": "172.28.0.23",
        "storage_head_miss": "172.28.0.24",
    }

    PROFILE_SHA256 = "sha256:a6b35a495f88bd8e723e395d748de40fbb4dcc08619d02cf92fa580fef2a18ec"

    def __init__(self, harness: Any, temporary: Path) -> None:
        if harness.a is None or harness.b is None:
            raise FullGateError("nested Docker Engines were not initialized")
        self.h = harness
        self.root = harness.root
        self.a = harness.a
        self.b = harness.b
        self.runner = harness.runner
        self.repo = harness.repo_root
        self.tmp = temporary
        # Every production-equivalent run gets isolated credentials.  Evidence only
        # records fingerprints and never serializes these values.
        self.postgres_password = secrets.token_urlsafe(32)
        self.jwt_secret = secrets.token_urlsafe(48)
        self.internal_token = secrets.token_urlsafe(48)
        self.auth_internal_token = secrets.token_urlsafe(48)
        self.workload_issuer_token = secrets.token_urlsafe(48)
        self.auth_management_token = secrets.token_urlsafe(48)
        self.gateway_management_token = secrets.token_urlsafe(48)
        self.auth_contribution_ack_token = secrets.token_urlsafe(48)
        self.gateway_contribution_ack_token = secrets.token_urlsafe(48)
        self.auth_bootstrap_secret = secrets.token_urlsafe(48)
        self.minio_access = "ojos" + secrets.token_hex(10)
        self.minio_secret = secrets.token_urlsafe(40)
        self.admin_username = "cross_machine_admin_" + harness.run_id
        self.admin_password = "Admin-" + secrets.token_urlsafe(32) + "-9!"
        self.admin_token = ""
        self.auth_bootstrap_delivery_evidence: dict[str, Any] = {}
        self.a_network = f"ojos-full-{harness.run_id}-a"
        self.b_service_network = f"ojos-full-{harness.run_id}-service"
        self.b_agent_network = f"ojos-full-{harness.run_id}-agent"
        self.commit = ""
        self.ca_cert = temporary / "pki" / "ca.pem"
        self.ca_key = temporary / "pki" / "ca.key"
        self.server_cert = temporary / "pki" / "server.pem"
        self.server_key = temporary / "pki" / "server.key"
        self.workload_private = temporary / "pki" / "workload-private.pem"
        self.workload_public = temporary / "pki" / "workload-public.pem"
        self.fixture_image = f"ojos/cross-machine-fixture:{harness.run_id}"
        self.secure_fixture_image = f"ojos/cross-machine-secure-fixture:{harness.run_id}"
        self.images: dict[str, str] = {}
        self.oci: dict[str, str] = {}
        self.catalog_sources: list[dict[str, Any]] = []
        self.catalog_trust: dict[str, str] = {}
        self.catalog_contract_audits: list[dict[str, Any]] = []
        self.control_client: JsonClient | None = None
        self.gateway_client: JsonClient | None = None
        self.agent_identity: dict[str, Any] = {}
        self.a_agent_identity: dict[str, Any] = {}
        self.provider_deployments: dict[str, str] = {}
        self.external_dependency_runtimes: dict[str, dict[str, Any]] = {}
        self.managed_a_runtimes: dict[str, dict[str, Any]] = {}
        self.topology_id = "cross-machine-a-b"
        self.worker_deployment_id = ""
        self.worker_container_id = ""
        self.image_bundle_save_timeout = _configured_timeout_seconds(
            IMAGE_BUNDLE_SAVE_TIMEOUT_ENV, DEFAULT_IMAGE_BUNDLE_SAVE_TIMEOUT_SECONDS
        )
        self.image_bundle_load_timeout = _configured_timeout_seconds(
            IMAGE_BUNDLE_LOAD_TIMEOUT_ENV, DEFAULT_IMAGE_BUNDLE_LOAD_TIMEOUT_SECONDS
        )

    def run(self) -> None:
        self.h.checkpoint("full.preflight")
        self._preflight()
        self.h.checkpoint("full.networks")
        self._create_networks()
        self.h.checkpoint("full.pki")
        self._generate_pki()
        self.h.checkpoint("full.build-images")
        self._build_images()
        self.h.checkpoint("full.infrastructure")
        self._start_infrastructure()
        self.h.checkpoint("full.migrations")
        self._migrate_business_databases()
        self.h.checkpoint("full.bootstrap-services")
        self._start_bootstrap_services()
        self.h.checkpoint("full.catalogs")
        self._publish_release_images_and_catalogs()
        self.h.checkpoint("full.control-plane")
        self._start_identity_catalog_and_orchestrator()
        self.h.checkpoint("full.network-policy")
        self._apply_network_policy_and_probe()
        self.h.checkpoint("full.agents")
        self._enroll_and_start_agents()
        self.h.checkpoint("full.managed-a-network")
        self._probe_managed_a_network()
        self.h.checkpoint("full.external-providers")
        self._install_external_providers()
        self.h.checkpoint("full.provider-topology")
        topology_etag = self._create_and_apply_provider_topology()
        self.h.checkpoint("full.managed-a-services")
        topology_etag = self._install_managed_a_services(topology_etag)
        self.h.checkpoint("full.problem-artifact-gc")
        topology_etag = self._prove_problem_artifact_gc_failure_recovery()
        self.h.checkpoint("full.worker-install-compensation")
        compensation_evidence, topology_etag = (
            self._prove_worker_install_failure_compensation(topology_etag)
        )
        self.h.evidence["worker_install_failure_compensation"] = compensation_evidence
        self.h.checkpoint("full.worker-install")
        install = self._install_worker(topology_etag)
        self.h.checkpoint("full.worker-inspect")
        runtime = self._inspect_managed_worker(install)
        self.h.checkpoint("full.actual-flow")
        first_flow = self._run_actual_flow("first")
        self.h.checkpoint("full.worker-recovery")
        self.h.evidence["worker_recovery"] = self._prove_worker_recovery(first_flow)
        self.h.checkpoint("full.volume-isolation")
        self._collect_volume_isolation(runtime)
        self.h.checkpoint("full.binding-reconfigure")
        self._reconfigure_bindings_and_prove_in_place(runtime)
        self.h.checkpoint("full.generic-fixture")
        self._run_generic_store_topology_agent_fixture()
        # Enrollment only proves that the certificate exchange succeeded.  The
        # release evidence must end with a fresh authenticated runtime report
        # from each Agent after every managed workload and recovery exercise.
        final_a_health = self._wait_node_ready("node-a", timeout=60)
        final_b_health = self._wait_node_ready("node-b", timeout=60)
        self.a_agent_identity = {
            **self.a_agent_identity,
            "runtime_health": final_a_health,
            "runtime_health_sample": "final-agent-report",
        }
        self.agent_identity = {
            **self.agent_identity,
            "runtime_health": final_b_health,
            "runtime_health_sample": "final-agent-report",
        }
        self.h.evidence["final_provider_projection_integrity"] = (
            self._capture_provider_projection_integrity("final-applied")
        )
        store_agent = self.h.evidence.get("store_agent_evidence")
        if isinstance(store_agent, dict):
            store_agent["agent"] = copy.deepcopy(self.agent_identity)
        self.h.evidence["deployment_via_store_agent"] = True
        self.h.evidence["a_business_stack_mode"] = "production-service-contract-v2"
        self.h.evidence["a_agent_evidence"] = {
            **self.a_agent_identity,
            "engine_id": self.h.evidence["engines"]["a"]["engine_id"],
        }
        self.h.evidence["managed_a_deployments"] = self.managed_a_runtimes
        self.h.evidence["external_dependency_runtimes"] = self.external_dependency_runtimes
        self.h.evidence["worker_implementation"] = (
            "repository Rust judge-worker image (Agent-created)"
        )
        self.h.evidence["component_flow"] = first_flow

    # ------------------------------------------------------------------ setup

    def _preflight(self) -> None:
        for executable in ("git", "openssl", "cargo"):
            if shutil.which(executable) is None:
                raise FullGateError(f"full-components gate requires {executable}")
        self.commit = self.runner.run(
            ["git", "rev-parse", "HEAD"], cwd=self.repo, timeout=30
        ).stdout.strip().lower()
        if not re.fullmatch(r"[0-9a-f]{40}", self.commit):
            raise FullGateError("full-components build requires a real 40-character Git commit")

    def _create_networks(self) -> None:
        self.a.command("network", "create", "--subnet", self.A_NETWORK, self.a_network)
        self.b.command(
            "network", "create", "--subnet", self.B_SERVICE_NETWORK, self.b_service_network
        )
        self.b.command(
            "network", "create", "--subnet", self.B_AGENT_NETWORK, self.b_agent_network
        )

    def _generate_pki(self) -> None:
        pki = self.ca_cert.parent
        pki.mkdir(parents=True, exist_ok=True)
        self.runner.run(
            [
                "openssl",
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                self.ca_key,
                "-out",
                self.ca_cert,
                "-days",
                "3",
                "-subj",
                "/CN=OJOS cross-machine test CA",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
                "-addext",
                "keyUsage=critical,keyCertSign,cRLSign",
            ],
            timeout=60,
        )
        request = pki / "server.csr"
        extension = pki / "server.ext"
        extension.write_text(
            "\n".join(
                [
                    "basicConstraints=critical,CA:FALSE",
                    "keyUsage=critical,digitalSignature,keyEncipherment",
                    "extendedKeyUsage=serverAuth,clientAuth",
                    "subjectAltName="
                    + ",".join(
                        [
                            "DNS:engine-a",
                            "DNS:orchestrator-a",
                            "DNS:gateway-a",
                            "DNS:oidc-a",
                            "DNS:catalog-a",
                            "DNS:postgres-a",
                            "DNS:localhost",
                            "IP:127.0.0.1",
                            "IP:" + self.h.a_ip,
                        ]
                    ),
                ]
            )
            + "\n",
            encoding="utf-8",
        )
        self.runner.run(
            [
                "openssl",
                "req",
                "-new",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                self.server_key,
                "-out",
                request,
                "-subj",
                "/CN=engine-a",
            ],
            timeout=60,
        )
        self.runner.run(
            [
                "openssl",
                "x509",
                "-req",
                "-in",
                request,
                "-CA",
                self.ca_cert,
                "-CAkey",
                self.ca_key,
                "-CAcreateserial",
                "-out",
                self.server_cert,
                "-days",
                "3",
                "-sha256",
                "-extfile",
                extension,
            ],
            timeout=60,
        )
        self.runner.run(
            ["openssl", "genpkey", "-algorithm", "ED25519", "-out", self.workload_private],
            timeout=60,
        )
        self.runner.run(
            [
                "openssl",
                "pkey",
                "-in",
                self.workload_private,
                "-pubout",
                "-out",
                self.workload_public,
            ],
            timeout=60,
        )

    def _build_images(self) -> None:
        service_dockerfiles = {
            "auth": "services/auth-service/Dockerfile",
            "gateway": "services/gateway/Dockerfile",
            "problem": "services/problem-service/Dockerfile",
            "judge": "services/judge-api/Dockerfile",
            "storage": "services/storage-service/Dockerfile",
            "worker": "services/judge-worker/Dockerfile",
            "orchestrator": "services/orchestrator/backend/Dockerfile",
            "agent": "services/orchestrator/agent/Dockerfile",
        }
        # Register the target before building so cleanup also covers a build
        # that succeeds but whose retry-resolution checkpoint fails.
        self.h.root_images_created.append(self.fixture_image)
        self._host_docker_build(
            "build",
            "--tag",
            self.fixture_image,
            self.repo / "deploy/cross-machine/fixture",
            label="fixture",
            timeout=900,
        )
        for key, dockerfile in service_dockerfiles.items():
            tag = f"ojos/cross-machine-{key}:{self.h.run_id}"
            args: list[str | Path] = [
                "build",
                "--tag",
                tag,
                "--file",
                self.repo / dockerfile,
            ]
            if key in {"orchestrator", "agent"}:
                args.extend(["--build-arg", "GITHUB_SHA=" + self.commit])
            args.append(self.repo)
            self.h.root_images_created.append(tag)
            self._host_docker_build(*args, label=key, timeout=3600)
            self.images[key] = tag

        self.secure_fixture_image = self._derive_image(
            "secure-fixture",
            self.fixture_image,
            {
                "ca.pem": self.ca_cert.read_bytes(),
                "server.pem": self.server_cert.read_bytes(),
                "server.key": self.server_key.read_bytes(),
            },
            [
                "USER root",
                "COPY ca.pem server.pem server.key /opt/ojos-tls/",
                "RUN chmod 0644 /opt/ojos-tls/ca.pem /opt/ojos-tls/server.pem && chmod 0600 /opt/ojos-tls/server.key",
            ],
        )
        self.images["echo"] = self._derive_image(
            "generic-echo",
            self.fixture_image,
            {},
            [
                "USER 65532:65532",
                "HEALTHCHECK --interval=1s --timeout=2s --start-period=1s --retries=30 "
                "CMD python -c \"import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080/health', timeout=1)\"",
            ],
        )
        self.images["auth"] = self._derive_app_image(
            "auth-configured",
            self.images["auth"],
            self._auth_config(),
            "auth.yaml",
            private_key=self.workload_private,
            public_key=None,
        )
        self.images["gateway"] = self._derive_app_image(
            "gateway-configured",
            self.images["gateway"],
            self._gateway_config(),
            "gateway.yaml",
            private_key=None,
            public_key=self.workload_public,
        )
        self.images["problem"] = self._derive_app_image(
            "problem-configured", self.images["problem"], self._problem_config(), "problem.yaml"
        )
        self.images["judge"] = self._derive_app_image(
            "judge-configured",
            self.images["judge"],
            self._judge_config(),
            "judge.yaml",
            private_key=None,
            public_key=self.workload_public,
        )
        self.images["storage"] = self._derive_app_image(
            "storage-configured", self.images["storage"], self._storage_config(), "storage.yaml"
        )
        self.images["orchestrator"] = self._derive_orchestrator_image()

        external_images = {
            "postgres": os.environ.get("OJOS_CROSS_MACHINE_POSTGRES_IMAGE", "postgres:17"),
            "redis": os.environ.get("OJOS_CROSS_MACHINE_REDIS_IMAGE", "redis:8.8.0"),
            "minio": os.environ.get(
                "OJOS_CROSS_MACHINE_MINIO_IMAGE", "minio/minio:RELEASE.2025-09-07T16-13-09Z"
            ),
            "registry": os.environ.get("OJOS_CROSS_MACHINE_REGISTRY_IMAGE", "registry:2.8.3"),
        }
        for key, image in external_images.items():
            self._ensure_root_image(image)
            self.images[key] = image
        self.images["postgres"] = self._derive_postgres_image(self.images["postgres"])

        a_images, b_images = self._initial_image_bundle_sets()
        self._distribute_image_bundles(a_images, b_images)

    def _host_docker_build(
        self,
        *args: str | Path,
        label: str,
        timeout: float,
    ) -> Any:
        """Run a host Docker build with bounded, transient-only retries."""

        if not args or str(args[0]) != "build":
            raise FullGateError("host Docker build retry wrapper requires a build argv")
        for attempt in range(1, DOCKER_BUILD_MAX_ATTEMPTS + 1):
            try:
                result = self.root.command(*args, timeout=timeout)
            except Exception as exc:
                failure_kind = _transient_docker_build_failure_kind(exc)
                if failure_kind is None:
                    raise
                exhausted = attempt == DOCKER_BUILD_MAX_ATTEMPTS
                delay = None if exhausted else DOCKER_BUILD_RETRY_DELAYS_SECONDS[attempt - 1]
                self._record_docker_build_retry(
                    label=label,
                    attempt=attempt,
                    outcome="EXHAUSTED" if exhausted else "RETRYING",
                    failure_kind=failure_kind,
                    error=exc,
                    retry_after_seconds=delay,
                )
                # Persist the failed attempt before waiting or returning a
                # terminal error. LiveGate.checkpoint uses atomic_json.
                self.h.checkpoint("full.docker-build-retry")
                if exhausted:
                    raise
                assert delay is not None
                time.sleep(delay)
                continue

            if attempt > 1:
                self._docker_build_retry_events().append(
                    {
                        "sequence": len(self._docker_build_retry_events()) + 1,
                        "build": label,
                        "attempt": attempt,
                        "max_attempts": DOCKER_BUILD_MAX_ATTEMPTS,
                        "outcome": "SUCCEEDED",
                    }
                )
                self.h.checkpoint("full.docker-build-retry-resolved")
            return result
        raise AssertionError("bounded Docker build retry loop did not terminate")

    def _ensure_root_image(self, image: str) -> None:
        if self.root.command("image", "inspect", image, check=False).returncode == 0:
            return
        self._host_docker_pull(image, timeout=600)

    def _host_docker_pull(self, image: str, *, timeout: float) -> Any:
        """Pull one host image with the build lane's strict retry semantics."""

        if not image.strip():
            raise FullGateError("host Docker pull retry wrapper requires an image")
        for attempt in range(1, DOCKER_BUILD_MAX_ATTEMPTS + 1):
            try:
                result = self.root.command("pull", image, timeout=timeout)
            except Exception as exc:
                failure_kind = _transient_docker_build_failure_kind(exc)
                if failure_kind is None:
                    raise
                exhausted = attempt == DOCKER_BUILD_MAX_ATTEMPTS
                delay = None if exhausted else DOCKER_BUILD_RETRY_DELAYS_SECONDS[attempt - 1]
                self._record_docker_pull_retry(
                    image=image,
                    attempt=attempt,
                    outcome="EXHAUSTED" if exhausted else "RETRYING",
                    failure_kind=failure_kind,
                    error=exc,
                    retry_after_seconds=delay,
                )
                self.h.checkpoint("full.docker-pull-retry")
                if exhausted:
                    raise
                assert delay is not None
                time.sleep(delay)
                continue

            if attempt > 1:
                events = self._docker_pull_retry_events()
                events.append(
                    {
                        "sequence": len(events) + 1,
                        "image": image,
                        "attempt": attempt,
                        "max_attempts": DOCKER_BUILD_MAX_ATTEMPTS,
                        "outcome": "SUCCEEDED",
                    }
                )
                self.h.checkpoint("full.docker-pull-retry-resolved")
            return result
        raise AssertionError("bounded Docker pull retry loop did not terminate")

    def _docker_pull_retry_events(self) -> list[dict[str, Any]]:
        events = self.h.evidence.setdefault("docker_pull_retry_events", [])
        if not isinstance(events, list):
            raise FullGateError("docker_pull_retry_events evidence must be an array")
        return events

    def _record_docker_pull_retry(
        self,
        *,
        image: str,
        attempt: int,
        outcome: str,
        failure_kind: str,
        error: BaseException,
        retry_after_seconds: float | None,
    ) -> None:
        events = self._docker_pull_retry_events()
        events.append(
            {
                "sequence": len(events) + 1,
                "image": image,
                "attempt": attempt,
                "max_attempts": DOCKER_BUILD_MAX_ATTEMPTS,
                "outcome": outcome,
                "failure_kind": failure_kind,
                "error_fingerprint": _sha256(str(error)),
                "retry_after_seconds": retry_after_seconds,
            }
        )

    def _docker_build_retry_events(self) -> list[dict[str, Any]]:
        events = self.h.evidence.setdefault("docker_build_retry_events", [])
        if not isinstance(events, list):
            raise FullGateError("docker_build_retry_events evidence must be an array")
        return events

    def _record_docker_build_retry(
        self,
        *,
        label: str,
        attempt: int,
        outcome: str,
        failure_kind: str,
        error: BaseException,
        retry_after_seconds: float | None,
    ) -> None:
        events = self._docker_build_retry_events()
        events.append(
            {
                "sequence": len(events) + 1,
                "build": label,
                "attempt": attempt,
                "max_attempts": DOCKER_BUILD_MAX_ATTEMPTS,
                "outcome": outcome,
                "failure_kind": failure_kind,
                "error_fingerprint": _sha256(str(error)),
                "retry_after_seconds": retry_after_seconds,
            }
        )

    def _initial_image_bundle_sets(self) -> tuple[list[str], list[str]]:
        """Return the images needed before A's Registry is available.

        B intentionally receives neither Worker nor generic consumer images;
        its Agent must pull those digest-pinned releases from A's Registry.
        """

        a_keys = (
            "auth",
            "gateway",
            "problem",
            "judge",
            "storage",
            "worker",
            "orchestrator",
            "agent",
            "echo",
            "postgres",
            "redis",
            "minio",
            "registry",
        )
        missing = [key for key in a_keys if not self.images.get(key)]
        if missing or not self.fixture_image or not self.secure_fixture_image:
            raise FullGateError(
                "cannot distribute incomplete full-component image set: "
                + ", ".join(missing or ["fixture images"])
            )

        # A owns all business services, both Agents' release source, and the
        # Registry. Catalog is built directly in A after RepoDigests exist.
        a_images = [
            self.fixture_image,
            self.secure_fixture_image,
            *(self.images[key] for key in a_keys),
        ]
        # B owns only its bootstrap/probe fixture and Agent. Store-managed
        # Worker and generic consumer images must arrive through Registry pull.
        b_images = [self.secure_fixture_image, self.images["agent"]]
        return a_images, b_images

    def _distribute_image_bundles(
        self, a_images: Sequence[str], b_images: Sequence[str]
    ) -> None:
        """Load one target-specific multi-image archive into each nested Engine.

        Docker's archive format stores a shared layer once even when several
        image tags reference it.  Keeping A and B in separate bundles avoids
        importing the entire business stack into B while reducing each vfs
        Engine to one expensive ``docker image load`` operation.
        """

        a_result = self._transfer_image_bundle("engine-a", a_images, self.a)
        b_result = self._transfer_image_bundle("engine-b", b_images, self.b)
        # Publish the distribution claim only after both loads and their image
        # inspections succeed.  checkpoint() writes evidence with atomic_json.
        self.h.evidence["image_distribution"] = {
            "strategy": "target-specific-multi-image-bundles",
            "host_save_invocations": 2,
            "engine_load_invocations": {"a": 1, "b": 1},
            "save_timeout_seconds": self.image_bundle_save_timeout,
            "load_timeout_seconds": self.image_bundle_load_timeout,
            "a": a_result,
            "b": b_result,
        }
        self.h.checkpoint("full.images-distributed")

    def _transfer_image_bundle(
        self, name: str, images: Sequence[str], engine: Any
    ) -> dict[str, Any]:
        unique_images = list(dict.fromkeys(images))
        if not unique_images or any(not image for image in unique_images):
            raise FullGateError(f"{name} image bundle cannot be empty")

        bundle_root = self.tmp / "image-bundles"
        bundle_root.mkdir(parents=True, exist_ok=True)
        archive = bundle_root / f"{name}.tar"
        partial_archive = archive.with_suffix(archive.suffix + ".partial")
        if archive.exists() or partial_archive.exists():
            raise FullGateError(f"refusing to overwrite image bundle {archive}")

        archive_bytes = 0
        primary_error: BaseException | None = None
        primary_traceback: Any = None
        try:
            self.root.command(
                "image",
                "save",
                "--output",
                partial_archive,
                *unique_images,
                timeout=self.image_bundle_save_timeout,
            )
            if not partial_archive.is_file() or partial_archive.stat().st_size == 0:
                raise FullGateError(f"host daemon did not export the {name} image bundle")
            # A completed archive becomes visible under its load path atomically.
            # The files live in the same directory/volume, including on Windows.
            os.replace(partial_archive, archive)
            archive_bytes = archive.stat().st_size
            engine.command(
                "image",
                "load",
                "--input",
                archive,
                timeout=self.image_bundle_load_timeout,
            )
            # One inspect command verifies that a successful load exposed every
            # requested tag; it does not repeat the expensive archive import.
            engine.command("image", "inspect", *unique_images, timeout=300)
        except BaseException as exc:
            primary_error = exc
            primary_traceback = exc.__traceback__
        finally:
            # Bundles can be many gigabytes.  Remove each as soon as its target
            # Engine has consumed it.  Include the partial path so a timed-out
            # save cannot leave a multi-gigabyte file behind.
            cleanup_error = self._cleanup_image_bundle_files(
                name, (partial_archive, archive)
            )

        if primary_error is not None:
            if cleanup_error is not None:
                raise FullGateError(
                    f"{name} image bundle transfer failed: {primary_error}; "
                    f"temporary archive cleanup also failed: {cleanup_error}"
                ) from primary_error
            raise primary_error.with_traceback(primary_traceback)
        if cleanup_error is not None:
            raise FullGateError(
                f"{name} image bundle loaded but temporary archive cleanup failed: "
                f"{cleanup_error}"
            )

        return {
            "image_count": len(unique_images),
            "archive_bytes": archive_bytes,
            "images": unique_images,
            "load_invocations": 1,
            "verified": True,
        }

    @staticmethod
    def _cleanup_image_bundle_files(
        name: str, paths: Sequence[Path]
    ) -> str | None:
        failures: list[str] = []
        for path in paths:
            for attempt in range(5):
                try:
                    path.unlink(missing_ok=True)
                    break
                except OSError as exc:
                    if attempt == 4:
                        error_code = getattr(exc, "winerror", None) or exc.errno
                        failures.append(
                            f"{name}/{path.name}: {type(exc).__name__} "
                            f"(error {error_code})"
                        )
                    else:
                        # Windows virus scanners and Docker/credential helpers
                        # can briefly retain a handle after the CLI exits.
                        time.sleep(0.05 * (attempt + 1))
        return "; ".join(failures) or None

    def _derive_image(
        self,
        name: str,
        base: str,
        files: Mapping[str, bytes | str],
        docker_lines: Sequence[str],
    ) -> str:
        context = self.tmp / "derived" / name
        context.mkdir(parents=True, exist_ok=False)
        for filename, contents in files.items():
            path = context / filename
            if isinstance(contents, bytes):
                path.write_bytes(contents)
            else:
                path.write_text(contents, encoding="utf-8")
        (context / "Dockerfile").write_text(
            "FROM " + base + "\n" + "\n".join(docker_lines) + "\n", encoding="utf-8"
        )
        tag = f"ojos/cross-machine-{name}:{self.h.run_id}"
        self.h.root_images_created.append(tag)
        self._host_docker_build(
            "build", "--tag", tag, context, label=name, timeout=900
        )
        return tag

    def _derive_app_image(
        self,
        name: str,
        base: str,
        config: str,
        config_name: str,
        *,
        private_key: Path | None = None,
        public_key: Path | None = None,
    ) -> str:
        files: dict[str, bytes | str] = {
            config_name: config,
            "ca.pem": self.ca_cert.read_bytes(),
        }
        lines = [
            "USER root",
            f"COPY {config_name} /opt/ojos-config/{config_name}",
            "COPY ca.pem /usr/local/share/ca-certificates/ojos-cross-machine.crt",
            "RUN update-ca-certificates",
        ]
        if private_key is not None:
            files["workload-private.pem"] = private_key.read_bytes()
            lines.extend(
                [
                    "COPY workload-private.pem /opt/ojos-config/workload-private.pem",
                    "RUN chmod 0600 /opt/ojos-config/workload-private.pem",
                ]
            )
        if public_key is not None:
            files["workload-public.pem"] = public_key.read_bytes()
            lines.append("COPY workload-public.pem /opt/ojos-config/workload-public.pem")
        return self._derive_image(name, base, files, lines)

    def _derive_orchestrator_image(self) -> str:
        files = {
            "ca.pem": self.ca_cert.read_bytes(),
            "server.pem": self.server_cert.read_bytes(),
            "server.key": self.server_key.read_bytes(),
            "node-ca.pem": self.ca_cert.read_bytes(),
            "node-ca.key": self.ca_key.read_bytes(),
        }
        return self._derive_image(
            "orchestrator-production",
            self.images["orchestrator"],
            files,
            [
                "USER root",
                "COPY ca.pem /usr/local/share/ca-certificates/ojos-cross-machine.crt",
                "COPY ca.pem server.pem server.key node-ca.pem node-ca.key /opt/ojos-pki/",
                "RUN update-ca-certificates && chmod 0644 /opt/ojos-pki/*.pem && chmod 0600 /opt/ojos-pki/*.key && chown -R 10003:10003 /opt/ojos-pki",
                "USER 10003:10003",
            ],
        )

    def _derive_postgres_image(self, base: str) -> str:
        return self._derive_image(
            "postgres-tls",
            base,
            {
                "server.pem": self.server_cert.read_bytes(),
                "server.key": self.server_key.read_bytes(),
                "ca.pem": self.ca_cert.read_bytes(),
                "pg_hba.conf": (
                    "local all all trust\n"
                    "hostnossl all all 0.0.0.0/0 reject\n"
                    "hostssl all all 0.0.0.0/0 scram-sha-256\n"
                    "hostnossl all all ::/0 reject\n"
                    "hostssl all all ::/0 scram-sha-256\n"
                ),
            },
            [
                "USER root",
                "COPY server.pem server.key ca.pem pg_hba.conf /opt/ojos-pg-tls/",
                "RUN chown -R postgres:postgres /opt/ojos-pg-tls && chmod 0644 /opt/ojos-pg-tls/server.pem /opt/ojos-pg-tls/ca.pem /opt/ojos-pg-tls/pg_hba.conf && chmod 0600 /opt/ojos-pg-tls/server.key",
                "USER postgres",
            ],
        )

    # ------------------------------------------------------------- A services

    def _start_infrastructure(self) -> None:
        self.a.command(
            "run", "-d", "--name", "registry-a", "--network", self.a_network,
            "--network-alias", "registry-a", "--ip", self.A_IPS["registry"],
            "--publish", "5000:5000", self.images["registry"], timeout=180,
        )
        self.a.command(
            "run", "-d", "--name", "redis-a", "--network", self.a_network,
            "--network-alias", "redis-a", "--ip", self.A_IPS["redis"],
            "--publish", "6379:6379", self.images["redis"],
            "redis-server", "--appendonly", "yes", timeout=180,
        )
        self.a.command(
            "run", "-d", "--name", "minio-a", "--network", self.a_network,
            "--network-alias", "minio-a", "--ip", self.A_IPS["minio"],
            "--publish", "9000:9000", "--env", "MINIO_ROOT_USER=" + self.minio_access,
            "--env", "MINIO_ROOT_PASSWORD=" + self.minio_secret,
            self.images["minio"], "server", "/data", "--console-address", ":9001", timeout=180,
        )
        self.a.command(
            "run", "-d", "--name", "postgres-a", "--network", self.a_network,
            "--network-alias", "postgres-a", "--ip", self.A_IPS["postgres"],
            "--publish", "5432:5432", "--env", "POSTGRES_PASSWORD=" + self.postgres_password,
            "--env", "POSTGRES_DB=postgres", self.images["postgres"],
            "postgres", "-c", "ssl=on", "-c", "ssl_cert_file=/opt/ojos-pg-tls/server.pem",
            "-c", "ssl_key_file=/opt/ojos-pg-tls/server.key", "-c", "ssl_ca_file=/opt/ojos-pg-tls/ca.pem",
            "-c", "hba_file=/opt/ojos-pg-tls/pg_hba.conf",
            timeout=180,
        )
        self._wait_postgres()
        self._wait_redis()
        self._wait_minio()
        for database in ("ojos_orchestrator", "ojos_auth", "ojos_problem", "ojos_judge"):
            self.a.command("exec", "postgres-a", "createdb", "-U", "postgres", database)

    def _wait_postgres(self) -> None:
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            result = self.a.command(
                "exec", "postgres-a", "pg_isready", "-U", "postgres", timeout=5, check=False
            )
            if result.returncode == 0:
                return
            time.sleep(1)
        raise FullGateError("PostgreSQL A did not become ready")

    def _wait_redis(self) -> None:
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            result = self.a.command(
                "exec", "redis-a", "redis-cli", "PING", timeout=5, check=False
            )
            if result.returncode == 0 and result.stdout.strip() == "PONG":
                return
            time.sleep(1)
        raise FullGateError("Redis A did not become ready")

    def _wait_minio(self) -> None:
        deadline = time.monotonic() + 120
        last = ""
        while time.monotonic() < deadline:
            result = self.root.command(
                "exec",
                self.h.a_name,
                "wget",
                "-qO-",
                # This command runs in the outer DinD container, not in an
                # inner workload container.  Inner Docker DNS names such as
                # ``minio-a`` are therefore not resolvable here; the inner
                # bridge address is reachable from the DinD host namespace.
                f"http://{self.A_IPS['minio']}:9000/minio/health/ready",
                timeout=5,
                check=False,
            )
            if result.returncode == 0:
                return
            last = result.stderr or result.stdout
            time.sleep(1)
        raise FullGateError(f"MinIO A did not become ready: {last[-1000:]}")

    def _migrate_business_databases(self) -> None:
        migrations = {
            "ojos_auth": self.repo / "services/auth-service/migrations",
            "ojos_problem": self.repo / "services/problem-service/migrations",
            "ojos_judge": self.repo / "services/judge-api/migrations",
        }
        for database, directory in migrations.items():
            for index, migration in enumerate(sorted(directory.glob("*.up.sql"))):
                remote = f"/tmp/{database}-{index:03d}.sql"
                self.a.command("cp", migration, "postgres-a:" + remote, timeout=30)
                self.a.command(
                    "exec", "postgres-a", "psql", "-v", "ON_ERROR_STOP=1", "-U", "postgres",
                    "-d", database, "-f", remote, timeout=120,
                )

    def _start_bootstrap_services(self) -> None:
        self._require_distinct_platform_credentials()
        redis_url = "redis://redis-a:6379/0"
        db = lambda name: (
            f"postgresql://postgres:{self.postgres_password}@{self.h.a_ip}:5432/{name}"
            "?sslmode=verify-full&sslrootcert=/usr/local/share/ca-certificates/ojos-cross-machine.crt"
        )
        bootstrap_file = self._materialize_auth_bootstrap_secret_file()
        self._run_a_service(
            "auth-a", "auth", 8081,
            [
                # Auth and Gateway form the bootstrap/control plane. Business
                # workloads are installed later by the enrolled node-a Agent.
                "--mount",
                (
                    f"type=bind,source={AUTH_BOOTSTRAP_SECRET_HOST_FILE},"
                    f"target={AUTH_BOOTSTRAP_SECRET_CONTAINER_FILE},readonly"
                ),
                "--env", "OJOS_ENVIRONMENT=production",
                "--env", "OJOS_PLATFORM_BOOTSTRAP=1",
                "--env", "AUTH_DATABASE_URL=" + db("ojos_auth"),
                "--env", "JWT_SECRET=" + self.jwt_secret,
                "--env", "AUTH_INTERNAL_TOKEN=" + self.auth_internal_token,
                "--env", "OJOS_WORKLOAD_PRIVATE_KEY_FILE=/opt/ojos-config/workload-private.pem",
                "--env", "ORCHESTRATOR_AUTH_WORKLOAD_TOKEN=" + self.workload_issuer_token,
                "--env", "OJOS_WORKLOAD_KEY_ID=" + WORKLOAD_KEY_ID,
                "--env", "OJOS_WORKLOAD_ISSUER=" + WORKLOAD_ISSUER,
                "--env", "OJOS_WORKLOAD_AUDIENCE=" + WORKLOAD_AUDIENCE,
                "--env", "ORCHESTRATOR_PLATFORM_ORIGIN=https://orchestrator-a:8090",
                "--env", "ORCHESTRATOR_INTERNAL_TOKEN=" + self.internal_token,
                "--env", "ORCHESTRATOR_AUTH_ADMIN_TOKEN=" + self.auth_management_token,
                "--env",
                (
                    "ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN="
                    + self.auth_contribution_ack_token
                ),
                "--env",
                "AUTH_ADMIN_BOOTSTRAP_SECRET_FILE="
                + AUTH_BOOTSTRAP_SECRET_CONTAINER_FILE,
            ],
            ["./auth", "-f", "/opt/ojos-config/auth.yaml"],
        )
        self.auth_bootstrap_delivery_evidence = (
            self._inspect_auth_bootstrap_secret_delivery(bootstrap_file)
        )
        self._run_a_service(
            "gateway-a", "gateway", 8080,
            [
                "--env", "OJOS_ENVIRONMENT=production",
                "--env", "OJOS_PLATFORM_BOOTSTRAP=1",
                "--env", "REDIS_URL=" + redis_url,
                "--env", "JWT_SECRET=" + self.jwt_secret,
                "--env", "AUTH_SERVICE_ENDPOINT=http://auth-a:8081",
                "--env", "ORCHESTRATOR_PLATFORM_ORIGIN=https://orchestrator-a:8090",
                "--env", "ORCHESTRATOR_INTERNAL_TOKEN=" + self.internal_token,
                "--env", "ORCHESTRATOR_GATEWAY_ADMIN_TOKEN=" + self.gateway_management_token,
                "--env",
                (
                    "ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN="
                    + self.gateway_contribution_ack_token
                ),
                "--env", "ORCHESTRATOR_NODE_ID=node-a",
                "--env", "OJOS_WORKLOAD_PUBLIC_KEY_FILE=/opt/ojos-config/workload-public.pem",
                "--env", "OJOS_WORKLOAD_KEY_ID=" + WORKLOAD_KEY_ID,
                "--env", "OJOS_WORKLOAD_ISSUER=" + WORKLOAD_ISSUER,
                "--env", "OJOS_WORKLOAD_AUDIENCE=" + WORKLOAD_AUDIENCE,
            ],
            ["./gateway", "-f", "/opt/ojos-config/gateway.yaml"],
        )
        self._run_a_service(
            "echo-provider-a",
            "echo",
            8080,
            [],
            ["provider", "--port", "8080"],
        )
        self.a.command(
            "run", "-d", "--name", "storage-head-provenance-miss-a",
            "--network", self.a_network,
            "--network-alias", "storage-head-provenance-miss-a",
            "--ip", self.A_IPS["storage_head_miss"],
            self.images["echo"],
            "storage-head-provenance-miss", "--port", "8080",
            timeout=180,
        )
        for name, port in (
            ("auth-a", 8081),
            ("gateway-a", 8080),
            ("echo-provider-a", 8080),
            ("storage-head-provenance-miss-a", 8080),
        ):
            self._wait_a_http(name, port, "/health")
        self.a.command(
            "run", "-d", "--name", "gateway-tls-a", "--network", self.a_network,
            "--network-alias", "gateway-tls-a", "--ip", self.A_IPS["gateway_tls"],
            "--publish", "8443:8443", "--env", "UPSTREAM_URL=http://gateway-a:8080",
            "--env", "TLS_CERT_FILE=/opt/ojos-tls/server.pem",
            "--env", "TLS_KEY_FILE=/opt/ojos-tls/server.key",
            "--env",
            'CAPTURE_PATHS_JSON=["/internal/apis/judge.worker.control/*",'
            '"/internal/apis/storage.object.get/*"]',
            self.secure_fixture_image, "tls-proxy", "--port", "8443",
        )

    def _require_distinct_platform_credentials(self) -> None:
        credentials = {
            "jwt": self.jwt_secret,
            "orchestrator-internal": self.internal_token,
            "auth-internal": self.auth_internal_token,
            "auth-workload": self.workload_issuer_token,
            "auth-management": self.auth_management_token,
            "gateway-management": self.gateway_management_token,
            "auth-contribution-ack": self.auth_contribution_ack_token,
            "gateway-contribution-ack": self.gateway_contribution_ack_token,
            "auth-admin-bootstrap": self.auth_bootstrap_secret,
        }
        if any(len(value) < 32 for value in credentials.values()) or len(
            set(credentials.values())
        ) != len(credentials):
            raise FullGateError(
                "platform bootstrap requires distinct credentials of at least 32 bytes"
            )

    def _run_a_service(
        self,
        name: str,
        image_key: str,
        port: int,
        extra: Sequence[str],
        command: Sequence[str],
    ) -> None:
        argv: list[str] = [
            "run", "-d", "--name", name, "--network", self.a_network,
            "--network-alias", name, "--ip", self.A_IPS[image_key],
        ]
        argv.extend(extra)
        argv.append(self.images[image_key])
        argv.extend(command)
        self.a.command(*argv, timeout=180)

    def _materialize_auth_bootstrap_secret_file(self) -> dict[str, Any]:
        """Create the one-time Auth credential without putting it in argv/env."""

        outer_engine = self.h.a_name
        self.root.command(
            "exec",
            outer_engine,
            "mkdir",
            "-p",
            AUTH_BOOTSTRAP_SECRET_HOST_DIRECTORY,
            timeout=30,
        )
        self.root.command(
            "exec",
            outer_engine,
            "chown",
            f"{STANDARD_WORKLOAD_UID}:{STANDARD_WORKLOAD_GID}",
            AUTH_BOOTSTRAP_SECRET_HOST_DIRECTORY,
            timeout=30,
        )
        self.root.command(
            "exec",
            outer_engine,
            "chmod",
            "0700",
            AUTH_BOOTSTRAP_SECRET_HOST_DIRECTORY,
            timeout=30,
        )
        # docker exec stdin is not retained in Completed.argv or evidence. dd
        # writes no file content to stdout, unlike tee.
        self.root.command(
            "exec",
            "--interactive",
            outer_engine,
            "dd",
            f"of={AUTH_BOOTSTRAP_SECRET_HOST_FILE}",
            input_data=(self.auth_bootstrap_secret + "\n").encode("ascii"),
            timeout=30,
        )
        self.root.command(
            "exec",
            outer_engine,
            "chown",
            f"{STANDARD_WORKLOAD_UID}:{STANDARD_WORKLOAD_GID}",
            AUTH_BOOTSTRAP_SECRET_HOST_FILE,
            timeout=30,
        )
        self.root.command(
            "exec",
            outer_engine,
            "chmod",
            "0600",
            AUTH_BOOTSTRAP_SECRET_HOST_FILE,
            timeout=30,
        )
        file_stat = self.root.command(
            "exec",
            outer_engine,
            "stat",
            "-c",
            "%u %g %a %F",
            AUTH_BOOTSTRAP_SECRET_HOST_FILE,
            timeout=30,
        ).stdout.strip().split(maxsplit=3)
        directory_stat = self.root.command(
            "exec",
            outer_engine,
            "stat",
            "-c",
            "%u %g %a %F",
            AUTH_BOOTSTRAP_SECRET_HOST_DIRECTORY,
            timeout=30,
        ).stdout.strip().split(maxsplit=3)
        if file_stat != ["65532", "65532", "600", "regular file"] or directory_stat != [
            "65532",
            "65532",
            "700",
            "directory",
        ]:
            raise FullGateError(
                "Auth bootstrap credential is not a private workload-owned host file"
            )
        return {
            "host_file_path": AUTH_BOOTSTRAP_SECRET_HOST_FILE,
            "host_file_uid": STANDARD_WORKLOAD_UID,
            "host_file_gid": STANDARD_WORKLOAD_GID,
            "host_file_mode": "0600",
            "host_directory_mode": "0700",
            "write_transport": "docker-exec-stdin",
        }

    def _inspect_auth_bootstrap_secret_delivery(
        self, host_file: Mapping[str, Any]
    ) -> dict[str, Any]:
        inspected = json.loads(self.a.command("inspect", "auth-a", timeout=30).stdout)
        if not isinstance(inspected, list) or len(inspected) != 1:
            raise FullGateError("Auth bootstrap secret mount inspect is not singular")
        container = inspected[0]
        environment = [str(value) for value in container.get("Config", {}).get("Env", [])]
        mount = next(
            (
                value
                for value in container.get("Mounts", [])
                if value.get("Destination") == AUTH_BOOTSTRAP_SECRET_CONTAINER_FILE
            ),
            None,
        )
        container_stat = self.a.command(
            "exec",
            "auth-a",
            "stat",
            "-c",
            "%u %g %a %F",
            AUTH_BOOTSTRAP_SECRET_CONTAINER_FILE,
            timeout=30,
        ).stdout.strip().split(maxsplit=3)
        expected_file_env = (
            "AUTH_ADMIN_BOOTSTRAP_SECRET_FILE="
            + AUTH_BOOTSTRAP_SECRET_CONTAINER_FILE
        )
        if (
            environment.count(expected_file_env) != 1
            or any(value.startswith("AUTH_ADMIN_BOOTSTRAP_SECRET=") for value in environment)
            or not isinstance(mount, Mapping)
            or mount.get("Type") != "bind"
            or mount.get("Source") != AUTH_BOOTSTRAP_SECRET_HOST_FILE
            or mount.get("Destination") != AUTH_BOOTSTRAP_SECRET_CONTAINER_FILE
            or mount.get("RW") is not False
            or container_stat != ["65532", "65532", "600", "regular file"]
        ):
            raise FullGateError(
                "Auth bootstrap credential is not delivered by the exact read-only private file boundary"
            )
        return {
            **dict(host_file),
            "delivery_mode": "host-private-file-read-only-bind",
            "container_file_path": AUTH_BOOTSTRAP_SECRET_CONTAINER_FILE,
            "mount_type": "bind",
            "mount_read_only": True,
            "inline_environment_absent": True,
            "container_file_uid": STANDARD_WORKLOAD_UID,
            "container_file_gid": STANDARD_WORKLOAD_GID,
            "container_file_mode": "0600",
            "cleanup_scope": "run-scoped-outer-dind-container-teardown",
            "production_one_time_unmount_proven": False,
        }

    def _wait_a_http(self, host: str, port: int, path: str, timeout: float = 120) -> None:
        inner_ip = self.a.command(
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            host,
            timeout=30,
        ).stdout.strip()
        if not inner_ip:
            raise FullGateError(f"{host} has no inner Docker network address")
        deadline = time.monotonic() + timeout
        last = ""
        while time.monotonic() < deadline:
            result = self.root.command(
                "exec",
                self.h.a_name,
                "wget",
                "-qO-",
                f"http://{inner_ip}:{port}{path}",
                timeout=5, check=False,
            )
            if result.returncode == 0:
                return
            last = result.stderr or result.stdout
            time.sleep(1)
        raise FullGateError(f"{host}:{port}{path} did not become ready: {last[-1000:]}")

    # ----------------------------------------------------------- catalog/control

    def _publish_release_images_and_catalogs(self) -> None:
        for key in (
            "problem",
            "judge",
            "storage",
            "worker",
            "echo",
            "auth",
            "postgres",
            "redis",
            "minio",
        ):
            local = f"127.0.0.1:5000/ojos/{key}:{self.h.run_id}"
            self.a.command("tag", self.images[key], local)
            self.a.command("push", local, timeout=900)
            values = json.loads(
                self.a.command("image", "inspect", "--format", "{{json .RepoDigests}}", local).stdout
            )
            digest = next(
                (item.split("@", 1)[1] for item in values if item.startswith("127.0.0.1:5000/")),
                "",
            )
            if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
                raise FullGateError(f"registry push for {key} did not produce a RepoDigest")
            self.oci[key] = f"engine-a:5000/ojos/{key}@{digest}"

        signing_key = self.tmp / "catalog-signing-key.base64"
        signing_key.write_text(base64.b64encode(bytes([37]) * 32).decode("ascii") + "\n", encoding="ascii")
        derived_manifests = self.tmp / "derived" / "release-manifests"
        derived_manifests.mkdir(parents=True, exist_ok=False)
        dependency_contracts = {
            "postgresql": _preprovisioned_dependency_contract(
                service_id="postgresql",
                version="17.0.0",
                service_type="database",
                protocol="postgres",
                port=5432,
                health_path="",
            ),
            "redis": _preprovisioned_dependency_contract(
                service_id="redis",
                version="8.8.0",
                service_type="cache",
                protocol="redis",
                port=6379,
                health_path="",
            ),
            "minio": _preprovisioned_dependency_contract(
                service_id="minio",
                version="2025.9.7",
                service_type="storage",
                protocol="http",
                port=9000,
                health_path="/minio/health/ready",
            ),
        }
        dependency_paths: dict[str, Path] = {}
        for service_id, contract in dependency_contracts.items():
            destination = derived_manifests / f"{service_id}.release.json"
            destination.write_text(
                json.dumps(contract, ensure_ascii=False, indent=2) + "\n",
                encoding="utf-8",
            )
            dependency_paths[service_id] = destination

        storage_manifest = (self.repo / "services/storage-service/release.yaml").read_text(
            encoding="utf-8"
        )
        storage_canary_manifest, replacements = re.subn(
            r"(?m)^version:\s*0\.1\.0\s*$",
            "version: 0.1.1",
            storage_manifest,
            count=1,
        )
        if replacements != 1:
            raise FullGateError(
                "storage-service canary Catalog requires exactly one 0.1.0 release version"
            )
        storage_canary_path = derived_manifests / "storage-service-canary.release.yaml"
        storage_canary_path.write_text(storage_canary_manifest, encoding="utf-8")
        releases = [
            (
                self.repo / "services/problem-service/release.yaml",
                self.oci["problem"],
            ),
            (
                self.repo / "services/judge-api/release.yaml",
                self.oci["judge"],
            ),
            (
                self.repo / "services/storage-service/release.yaml",
                self.oci["storage"],
            ),
            (
                storage_canary_path,
                self.oci["storage"],
            ),
            (
                self.repo / "services/judge-worker/release.yaml",
                self.oci["worker"],
            ),
            (
                self.repo / "deploy/cross-machine/fixture/contracts/echo-provider.release.yaml",
                self.oci["echo"],
            ),
            (
                self.repo
                / "deploy/cross-machine/fixture/contracts/storage-head-provenance-miss-provider.release.yaml",
                self.oci["echo"],
            ),
            (
                self.repo / "deploy/cross-machine/fixture/contracts/echo-consumer.release.yaml",
                self.oci["echo"],
            ),
            (
                self.repo
                / "deploy/cross-machine/fixture/contracts/auth-permission-provider.release.yaml",
                self.oci["auth"],
            ),
            (dependency_paths["postgresql"], self.oci["postgres"]),
            (dependency_paths["redis"], self.oci["redis"]),
            (dependency_paths["minio"], self.oci["minio"]),
        ]
        catalogs_root = self.tmp / "catalogs"
        catalogs_root.mkdir()
        output = catalogs_root / "service-contracts"
        root_manifest, root_image = releases[0]
        command: list[str | Path] = [
            "cargo", "run", "--locked", "--release", "-p", "orchestrator-manager",
            "--example", "generate_service_contract_catalog", "--",
            "--output", output,
            "--release-manifest", root_manifest,
            "--signing-key-file", signing_key,
            "--public-base-url", "https://catalog-a:9444/service-contracts",
            "--oci-image", root_image,
            "--key-id", "cross-machine-release-key",
            "--catalog-id", "cross-machine-services",
            "--target-os", "linux", "--target-arch", "x86_64",
        ]
        for manifest, image in releases[1:]:
            command.extend(
                [
                    "--additional-release-manifest",
                    manifest,
                    "--additional-oci-image",
                    image,
                ]
            )
        self.runner.run(command, cwd=self.repo, timeout=1800)
        expected_contracts = {
            ("problem-service", "0.1.0"): {
                "dependencies": ["postgresql"],
                "oci_image": self.oci["problem"],
            },
            ("judge-api", "0.1.0"): {
                "dependencies": ["postgresql", "redis"],
                "oci_image": self.oci["judge"],
            },
            ("storage-service", "0.1.0"): {
                "dependencies": ["minio"],
                "oci_image": self.oci["storage"],
            },
            ("storage-service", "0.1.1"): {
                "dependencies": ["minio"],
                "oci_image": self.oci["storage"],
            },
            ("judge-worker", "0.1.0"): {
                "dependencies": [],
                "oci_image": self.oci["worker"],
            },
            ("contract-echo-provider", "1.0.0"): {
                "dependencies": [],
                "oci_image": self.oci["echo"],
            },
            ("storage-head-provenance-miss-provider", "1.0.0"): {
                "dependencies": [],
                "oci_image": self.oci["echo"],
            },
            ("contract-echo-consumer", "1.0.0"): {
                "dependencies": [],
                "oci_image": self.oci["echo"],
            },
            ("auth-service", "0.1.0"): {
                "dependencies": [],
                "oci_image": self.oci["auth"],
            },
            ("postgresql", "17.0.0"): {
                "dependencies": [],
                "oci_image": self.oci["postgres"],
            },
            ("redis", "8.8.0"): {
                "dependencies": [],
                "oci_image": self.oci["redis"],
            },
            ("minio", "2025.9.7"): {
                "dependencies": [],
                "oci_image": self.oci["minio"],
            },
        }
        self.catalog_contract_audits = _audit_generated_catalog(
            output,
            expected_catalog_id="cross-machine-services",
            expected_os="linux",
            expected_arch="x86_64",
            expected_contracts=expected_contracts,
        )
        source = json.loads((output / "catalog-source.json").read_text(encoding="utf-8"))[0]
        source_aliases = (
            "problem-service",
            "judge-api",
            "storage-service",
            "storage-service-canary",
            "judge-worker",
            "contract-echo-provider",
            "storage-head-provenance-miss-provider",
            "contract-echo-consumer",
            "auth-permission-provider",
            "postgresql",
            "redis",
            "minio",
        )
        self.catalog_sources = [
            {**copy.deepcopy(source), "id": alias} for alias in source_aliases
        ]
        self.catalog_trust.update(
            json.loads((output / "trust.json").read_text(encoding="utf-8"))
        )
        self.h.evidence["catalog_contracts"] = copy.deepcopy(self.catalog_contract_audits)
        # The complete immutable tree must be in the context before Docker
        # evaluates COPY.  Building an empty placeholder first would make the
        # strict lane fail for an infrastructure error instead of exercising
        # the catalog signature/digest checks we intend to prove.
        catalog_context = self.tmp / "derived" / "catalog-server"
        catalog_context.mkdir(parents=True, exist_ok=False)
        shutil.copytree(catalogs_root, catalog_context / "catalogs")
        (catalog_context / "Dockerfile").write_text(
            "FROM " + self.secure_fixture_image + "\nCOPY catalogs /catalogs\n",
            encoding="utf-8",
        )
        catalog_image = f"ojos/cross-machine-catalog-server:{self.h.run_id}"
        # The catalog is produced only after Registry-backed RepoDigests exist.
        # Build it directly in A so the full lane still performs exactly one
        # image archive load per nested Engine.
        self.a.command("build", "--tag", catalog_image, catalog_context, timeout=900)
        self.images["catalog"] = catalog_image

    def _start_identity_catalog_and_orchestrator(self) -> None:
        self.a.command(
            "run", "-d", "--name", "oidc-a", "--network", self.a_network,
            "--network-alias", "oidc-a", "--ip", self.A_IPS["oidc"],
            "--env", "OIDC_ISSUER=https://oidc-a:9443",
            "--env", "TLS_CERT_FILE=/opt/ojos-tls/server.pem",
            "--env", "TLS_KEY_FILE=/opt/ojos-tls/server.key",
            self.secure_fixture_image, "oidc", "--port", "9443",
        )
        self.a.command(
            "run", "-d", "--name", "catalog-a", "--network", self.a_network,
            "--network-alias", "catalog-a", "--ip", self.A_IPS["catalog"],
            "--env", "STATIC_ROOT=/catalogs",
            "--env", "TLS_CERT_FILE=/opt/ojos-tls/server.pem",
            "--env", "TLS_KEY_FILE=/opt/ojos-tls/server.key",
            self.images["catalog"], "static-tls", "--port", "9444",
        )
        db_url = _orchestrator_postgres_database_url(self.postgres_password)
        env = {
            "OJOS_ENVIRONMENT": "production",
            "ORCHESTRATOR_DATABASE_URL": db_url,
            "ORCHESTRATOR_POSTGRES_CA_CERT": "/opt/ojos-pki/ca.pem",
            "ORCHESTRATOR_TLS_CERT": "/opt/ojos-pki/server.pem",
            "ORCHESTRATOR_TLS_KEY": "/opt/ojos-pki/server.key",
            "ORCHESTRATOR_HEALTHCHECK_URL": CONTROL_PLANE_HEALTHCHECK_URL,
            "ORCHESTRATOR_HEALTHCHECK_CA_CERT": CONTROL_PLANE_HEALTHCHECK_CA_CERT,
            "ORCHESTRATOR_NODE_CA_CERT": "/opt/ojos-pki/node-ca.pem",
            "ORCHESTRATOR_NODE_CA_KEY": "/opt/ojos-pki/node-ca.key",
            "ORCHESTRATOR_CATALOG_TRUST_KEYS": _canonical(self.catalog_trust),
            "ORCHESTRATOR_CATALOG_SOURCES": _canonical(self.catalog_sources),
            "ORCHESTRATOR_CATALOG_CA_FILE": "/opt/ojos-pki/ca.pem",
            "ORCHESTRATOR_OIDC_ISSUER": "https://oidc-a:9443",
            "ORCHESTRATOR_OIDC_AUDIENCE": "ojos-orchestrator",
            "ORCHESTRATOR_OIDC_CLIENT_ID": "cross-machine-client",
            "ORCHESTRATOR_OIDC_CA_CERT": "/opt/ojos-pki/ca.pem",
            "ORCHESTRATOR_PUBLIC_BASE_URL": f"https://{self.h.a_ip}:8090",
            "ORCHESTRATOR_INTERNAL_TOKEN": self.internal_token,
            "ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN": "http://gateway-a:8080",
            "ORCHESTRATOR_GATEWAY_ADMIN_TOKEN": self.gateway_management_token,
            "ORCHESTRATOR_AUTH_ADMIN_ORIGIN": "http://auth-a:8081",
            "ORCHESTRATOR_AUTH_ADMIN_TOKEN": self.auth_management_token,
            "ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN": "http://auth-a:8081",
            "ORCHESTRATOR_AUTH_WORKLOAD_TOKEN": self.workload_issuer_token,
            "ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN_SHA256": _sha256(
                self.gateway_contribution_ack_token
            ),
            "ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN_SHA256": _sha256(
                self.auth_contribution_ack_token
            ),
            "ORCHESTRATOR_GATEWAY_WORKLOAD_ORIGIN": f"https://{self.h.a_ip}:8443",
            "ORCHESTRATOR_GATEWAY_WORKLOAD_CA_FILE": "/opt/ojos-pki/ca.pem",
            "ORCHESTRATOR_ALLOW_PRIVATE_RELEASE_SOURCE": "1",
            "ORCHESTRATOR_LEGACY_API_MODE": "gone",
            "ORCHESTRATOR_MAX_WORKERS": "64",
        }
        argv = [
            "run", "-d", "--name", "orchestrator-a", "--network", self.a_network,
            "--network-alias", "orchestrator-a", "--ip", self.A_IPS["orchestrator"],
            "--publish", "8090:8090",
        ]
        for key, value in env.items():
            argv.extend(["--env", key + "=" + value])
        argv.append(self.images["orchestrator"])
        self.a.command(*argv, timeout=180)

        control_port = self._outer_port(8090)
        gateway_port = self._outer_port(8443)
        self.control_client = JsonClient(
            f"https://127.0.0.1:{control_port}", self.ca_cert,
            default_headers={"x-ojos-orchestrator-token": self.internal_token},
        )
        self.gateway_client = JsonClient(f"https://127.0.0.1:{gateway_port}", self.ca_cert)
        ready = self._wait_json(self.control_client, "/api/v1/healthz/ready", timeout=180)
        build = ready.get("data", {}).get("build", {})
        if build.get("commit_sha") != self.commit or build.get("profile") != "production":
            raise FullGateError(f"control-plane build identity is not production candidate: {build}")
        self._wait_control_plane_container_healthy(timeout=90)
        self._wait_json(self.gateway_client, "/health", timeout=120)
        self.h.evidence["build_identity"] = build

    def _wait_control_plane_container_healthy(self, *, timeout: float) -> dict[str, Any]:
        """Wait for Docker's TLS readiness probe and retain only non-secret evidence."""

        deadline = time.monotonic() + timeout
        last_status = "missing"
        last_output = ""
        while time.monotonic() < deadline:
            inspected = json.loads(
                self.a.command("inspect", "orchestrator-a", timeout=30).stdout
            )
            if not isinstance(inspected, list) or len(inspected) != 1:
                raise FullGateError(
                    "control-plane Docker inspect did not return exactly one container"
                )
            container = inspected[0]
            if not isinstance(container, Mapping):
                raise FullGateError("control-plane Docker inspect payload is invalid")
            state = container.get("State", {})
            config = container.get("Config", {})
            if not isinstance(state, Mapping) or not isinstance(config, Mapping):
                raise FullGateError("control-plane Docker inspect state/config is invalid")
            environment: dict[str, str] = {}
            for item in config.get("Env", []) or []:
                name, separator, value = str(item).partition("=")
                if separator:
                    environment[name] = value
            healthcheck_url = environment.get("ORCHESTRATOR_HEALTHCHECK_URL", "")
            healthcheck_ca = environment.get(
                "ORCHESTRATOR_HEALTHCHECK_CA_CERT", ""
            )
            tls_enabled = bool(
                environment.get("ORCHESTRATOR_TLS_CERT")
                and environment.get("ORCHESTRATOR_TLS_KEY")
            )
            if (
                healthcheck_url != CONTROL_PLANE_HEALTHCHECK_URL
                or healthcheck_ca != CONTROL_PLANE_HEALTHCHECK_CA_CERT
                or not tls_enabled
            ):
                raise FullGateError(
                    "control-plane Docker TLS healthcheck environment is invalid"
                )
            health = state.get("Health", {})
            if not isinstance(health, Mapping):
                raise FullGateError("control-plane image has no Docker HEALTHCHECK")
            last_status = str(health.get("Status", "missing"))
            logs = health.get("Log", [])
            if isinstance(logs, list) and logs and isinstance(logs[-1], Mapping):
                last_output = str(logs[-1].get("Output", ""))[-1000:]
            if state.get("Running") is True and last_status.lower() == "healthy":
                evidence = {
                    "evidence_source": "docker-inspect",
                    "container_id": str(container.get("Id", "")),
                    "engine_id": str(
                        self.h.evidence.get("engines", {})
                        .get("a", {})
                        .get("engine_id", "")
                    ),
                    "running": True,
                    "docker_health": "HEALTHY",
                    "healthcheck_url": healthcheck_url,
                    "healthcheck_ca_cert": healthcheck_ca,
                    "tls_enabled": tls_enabled,
                }
                self.h.evidence["control_plane_runtime"] = evidence
                return evidence
            time.sleep(1)
        detail = f": {last_output}" if last_output else ""
        raise FullGateError(
            f"control-plane Docker HEALTHCHECK did not become healthy "
            f"within {timeout:g}s (last status {last_status}){detail}"
        )

    def _outer_port(self, port: int) -> int:
        output = self.root.command("port", self.h.a_name, f"{port}/tcp").stdout.strip().splitlines()
        match = re.search(r":([0-9]+)$", output[0] if output else "")
        if match is None:
            raise FullGateError(f"outer Engine A did not publish {port}")
        return int(match.group(1))

    def _wait_json(self, client: JsonClient, path: str, *, timeout: float) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        last = ""
        while time.monotonic() < deadline:
            try:
                return client.request("GET", path, expected=(200,), timeout=5)[0]
            except Exception as error:  # the final error is surfaced below
                last = str(error)
                time.sleep(1)
        raise FullGateError(f"{path} did not become ready: {last}")

    # --------------------------------------------------------------- boundary

    def _apply_network_policy_and_probe(self) -> None:
        bridge = json.loads(
            self.b.command("network", "inspect", "--format", "{{json .IPAM.Config}}", "bridge").stdout
        )
        bridge_subnet = str(bridge[0]["Subnet"])
        iptables = lambda *args: self.root.command("exec", self.h.b_name, "iptables", *args)
        iptables("-F", "DOCKER-USER")
        iptables("-A", "DOCKER-USER", "-m", "conntrack", "--ctstate", "ESTABLISHED,RELATED", "-j", "ACCEPT")
        for subnet in (self.B_SERVICE_NETWORK, bridge_subnet):
            iptables("-A", "DOCKER-USER", "-s", subnet, "-d", self.h.a_ip, "-p", "tcp", "--dport", "8443", "-j", "ACCEPT")
            iptables("-A", "DOCKER-USER", "-s", subnet, "-d", self.h.a_ip, "-j", "REJECT")
        for port in ("8090", "5000"):
            iptables("-A", "DOCKER-USER", "-s", self.B_AGENT_NETWORK, "-d", self.h.a_ip, "-p", "tcp", "--dport", port, "-j", "ACCEPT")
        iptables("-A", "DOCKER-USER", "-s", self.B_AGENT_NETWORK, "-d", self.h.a_ip, "-j", "REJECT")
        iptables("-A", "DOCKER-USER", "-j", "RETURN")

        denied_targets = [
            {"name": name, "host": self.h.a_ip, "port": port}
            for name, port in {
                "postgresql": 5432, "redis": 6379, "minio": 9000,
                "judge-api-direct": 8082, "control-plane": 8090, "oci-registry": 5000,
            }.items()
        ]
        boundary = self.b.command(
            "run", "--rm", "--network", self.b_service_network, "--ip", "172.30.0.10",
            "--env", "DENIED_TARGETS_JSON=" + _canonical(denied_targets),
            self.secure_fixture_image, "boundary-probe", timeout=60,
        )
        # The managed Judge Worker is deliberately created on Docker's
        # default bridge.  Probe that exact network too: a denial observed on
        # the fixture service network alone does not prove the Worker's real
        # egress boundary.
        worker_bridge_boundary = self.b.command(
            "run",
            "--rm",
            "--network",
            "bridge",
            "--env",
            "DENIED_TARGETS_JSON=" + _canonical(denied_targets),
            self.secure_fixture_image,
            "boundary-probe",
            timeout=60,
        )
        workload_gateway = self.b.command(
            "run",
            "--rm",
            "--network",
            self.b_service_network,
            "--ip",
            "172.30.0.11",
            "--env",
            "AGENT_TARGETS_JSON="
            + _canonical(
                [
                    {
                        "name": "gateway",
                        "url": f"https://{self.h.a_ip}:8443/health",
                    }
                ]
            ),
            "--env",
            "AGENT_CA_FILE=/opt/ojos-tls/ca.pem",
            self.secure_fixture_image,
            "agent-probe",
            timeout=60,
        )
        agent_targets = [
            {"name": "control-plane", "url": f"https://{self.h.a_ip}:8090/api/v1/healthz/ready"},
            {"name": "oci-registry", "url": f"http://{self.h.a_ip}:5000/v2/"},
        ]
        agent = self.b.command(
            "run", "--rm", "--network", self.b_agent_network, "--ip", "172.31.0.10",
            "--env", "AGENT_TARGETS_JSON=" + _canonical(agent_targets),
            "--env", "AGENT_CA_FILE=/opt/ojos-tls/ca.pem",
            self.secure_fixture_image, "agent-probe", timeout=60,
        )
        boundary_json = _json_from_last_line(boundary.stdout)
        worker_bridge_boundary_json = _json_from_last_line(worker_bridge_boundary.stdout)
        workload_gateway_json = _json_from_last_line(workload_gateway.stdout)
        agent_json = _json_from_last_line(agent.stdout)
        self.h.evidence["network_boundary"] = {
            "policy": "B workload/default bridge may reach only A Gateway; B Agent may reach only control and OCI",
            "gateway_ready": workload_gateway_json.get("targets")
            == [{"name": "gateway", "status": 200}],
            "gateway_connectivity": workload_gateway_json.get("targets", []),
            "denied": boundary_json.get("denied", []),
            "worker_bridge_denied": worker_bridge_boundary_json.get("denied", []),
            "agent_connectivity": agent_json.get("targets", []),
            "service_subnet": self.B_SERVICE_NETWORK,
            "worker_bridge_subnet": bridge_subnet,
            "agent_subnet": self.B_AGENT_NETWORK,
        }

    # ------------------------------------------------------------ Agent/Store

    def _enroll_and_start_agents(self) -> None:
        self._enroll_and_start_a_agent()
        self._enroll_and_start_b_agent()

    @staticmethod
    def _agent_docker_arguments(
        state_root: str, workload_export_root: str, network: str
    ) -> list[str]:
        """Return the capability-free non-root Agent container boundary."""

        return [
            "--network",
            network,
            "--user",
            AGENT_CONFIG_USER,
            "--group-add",
            str(AGENT_DOCKER_SOCKET_GID),
            "--mount",
            f"type=bind,source={state_root},target={state_root}",
            "--mount",
            (
                "type=bind,source="
                f"{workload_export_root},target={workload_export_root}"
            ),
            "--mount",
            "type=bind,source=/var/run/docker.sock,target=/var/run/docker.sock",
        ]

    def _nested_container_process_identity(
        self,
        engine: Any,
        inspected: Mapping[str, Any],
        container_name: str,
    ) -> dict[str, Any]:
        """Read the real main-process credentials in the owning DinD namespace."""

        if engine is self.a:
            outer_engine = self.h.a_name
        elif engine is self.b:
            outer_engine = self.h.b_name
        else:
            raise FullGateError("cannot inspect a process on an unknown nested Engine")
        pid = inspected.get("State", {}).get("Pid")
        if isinstance(pid, bool) or not isinstance(pid, int) or pid <= 0:
            raise FullGateError(f"{container_name} has no live DinD process ID")
        status = self.root.command(
            "exec", outer_engine, "cat", f"/proc/{pid}/status", timeout=30
        ).stdout
        fields = {
            name: value.strip()
            for line in status.splitlines()
            if ":" in line
            for name, value in [line.split(":", 1)]
        }
        try:
            uids = [int(value) for value in fields["Uid"].split()]
            gids = [int(value) for value in fields["Gid"].split()]
            supplementary = sorted(
                int(value) for value in fields.get("Groups", "").split()
            )
        except (KeyError, ValueError) as exc:
            raise FullGateError(
                f"{container_name} returned malformed /proc identity evidence"
            ) from exc
        if len(uids) != 4 or len(gids) != 4:
            raise FullGateError(
                f"{container_name} returned incomplete /proc identity evidence"
            )
        return {
            "pid": pid,
            "euid": uids[1],
            "egid": gids[1],
            "groups": sorted(set([gids[1], *supplementary])),
            "supplementary_groups": supplementary,
            "outer_engine": outer_engine,
            "evidence_source": "dind-main-process-proc-status",
        }

    def _inspect_agent_process_identity(
        self,
        engine: Any,
        container_name: str,
        expected_state_root: str,
        expected_workload_export_root: str,
        expected_socket: Mapping[str, Any],
    ) -> dict[str, Any]:
        """Fail closed unless inspect and /proc agree on the exact Agent identity."""

        inspected = json.loads(engine.command("inspect", container_name).stdout)[0]
        config = inspected.get("Config", {})
        host = inspected.get("HostConfig", {})
        config_user = str(config.get("User", ""))
        group_add = [str(value) for value in host.get("GroupAdd", []) or []]
        cap_add = [str(value) for value in host.get("CapAdd", []) or []]
        privileged = host.get("Privileged") is True
        process_identity = self._nested_container_process_identity(
            engine, inspected, container_name
        )
        mounts = inspected.get("Mounts", []) or []
        state_mount = next(
            (item for item in mounts if item.get("Destination") == expected_state_root),
            None,
        )
        workload_export_mount = next(
            (
                item
                for item in mounts
                if item.get("Destination") == expected_workload_export_root
            ),
            None,
        )
        docker_mount = next(
            (item for item in mounts if item.get("Destination") == "/var/run/docker.sock"),
            None,
        )
        if (
            config_user != AGENT_CONFIG_USER
            or group_add != [str(AGENT_DOCKER_SOCKET_GID)]
            or cap_add
            or privileged
            or process_identity.get("euid") != STANDARD_WORKLOAD_UID
            or process_identity.get("egid") != STANDARD_WORKLOAD_GID
            or process_identity.get("groups")
            != [AGENT_DOCKER_SOCKET_GID, STANDARD_WORKLOAD_GID]
            or process_identity.get("supplementary_groups")
            != [AGENT_DOCKER_SOCKET_GID]
            or process_identity.get("evidence_source")
            != "dind-main-process-proc-status"
            or not isinstance(state_mount, Mapping)
            or state_mount.get("Type") != "bind"
            or state_mount.get("Source") != expected_state_root
            or state_mount.get("RW") is not True
            or expected_workload_export_root == expected_state_root
            or expected_workload_export_root.startswith(expected_state_root.rstrip("/") + "/")
            or expected_state_root.startswith(
                expected_workload_export_root.rstrip("/") + "/"
            )
            or not isinstance(workload_export_mount, Mapping)
            or workload_export_mount.get("Type") != "bind"
            or workload_export_mount.get("Source") != expected_workload_export_root
            or workload_export_mount.get("RW") is not True
            or not isinstance(docker_mount, Mapping)
            or docker_mount.get("Type") != "bind"
            or docker_mount.get("Source") != "/var/run/docker.sock"
            or expected_socket.get("gid") != AGENT_DOCKER_SOCKET_GID
            or expected_socket.get("mode") != "0660"
            or expected_socket.get("file_type") != "socket"
        ):
            raise FullGateError(
                f"{container_name} is not the capability-free 65532 Agent with its exact DinD paths"
            )
        return {
            "config_user": config_user,
            "group_add": group_add,
            "cap_add": cap_add,
            "privileged": privileged,
            "process": process_identity,
            "state_root": expected_state_root,
            "state_mount_source": state_mount.get("Source"),
            "state_mount_destination": state_mount.get("Destination"),
            "state_mount_read_write": state_mount.get("RW"),
            "workload_export_root": expected_workload_export_root,
            "workload_export_mount_source": workload_export_mount.get("Source"),
            "workload_export_mount_destination": workload_export_mount.get("Destination"),
            "workload_export_mount_read_write": workload_export_mount.get("RW"),
            "state_export_roots_disjoint": True,
            "daemon_namespace_path_exact": True,
            "docker_socket_mount_source": docker_mount.get("Source"),
            "docker_socket_mount_destination": docker_mount.get("Destination"),
            "docker_socket": copy.deepcopy(dict(expected_socket)),
            "evidence_source": "container-inspect-and-proc",
        }

    def _enroll_and_start_a_agent(self) -> None:
        assert self.control_client is not None
        host_root = A_AGENT_STATE_ROOT
        workload_export_root = A_WORKLOAD_EXPORT_ROOT
        body = _standalone_node_enrollment(
            "node-a",
            self.h.a_ip,
            {
                "purpose": "managed-business-services",
                "providers": {
                    "redis": {"enabled": True, "connection_id": "a-events"},
                    "storage": {
                        "enabled": True,
                        "backend": "s3",
                        "connection_id": "a-minio",
                    },
                    "materialization": {
                        "enabled": True,
                        "secret_provider": "file",
                    },
                    "frontend": {"enabled": True, "asset_store_id": "a-assets"},
                },
            },
        )
        response, _, _ = self.control_client.request(
            "POST",
            "/api/v1/nodes/enrollment-codes",
            body,
            headers={"idempotency-key": "cross-machine-node-a-enroll"},
            expected=(201,),
        )
        code = response.get("data", {}).get("enrollment_code")
        if not isinstance(code, str) or not code:
            raise FullGateError("control plane did not return a Node A enrollment code")
        policy = {
            "schema_version": 1,
            "allowed_profiles": ["standard-container-v1"],
            "service_context_root": workload_export_root + "/runtime-contexts",
        }
        database_url = lambda name: (
            f"postgresql://postgres:{self.postgres_password}@{self.h.a_ip}:5432/{name}"
            "?sslmode=verify-full&sslrootcert=/usr/local/share/ca-certificates/ojos-cross-machine.crt"
        )
        files = {
            "server-ca.pem": self.ca_cert.read_bytes(),
            "runtime-policy.json": (_canonical(policy) + "\n").encode(),
            "enrollment-code": (code + "\n").encode(),
            "redis-connections.json": (
                _canonical({"a-events": {"url": f"redis://{self.h.a_ip}:6379/0"}}) + "\n"
            ).encode(),
            "storage-connections.json": (
                _canonical(
                    {
                        "a-minio": {
                            "backend": "s3",
                            "endpoint": f"http://{self.h.a_ip}:9000",
                            "access_key": self.minio_access,
                            "secret_key": self.minio_secret,
                            "region": "us-east-1",
                            "path_style": True,
                        }
                    }
                )
                + "\n"
            ).encode(),
            "frontend-stores.json": (
                _canonical({"a-assets": {"root": host_root + "/frontend-assets"}}) + "\n"
            ).encode(),
            "secrets/problem-database-url": (database_url("ojos_problem") + "\n").encode(),
            "secrets/judge-database-url": (database_url("ojos_judge") + "\n").encode(),
            "secrets/minio-access": (self.minio_access + "\n").encode(),
            "secrets/minio-secret": (self.minio_secret + "\n").encode(),
        }
        bootstrap_files = self._seed_agent_host(
            self.a, host_root, workload_export_root, files
        )
        common = [
            *self._agent_docker_arguments(
                host_root, workload_export_root, self.a_network
            ),
            "--env",
            "OJOS_ENVIRONMENT=production",
            "--env",
            "ORCHESTRATOR_SECRET_DIRECTORY=" + host_root + "/secrets",
            "--env",
            "ORCHESTRATOR_REDIS_CONNECTIONS_FILE=" + host_root + "/redis-connections.json",
            "--env",
            "ORCHESTRATOR_STORAGE_CONNECTIONS_FILE=" + host_root + "/storage-connections.json",
            "--env",
            "ORCHESTRATOR_FRONTEND_ASSET_STORES_FILE=" + host_root + "/frontend-stores.json",
        ]
        control_plane = f"https://{self.h.a_ip}:8090"
        enroll = self.a.command(
            "run",
            "--rm",
            *common,
            self.images["agent"],
            "enroll",
            "--control-plane",
            control_plane,
            "--enrollment-code-file",
            host_root + "/enrollment-code",
            "--ca",
            host_root + "/server-ca.pem",
            "--identity-dir",
            host_root + "/identity",
            "--expected-node-id",
            "node-a",
            timeout=120,
        )
        identity = _json_from_last_line(enroll.stdout)
        instance = "agent-a-" + self.h.run_id
        self.a.command(
            "run",
            "-d",
            "--name",
            "orchestrator-agent-a",
            *common,
            self.images["agent"],
            "run",
            "--control-plane",
            control_plane,
            "--identity-dir",
            host_root + "/identity",
            "--ledger",
            host_root + "/execution-ledger.sqlite3",
            "--workload-export-dir",
            workload_export_root,
            "--runtime-policy",
            host_root + "/runtime-policy.json",
            "--instance",
            instance,
            "--heartbeat-ms",
            "2000",
            "--transport-retry-ms",
            "500",
            timeout=120,
        )
        health = self._wait_node_ready("node-a", timeout=120)
        agent_inspect = json.loads(
            self.a.command("inspect", "orchestrator-agent-a").stdout
        )[0]
        runtime_identity = self._inspect_agent_process_identity(
            self.a,
            "orchestrator-agent-a",
            host_root,
            workload_export_root,
            self.h.evidence["engines"]["a"]["docker_socket"],
        )
        environment_names = {
            str(value).split("=", 1)[0]
            for value in agent_inspect.get("Config", {}).get("Env", []) or []
        }
        forbidden_management = {
            "ORCHESTRATOR_AUTH_ADMIN_ENDPOINT",
            "ORCHESTRATOR_AUTH_ADMIN_ORIGIN",
            "ORCHESTRATOR_AUTH_ADMIN_TOKEN",
            "AUTH_SERVICE_ENDPOINT",
            "AUTH_SERVICE_ADMIN_TOKEN",
            "ORCHESTRATOR_GATEWAY_ADMIN_ENDPOINT",
            "ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN",
            "ORCHESTRATOR_GATEWAY_ADMIN_TOKEN",
            "GATEWAY_ENDPOINT",
            "GATEWAY_ADMIN_TOKEN",
            "ORCHESTRATOR_GATEWAY_TOKEN",
            "ORCHESTRATOR_RELEASE_PROVISIONER_ENDPOINT",
            "ORCHESTRATOR_RELEASE_PROVISIONER_ORIGIN",
            "ORCHESTRATOR_RELEASE_PROVISIONER_TOKEN",
            "ORCHESTRATOR_API_REGISTRIES_JSON",
            "ORCHESTRATOR_API_REGISTRIES_FILE",
        }
        leaked_management = sorted(environment_names & forbidden_management)
        if leaked_management:
            raise FullGateError(
                f"node-a Agent received forbidden management credentials: {leaked_management}"
            )
        self.a_agent_identity = {
            "enrolled": identity.get("status") in {"ENROLLED", "RECOVERED"},
            "mtls": True,
            "node_id": identity.get("node_id"),
            "instance_id": instance,
            "certificate_serial": identity.get("serial_hex"),
            "runtime_health": health,
            "identity_source": "agent-enroll-output-and-final-runtime-facts",
            "management_credentials_present": False,
            "management_environment_inspected": True,
            "forbidden_management_environment": leaked_management,
            "container_id": agent_inspect.get("Id"),
            "runtime_identity": runtime_identity,
            "bootstrap_files": bootstrap_files,
        }

    def _enroll_and_start_b_agent(self) -> None:
        assert self.control_client is not None
        b_outer_ip = self.h.evidence["engines"]["b"]["outer_ip"]
        body = _standalone_node_enrollment(
            "node-b", b_outer_ip, {"purpose": "judge"}
        )
        response, _, _ = self.control_client.request(
            "POST", "/api/v1/nodes/enrollment-codes", body,
            headers={"idempotency-key": "cross-machine-node-b-enroll"}, expected=(201,),
        )
        code = response.get("data", {}).get("enrollment_code")
        if not isinstance(code, str) or not code:
            raise FullGateError("control plane did not return a Node enrollment code")
        policy = {
            "schema_version": 1,
            "allowed_profiles": ["standard-container-v1", "judge-sandbox-v1"],
            "service_context_root": B_WORKLOAD_EXPORT_ROOT + "/runtime-contexts",
            "judge_sandbox": {
                "profile_sha256": self.PROFILE_SHA256,
                "context_root": B_WORKLOAD_EXPORT_ROOT + "/runtime-contexts",
                "allowed_images": [self.oci["worker"]],
            },
        }
        bootstrap_files = self._seed_b_agent_host(
            {
                "server-ca.pem": self.ca_cert.read_bytes(),
                "runtime-policy.json": (_canonical(policy) + "\n").encode(),
                "enrollment-code": (code + "\n").encode(),
            }
        )
        common = self._agent_docker_arguments(
            B_AGENT_STATE_ROOT, B_WORKLOAD_EXPORT_ROOT, self.b_agent_network
        )
        enroll = self.b.command(
            "run", "--rm", *common, self.images["agent"], "enroll",
            "--control-plane", f"https://{self.h.a_ip}:8090",
            "--enrollment-code-file", "/var/lib/ojos-agent/enrollment-code",
            "--ca", "/var/lib/ojos-agent/server-ca.pem",
            "--identity-dir", "/var/lib/ojos-agent/identity",
            "--expected-node-id", "node-b", timeout=120,
        )
        identity = _json_from_last_line(enroll.stdout)
        instance = "agent-b-" + self.h.run_id
        self.b.command(
            "run", "-d", "--name", "orchestrator-agent-b", *common,
            self.images["agent"], "run",
            "--control-plane", f"https://{self.h.a_ip}:8090",
            "--identity-dir", "/var/lib/ojos-agent/identity",
            "--ledger", "/var/lib/ojos-agent/execution-ledger.sqlite3",
            "--workload-export-dir", B_WORKLOAD_EXPORT_ROOT,
            "--runtime-policy", "/var/lib/ojos-agent/runtime-policy.json",
            "--instance", instance, "--heartbeat-ms", "2000", "--transport-retry-ms", "500",
            timeout=120,
        )
        health = self._wait_node_ready("node-b", timeout=120)
        agent_inspect = json.loads(
            self.b.command("inspect", "orchestrator-agent-b").stdout
        )[0]
        runtime_identity = self._inspect_agent_process_identity(
            self.b,
            "orchestrator-agent-b",
            B_AGENT_STATE_ROOT,
            B_WORKLOAD_EXPORT_ROOT,
            self.h.evidence["engines"]["b"]["docker_socket"],
        )
        self.agent_identity = {
            "enrolled": identity.get("status") in {"ENROLLED", "RECOVERED"},
            "mtls": True,
            "node_id": identity.get("node_id"),
            "instance_id": instance,
            "certificate_serial": identity.get("serial_hex"),
            "runtime_health": health,
            "identity_source": "agent-enroll-output-and-final-runtime-facts",
            "container_id": agent_inspect.get("Id"),
            "runtime_identity": runtime_identity,
            "bootstrap_files": bootstrap_files,
        }

    def _probe_managed_a_network(self) -> None:
        targets = [
            {"name": name, "host": self.h.a_ip, "port": port}
            for name, port in {
                "postgresql-tls": 5432,
                "redis-events": 6379,
                "minio-s3": 9000,
                "gateway-workload": 8443,
                "control-plane": 8090,
                "oci-registry": 5000,
            }.items()
        ]
        result = self.a.command(
            "run",
            "--rm",
            "--env",
            "CONNECT_TARGETS_JSON=" + _canonical(targets),
            self.secure_fixture_image,
            "connect-probe",
            timeout=60,
        )
        evidence = _json_from_last_line(result.stdout)
        observed = {
            str(item.get("name", ""))
            for item in evidence.get("targets", [])
            if item.get("connected") is True
        }
        expected = {str(item["name"]) for item in targets}
        if observed != expected:
            raise FullGateError(
                f"managed A default bridge cannot reach required host providers: {evidence}"
            )
        verified_tls = self.a.command(
            "run",
            "--rm",
            "--env",
            "PGPASSWORD=" + self.postgres_password,
            "--entrypoint",
            "psql",
            self.images["postgres"],
            (
                f"postgresql://postgres@{self.h.a_ip}:5432/postgres"
                "?sslmode=verify-full&sslrootcert="
                "/opt/ojos-pg-tls/ca.pem"
            ),
            "-X",
            "-A",
            "-t",
            "-c",
            "SELECT current_setting('ssl')",
            timeout=30,
        )
        if verified_tls.stdout.strip() != "on":
            raise FullGateError(
                "PostgreSQL verify-full probe did not negotiate TLS: "
                + verified_tls.stdout[-1000:]
            )
        plaintext = self.a.command(
            "run",
            "--rm",
            "--env",
            "PGPASSWORD=" + self.postgres_password,
            "--entrypoint",
            "psql",
            self.images["postgres"],
            f"postgresql://postgres@{self.h.a_ip}:5432/postgres?sslmode=disable",
            "-c",
            "SELECT 1",
            timeout=30,
            check=False,
        )
        if plaintext.returncode == 0:
            raise FullGateError("PostgreSQL accepted a plaintext default-bridge connection")
        self.h.evidence["managed_a_network"] = {
            "engine_id": self.h.evidence["engines"]["a"]["engine_id"],
            "source_network": "engine-a-default-bridge",
            "targets": evidence.get("targets", []),
            "postgres_plaintext_rejected": True,
            "postgres_tls": {
                "verify_full_succeeded": True,
                "server_ssl_enabled": True,
                "plaintext_rejected": True,
                "ca_sha256": _sha256(self.ca_cert.read_bytes()),
            },
        }

    def _wait_node_ready(self, node_id: str, *, timeout: float) -> dict[str, Any]:
        assert self.control_client is not None
        deadline = time.monotonic() + timeout
        health: dict[str, Any] = {}
        while time.monotonic() < deadline:
            try:
                health = self.control_client.request(
                    "GET", f"/api/v1/nodes/{node_id}/health", expected=(200,), timeout=5
                )[0].get("data", {})
                if health.get("ready") is True and health.get("agent_reachable") is True:
                    return health
            except FullGateError:
                pass
            time.sleep(1)
        raise FullGateError(f"enrolled {node_id} did not become Ready: {health}")

    @staticmethod
    def _require_healthy_attested_deployment(
        deployment: Mapping[str, Any],
        *,
        deployment_id: str,
        node_id: str,
        phase: str,
    ) -> None:
        instance = deployment.get("instance", {})
        drift_reason = str(deployment.get("drift_reason", "")).strip()
        if (
            not isinstance(instance, Mapping)
            or str(instance.get("deployment_id", "")) != deployment_id
            or str(deployment.get("node_id", "")) != node_id
            or str(instance.get("desired_state", "")).upper() != "RUNNING"
            or str(instance.get("observed_state", "")).upper() != "RUNNING"
            or str(instance.get("health", "")).upper() != "HEALTHY"
            or instance.get("runtime_attested") is not True
            or drift_reason
        ):
            raise FullGateError(
                f"{phase} deployment {deployment_id} is not "
                "Running/Healthy/runtime-attested without drift: "
                + _canonical(deployment)
            )

    @staticmethod
    def _runtime_completion_watermark(job: Mapping[str, Any], *, deployment_id: str) -> int:
        result = job.get("result", {})
        value = result.get("runtime_observed_at_ms") if isinstance(result, Mapping) else None
        if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
            raise FullGateError(
                f"Agent completion for deployment {deployment_id} omitted its positive "
                "runtime_observed_at_ms watermark: "
                + _canonical(job)
            )
        return value

    def _managed_runtime_convergence(
        self,
        deployment_id: str,
        node_id: str,
        job: Mapping[str, Any],
        *,
        timeout: float = 90,
    ) -> dict[str, Any]:
        """Prove lifecycle projection and a causally newer full Agent inventory.

        A successful lifecycle Job projects its own authenticated Docker result
        immediately.  A later *complete* Agent inventory must then observe the
        same healthy, attested runtime.  The control plane only advances
        ``last_observed_at_ms`` past the lifecycle watermark for such a newer
        inventory, so the strict comparison is the causal takeover proof.
        """

        completion_watermark_ms = self._runtime_completion_watermark(
            job, deployment_id=deployment_id
        )
        path = f"/api/v1/deployments/{deployment_id}"

        def read_projection() -> dict[str, Any]:
            payload = self._control_get(path)
            deployment = payload.get("data", {}).get("deployment")
            if not isinstance(deployment, dict):
                raise FullGateError(
                    f"deployment projection response for {deployment_id} is incomplete: "
                    + _canonical(payload)
                )
            return deployment

        immediate = read_projection()
        self._require_healthy_attested_deployment(
            immediate,
            deployment_id=deployment_id,
            node_id=node_id,
            phase="immediate lifecycle",
        )
        latest = immediate
        deadline = time.monotonic() + timeout
        while True:
            observed_at_ms = latest.get("last_observed_at_ms")
            if (
                not isinstance(observed_at_ms, bool)
                and isinstance(observed_at_ms, int)
                and observed_at_ms > completion_watermark_ms
            ):
                self._require_healthy_attested_deployment(
                    latest,
                    deployment_id=deployment_id,
                    node_id=node_id,
                    phase="post-completion Agent inventory",
                )
                return {
                    "completion_watermark_ms": completion_watermark_ms,
                    "immediate_payload": immediate,
                    "inventory_payload": latest,
                }
            if time.monotonic() >= deadline:
                failures = self.h.evidence.setdefault(
                    "runtime_inventory_convergence_failures", {}
                )
                if isinstance(failures, dict):
                    failures[deployment_id] = {
                        "completion_watermark_ms": completion_watermark_ms,
                        "last_payload": latest,
                    }
                raise FullGateError(
                    f"deployment {deployment_id} did not receive a complete Agent inventory "
                    f"newer than lifecycle watermark {completion_watermark_ms} within "
                    f"{timeout:g}s; last full payload: {_canonical(latest)}"
                )
            time.sleep(1)
            latest = read_projection()

    def _seed_b_agent_host(self, files: Mapping[str, bytes]) -> dict[str, Any]:
        return self._seed_agent_host(
            self.b, B_AGENT_STATE_ROOT, B_WORKLOAD_EXPORT_ROOT, files
        )

    def _seed_agent_host(
        self,
        engine: Any,
        host_root: str,
        workload_export_root: str,
        files: Mapping[str, bytes],
    ) -> dict[str, Any]:
        if engine is self.a:
            outer_engine = self.h.a_name
        elif engine is self.b:
            outer_engine = self.h.b_name
        else:
            raise FullGateError("cannot seed an unknown nested Docker Engine host")
        if (
            workload_export_root == host_root
            or workload_export_root.startswith(host_root.rstrip("/") + "/")
            or host_root.startswith(workload_export_root.rstrip("/") + "/")
        ):
            raise FullGateError(
                "Agent state and daemon-visible workload export roots must be disjoint"
            )
        # Docker's --mount bind syntax fails when the source is absent.  A new
        # DinD host has no /var/lib/ojos-agent* tree, so create the exact source
        # on that Engine host before the helper container materializes files.
        # The outer helper is the sole privileged setup boundary.  It creates
        # the DinD-visible bind source, then the disposable inner helper writes
        # every bootstrap object directly as uid/gid 65532.  The long-running
        # Agent receives neither root nor CAP_CHOWN.
        self.root.command(
            "exec",
            outer_engine,
            "mkdir",
            "-p",
            host_root,
            workload_export_root,
            timeout=30,
        )
        self.root.command(
            "exec",
            outer_engine,
            "chown",
            f"{STANDARD_WORKLOAD_UID}:{STANDARD_WORKLOAD_GID}",
            host_root,
            workload_export_root,
            timeout=30,
        )
        self.root.command(
            "exec",
            outer_engine,
            "chmod",
            AGENT_PRIVATE_DIRECTORY_MODE.removeprefix("0"),
            host_root,
            workload_export_root,
            timeout=30,
        )
        payload = {name: base64.b64encode(value).decode("ascii") for name, value in files.items()}
        directories = sorted(
            {"identity"}
            | {
                str(Path(name).parent).replace("\\", "/")
                for name in files
                if str(Path(name).parent) != "."
            }
        )
        program = (
            "import base64,json,os,pathlib;"
            "root=pathlib.Path('/host');"
            "[(root.joinpath(d).mkdir(parents=True,exist_ok=True,mode=0o700),"
            "os.chmod(root.joinpath(d),0o700)) for d in "
            "json.loads(os.environ['OJOS_DIRECTORIES'])];"
            "[(root.joinpath(k).parent.mkdir(parents=True,exist_ok=True,mode=0o700),"
            "root.joinpath(k).write_bytes(base64.b64decode(v)),os.chmod(root.joinpath(k),0o600)) "
            "for k,v in json.loads(os.environ['OJOS_FILES']).items()]"
        )
        engine.command(
            "run", "--rm", "--user", AGENT_CONFIG_USER,
            "--mount", f"type=bind,source={host_root},target=/host",
            "--env", "OJOS_FILES=" + _canonical(payload),
            "--env", "OJOS_DIRECTORIES=" + _canonical(directories),
            "--entrypoint", "python",
            self.secure_fixture_image, "-c", program, timeout=60,
        )
        verification = engine.command(
            "run",
            "--rm",
            "--user",
            AGENT_CONFIG_USER,
            "--mount",
            f"type=bind,source={host_root},target=/host,readonly",
            "--env",
            "OJOS_FILE_NAMES=" + _canonical(sorted(files)),
            "--env",
            "OJOS_DIRECTORY_NAMES=" + _canonical(directories),
            "--entrypoint",
            "python",
            self.secure_fixture_image,
            "-c",
            "import json,os,pathlib,stat;root=pathlib.Path('/host');"
            "rows=[];dirs=[];"
            "[(lambda p,s:dirs.append({'path':d,'uid':s.st_uid,'gid':s.st_gid,"
            "'mode':format(stat.S_IMODE(s.st_mode),'04o'),'directory':p.is_dir()}))"
            "(root.joinpath(d),root.joinpath(d).stat()) for d in "
            "json.loads(os.environ['OJOS_DIRECTORY_NAMES'])];"
            "[(lambda p,s:rows.append({'path':k,'uid':s.st_uid,'gid':s.st_gid,"
            "'mode':format(stat.S_IMODE(s.st_mode),'04o'),'regular':p.is_file()}))"
            "(root.joinpath(k),root.joinpath(k).stat()) for k in "
            "json.loads(os.environ['OJOS_FILE_NAMES'])];"
            "print(json.dumps({'root_uid':root.stat().st_uid,'root_gid':root.stat().st_gid,"
            "'root_mode':format(stat.S_IMODE(root.stat().st_mode),'04o'),"
            "'directories':dirs,'files':rows},"
            "sort_keys=True))",
            timeout=60,
        )
        observed = json.loads(verification.stdout.strip())
        export_verification = engine.command(
            "run",
            "--rm",
            "--user",
            AGENT_CONFIG_USER,
            "--mount",
            (
                f"type=bind,source={workload_export_root},"
                "target=/workload-export,readonly"
            ),
            "--entrypoint",
            "python",
            self.secure_fixture_image,
            "-c",
            "import json,pathlib,stat;p=pathlib.Path('/workload-export');s=p.stat();"
            "print(json.dumps({'uid':s.st_uid,'gid':s.st_gid,"
            "'mode':format(stat.S_IMODE(s.st_mode),'04o'),'directory':p.is_dir()}))",
            timeout=60,
        )
        workload_export = json.loads(export_verification.stdout.strip())
        if (
            observed.get("root_uid") != STANDARD_WORKLOAD_UID
            or observed.get("root_gid") != STANDARD_WORKLOAD_GID
            or observed.get("root_mode") != AGENT_PRIVATE_DIRECTORY_MODE
            or len(observed.get("directories", [])) != len(directories)
            or any(
                row.get("uid") != STANDARD_WORKLOAD_UID
                or row.get("gid") != STANDARD_WORKLOAD_GID
                or row.get("mode") != AGENT_PRIVATE_DIRECTORY_MODE
                or row.get("directory") is not True
                for row in observed.get("directories", [])
            )
            or len(observed.get("files", [])) != len(files)
            or any(
                row.get("uid") != STANDARD_WORKLOAD_UID
                or row.get("gid") != STANDARD_WORKLOAD_GID
                or row.get("mode") != AGENT_PRIVATE_FILE_MODE
                or row.get("regular") is not True
                for row in observed.get("files", [])
            )
            or workload_export
            != {
                "uid": STANDARD_WORKLOAD_UID,
                "gid": STANDARD_WORKLOAD_GID,
                "mode": AGENT_PRIVATE_DIRECTORY_MODE,
                "directory": True,
            }
        ):
            raise FullGateError(
                f"nested Agent bootstrap files are not private workload-owned objects: {observed}"
            )
        return {
            **observed,
            "host_root": host_root,
            "workload_export_root": workload_export_root,
            "workload_export": workload_export,
            "state_export_roots_disjoint": True,
            "helper_user": AGENT_CONFIG_USER,
            "privileged_setup_lifetime": "one-shot-before-agent-start",
            "agent_cap_chown": False,
            "evidence_source": "uid-65532-read-only-stat-helper",
        }

    def _install_external_providers(self) -> None:
        providers = [
            (
                "postgresql",
                "postgresql",
                "17.0.0",
                f"postgres://{self.A_IPS['postgres']}:5432",
            ),
            (
                "redis",
                "redis",
                "8.8.0",
                f"redis://{self.A_IPS['redis']}:6379",
            ),
            (
                "minio",
                "minio",
                "2025.9.7",
                f"http://{self.A_IPS['minio']}:9000",
            ),
            (
                "contract-echo-provider",
                "contract-echo-provider",
                "1.0.0",
                f"{self.A_IPS['echo']}:8080:contract-echo-provider",
            ),
            (
                "storage-head-provenance-miss-provider",
                "storage-head-provenance-miss-provider",
                "1.0.0",
                (
                    f"{self.A_IPS['storage_head_miss']}:8080:"
                    "storage-head-provenance-miss-provider"
                ),
            ),
            (
                "auth-service",
                "auth-permission-provider",
                "0.1.0",
                f"{self.A_IPS['auth']}:8081:auth-service",
            ),
        ]
        for service_id, source_id, version, endpoint in providers:
            response, _, _ = self._control_mutation(
                "/api/v1/store/releases:install",
                {
                    "service_id": service_id,
                    "catalog_source_id": source_id,
                    "version": version,
                    "channel": "stable",
                    # StoreInstallRequest keeps target_node_id as a required
                    # wire member.  External providers deliberately have no
                    # Agent placement, represented by the explicit empty value.
                    "target_node_id": "",
                    "endpoint": endpoint,
                    "mode": "EXTERNAL",
                    "start": True,
                },
                "external-" + service_id,
                expected=(202,),
            )
            data = response.get("data", {})
            operation = self._wait_operation(str(data.get("operation_id", "")), timeout=180)
            if operation.get("status") != "SUCCEEDED":
                raise FullGateError(f"External provider {service_id} did not install: {operation}")
            deployment_id = str(data.get("deployment_id", ""))
            if not deployment_id:
                raise FullGateError(f"External provider {service_id} omitted deployment_id")
            self.provider_deployments[service_id] = deployment_id
            if service_id in {"postgresql", "redis", "minio"}:
                deployment = self._control_get(
                    f"/api/v1/deployments/{deployment_id}"
                ).get("data", {}).get("deployment", {})
                instance = deployment.get("instance", {})
                image_key = {"postgresql": "postgres"}.get(service_id, service_id)
                expected_image = self.oci[image_key]
                digest = expected_image.rsplit("@", 1)[-1]
                local_repo_digest = f"127.0.0.1:5000/ojos/{image_key}@{digest}"
                running_container = {
                    "postgresql": "postgres-a",
                    "redis": "redis-a",
                    "minio": "minio-a",
                }[service_id]
                running_image_id = str(
                    json.loads(self.a.command("inspect", running_container).stdout)[0].get(
                        "Image", ""
                    )
                )
                catalog_image_id = str(
                    json.loads(self.a.command("image", "inspect", local_repo_digest).stdout)[0].get(
                        "Id", ""
                    )
                )
                if (
                    str(deployment.get("management_mode", "")).upper() != "EXTERNAL"
                    or str(instance.get("observed_state", "")).upper() != "RUNNING"
                    or str(instance.get("health", "")).upper() != "HEALTHY"
                    or instance.get("artifact_digest") != expected_image
                    or not running_image_id
                    or running_image_id != catalog_image_id
                ):
                    raise FullGateError(
                        f"External dependency {service_id} has wrong runtime identity: {deployment}"
                    )
                self.external_dependency_runtimes[service_id] = {
                    "deployment_id": deployment_id,
                    "management_mode": deployment.get("management_mode"),
                    "observed_state": instance.get("observed_state"),
                    "health": instance.get("health"),
                    "artifact_digest": instance.get("artifact_digest"),
                    "catalog_repo_digest": expected_image,
                    "running_image_id": running_image_id,
                    "catalog_image_id": catalog_image_id,
                    "protocol_health_operation_id": operation.get("operation_id"),
                    "reused_by_managed_releases": [],
                }

    def _create_and_apply_provider_topology(self) -> str:
        assert self.control_client is not None
        echo_endpoint = f"{self.A_IPS['echo']}:8080:contract-echo-provider"
        auth_endpoint = f"{self.A_IPS['auth']}:8081:auth-service"
        storage_head_miss_endpoint = (
            f"{self.A_IPS['storage_head_miss']}:8080:"
            "storage-head-provenance-miss-provider"
        )
        spec = {
            "api_version": "v1",
            "topology_id": self.topology_id,
            "root_endpoint": auth_endpoint,
            "authority": {"root_endpoint": auth_endpoint, "exposure_policy": "internal"},
            "endpoints": [
                {
                    "endpoint": echo_endpoint,
                    "service_id": "contract-echo-provider",
                    "protocol": "http",
                    "health_path": "/health",
                    "display_name": "Generic manifest provider A",
                    "note": "no Orchestrator or Gateway product-specific code",
                    "config": {
                        "deployment_id": self.provider_deployments["contract-echo-provider"]
                    },
                },
                {
                    "endpoint": auth_endpoint,
                    "service_id": "auth-service",
                    "protocol": "http",
                    "health_path": "/health",
                    "display_name": "Auth permission provider A",
                    "note": "workload JWT permission binding authority",
                    "config": {"deployment_id": self.provider_deployments["auth-service"]},
                },
                {
                    "endpoint": storage_head_miss_endpoint,
                    "service_id": "storage-head-provenance-miss-provider",
                    "protocol": "http",
                    "health_path": "/health",
                    "display_name": "Storage HEAD unproven-404 fault provider",
                    "note": (
                        "healthy compatible provider used only by the artifact-GC "
                        "failure-recovery gate"
                    ),
                    "config": {
                        "deployment_id": self.provider_deployments[
                            "storage-head-provenance-miss-provider"
                        ]
                    },
                },
            ],
            "links": [],
        }
        created, headers, _ = self._control_mutation(
            "/api/v1/topologies", spec, "topology-initial", expected=(201,)
        )
        revision = created.get("data", {}).get("revision", {})
        revision_id = str(revision.get("revision_id", ""))
        etag = str(headers.get("etag", ""))
        if not revision_id or etag != '"' + revision_id + '"':
            raise FullGateError("initial Topology did not return a strong revision ETag")
        applied, _, _ = self._control_mutation(
            f"/api/v1/topologies/{self.topology_id}:apply", {}, "topology-initial-apply",
            expected=(202,), headers={"if-match": etag},
        )
        operation = self._wait_operation(str(applied.get("data", {}).get("operation_id", "")), 180)
        if operation.get("status") != "SUCCEEDED":
            raise FullGateError(f"initial provider Topology apply failed: {operation}")
        return etag

    def _install_managed_a_services(self, topology_etag: str) -> str:
        services = [
            {
                "service_id": "storage-service",
                "port": 8085,
                "bindings": [],
                "config": {
                    "STORAGE_BACKEND": "minio",
                    "MINIO_ENDPOINT": f"{self.h.a_ip}:9000",
                    "MINIO_USE_SSL": False,
                },
                "secret_refs": {
                    "MINIO_ACCESS_KEY": "minio-access",
                    "MINIO_SECRET_KEY": "minio-secret",
                },
            },
            {
                "service_id": "problem-service",
                "port": 8083,
                "bindings": [
                    {
                        "name": "permission_check",
                        "provider_service": "auth-service",
                    },
                    {"name": "storage_put", "provider_service": "storage-service"},
                    {"name": "storage_head", "provider_service": "storage-service"},
                    {"name": "storage_delete", "provider_service": "storage-service"},
                ],
                "config": {},
                "secret_refs": {"database-url": "problem-database-url"},
            },
            {
                "service_id": "judge-api",
                "port": 8082,
                "bindings": [
                    {
                        "name": "permission_check",
                        "provider_service": "auth-service",
                    },
                    {"name": "storage_get", "provider_service": "storage-service"},
                    {"name": "storage_put", "provider_service": "storage-service"},
                    {"name": "storage_head", "provider_service": "storage-service"},
                ],
                "config": {},
                "secret_refs": {"database-url": "judge-database-url"},
            },
        ]
        current_etag = topology_etag
        for service in services:
            service_id = str(service["service_id"])
            selections = [
                {
                    "name": str(item["name"]),
                    "provider_deployment_id": self.provider_deployments[
                        str(item["provider_service"])
                    ],
                }
                for item in service["bindings"]
            ]
            base = {
                "service_id": service_id,
                "catalog_source_id": service_id,
                "version": "0.1.0",
                "channel": "stable",
                "target_node_id": "node-a",
                "endpoint": f"{self.h.a_ip}:{service['port']}:{service_id}",
                "bindings": selections,
                "topology_id": self.topology_id,
                "topology_etag": current_etag,
            }
            validate_request = {
                **base,
                "start": True,
                "migration_policy": "SKIP",
                "config": service["config"],
                "secret_refs": service["secret_refs"],
            }
            validated, _, _ = self._control_mutation(
                "/api/v1/store/releases:validate",
                validate_request,
                "managed-a-validate-" + service_id,
                expected=(200,),
            )
            validation = validated.get("data", {})
            if validation.get("valid") is not True:
                raise FullGateError(
                    f"Store rejected managed A service {service_id}: {validated}"
                )
            topology_diff = validation.get("topology_diff")
            if (
                not isinstance(topology_diff, dict)
                or not isinstance(topology_diff.get("changes"), list)
                or not topology_diff["changes"]
            ):
                raise FullGateError(
                    f"Store validation omitted the prospective Topology diff for {service_id}: "
                    f"{validated}"
                )
            installed, _, _ = self._control_mutation(
                "/api/v1/store/releases:install",
                {
                    **base,
                    "mode": "MANAGED",
                    "start": True,
                    "migration_policy": "SKIP",
                    "config": service["config"],
                    "secret_refs": service["secret_refs"],
                },
                "managed-a-install-" + service_id,
                expected=(202,),
            )
            install = installed.get("data", {})
            operation = self._wait_operation(str(install.get("operation_id", "")), timeout=420)
            if operation.get("status") != "SUCCEEDED":
                raise FullGateError(
                    f"Store/Agent managed A install failed for {service_id}: {operation}"
                )
            expected_dependencies = {
                "storage-service": ("minio",),
                "problem-service": ("postgresql",),
                "judge-api": ("postgresql", "redis"),
            }[service_id]
            step_ids = {
                str(binding.get("step_id", ""))
                for binding in operation.get("job_bindings", [])
                if isinstance(binding, Mapping)
            }
            duplicate_dependency_steps = sorted(
                step_id
                for step_id in step_ids
                if any(
                    step_id.startswith(f"install-{dependency}-")
                    for dependency in expected_dependencies
                )
            )
            if duplicate_dependency_steps:
                raise FullGateError(
                    f"managed {service_id} reinstalled healthy External dependencies: "
                    f"{duplicate_dependency_steps}"
                )
            for dependency in expected_dependencies:
                evidence = self.external_dependency_runtimes[dependency]
                external = self._control_get(
                    f"/api/v1/deployments/{evidence['deployment_id']}"
                ).get("data", {}).get("deployment", {})
                external_instance = external.get("instance", {})
                if (
                    str(external.get("management_mode", "")).upper() != "EXTERNAL"
                    or str(external_instance.get("health", "")).upper() != "HEALTHY"
                    or external_instance.get("artifact_digest")
                    != evidence["catalog_repo_digest"]
                ):
                    raise FullGateError(
                        f"managed {service_id} did not preserve External dependency "
                        f"{dependency}: {external}"
                    )
                evidence["reused_by_managed_releases"].append(service_id)
            deployment_id = str(install.get("deployment_id", ""))
            if not deployment_id:
                raise FullGateError(f"managed A install omitted deployment_id for {service_id}")
            self.provider_deployments[service_id] = deployment_id
            observed = self._inspect_managed_a_service(
                service_id,
                deployment_id,
                selections,
                int(service["port"]),
                operation,
            )
            self.managed_a_runtimes[service_id] = observed
            current_etag = self._current_topology_etag()
        return current_etag

    def _current_topology_etag(self) -> str:
        assert self.control_client is not None
        topology, headers, _ = self.control_client.request(
            "GET",
            f"/api/v1/topologies/{self.topology_id}",
            expected=(200,),
            timeout=30,
        )
        data = topology.get("data", {})
        draft = data.get("draft", {})
        heads = data.get("heads", {})
        revision_id = str(draft.get("revision_id", ""))
        applied = str(heads.get("applied_revision_id", ""))
        if not revision_id or applied != revision_id:
            raise FullGateError(
                f"Topology draft/applied head did not converge: {data}"
            )
        expected = f'"{revision_id}"'
        actual = str(headers.get("etag", ""))
        if actual != expected:
            raise FullGateError(
                "Topology GET did not return its latest strong revision ETag: "
                f"expected={expected} actual={actual!r}"
            )
        return actual

    def _inspect_managed_a_service(
        self,
        service_id: str,
        deployment_id: str,
        selections: Sequence[Mapping[str, str]],
        port: int,
        operation: Mapping[str, Any],
    ) -> dict[str, Any]:
        operation_binding = next(
            (
                item
                for item in operation.get("job_bindings", [])
                if str(item.get("step_id", "")).startswith(
                    "install-" + service_id + "-"
                )
            ),
            None,
        )
        if not isinstance(operation_binding, dict):
            raise FullGateError(
                f"managed A service {service_id} Operation has no install Job binding"
            )
        job = self._query_json(
            "ojos_orchestrator",
            "SELECT payload::text FROM orchestrator_jobs WHERE job_id="
            + self._sql(str(operation_binding.get("job_id", ""))),
        )
        lease_token = str(job.get("lease_token", ""))
        if not lease_token:
            raise FullGateError(
                f"managed A service {service_id} Job did not retain completed lease evidence"
            )
        if (
            job.get("node_id") != "node-a"
            or str(job.get("status", "")).upper() != "SUCCEEDED"
            or job.get("lease_owner") != self.a_agent_identity.get("instance_id")
        ):
            raise FullGateError(
                f"managed A service {service_id} install was not completed by node-a Agent: {job}"
            )
        runtime_convergence = self._managed_runtime_convergence(
            deployment_id, "node-a", job
        )
        deployment = runtime_convergence["inventory_payload"]
        instance = deployment["instance"]
        containers = self.a.command(
            "ps",
            "--filter",
            "label=ojos.deployment_id=" + deployment_id,
            "--format",
            "{{.ID}}",
            "--no-trunc",
        ).stdout.strip().splitlines()
        if len(containers) != 1:
            raise FullGateError(
                f"expected one Agent-created {service_id} container, got {containers}"
            )
        container_id = containers[0]
        inspected = json.loads(self.a.command("inspect", container_id).stdout)[0]
        config_user = str(inspected.get("Config", {}).get("User", ""))
        if config_user != AGENT_CONFIG_USER:
            raise FullGateError(
                f"managed A service {service_id} does not use signed standard-v3 user {AGENT_CONFIG_USER}"
            )
        process_identity = self._nested_container_process_identity(
            self.a, inspected, container_id
        )
        if (
            process_identity.get("euid") != STANDARD_WORKLOAD_UID
            or process_identity.get("egid") != STANDARD_WORKLOAD_GID
        ):
            raise FullGateError(
                f"managed A service {service_id} does not run as exact uid/gid 65532"
            )
        host_config = inspected.get("HostConfig", {})
        health = str(inspected.get("State", {}).get("Health", {}).get("Status", ""))
        if health.lower() != "healthy":
            raise FullGateError(f"managed A service {service_id} is not Docker healthy: {health}")
        image = str(inspected.get("Config", {}).get("Image", ""))
        image_key = {"problem-service": "problem", "judge-api": "judge"}.get(
            service_id, "storage"
        )
        if image != self.oci[image_key]:
            raise FullGateError(
                f"managed A service {service_id} did not use the Catalog RepoDigest: {image}"
            )
        environment = inspected.get("Config", {}).get("Env", []) or []
        names = {str(value).split("=", 1)[0] for value in environment}
        forbidden = {
            "AUTH_SERVICE_ADMIN_TOKEN",
            "AUTH_INTERNAL_TOKEN",
            "OJOS_SERVICE_TOKEN",
            "OJOS_CALLER_SERVICE",
            "OJOS_CALLER_NODE_ID",
            "REDIS_URL",
            "OJOS_STORAGE_SERVICE_URL",
            "OJOS_STORAGE_SERVICE_ENDPOINT",
            "OJOS_INTERNAL_GATEWAY_ENDPOINT",
        }
        leaked = sorted(names & forbidden)
        if leaked:
            raise FullGateError(
                f"managed A service {service_id} contains legacy/global environment: {leaked}"
            )
        bindings = self._control_get(f"/api/v1/deployments/{deployment_id}/bindings").get(
            "data", {}
        ).get("items", [])
        expected_requirements = {str(item["name"]) for item in selections}
        if _binding_names(bindings) != expected_requirements:
            raise FullGateError(
                f"managed A service {service_id} bindings do not match selection: {bindings}"
            )
        context: dict[str, Any] | None = None
        events: dict[str, Any] | None = None
        mounts = inspected.get("Mounts", []) or []
        context_mount = next(
            (item for item in mounts if item.get("Destination") == "/run/ojos/service"),
            None,
        )
        if expected_requirements or service_id in {"problem-service", "judge-api"}:
            if not context_mount or context_mount.get("RW") is not False:
                raise FullGateError(f"managed A service {service_id} has no read-only context")
            context_file = self.tmp / f"{service_id}-context.json"
            self.a.command(
                "cp", container_id + ":/run/ojos/service/context.json", context_file
            )
            context = json.loads(context_file.read_text(encoding="utf-8"))
            if context.get("deployment", {}).get("id") != deployment_id:
                raise FullGateError(f"managed A {service_id} context deployment mismatch")
            events_file = self.tmp / f"{service_id}-events.json"
            copied = self.a.command(
                "cp",
                container_id + ":/run/ojos/service/events.json",
                events_file,
                check=False,
            )
            if copied.returncode == 0:
                events = json.loads(events_file.read_text(encoding="utf-8"))
        context_file_identity: dict[str, Any] | None = None
        if context is not None:
            component = hashlib.sha256(deployment_id.encode("utf-8")).hexdigest()[:32]
            context_source = (
                f"{A_WORKLOAD_EXPORT_ROOT}/runtime-contexts/{component}/service"
            )
            if not isinstance(context_mount, Mapping) or context_mount.get(
                "Source"
            ) != context_source:
                raise FullGateError(
                    f"managed A service {service_id} context source escaped the Agent namespace"
                )
            context_file_identity = self._service_context_file_identity(
                self.a, context_source, container_id
            )
        context_required = bool(expected_requirements) or service_id in {
            "problem-service",
            "judge-api",
        }
        events_required = service_id in {"problem-service", "judge-api"}
        if events_required and not isinstance(events, dict):
            raise FullGateError(f"managed A service {service_id} has no event context")
        if context is not None:
            context_serialized = _canonical(context).lower()
            if any(
                marker in context_serialized
                for marker in ("admin_token", "management_token", "access_token")
            ):
                raise FullGateError(
                    f"managed A service {service_id} context contains a privileged token"
                )
        self._wait_managed_a_http(port)
        binding_ids = sorted(
            str(item.get("binding_id", ""))
            for item in bindings
            if item.get("binding_id")
        )
        return {
            "service_id": service_id,
            "deployment_id": deployment_id,
            "node_id": "node-a",
            "created_by_agent": True,
            "container_id": container_id,
            "config_user": config_user,
            "process_identity": process_identity,
            "image_repo_digest": image.split("@", 1)[-1],
            "host_config_digest": _sha256(_canonical(host_config)),
            "engine_id": self.h.evidence["engines"]["a"]["engine_id"],
            "desired_state": instance.get("desired_state"),
            "observed_state": instance.get("observed_state"),
            "health": instance.get("health"),
            "runtime_attested": instance.get("runtime_attested"),
            "drift_reason": deployment.get("drift_reason"),
            "last_observed_at_ms": deployment.get("last_observed_at_ms"),
            "runtime_projection": runtime_convergence,
            "operation_id": operation.get("operation_id"),
            "operation_status": operation.get("status"),
            "agent_job": {
                "job_id": job.get("job_id"),
                "attempt_id": f"{job.get('job_id')}:attempt:{job.get('attempt')}",
                "lease_id": _sha256(lease_token),
                "lease_owner_instance_id": job.get("lease_owner"),
                "status": job.get("status"),
                "completed_by_agent": True,
            },
            "bindings": bindings,
            "binding_requirements": sorted(expected_requirements),
            "service_context": {
                "required": context_required,
                "present": context is not None,
                "generation": context.get("generation") if context else None,
                "binding_ids": binding_ids,
                "mount_read_only": context_mount is not None,
                "credential_embedded": False,
                "management_token_present": False,
                "file_identity": context_file_identity,
            },
            "event_context": {
                "required": events_required,
                "present": events is not None,
                "generation": events.get("generation") if events else None,
                "connection_id": events.get("connection_id") if events else None,
                "stream": events.get("stream") if events else None,
                "publish_types": events.get("publish_types", []) if events else [],
                "subscriptions": events.get("subscriptions", []) if events else [],
                "connection_secret_recorded": False,
            },
            "legacy_environment_present": False,
        }

    def _wait_managed_a_http(self, port: int, timeout: float = 120) -> None:
        deadline = time.monotonic() + timeout
        last = ""
        while time.monotonic() < deadline:
            result = self.root.command(
                "exec",
                self.h.a_name,
                "wget",
                "-qO-",
                f"http://127.0.0.1:{port}/health",
                timeout=5,
                check=False,
            )
            if result.returncode == 0:
                return
            last = result.stderr or result.stdout
            time.sleep(1)
        raise FullGateError(f"managed A service port {port} did not become ready: {last[-1000:]}")

    def _managed_service_context_source(
        self, service_id: str, deployment_id: str
    ) -> tuple[str, str]:
        """Resolve the live Agent-owned Service Context directory on Engine A."""

        containers = self.a.command(
            "ps",
            "--filter",
            "label=ojos.deployment_id=" + deployment_id,
            "--format",
            "{{.ID}}",
            "--no-trunc",
        ).stdout.strip().splitlines()
        if len(containers) != 1:
            raise FullGateError(
                f"expected one Agent-created {service_id} container, got {containers}"
            )
        container_id = containers[0]
        if not re.fullmatch(r"[a-f0-9]{64}", container_id):
            raise FullGateError(
                f"managed {service_id} returned a non-canonical container ID"
            )
        inspected = json.loads(self.a.command("inspect", container_id).stdout)[0]
        mount = next(
            (
                item
                for item in inspected.get("Mounts", []) or []
                if item.get("Destination") == "/run/ojos/service"
            ),
            None,
        )
        component = hashlib.sha256(deployment_id.encode("utf-8")).hexdigest()[:32]
        expected_source = (
            f"{A_WORKLOAD_EXPORT_ROOT}/runtime-contexts/{component}/service"
        )
        if (
            not isinstance(mount, Mapping)
            or str(mount.get("Type", "")).lower() != "bind"
            or mount.get("RW") is not False
            or mount.get("Source") != expected_source
        ):
            raise FullGateError(
                f"managed {service_id} has no read-only Agent Service Context bind mount"
            )
        return expected_source, container_id

    def _service_context_file_identity(
        self,
        engine: Any,
        context_source: str,
        workload_container_id: str,
        expected_workload_user: str = AGENT_CONFIG_USER,
    ) -> dict[str, Any]:
        """Prove private Agent files are workload-owned and actually readable."""

        names = ["context.json", "token", "ca.pem"]
        program = (
            "import hashlib,json,os,pathlib,stat;root=pathlib.Path('/context');"
            "rows=[];"
            "[(lambda p,s,b:rows.append({'path':n,'uid':s.st_uid,'gid':s.st_gid,"
            "'mode':format(stat.S_IMODE(s.st_mode),'04o'),'regular':p.is_file(),"
            "'size':len(b),'sha256':hashlib.sha256(b).hexdigest()}))"
            "(root.joinpath(n),root.joinpath(n).stat(),root.joinpath(n).read_bytes()) "
            "for n in json.loads(os.environ['OJOS_CONTEXT_FILES'])];"
            "s=root.stat();print(json.dumps({'directory':{'uid':s.st_uid,'gid':s.st_gid,"
            "'mode':format(stat.S_IMODE(s.st_mode),'04o'),'directory':root.is_dir()},"
            "'files':rows},sort_keys=True))"
        )
        observed = _json_from_last_line(
            engine.command(
                "run",
                "--rm",
                "--user",
                AGENT_CONFIG_USER,
                "--network",
                "container:" + workload_container_id,
                "--mount",
                f"type=bind,source={context_source},target=/context,readonly",
                "--env",
                "OJOS_CONTEXT_FILES=" + _canonical(names),
                "--entrypoint",
                "python",
                self.secure_fixture_image,
                "-c",
                program,
                timeout=60,
            ).stdout
        )
        directory = observed.get("directory", {})
        rows = observed.get("files", [])
        inspected = json.loads(engine.command("inspect", workload_container_id).stdout)[0]
        actual_user = str(inspected.get("Config", {}).get("User", ""))
        if actual_user != expected_workload_user:
            raise FullGateError(
                "ServiceContext readability proof observed the wrong actual workload identity"
            )
        workload_process = self._nested_container_process_identity(
            engine, inspected, workload_container_id
        )
        expected_uid, expected_gid = (
            int(value) for value in expected_workload_user.split(":", 1)
        )
        if (
            workload_process.get("euid") != expected_uid
            or workload_process.get("egid") != expected_gid
        ):
            raise FullGateError(
                "ServiceContext workload process identity differs from its signed runtime user"
            )
        for name in names:
            engine.command(
                "exec",
                workload_container_id,
                "test",
                "-r",
                "/run/ojos/service/" + name,
                timeout=30,
            )
        if (
            directory
            != {
                "uid": STANDARD_WORKLOAD_UID,
                "gid": STANDARD_WORKLOAD_GID,
                "mode": AGENT_PRIVATE_DIRECTORY_MODE,
                "directory": True,
            }
            or len(rows) != len(names)
            or {row.get("path") for row in rows} != set(names)
            or any(
                row.get("uid") != STANDARD_WORKLOAD_UID
                or row.get("gid") != STANDARD_WORKLOAD_GID
                or row.get("mode") != AGENT_PRIVATE_FILE_MODE
                or row.get("regular") is not True
                or not re.fullmatch(r"[0-9a-f]{64}", str(row.get("sha256", "")))
                for row in rows
            )
        ):
            raise FullGateError(
                "Agent ServiceContext is not exact 65532:65532 0700/0600 workload-readable material"
            )
        return {
            **observed,
            "source": context_source,
            "mounted_read_only": True,
            "reader_user": AGENT_CONFIG_USER,
            "actual_workload_config_user": actual_user,
            "actual_workload_process": workload_process,
            "actual_workload_files_readable": names,
            "reader_network_namespace": "container:" + workload_container_id,
            "evidence_source": "fresh-standard-workload-read-and-stat",
        }

    def _binding_head_probe(
        self,
        context_source: str,
        relative_path: str,
        deployment_id: str,
        network_container_id: str,
    ) -> dict[str, Any]:
        """Run one workload-authenticated storage HEAD from Engine A."""

        result = self.a.command(
            "run",
            "--rm",
            "--network",
            "container:" + network_container_id,
            "--mount",
            "type=bind,source="
            + context_source
            + ",target=/run/ojos/service,readonly",
            self.fixture_image,
            "binding-head",
            "--context",
            "/run/ojos/service/context.json",
            "--binding",
            "storage_head",
            "--relative-path",
            relative_path,
            "--deployment",
            deployment_id,
            timeout=30,
        )
        probe = _json_from_last_line(result.stdout)
        if set(probe) != {
            "status",
            "sha256_header",
            "size_bytes",
            "storage_result_header",
        }:
            raise FullGateError(
                "binding HEAD helper returned a non-canonical result: "
                + _canonical(probe)
            )
        if (
            isinstance(probe.get("status"), bool)
            or not isinstance(probe.get("status"), int)
            or not isinstance(probe.get("sha256_header"), str)
            or isinstance(probe.get("size_bytes"), bool)
            or not isinstance(probe.get("size_bytes"), int)
            or not isinstance(probe.get("storage_result_header"), str)
        ):
            raise FullGateError(
                "binding HEAD helper returned invalid response metadata: "
                + _canonical(probe)
            )
        return probe

    def _problem_artifact_gc_intents(self, status: str) -> list[dict[str, Any]]:
        """Read one complete operator-visible intent state without DB access."""

        assert self.gateway_client is not None
        normalized_status = status.strip().upper()
        if normalized_status not in {"PENDING", "DELETING", "NEEDS_ATTENTION"}:
            raise FullGateError(f"invalid artifact GC intent status {status!r}")
        cursor = ""
        seen_cursors: set[str] = set()
        result: list[dict[str, Any]] = []
        while True:
            query = {"status": normalized_status, "limit": "200"}
            if cursor:
                query["cursor"] = cursor
            response, _, _ = self.gateway_client.request(
                "GET",
                "/api/problem/admin/artifact-gc/intents?"
                + urllib.parse.urlencode(query),
                headers={"authorization": "Bearer " + self._ensure_admin_token()},
                expected=(200,),
                timeout=30,
            )
            items = response.get("intents")
            next_cursor = response.get("next_cursor", "")
            if (
                not {"intents"}.issubset(response)
                or not set(response).issubset({"intents", "next_cursor"})
                or not isinstance(items, list)
                or not isinstance(next_cursor, str)
            ):
                raise FullGateError(
                    "artifact GC operator list returned a non-canonical document: "
                    + _canonical(response)
                )
            for item in items:
                if not isinstance(item, Mapping):
                    raise FullGateError("artifact GC operator list contains a non-object")
                normalized = dict(item)
                required_fields = {
                    "artifact_uri",
                    "artifact_sha256",
                    "artifact_size_bytes",
                    "status",
                    "failure_count",
                    "last_failure",
                    "updated_at",
                }
                optional_fields = {
                    "upload_completed_at",
                    "needs_attention_at",
                    "manual_reconcile_requested_at",
                    "last_operator_retry_reason",
                    "last_operator_retry_at",
                }
                if (
                    not required_fields.issubset(normalized)
                    or not set(normalized).issubset(required_fields | optional_fields)
                ):
                    raise FullGateError(
                        "artifact GC operator list returned a non-canonical intent: "
                        + _canonical(normalized)
                    )
                last_failure = normalized.get("last_failure")
                required_failure_fields = {
                    "message",
                    "stage",
                    "kind",
                    "deterministic",
                }
                optional_failure_fields = {"http_status", "provider_result"}
                if (
                    not isinstance(last_failure, Mapping)
                    or not required_failure_fields.issubset(last_failure)
                    or not set(last_failure).issubset(
                        required_failure_fields | optional_failure_fields
                    )
                ):
                    raise FullGateError(
                        "artifact GC operator list returned a non-canonical failure"
                    )
                if (
                    not str(normalized.get("artifact_uri", "")).startswith("storage://")
                    or not re.fullmatch(
                        r"[a-f0-9]{64}",
                        str(normalized.get("artifact_sha256", "")),
                    )
                    or isinstance(normalized.get("artifact_size_bytes"), bool)
                    or not isinstance(normalized.get("artifact_size_bytes"), int)
                    or normalized["artifact_size_bytes"] < 0
                    or isinstance(normalized.get("failure_count"), bool)
                    or not isinstance(normalized.get("failure_count"), int)
                    or normalized["failure_count"] < 0
                    or not _rfc3339_timestamp(normalized.get("updated_at"))
                    or not all(
                        isinstance(last_failure.get(field), str)
                        for field in ("message", "stage", "kind")
                    )
                    or not isinstance(last_failure.get("deterministic"), bool)
                ):
                    raise FullGateError(
                        "artifact GC operator list returned invalid intent metadata"
                    )
                for field in (
                    "upload_completed_at",
                    "needs_attention_at",
                    "manual_reconcile_requested_at",
                    "last_operator_retry_at",
                ):
                    if field in normalized and not _rfc3339_timestamp(
                        normalized[field]
                    ):
                        raise FullGateError(
                            f"artifact GC operator list returned invalid {field}"
                        )
                if (
                    "last_operator_retry_reason" in normalized
                    and not str(normalized["last_operator_retry_reason"]).strip()
                ):
                    raise FullGateError(
                        "artifact GC operator list returned an empty optional retry reason"
                    )
                if (
                    "http_status" in last_failure
                    and (
                        isinstance(last_failure["http_status"], bool)
                        or not isinstance(last_failure["http_status"], int)
                        or not 100 <= last_failure["http_status"] <= 599
                    )
                ):
                    raise FullGateError(
                        "artifact GC operator list returned an invalid failure HTTP status"
                    )
                if (
                    "provider_result" in last_failure
                    and not str(last_failure["provider_result"]).strip()
                ):
                    raise FullGateError(
                        "artifact GC operator list returned an empty provider result"
                    )
                if str(normalized.get("status", "")).upper() != normalized_status:
                    raise FullGateError(
                        "artifact GC operator list returned an item outside its status filter"
                    )
                if any("claim_token" in str(key).lower() for key in normalized):
                    raise FullGateError("artifact GC operator API exposed a claim token")
                result.append(normalized)
            if not next_cursor:
                break
            if next_cursor in seen_cursors or len(result) > 2000:
                raise FullGateError("artifact GC operator pagination did not converge")
            seen_cursors.add(next_cursor)
            cursor = next_cursor
        result.sort(key=lambda item: str(item.get("artifact_uri", "")))
        return result

    def _problem_artifact_gc_intent(self, uri: str) -> dict[str, Any] | None:
        # The three filtered reads cannot share a database snapshot. An intent
        # may legitimately move PENDING -> DELETING -> NEEDS_ATTENTION between
        # them, so seeing the same URI in two successive state snapshots is a
        # read race rather than an API duplicate. Duplicates inside one filter
        # remain invalid; across filters prefer the furthest observed state.
        matches: list[dict[str, Any]] = []
        for status in ("PENDING", "DELETING", "NEEDS_ATTENTION"):
            state_matches = [
                item
                for item in self._problem_artifact_gc_intents(status)
                if item.get("artifact_uri") == uri
            ]
            if len(state_matches) > 1:
                raise FullGateError(
                    f"artifact GC operator API duplicated intent {uri} in {status}"
                )
            matches.extend(state_matches)
        if not matches:
            return None
        precedence = {"PENDING": 0, "DELETING": 1, "NEEDS_ATTENTION": 2}
        return max(
            matches,
            key=lambda item: precedence.get(str(item.get("status", "")).upper(), -1),
        )

    def _wait_problem_artifact_gc_intent(
        self,
        uri: str,
        expected_status: str | None,
        *,
        timeout: float = 60,
    ) -> dict[str, Any] | None:
        deadline = time.monotonic() + timeout
        latest: dict[str, Any] | None = None
        while time.monotonic() < deadline:
            latest = self._problem_artifact_gc_intent(uri)
            if expected_status is None and latest is None:
                return None
            if (
                latest is not None
                and str(latest.get("status", "")).upper() == expected_status
            ):
                return latest
            time.sleep(0.25)
        raise FullGateError(
            f"artifact GC intent {uri} did not reach "
            f"{expected_status or 'ABSENT'}: {latest}"
        )

    def _problem_artifact_gc_action(
        self,
        action: str,
        body: Mapping[str, Any],
        key: str,
    ) -> dict[str, Any]:
        """Submit and replay one idempotent exact-target operator action."""

        assert self.gateway_client is not None
        if action not in {"reconcile", "retry"}:
            raise FullGateError(f"unsupported artifact GC operator action {action}")
        idempotency_key = self.h.run_id + "-artifact-gc-" + key
        headers = {
            "authorization": "Bearer " + self._ensure_admin_token(),
            "idempotency-key": idempotency_key,
        }
        path = f"/api/problem/admin/artifact-gc/intents:{action}"
        first, _, first_status = self.gateway_client.request(
            "POST", path, dict(body), headers=headers, expected=(202,), timeout=30
        )
        replay, _, replay_status = self.gateway_client.request(
            "POST", path, dict(body), headers=headers, expected=(202,), timeout=30
        )
        action_id = str(first.get("action_id", ""))
        request_id = str(first.get("request_id", ""))
        expected_from = "NEEDS_ATTENTION" if action == "retry" else "PENDING"
        expected_to = "PENDING"
        response_fields = {
            "action_id",
            "request_id",
            "artifact_uri",
            "queued",
            "idempotent_replay",
            "from_status",
            "to_status",
            "reason_recorded",
        }
        if (
            set(first) != response_fields
            or set(replay) != response_fields
            or not action_id
            or not request_id
            or first.get("artifact_uri") != body.get("artifact_uri")
            or first.get("queued") is not True
            or first.get("idempotent_replay") is not False
            or str(first.get("from_status", "")).upper() != expected_from
            or str(first.get("to_status", "")).upper() != expected_to
            or first.get("reason_recorded") is not True
            or str(replay.get("action_id", "")) != action_id
            or replay.get("request_id") != request_id
            or replay.get("artifact_uri") != body.get("artifact_uri")
            or replay.get("queued") is not True
            or replay.get("idempotent_replay") is not True
            or str(replay.get("from_status", "")).upper() != expected_from
            or str(replay.get("to_status", "")).upper() != expected_to
            or replay.get("reason_recorded") is not True
        ):
            raise FullGateError(
                f"artifact GC {action} idempotency contract failed: "
                f"first={first} replay={replay}"
            )
        result = {
            "endpoint": path,
            "first_http_status": first_status,
            "replay_http_status": replay_status,
            "action_id": action_id,
            "request_id": request_id,
            "artifact_uri": body.get("artifact_uri"),
            "queued": True,
            "first_request_replay": False,
            "duplicate_request_replay": True,
            "duplicate_action_id_matched": True,
            "duplicate_request_id_matched": True,
            "idempotency_key_used": True,
            "idempotency_key_recorded": False,
            "reason_recorded": True,
            "from_status": expected_from,
            "to_status": expected_to,
        }
        return result

    def _wait_deployment_binding_state(
        self,
        deployment_id: str,
        requirement_name: str,
        desired_state: str,
        *,
        provider_deployment_id: str | None = None,
        timeout: float = 60,
    ) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        latest: dict[str, Any] = {}
        while time.monotonic() < deadline:
            items = self._control_get(
                f"/api/v1/deployments/{deployment_id}/bindings"
            ).get("data", {}).get("items", [])
            if isinstance(items, list):
                matches = [
                    dict(item)
                    for item in items
                    if isinstance(item, Mapping)
                    and item.get("requirement_name") == requirement_name
                ]
                if len(matches) == 1:
                    latest = matches[0]
                    if (
                        str(latest.get("desired_state", "")).upper() == desired_state
                        and (
                            provider_deployment_id is None
                            or latest.get("provider_deployment_id")
                            == provider_deployment_id
                        )
                    ):
                        return latest
            time.sleep(0.25)
        raise FullGateError(
            f"deployment binding {requirement_name} did not reach {desired_state}"
            f" via {provider_deployment_id or 'any provider'}: {latest}"
        )

    def _prove_problem_artifact_gc_failure_recovery(self) -> str:
        """Prove route-failure quarantine and explicit operator recovery."""

        assert self.gateway_client is not None
        deployment_id = str(
            self.managed_a_runtimes.get("problem-service", {}).get(
                "deployment_id", ""
            )
        )
        if not deployment_id:
            raise FullGateError("artifact GC proof requires managed Problem deployment")
        auth = {"authorization": "Bearer " + self._ensure_admin_token()}
        baseline = self._problem_artifact_gc_intents("PENDING")
        baseline_uris = {
            str(item.get("artifact_uri", "")) for item in baseline
        }
        request_marker = "artifact-gc-recovery-" + self.h.run_id
        problem_no = ("GC" + self.h.run_id.replace("-", ""))[:32]
        seed, _, seed_status = self.gateway_client.request(
            "POST",
            "/api/problem/problems",
            {
                "problem_no": problem_no,
                "title": "Artifact GC seed " + self.h.run_id,
                "slug": request_marker + "-seed",
                "statement": "This committed seed owns the duplicate problem number.",
                "visibility": "private",
                "time_limit_ms": 1000,
                "memory_limit_mb": 64,
            },
            headers=auth,
            expected=(200,),
        )
        seed_problem_id = int(seed.get("problem_id", 0))
        if (
            seed_status != 200
            or seed_problem_id <= 0
            or seed.get("problem_no") != problem_no
        ):
            raise FullGateError(f"artifact GC seed Problem was not committed: {seed}")
        _, _, failure_status = self.gateway_client.request(
            "POST",
            "/api/problem/problems",
            {
                "problem_no": problem_no,
                "title": "Artifact GC natural conflict " + self.h.run_id,
                "slug": request_marker + "-conflict",
                "statement": (
                    "This request uploads immutable package and content objects, then "
                    "fails on the real unique problem_no constraint."
                ),
                "visibility": "private",
                "time_limit_ms": 1000,
                "memory_limit_mb": 64,
            },
            headers=auth,
            expected=(500,),
        )

        raw_intents: list[dict[str, Any]] = []
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            pending = self._problem_artifact_gc_intents("PENDING")
            raw_intents = [
                item
                for item in pending
                if str(item.get("artifact_uri", "")) not in baseline_uris
            ]
            kinds = " ".join(str(item.get("artifact_uri", "")) for item in raw_intents)
            if "package-sha256-" in kinds and "-objects-sha256-" in kinds:
                break
            time.sleep(0.25)
        intents: list[dict[str, Any]] = []
        prefix = "storage://problems/"
        if failure_status != 500 or not 1 <= len(raw_intents) <= 100:
            raise FullGateError(
                "natural duplicate problem_no failure did not leave a bounded nonempty "
                f"operator-visible intent set: failure_status={failure_status} "
                f"baseline={baseline} observed={raw_intents}"
            )
        intent_uris: list[str] = []
        package_intent_count = 0
        content_intent_count = 0
        for raw_intent in raw_intents:
            uri = str(raw_intent.get("artifact_uri", ""))
            digest = str(raw_intent.get("artifact_sha256", ""))
            raw_size = raw_intent.get("artifact_size_bytes")
            size = int(raw_size) if isinstance(raw_size, int) and not isinstance(raw_size, bool) else -1
            key = uri[len(prefix) :] if uri.startswith(prefix) else ""
            package_key = re.fullmatch(
                rf"package-sha256-{re.escape(digest)}\.zip", key
            )
            content_key = re.fullmatch(
                rf"problem-[1-9][0-9]*-objects-sha256-{re.escape(digest)}", key
            )
            if (
                uri in intent_uris
                or not re.fullmatch(r"[a-f0-9]{64}", digest)
                or (package_key is None and content_key is None)
                or (package_key is not None and size <= 0)
                or (content_key is not None and size < 0)
                or str(raw_intent.get("status", "")).upper() != "PENDING"
                or int(raw_intent.get("failure_count", -1)) != 0
                or not str(raw_intent.get("upload_completed_at", "")).strip()
                or not str(raw_intent.get("updated_at", "")).strip()
            ):
                raise FullGateError(
                    "natural Problem conflict left an invalid upload intent: "
                    + _canonical(raw_intent)
                )
            kind = "package" if package_key is not None else "content"
            if package_key is not None:
                package_intent_count += 1
            else:
                content_intent_count += 1
            intents.append(
                {
                    "artifact_uri": uri,
                    "sha256": digest,
                    "size_bytes": size,
                    "kind": kind,
                    "initial_status": "PENDING",
                    "upload_completed_at": str(raw_intent["upload_completed_at"]),
                    "relative_path": "/" + uri[len("storage://") :],
                }
            )
            intent_uris.append(uri)
        if package_intent_count < 1 or content_intent_count < 1:
            raise FullGateError(
                "natural Problem conflict did not expose both package and content "
                f"upload intents: package={package_intent_count} content={content_intent_count}"
            )
        intents.sort(key=lambda item: str(item["artifact_uri"]))

        context_source, problem_container_id = self._managed_service_context_source(
            "problem-service", deployment_id
        )
        for intent in intents:
            before_probe = self._binding_head_probe(
                context_source,
                str(intent["relative_path"]),
                deployment_id,
                problem_container_id,
            )
            if (
                before_probe["status"] != 200
                or before_probe["sha256_header"] != intent["sha256"]
                or before_probe["size_bytes"] != intent["size_bytes"]
                or str(before_probe["storage_result_header"]).lower() != "present"
            ):
                raise FullGateError(
                    "natural-conflict proof did not observe an exact uploaded object: "
                    f"probe={before_probe} intent={intent}"
                )
            intent["head_before"] = before_probe

        target = next(item for item in intents if item["kind"] == "content")
        topology = self._control_get(f"/api/v1/topologies/{self.topology_id}")
        topology_data = topology.get("data", {})
        draft = topology_data.get("draft", {})
        original_revision = str(draft.get("revision_id", ""))
        original_spec = copy.deepcopy(draft.get("spec"))
        if (
            not original_revision
            or topology_data.get("heads", {}).get("applied_revision_id")
            != original_revision
            or not isinstance(original_spec, dict)
        ):
            raise FullGateError(
                "artifact GC proof requires a converged applied Topology draft"
            )
        context_before = self._container_json(
            self.a,
            problem_container_id,
            "/run/ojos/service/context.json",
            "artifact-gc-context-before-fault-provider",
        )
        generation_before = int(context_before.get("generation", 0))
        storage_provider_id = self.provider_deployments.get("storage-service", "")
        fault_provider_id = self.provider_deployments.get(
            "storage-head-provenance-miss-provider", ""
        )
        expected_problem_bindings = [
            "permission_check",
            "storage_delete",
            "storage_head",
            "storage_put",
        ]

        def context_binding_proof(
            context: Mapping[str, Any],
            durable_binding: Mapping[str, Any],
            expected_provider_id: str,
            phase: str,
        ) -> dict[str, Any]:
            bindings = context.get("bindings", {})
            names = sorted(bindings) if isinstance(bindings, Mapping) else []
            storage_head = (
                bindings.get("storage_head", {})
                if isinstance(bindings, Mapping)
                else {}
            )
            if (
                names != expected_problem_bindings
                or not isinstance(storage_head, Mapping)
                or storage_head.get("api_id") != "storage.object.head"
                or not str(storage_head.get("binding_id", "")).strip()
                or storage_head.get("binding_id")
                != durable_binding.get("binding_id")
                or durable_binding.get("provider_deployment_id")
                != expected_provider_id
                or str(durable_binding.get("desired_state", "")).upper()
                != "ACTIVE"
                or str(durable_binding.get("state", "")).upper() != "ACTIVE"
            ):
                raise FullGateError(
                    f"artifact GC {phase} context did not preserve every required "
                    "Problem binding or did not correlate storage_head to the selected "
                    f"provider: context={_canonical(context)} "
                    f"durable={_canonical(durable_binding)}"
                )
            return {
                "source": "agent-materialized-service-context+deployment-binding-api",
                "required_binding_names": names,
                "required_bindings_complete": names == expected_problem_bindings,
                "storage_head_binding_id": storage_head.get("binding_id"),
                "storage_head_api_id": storage_head.get("api_id"),
                "storage_head_provider_deployment_id": durable_binding.get(
                    "provider_deployment_id"
                ),
                "binding_desired_state": durable_binding.get("desired_state"),
                "binding_observed_state": durable_binding.get("state"),
                "context_generation": int(context.get("generation", 0)),
            }

        initial_binding = self._wait_deployment_binding_state(
            deployment_id,
            "storage_head",
            "ACTIVE",
            provider_deployment_id=storage_provider_id,
        )
        initial_context_proof = context_binding_proof(
            context_before,
            initial_binding,
            storage_provider_id,
            "initial",
        )
        faulted_spec, fault_metadata = _rebind_topology_requirement(
            original_spec,
            consumer_deployment_id=deployment_id,
            requirement_name="storage_head",
            old_provider_deployment_id=storage_provider_id,
            new_provider_deployment_id=fault_provider_id,
            require_same_service=False,
        )
        if (
            fault_metadata.get("api_id") != "storage.object.head"
            or fault_metadata.get("old_provider_deployment_id")
            != storage_provider_id
            or fault_metadata.get("new_provider_deployment_id")
            != fault_provider_id
        ):
            raise FullGateError(
                "artifact GC proof resolved the wrong storage_head provider: "
                + _canonical(fault_metadata)
            )
        fault_provider = self._control_get(
            f"/api/v1/deployments/{fault_provider_id}"
        ).get("data", {}).get("deployment", {})
        fault_instance = (
            fault_provider.get("instance", {})
            if isinstance(fault_provider, Mapping)
            else {}
        )
        if (
            str(fault_provider.get("management_mode", "")).upper() != "EXTERNAL"
            or str(fault_instance.get("observed_state", "")).upper() != "RUNNING"
            or str(fault_instance.get("health", "")).upper() != "HEALTHY"
        ):
            raise FullGateError(
                "artifact GC fault provider was not a healthy External deployment: "
                + _canonical(fault_provider)
            )
        fault_operation, fault_revision = self._apply_topology_spec(
            faulted_spec,
            original_revision,
            "problem-artifact-gc-storage-head-fault-provider",
        )
        faulted_context = self._wait_container_context_generation(
            self.a,
            problem_container_id,
            generation_before,
            "artifact-gc-context-fault-provider",
        )
        generation_faulted = int(faulted_context.get("generation", 0))
        faulted_binding = self._wait_deployment_binding_state(
            deployment_id,
            "storage_head",
            "ACTIVE",
            provider_deployment_id=fault_provider_id,
        )
        faulted_context_proof = context_binding_proof(
            faulted_context,
            faulted_binding,
            fault_provider_id,
            "fault-provider",
        )
        fault_binding_probe = self._binding_head_probe(
            context_source,
            str(target["relative_path"]),
            deployment_id,
            problem_container_id,
        )
        if (
            fault_binding_probe.get("status") != 404
            or str(fault_binding_probe.get("storage_result_header", ""))
        ):
            raise FullGateError(
                "compatible fault provider did not return the required unproven 404: "
                + _canonical(fault_binding_probe)
            )

        reconcile_reason = (
            "release gate: prove a compatible healthy provider's unproven HEAD 404 "
            "quarantines the exact intent"
        )
        reconcile_action = self._problem_artifact_gc_action(
            "reconcile",
            {
                "artifact_uri": target["artifact_uri"],
                "artifact_sha256": target["sha256"],
                "artifact_size_bytes": target["size_bytes"],
                "reason": reconcile_reason,
            },
            "provider-unproven-404-reconcile",
        )
        needs_attention = self._wait_problem_artifact_gc_intent(
            str(target["artifact_uri"]), "NEEDS_ATTENTION"
        )
        assert needs_attention is not None
        last_failure = needs_attention.get("last_failure")
        if (
            not isinstance(last_failure, Mapping)
            or set(last_failure)
            != {
                "message",
                "stage",
                "kind",
                "http_status",
                "provider_result",
                "deterministic",
            }
            or last_failure.get("stage") != "inspect"
            or last_failure.get("kind") != "PROVIDER_HTTP"
            or last_failure.get("http_status") != 404
            or last_failure.get("provider_result") != "HTTP_404"
            or last_failure.get("deterministic") is not True
            or int(needs_attention.get("failure_count", 0)) < 1
            or needs_attention.get("upload_completed_at")
            != target["upload_completed_at"]
            or not str(needs_attention.get("needs_attention_at", "")).strip()
            or str(needs_attention.get("manual_reconcile_requested_at", "")).strip()
        ):
            raise FullGateError(
                "Gateway route 404 did not produce a structured NEEDS_ATTENTION state: "
                + _canonical(needs_attention)
            )

        fault_provider_path = (
            "/api/storage/objects" + str(target["relative_path"])
        )
        fault_provider_logs = self.a.command(
            "logs",
            "--since",
            "5m",
            "storage-head-provenance-miss-a",
            timeout=30,
            check=False,
        ).stdout
        fault_provider_head_observed = (
            f'HEAD {fault_provider_path} HTTP/1.1" 404' in fault_provider_logs
        )
        if not fault_provider_head_observed:
            raise FullGateError(
                "artifact GC failure request did not reach the selected fault provider: "
                + fault_provider_path
            )

        restored_spec, restore_metadata = _rebind_topology_requirement(
            faulted_spec,
            consumer_deployment_id=deployment_id,
            requirement_name="storage_head",
            old_provider_deployment_id=fault_provider_id,
            new_provider_deployment_id=storage_provider_id,
            require_same_service=False,
        )
        restore_operation, restore_revision = self._apply_topology_spec(
            restored_spec,
            fault_revision,
            "problem-artifact-gc-storage-head-restore",
        )
        restored_context = self._wait_container_context_generation(
            self.a,
            problem_container_id,
            generation_faulted,
            "artifact-gc-context-restored",
        )
        generation_restored = int(restored_context.get("generation", 0))
        restored_binding = self._wait_deployment_binding_state(
            deployment_id,
            "storage_head",
            "ACTIVE",
            provider_deployment_id=storage_provider_id,
        )
        restored_context_proof = context_binding_proof(
            restored_context,
            restored_binding,
            storage_provider_id,
            "restored",
        )
        object_before_retry = self._binding_head_probe(
            context_source,
            str(target["relative_path"]),
            deployment_id,
            problem_container_id,
        )
        if (
            object_before_retry.get("status") != 200
            or object_before_retry.get("sha256_header") != target["sha256"]
            or object_before_retry.get("size_bytes") != target["size_bytes"]
            or str(object_before_retry.get("storage_result_header", "")).lower()
            != "present"
        ):
            raise FullGateError(
                "Gateway route 404 cleared or changed the quarantined object: "
                + _canonical(object_before_retry)
            )

        operator_reason = "release gate: storage_head binding restored; retry exact quarantined intent"
        failure_count = int(needs_attention["failure_count"])
        retry_action = self._problem_artifact_gc_action(
            "retry",
            {
                "artifact_uri": target["artifact_uri"],
                "expected_failure_count": failure_count,
                "reason": operator_reason,
            },
            "operator-retry",
        )
        self._wait_problem_artifact_gc_intent(str(target["artifact_uri"]), None)
        actions_by_uri: dict[str, dict[str, Any]] = {
            str(target["artifact_uri"]): retry_action
        }
        for intent in intents:
            uri = str(intent["artifact_uri"])
            if uri == target["artifact_uri"]:
                continue
            action = self._problem_artifact_gc_action(
                "reconcile",
                {
                    "artifact_uri": uri,
                    "artifact_sha256": intent["sha256"],
                    "artifact_size_bytes": intent["size_bytes"],
                    "reason": "release gate: reclaim exact natural-conflict orphan",
                },
                "positive-reconcile-" + hashlib.sha256(uri.encode()).hexdigest()[:12],
            )
            actions_by_uri[uri] = action
            self._wait_problem_artifact_gc_intent(uri, None)

        remaining_uris = sorted(
            uri for uri in intent_uris if self._problem_artifact_gc_intent(uri) is not None
        )
        for intent in intents:
            uri = str(intent["artifact_uri"])
            after_probe = self._binding_head_probe(
                context_source,
                str(intent["relative_path"]),
                deployment_id,
                problem_container_id,
            )
            if (
                after_probe.get("status") != 404
                or str(after_probe.get("storage_result_header", "")).lower()
                != "object-not-found"
            ):
                raise FullGateError(
                    "operator recovery did not produce authoritative object absence: "
                    f"intent={intent} probe={after_probe}"
                )
            intent["recovery_action"] = (
                "retry" if uri == target["artifact_uri"] else "reconcile"
            )
            intent["recovery_action_id"] = actions_by_uri[uri]["action_id"]
            intent["head_after"] = after_probe

        gateway_logs = self.a.command(
            "logs", "--since", "10m", "gateway-a", timeout=30, check=False
        ).stdout
        expected_head_paths = sorted(
            "/internal/apis/storage.object.head" + str(intent["relative_path"])
            for intent in intents
        )
        expected_delete_paths = sorted(
            "/internal/apis/storage.object.delete" + str(intent["relative_path"])
            for intent in intents
        )
        observed_head_paths = sorted(
            path for path in expected_head_paths if path in gateway_logs
        )
        observed_delete_paths = sorted(
            path for path in expected_delete_paths if path in gateway_logs
        )
        binding_delete_observed = observed_delete_paths == expected_delete_paths
        binding_head_observed = observed_head_paths == expected_head_paths
        if (
            remaining_uris
            or not binding_head_observed
            or not binding_delete_observed
        ):
            raise FullGateError(
                "Problem operator GC did not close through the Gateway bindings: "
                f"ledger_remaining={remaining_uris} "
                f"head_paths={observed_head_paths} delete_paths={observed_delete_paths}"
            )
        latest_topology_etag = self._current_topology_etag()
        expected_latest_etag = f'"{restore_revision}"'
        if latest_topology_etag != expected_latest_etag:
            raise FullGateError(
                "artifact GC Topology restore did not become the latest strong ETag: "
                f"expected={expected_latest_etag} actual={latest_topology_etag}"
            )
        self.h.evidence["problem_artifact_gc"] = {
            "setup": {
                "method": "duplicate-problem-no-http-conflict",
                "request_marker": request_marker,
                "problem_no": problem_no,
                "seed_problem_id": str(seed_problem_id),
                "seed_status": seed_status,
                "failure_status": failure_status,
                "baseline_pending_count": len(baseline_uris),
                "new_intent_count": len(intents),
                "package_intent_count": package_intent_count,
                "content_intent_count": content_intent_count,
                "business_database_write_used": False,
                "intent_rows_fabricated": False,
                "storage_objects_fabricated": False,
            },
            "intent_count": len(intents),
            "intents": intents,
            "all_objects_observed_before_gc": True,
            "storage_head_probe": {
                "role": "binding-head",
                "binding": "storage_head",
                "api_id": "storage.object.head",
                "service_context_mount_read_only": True,
                "problem_network_namespace_reused": True,
                "deployment_jwt_used": True,
                "credential_recorded": False,
                "provider_result_header_recorded": True,
            },
            "failure_recovery": {
                "target_uri": target["artifact_uri"],
                "target_sha256": target["sha256"],
                "target_size_bytes": target["size_bytes"],
                "state_chain": [
                    "PENDING",
                    "NEEDS_ATTENTION",
                    "PENDING",
                    "ABSENT",
                ],
                "binding_context_proof": {
                    "expected_required_bindings": expected_problem_bindings,
                    "initial": initial_context_proof,
                    "fault_provider": faulted_context_proof,
                    "restored": restored_context_proof,
                },
                "route_fault_injection": {
                    **fault_metadata,
                    "api_version": "1.0.0",
                    "required_binding_preserved": (
                        initial_context_proof["required_bindings_complete"]
                        and faulted_context_proof["required_bindings_complete"]
                    ),
                    "revision_id": fault_revision,
                    "operation_id": fault_operation.get("operation_id"),
                    "operation_status": fault_operation.get("status"),
                    "context_generation_before": generation_before,
                    "context_generation_after": generation_faulted,
                    "binding_desired_state": faulted_binding.get("desired_state"),
                    "binding_observed_state": faulted_binding.get("state"),
                },
                "fault_provider": {
                    "service_id": "storage-head-provenance-miss-provider",
                    "deployment_id": fault_provider_id,
                    "endpoint": fault_metadata.get("new_provider_endpoint"),
                    "api_id": "storage.object.head",
                    "api_version": "1.0.0",
                    "management_mode": fault_provider.get("management_mode"),
                    "observed_state": fault_instance.get("observed_state"),
                    "health": fault_instance.get("health"),
                    "head_path": fault_provider_path,
                    "head_request_observed": fault_provider_head_observed,
                    "head_probe": fault_binding_probe,
                    "storage_result_header_present": False,
                },
                "targeted_reconcile": {
                    **reconcile_action,
                    "operator_reason": reconcile_reason,
                    "exact_identity_submitted": True,
                },
                "needs_attention": {
                    "status": needs_attention.get("status"),
                    "failure_count": failure_count,
                    "last_failure": dict(last_failure),
                    "upload_completed_at": needs_attention.get(
                        "upload_completed_at"
                    ),
                    "manual_reconcile_requested_at": needs_attention.get(
                        "manual_reconcile_requested_at", ""
                    ) or "",
                    "manual_reconcile_marker_consumed": True,
                    "needs_attention_at": needs_attention.get("needs_attention_at"),
                    "ledger_preserved": True,
                    "claim_credential_exposed": False,
                },
                "route_restore": {
                    **restore_metadata,
                    "api_version": "1.0.0",
                    "required_binding_preserved": restored_context_proof[
                        "required_bindings_complete"
                    ],
                    "revision_id": restore_revision,
                    "operation_id": restore_operation.get("operation_id"),
                    "operation_status": restore_operation.get("status"),
                    "context_generation_before": generation_faulted,
                    "context_generation_after": generation_restored,
                    "binding_desired_state": restored_binding.get("desired_state"),
                    "binding_observed_state": restored_binding.get("state"),
                },
                "object_before_operator_retry": object_before_retry,
                "operator_retry": {
                    **retry_action,
                    "expected_failure_count": failure_count,
                    "operator_reason": operator_reason,
                },
                "ledger_absent_after_retry": True,
                "object_absent_after_retry": True,
            },
            "latest_topology_etag": latest_topology_etag,
            "ledger_removed": not remaining_uris,
            "ledger_rows_remaining": len(remaining_uris),
            "all_objects_removed": True,
            "gateway_storage_head_paths": observed_head_paths,
            "gateway_storage_head_observed": binding_head_observed,
            "gateway_storage_delete_paths": observed_delete_paths,
            "gateway_storage_delete_observed": binding_delete_observed,
            "judge_database_connection_used": False,
            "direct_storage_management_credential_used": False,
            "runtime_health_fabricated": False,
        }
        return latest_topology_etag

    def _workload_context_snapshot(
        self, service_id: str, evidence_suffix: str
    ) -> dict[str, Any]:
        runtime = self.managed_a_runtimes.get(service_id, {})
        deployment_id = str(runtime.get("deployment_id", ""))
        container_id = str(runtime.get("container_id", ""))
        if not deployment_id or not container_id:
            raise FullGateError(
                f"Worker compensation proof requires managed {service_id} runtime evidence"
            )
        context_text = self._container_text(
            self.a,
            container_id,
            "/run/ojos/service/context.json",
            f"{service_id}-compensation-context-{evidence_suffix}",
        )
        credential_text = self._container_text(
            self.a,
            container_id,
            "/run/ojos/service/token",
            f"{service_id}-compensation-credential-{evidence_suffix}",
        )
        try:
            context = json.loads(context_text)
        except json.JSONDecodeError as error:
            raise FullGateError(
                f"managed {service_id} ServiceContext is not valid JSON"
            ) from error
        if not isinstance(context, Mapping):
            raise FullGateError(f"managed {service_id} ServiceContext is not an object")
        bindings = context.get("bindings", {})
        generation = context.get("generation")
        deployment_context = context.get("deployment", {})
        if (
            not isinstance(deployment_context, Mapping)
            or deployment_context.get("id") != deployment_id
            or deployment_context.get("service") != service_id
            or deployment_context.get("node") != "node-a"
            or not isinstance(bindings, Mapping)
            or isinstance(generation, bool)
            or not isinstance(generation, int)
            or generation < 1
        ):
            raise FullGateError(
                f"managed {service_id} ServiceContext identity/generation is invalid"
            )
        binding_ids = sorted(
            str(binding.get("binding_id", ""))
            for binding in bindings.values()
            if isinstance(binding, Mapping) and binding.get("binding_id")
        )
        if len(binding_ids) != len(bindings):
            raise FullGateError(
                f"managed {service_id} ServiceContext contains an invalid binding"
            )
        binding_routes = []
        for requirement_name in sorted(str(name) for name in bindings):
            binding = bindings.get(requirement_name, {})
            if not isinstance(binding, Mapping):
                raise FullGateError(
                    f"managed {service_id} ServiceContext binding is not an object"
                )
            route = {
                "requirement_name": requirement_name,
                "binding_id": binding.get("binding_id"),
                "api_id": binding.get("api_id"),
                "base_path": binding.get("base_path"),
                "timeout_ms": binding.get("timeout_ms"),
            }
            if (
                not all(
                    isinstance(route[field], str) and bool(route[field])
                    for field in (
                        "requirement_name",
                        "binding_id",
                        "api_id",
                        "base_path",
                    )
                )
                or isinstance(route["timeout_ms"], bool)
                or not isinstance(route["timeout_ms"], int)
                or int(route["timeout_ms"]) < 1
            ):
                raise FullGateError(
                    f"managed {service_id} ServiceContext binding route is invalid"
                )
            binding_routes.append(route)
        token_parts = credential_text.split(".")
        try:
            token_payload = json.loads(
                base64.urlsafe_b64decode(
                    token_parts[1] + "=" * (-len(token_parts[1]) % 4)
                ).decode("utf-8")
            )
        except (IndexError, ValueError, UnicodeDecodeError, json.JSONDecodeError) as error:
            raise FullGateError(
                f"managed {service_id} workload credential is not a JWT"
            ) from error
        if (
            len(token_parts) != 3
            or not isinstance(token_payload, Mapping)
            or token_payload.get("deployment_id") != deployment_id
            or token_payload.get("service_id") != service_id
            or token_payload.get("node_id") != "node-a"
            or token_payload.get("credential_generation") != generation
        ):
            raise FullGateError(
                f"managed {service_id} workload credential does not match ServiceContext generation"
            )
        token_audience_value = token_payload.get("aud")
        token_audience = sorted(
            str(value)
            for value in (
                token_audience_value
                if isinstance(token_audience_value, list)
                else [token_audience_value]
            )
            if isinstance(value, str) and value
        )
        token_expires_at = token_payload.get("exp")
        token_jti = token_payload.get("jti")
        if (
            not isinstance(token_payload.get("iss"), str)
            or not token_payload.get("iss")
            or not token_audience
            or isinstance(token_expires_at, bool)
            or not isinstance(token_expires_at, int)
            or token_expires_at <= int(time.time()) - 30
            or not isinstance(token_jti, str)
            or not token_jti
        ):
            raise FullGateError(
                f"managed {service_id} workload credential claims are incomplete or expired"
            )
        return {
            "service_id": service_id,
            "deployment_id": deployment_id,
            "node_id": "node-a",
            "container_id": container_id,
            "generation": generation,
            "binding_names": sorted(str(name) for name in bindings),
            "binding_ids": binding_ids,
            "binding_routes": binding_routes,
            "credential_generation": token_payload.get(
                "credential_generation"
            ),
            "credential_claims": {
                "deployment_id": token_payload.get("deployment_id"),
                "service_id": token_payload.get("service_id"),
                "node_id": token_payload.get("node_id"),
                "credential_generation": token_payload.get(
                    "credential_generation"
                ),
                "issuer": token_payload.get("iss"),
                "audience": token_audience,
                "expires_at_unix": token_expires_at,
                "jti_sha256": _sha256(token_jti),
            },
            "context_sha256": _sha256(context_text),
            "workload_credential_file_sha256": _sha256(credential_text),
        }

    def _failed_worker_database_counts(self, deployment_id: str) -> dict[str, Any]:
        if not re.fullmatch(r"deployment-[a-z0-9-]+", deployment_id):
            raise FullGateError("failed Worker deployment ID is not safe for read-only verification")
        query = _single_line_sql(
            """
            BEGIN READ ONLY;
            SELECT json_build_object(
                'runtime_instance_count',
                (SELECT count(*) FROM orchestrator_runtime_instances
                 WHERE deployment_id = current_setting('ojos.failed_deployment_id')),
                'binding_count',
                (SELECT count(*) FROM orchestrator_api_bindings
                 WHERE consumer_deployment_id = current_setting('ojos.failed_deployment_id')),
                'active_or_staged_binding_count',
                (SELECT count(*) FROM orchestrator_api_bindings
                 WHERE consumer_deployment_id = current_setting('ojos.failed_deployment_id')
                   AND binding_state IN ('PENDING', 'RESOLVED', 'ACTIVE'))
            )::text;
            ROLLBACK;
            """
        )
        result = self.a.command(
            "exec",
            "--env",
            "PGOPTIONS=-cojos.failed_deployment_id=" + deployment_id,
            "postgres-a",
            "psql",
            "-X",
            "-A",
            "-t",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "postgres",
            "-d",
            "ojos_orchestrator",
            "-c",
            query,
            timeout=30,
        )
        counts = _json_from_last_line(result.stdout)
        if any(
            isinstance(counts.get(name), bool)
            or not isinstance(counts.get(name), int)
            or int(counts[name]) != 0
            for name in (
                "runtime_instance_count",
                "binding_count",
                "active_or_staged_binding_count",
            )
        ):
            raise FullGateError(
                "failed Worker left control-plane runtime/binding rows: "
                + _canonical(counts)
            )
        return {
            **counts,
            "query_mode": "postgres-fixed-parameterized-read-only-transaction",
            "control_plane_database_read_only_verification": True,
            "business_database_write_used": False,
            "row_payload_recorded": False,
        }

    @staticmethod
    def _projection_business_fields(projection: Mapping[str, Any]) -> dict[str, Any]:
        """Return the provider projection fields compensation must not change."""

        return {
            "provider": projection.get("provider"),
            "topology_id": projection.get("topology_id"),
            "revision_id": projection.get("revision_id"),
            "content_sha256": projection.get("content_sha256"),
            "spec": projection.get("spec"),
            "routes": projection.get("routes"),
            "grants": projection.get("grants"),
        }

    @staticmethod
    def _projection_stable_fields(projection: Mapping[str, Any]) -> dict[str, Any]:
        """Projection identity excluding the revision-scoped credential epoch."""

        def stable(items: Any) -> list[dict[str, Any]]:
            if not isinstance(items, list):
                raise FullGateError("provider projection routes/grants must be arrays")
            normalized: list[dict[str, Any]] = []
            for item in items:
                if not isinstance(item, Mapping):
                    raise FullGateError("provider projection route/grant is malformed")
                value = dict(item)
                value.pop("credential_generation", None)
                normalized.append(value)
            return sorted(
                normalized, key=lambda value: str(value.get("binding_id", ""))
            )

        return {
            "provider": projection.get("provider"),
            "topology_id": projection.get("topology_id"),
            "content_sha256": projection.get("content_sha256"),
            "spec": projection.get("spec"),
            "routes": stable(projection.get("routes")),
            "grants": stable(projection.get("grants")),
        }

    @staticmethod
    def _projection_consumer_generations(
        projection: Mapping[str, Any]
    ) -> dict[str, int]:
        routes = projection.get("routes", [])
        grants = projection.get("grants", [])
        if not isinstance(routes, list) or not isinstance(grants, list):
            raise FullGateError("provider projection generation evidence is malformed")
        route_by_binding = {
            str(item.get("binding_id", "")): item
            for item in routes
            if isinstance(item, Mapping) and item.get("binding_id")
        }
        grant_by_binding = {
            str(item.get("binding_id", "")): item
            for item in grants
            if isinstance(item, Mapping) and item.get("binding_id")
        }
        if (
            len(route_by_binding) != len(routes)
            or len(grant_by_binding) != len(grants)
            or set(route_by_binding) != set(grant_by_binding)
        ):
            raise FullGateError(
                "provider projection routes and grants do not have exact Binding identity"
            )
        generations: dict[str, int] = {}
        for binding_id, route in route_by_binding.items():
            grant = grant_by_binding[binding_id]
            consumer = str(route.get("consumer_deployment_id", ""))
            route_generation = route.get("credential_generation")
            grant_generation = grant.get("credential_generation")
            if (
                not consumer
                or grant.get("consumer_deployment_id") != consumer
                or isinstance(route_generation, bool)
                or not isinstance(route_generation, int)
                or route_generation < 1
                or grant_generation != route_generation
                or (
                    consumer in generations
                    and generations[consumer] != route_generation
                )
            ):
                raise FullGateError(
                    "provider projection has split consumer credential generations"
                )
            generations[consumer] = route_generation
        return generations

    @staticmethod
    def _auth_grant_stable_rows(rows: Any) -> list[dict[str, Any]]:
        if not isinstance(rows, list):
            raise FullGateError("Auth materialized grant evidence must be an array")
        stable = []
        for row in rows:
            if not isinstance(row, Mapping):
                raise FullGateError("Auth materialized grant evidence row is malformed")
            value = dict(row)
            value.pop("credential_generation", None)
            stable.append(value)
        return sorted(stable, key=lambda value: str(value.get("binding_id", "")))

    @staticmethod
    def _auth_grant_consumer_generations(rows: Any) -> dict[str, int]:
        if not isinstance(rows, list):
            raise FullGateError("Auth materialized grant evidence must be an array")
        generations: dict[str, int] = {}
        for row in rows:
            if not isinstance(row, Mapping):
                raise FullGateError("Auth materialized grant evidence row is malformed")
            consumer = str(row.get("consumer_deployment_id", ""))
            generation = row.get("credential_generation")
            if (
                not consumer
                or isinstance(generation, bool)
                or not isinstance(generation, int)
                or generation < 1
                or (consumer in generations and generations[consumer] != generation)
            ):
                raise FullGateError(
                    "Auth materialized grants have split consumer generations"
                )
            generations[consumer] = generation
        return generations

    def _gateway_topology_projection_snapshot(self) -> dict[str, Any]:
        key = "ojos:gateway:topology-projection:v1:" + self.topology_id
        index_key = "ojos:gateway:topology-projections:v1"
        projection_text = self.a.command(
            "exec",
            "redis-a",
            "redis-cli",
            "--raw",
            "GET",
            key,
            timeout=30,
        ).stdout.strip()
        index_member_text = self.a.command(
            "exec",
            "redis-a",
            "redis-cli",
            "--raw",
            "SISMEMBER",
            index_key,
            self.topology_id,
            timeout=30,
        ).stdout.strip()
        try:
            projection = json.loads(projection_text)
            index_member = int(index_member_text)
        except (json.JSONDecodeError, ValueError) as error:
            raise FullGateError(
                "Gateway active Topology projection or index membership is invalid"
            ) from error
        if not isinstance(projection, Mapping) or index_member != 1:
            raise FullGateError(
                "Gateway active Topology projection is absent from its durable index"
            )
        return {
            "key": key,
            "index_key": index_key,
            "index_member": index_member,
            "projection": dict(projection),
            "business": self._projection_business_fields(projection),
        }

    def _auth_topology_projection_snapshot(
        self, failed_deployment_id: str
    ) -> dict[str, Any]:
        if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,127}", self.topology_id):
            raise FullGateError(
                "Topology ID is not safe for Auth projection read-only verification"
            )
        if not re.fullmatch(r"deployment-[a-z0-9-]+", failed_deployment_id):
            raise FullGateError(
                "failed deployment is not safe for Auth grant read-only verification"
            )
        query = _single_line_sql(
            """
            BEGIN READ ONLY;
            SELECT json_build_object(
                'projection', payload,
                'row_revision_id', revision_id,
                'row_content_sha256', content_sha256,
                'row_operation_id', operation_id,
                'row_updated_at', updated_at,
                'grant_count', (
                    SELECT count(*) FROM auth_topology_binding_grants
                    WHERE topology_id = current_setting('ojos.evidence_topology_id')
                ),
                'failed_consumer_grant_count', (
                    SELECT count(*) FROM auth_topology_binding_grants
                    WHERE topology_id = current_setting('ojos.evidence_topology_id')
                      AND consumer_deployment_id = current_setting('ojos.failed_deployment_id')
                ),
                'grant_rows', (
                    SELECT COALESCE(
                        json_agg(
                            json_build_object(
                                'binding_id', binding_id,
                                'topology_id', topology_id,
                                'consumer_deployment_id', consumer_deployment_id,
                                'requirement_name', requirement_name,
                                'consumer_service_id', consumer_service_id,
                                'consumer_node_id', consumer_node_id,
                                'credential_generation', credential_generation,
                                'api_id', api_id,
                                'permission_code', permission_code
                            ) ORDER BY binding_id
                        ),
                        '[]'::json
                    )
                    FROM auth_topology_binding_grants
                    WHERE topology_id = current_setting('ojos.evidence_topology_id')
                )
            )::text
            FROM auth_topology_projections
            WHERE topology_id = current_setting('ojos.evidence_topology_id');
            ROLLBACK;
            """
        )
        result = self.a.command(
            "exec",
            "--env",
            "PGOPTIONS=-cojos.evidence_topology_id="
            + self.topology_id
            + " -cojos.failed_deployment_id="
            + failed_deployment_id,
            "postgres-a",
            "psql",
            "-X",
            "-A",
            "-t",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "postgres",
            "-d",
            "ojos_auth",
            "-c",
            query,
            timeout=30,
        )
        row = _json_from_last_line(result.stdout)
        projection = row.get("projection", {})
        projection_grants = (
            projection.get("grants", [])
            if isinstance(projection, Mapping)
            else []
        )
        normalized_projection_grants = sorted(
            (
                {
                    "binding_id": grant.get("binding_id"),
                    "topology_id": self.topology_id,
                    "consumer_deployment_id": grant.get(
                        "consumer_deployment_id"
                    ),
                    "requirement_name": grant.get("requirement_name"),
                    "consumer_service_id": grant.get("consumer_service_id"),
                    "consumer_node_id": grant.get("consumer_node_id"),
                    "credential_generation": grant.get(
                        "credential_generation"
                    ),
                    "api_id": grant.get("api_id"),
                    "permission_code": grant.get("permission"),
                }
                for grant in projection_grants
                if isinstance(grant, Mapping)
            ),
            key=lambda grant: str(grant.get("binding_id", "")),
        )
        if (
            not isinstance(projection, Mapping)
            or row.get("row_revision_id") != projection.get("revision_id")
            or row.get("row_content_sha256") != projection.get("content_sha256")
            or row.get("row_operation_id") != projection.get("operation_id")
            or not isinstance(row.get("grant_rows"), list)
            or row.get("grant_count") != len(row.get("grant_rows", []))
            or row.get("grant_count") != len(projection_grants)
            or len(normalized_projection_grants) != len(projection_grants)
            or _canonical(normalized_projection_grants)
            != _canonical(row.get("grant_rows", []))
            or row.get("failed_consumer_grant_count") != 0
        ):
            raise FullGateError(
                "Auth active Topology projection row and payload identities disagree"
            )
        return {
            **row,
            "projection": dict(projection),
            "business": self._projection_business_fields(projection),
        }

    def _provider_topology_status(self, provider: str) -> dict[str, Any]:
        """Read a provider Status without placing its management token in argv."""

        if provider == "gateway":
            origin = "http://gateway-a:8080"
            token = self.gateway_management_token
        elif provider == "auth":
            origin = "http://auth-a:8081"
            token = self.auth_management_token
        else:
            raise FullGateError(f"unknown Topology provider {provider!r}")
        # curl reads this tiny config from stdin.  The bearer is therefore not
        # visible in docker-exec argv, command diagnostics, or evidence.
        curl_config = (
            "silent\n"
            "show-error\n"
            "fail\n"
            "max-time = 30\n"
            f'header = "Authorization: Bearer {token}"\n'
        )
        result = self.a.command(
            "exec",
            "-i",
            "orchestrator-a",
            "curl",
            "--config",
            "-",
            origin + "/api/v1/topologies/" + self.topology_id,
            input_data=curl_config,
            timeout=40,
        )
        try:
            status = json.loads(result.stdout)
        except json.JSONDecodeError as error:
            raise FullGateError(
                f"{provider} Topology Status was not JSON"
            ) from error
        if not isinstance(status, Mapping):
            raise FullGateError(f"{provider} Topology Status was not an object")
        return dict(status)

    def _projection_integrity_topology_read(
        self, stage: str
    ) -> tuple[dict[str, Any] | None, dict[str, Any]]:
        """Read one secret-free Topology convergence view for a digest checkpoint."""

        assert self.control_client is not None
        try:
            topology_document, topology_headers, _ = self.control_client.request(
                "GET",
                f"/api/v1/topologies/{self.topology_id}",
                expected=(200,),
                timeout=30,
            )
        except Exception:
            # The convergence loop deliberately reduces transport failures to
            # a fixed diagnostic code; HTTP/TLS exception text can contain
            # request material and must never enter release evidence.
            return None, {
                "stage": stage,
                "reason": "topology-request-failed",
                "topology_id": self.topology_id,
            }
        topology = topology_document.get("data", {})
        draft = topology.get("draft", {}) if isinstance(topology, Mapping) else {}
        heads = topology.get("heads", {}) if isinstance(topology, Mapping) else {}
        topology_status = (
            topology.get("status", {}) if isinstance(topology, Mapping) else {}
        )
        revision_id = str(draft.get("revision_id", ""))
        content_sha256 = str(draft.get("content_sha256", ""))
        etag = str(topology_headers.get("etag", ""))
        drift = topology_status.get("drift")
        diagnostic = {
            "stage": stage,
            "reason": "topology-not-converged",
            "topology_id": self.topology_id,
            "revision_id": revision_id,
            "content_sha256": content_sha256,
            "etag": etag,
            "heads": {
                "draft_revision_id": heads.get("draft_revision_id"),
                "applied_revision_id": heads.get("applied_revision_id"),
                "applying_revision_id": heads.get("applying_revision_id"),
                "applying_operation_present": bool(
                    heads.get("applying_operation_id")
                ),
            },
            "status": {
                "desired_revision_id": topology_status.get("desired_revision_id"),
                "observed_revision_id": topology_status.get("observed_revision_id"),
                "state": str(topology_status.get("state", "")).upper(),
                "drift_count": len(drift) if isinstance(drift, list) else None,
            },
        }
        converged = (
            bool(revision_id)
            and re.fullmatch(r"[0-9a-f]{64}", content_sha256) is not None
            and heads.get("draft_revision_id") == revision_id
            and heads.get("applied_revision_id") == revision_id
            and heads.get("applying_revision_id") is None
            and topology_status.get("desired_revision_id") == revision_id
            and topology_status.get("observed_revision_id") == revision_id
            and str(topology_status.get("state", "")).upper() == "IN_SYNC"
            and drift == []
            and etag == f'"{revision_id}"'
        )
        if not converged:
            return None, diagnostic
        return {
            "revision_id": revision_id,
            "content_sha256": content_sha256,
            "etag": etag,
        }, diagnostic

    def _capture_provider_projection_integrity(
        self,
        phase: str,
        *,
        timeout: float = PROJECTION_INTEGRITY_CONVERGENCE_TIMEOUT_SECONDS,
        poll_interval: float = PROJECTION_INTEGRITY_CONVERGENCE_POLL_SECONDS,
    ) -> dict[str, Any]:
        """Wait for one stable Topology and bind both providers to durable rows."""

        if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", phase):
            raise FullGateError(f"invalid projection integrity phase {phase!r}")
        if timeout < 0 or poll_interval < 0:
            raise FullGateError("projection integrity convergence timing must not be negative")
        assert self.control_client is not None
        started = time.monotonic()
        deadline = started + timeout
        attempts = 0
        last_transitional_diagnostic: dict[str, Any] = {}

        while True:
            attempts += 1
            before, diagnostic = self._projection_integrity_topology_read(
                "topology-before"
            )
            providers: dict[str, dict[str, Any]] = {}
            expected_projection_sha256 = ""

            if before is not None:
                revision_id = before["revision_id"]
                content_sha256 = before["content_sha256"]
                try:
                    snapshots = {
                        "gateway": self._gateway_topology_projection_snapshot(),
                        "auth": self._auth_topology_projection_snapshot(
                            "deployment-evidence-projection-" + self.h.run_id
                        ),
                    }
                except Exception:
                    diagnostic = {
                        "stage": "provider-snapshot",
                        "reason": "provider-snapshot-request-failed",
                        "topology_id": self.topology_id,
                        "revision_id": revision_id,
                    }
                else:
                    for provider in ("gateway", "auth"):
                        snapshot = snapshots.get(provider, {})
                        projection = (
                            snapshot.get("projection", {})
                            if isinstance(snapshot, Mapping)
                            else {}
                        )
                        routes = (
                            projection.get("routes", [])
                            if isinstance(projection, Mapping)
                            else []
                        )
                        grants = (
                            projection.get("grants", [])
                            if isinstance(projection, Mapping)
                            else []
                        )
                        try:
                            recomputed = _effective_projection_sha256(routes, grants)
                        except FullGateError:
                            diagnostic = {
                                "stage": f"provider-{provider}",
                                "reason": "provider-projection-non-canonical",
                                "topology_id": self.topology_id,
                                "revision_id": revision_id,
                                "route_count": (
                                    len(routes) if isinstance(routes, list) else None
                                ),
                                "grant_count": (
                                    len(grants) if isinstance(grants, list) else None
                                ),
                            }
                            break
                        try:
                            status = self._provider_topology_status(provider)
                        except Exception:
                            diagnostic = {
                                "stage": f"provider-{provider}",
                                "reason": "provider-status-request-failed",
                                "topology_id": self.topology_id,
                                "revision_id": revision_id,
                                "route_count": len(routes),
                                "grant_count": len(grants),
                                "recomputed_projection_sha256": recomputed,
                            }
                            break
                        observed_projection_sha256 = str(
                            status.get("observed_projection_sha256", "")
                        )
                        provider_diagnostic = {
                            "stage": f"provider-{provider}",
                            "reason": "provider-projection-not-converged",
                            "topology_id": self.topology_id,
                            "revision_id": revision_id,
                            "content_sha256": content_sha256,
                            "projection_revision_id": (
                                projection.get("revision_id")
                                if isinstance(projection, Mapping)
                                else None
                            ),
                            "projection_content_sha256": (
                                projection.get("content_sha256")
                                if isinstance(projection, Mapping)
                                else None
                            ),
                            "route_count": len(routes),
                            "grant_count": len(grants),
                            "recomputed_projection_sha256": recomputed,
                            "status": {
                                "api_version": status.get("api_version"),
                                "provider": status.get("provider"),
                                "topology_id": status.get("topology_id"),
                                "absent": status.get("absent"),
                                "observed_revision_id": status.get(
                                    "observed_revision_id"
                                ),
                                "observed_content_sha256": status.get(
                                    "observed_content_sha256"
                                ),
                                "observed_projection_sha256": (
                                    observed_projection_sha256
                                ),
                            },
                        }
                        matches = (
                            isinstance(projection, Mapping)
                            and projection.get("provider") == provider
                            and projection.get("topology_id") == self.topology_id
                            and projection.get("revision_id") == revision_id
                            and projection.get("content_sha256") == content_sha256
                            and status.get("api_version") == "v1"
                            and status.get("provider") == provider
                            and status.get("topology_id") == self.topology_id
                            and status.get("absent") is False
                            and status.get("observed_revision_id") == revision_id
                            and status.get("observed_content_sha256")
                            == content_sha256
                            and re.fullmatch(
                                r"[0-9a-f]{64}", observed_projection_sha256
                            )
                            is not None
                            and observed_projection_sha256 == recomputed
                            and (
                                not expected_projection_sha256
                                or recomputed == expected_projection_sha256
                            )
                        )
                        if not matches:
                            diagnostic = provider_diagnostic
                            break
                        expected_projection_sha256 = recomputed
                        providers[provider] = {
                            "source": "provider-present-status-and-durable-projection",
                            "api_version": status.get("api_version"),
                            "provider": provider,
                            "topology_id": self.topology_id,
                            "absent": False,
                            "observed_revision_id": status.get(
                                "observed_revision_id"
                            ),
                            "observed_content_sha256": status.get(
                                "observed_content_sha256"
                            ),
                            "observed_projection_sha256": observed_projection_sha256,
                            "recomputed_projection_sha256": recomputed,
                            "projection": {
                                "routes": copy.deepcopy(routes),
                                "grants": copy.deepcopy(grants),
                            },
                            "route_count": len(routes),
                            "grant_count": len(grants),
                            "matches_expected": True,
                        }

                    if len(providers) == 2:
                        after, after_diagnostic = (
                            self._projection_integrity_topology_read(
                                "topology-after"
                            )
                        )
                        if after is None:
                            diagnostic = after_diagnostic
                        elif after != before:
                            diagnostic = {
                                "stage": "topology-after",
                                "reason": "topology-changed-during-provider-read",
                                "topology_id": self.topology_id,
                                "before": before,
                                "after": after,
                            }
                        else:
                            return {
                                "phase": phase,
                                "captured_at_unix_ms": int(time.time() * 1000),
                                "topology_id": self.topology_id,
                                "applied_revision_id": revision_id,
                                "applied_content_sha256": content_sha256,
                                "topology_etag": before["etag"],
                                "topology_status_state": "IN_SYNC",
                                "topology_status_drift": [],
                                "expected_projection_sha256": (
                                    expected_projection_sha256
                                ),
                                "providers": providers,
                                "attempts": attempts,
                                "converged_after_ms": max(
                                    0, int((time.monotonic() - started) * 1000)
                                ),
                                "last_transitional_diagnostic": copy.deepcopy(
                                    last_transitional_diagnostic
                                ),
                                "all_match": True,
                            }

            last_transitional_diagnostic = diagnostic
            now = time.monotonic()
            if now >= deadline:
                failure = {
                    "phase": phase,
                    "attempts": attempts,
                    "waited_ms": max(0, int((now - started) * 1000)),
                    "last_transitional_diagnostic": copy.deepcopy(
                        last_transitional_diagnostic
                    ),
                }
                failures = self.h.evidence.setdefault(
                    "provider_projection_integrity_failures", {}
                )
                failures[phase] = failure
                raise FullGateError(
                    f"projection integrity phase {phase} did not converge after "
                    f"{attempts} attempts: {_canonical(last_transitional_diagnostic)}"
                )
            time.sleep(min(poll_interval, max(0.0, deadline - now)))

    def _durable_topology_bindings_snapshot(
        self, revision_id: str
    ) -> dict[str, Any]:
        if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,127}", self.topology_id):
            raise FullGateError(
                "Topology ID is not safe for Binding read-only verification"
            )
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9:._-]{0,255}", revision_id):
            raise FullGateError(
                "Topology revision is not safe for Binding read-only verification"
            )
        query = _single_line_sql(
            """
            BEGIN READ ONLY;
            SELECT json_build_object(
                'binding_count', count(*),
                'active_count', count(*) FILTER (WHERE binding_state = 'ACTIVE'),
                'non_active_count', count(*) FILTER (WHERE binding_state <> 'ACTIVE'),
                'wrong_revision_count', count(*) FILTER (
                    WHERE topology_revision_id <> current_setting('ojos.evidence_revision_id')
                ),
                'rows', COALESCE(
                    json_agg(
                        json_build_object(
                            'binding_id', binding_id,
                            'consumer_deployment_id', consumer_deployment_id,
                            'provider_deployment_id', provider_deployment_id,
                            'topology_id', topology_id,
                            'topology_revision_id', topology_revision_id,
                            'requirement_name', requirement_name,
                            'api_id', api_id,
                            'binding_state', binding_state,
                            'payload', payload
                        ) ORDER BY binding_id
                    ),
                    '[]'::json
                )
            )::text
            FROM orchestrator_api_bindings
            WHERE topology_id = current_setting('ojos.evidence_topology_id');
            ROLLBACK;
            """
        )
        result = self.a.command(
            "exec",
            "--env",
            "PGOPTIONS=-cojos.evidence_topology_id="
            + self.topology_id
            + " -cojos.evidence_revision_id="
            + revision_id,
            "postgres-a",
            "psql",
            "-X",
            "-A",
            "-t",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "postgres",
            "-d",
            "ojos_orchestrator",
            "-c",
            query,
            timeout=30,
        )
        snapshot = _json_from_last_line(result.stdout)
        rows = snapshot.get("rows", [])
        count = snapshot.get("binding_count")
        if (
            isinstance(count, bool)
            or not isinstance(count, int)
            or count < 1
            or snapshot.get("active_count") != count
            or snapshot.get("non_active_count") != 0
            or snapshot.get("wrong_revision_count") != 0
            or not isinstance(rows, list)
            or len(rows) != count
        ):
            raise FullGateError(
                "durable applied Topology Binding set is not wholly ACTIVE at the selected revision"
            )
        business_rows: list[dict[str, Any]] = []
        consumer_generations: dict[str, int] = {}
        for row in rows:
            if not isinstance(row, Mapping) or not isinstance(
                row.get("payload"), Mapping
            ):
                raise FullGateError("durable Binding evidence row is malformed")
            payload = dict(row["payload"])
            consumer = str(row.get("consumer_deployment_id", ""))
            credential_generation = payload.get("credential_generation")
            context_generation = payload.get("context_generation")
            if (
                not consumer
                or isinstance(credential_generation, bool)
                or not isinstance(credential_generation, int)
                or credential_generation < 1
                or context_generation != credential_generation
                or (
                    consumer in consumer_generations
                    and consumer_generations[consumer] != credential_generation
                )
            ):
                raise FullGateError(
                    "durable Bindings have split credential/context generations"
                )
            consumer_generations[consumer] = credential_generation
            for field in (
                "topology_revision_id",
                "last_operation_id",
                "created_at",
                "updated_at",
                "credential_generation",
                "context_generation",
                "credential_ref",
            ):
                payload.pop(field, None)
            business_rows.append(
                {
                    "binding_id": row.get("binding_id"),
                    "consumer_deployment_id": row.get("consumer_deployment_id"),
                    "provider_deployment_id": row.get("provider_deployment_id"),
                    "topology_id": row.get("topology_id"),
                    "requirement_name": row.get("requirement_name"),
                    "api_id": row.get("api_id"),
                    "binding_state": row.get("binding_state"),
                    "payload": payload,
                }
            )
        snapshot["business_rows"] = business_rows
        snapshot["consumer_generations"] = consumer_generations
        return snapshot

    def _prove_worker_install_failure_compensation(
        self, topology_etag: str
    ) -> tuple[dict[str, Any], str]:
        """Fail a real post-start Worker health gate and read every resource back.

        The Operation result is retained only as terminal-state correlation.  Cleanup
        is proven from Docker, the Agent-owned filesystem, public v1 reads, a fixed
        read-only control-plane query, and unchanged live consumer materialization.
        """

        assert self.control_client is not None
        assert self.gateway_client is not None
        service_id = "judge-worker"
        version = "0.1.0"
        node_id = "node-b"
        digest = hashlib.sha256(
            f"{service_id}\0{version}\0{node_id}".encode("utf-8")
        ).hexdigest()
        deployment_id = f"deployment-{service_id}-{digest}"[:56]
        component = hashlib.sha256(deployment_id.encode("utf-8")).hexdigest()[:32]
        cache_volume = "ojos-judge-cache-" + component
        context_directory = B_WORKLOAD_EXPORT_ROOT + "/runtime-contexts/" + component
        logical_endpoint = (
            f"{self.h.evidence['engines']['b']['outer_ip']}:9101:judge-worker"
        )

        before_document, before_headers, _ = self.control_client.request(
            "GET",
            f"/api/v1/topologies/{self.topology_id}",
            expected=(200,),
            timeout=30,
        )
        before_data = before_document.get("data", {})
        before_draft = before_data.get("draft", {})
        before_heads = before_data.get("heads", {})
        before_status = before_data.get("status", {})
        before_revision = str(before_draft.get("revision_id", ""))
        before_spec = before_draft.get("spec", {})
        before_content_sha256 = str(before_draft.get("content_sha256", ""))
        before_etag = str(before_headers.get("etag", ""))
        if (
            not before_revision
            or before_etag != topology_etag
            or not re.fullmatch(r"[0-9a-f]{64}", before_content_sha256)
            or before_heads.get("draft_revision_id") != before_revision
            or before_heads.get("applied_revision_id") != before_revision
            or before_heads.get("applying_revision_id") is not None
            or not isinstance(before_spec, Mapping)
            or not isinstance(before_status, Mapping)
            or before_status.get("desired_revision_id") != before_revision
            or before_status.get("observed_revision_id") != before_revision
            or str(before_status.get("state", "")).upper() != "IN_SYNC"
            or before_status.get("drift") != []
        ):
            raise FullGateError(
                "Worker compensation fault requires a converged strong Topology baseline"
            )
        consumer_before = {
            service: self._workload_context_snapshot(service, "before")
            for service in ("problem-service", "judge-api")
        }
        gateway_projection_before = self._gateway_topology_projection_snapshot()
        auth_projection_before = self._auth_topology_projection_snapshot(deployment_id)
        durable_bindings_before = self._durable_topology_bindings_snapshot(
            before_revision
        )
        for provider, snapshot in (
            ("gateway", gateway_projection_before),
            ("auth", auth_projection_before),
        ):
            business = snapshot.get("business", {})
            if (
                not isinstance(business, Mapping)
                or business.get("provider") != provider
                or business.get("topology_id") != self.topology_id
                or business.get("revision_id") != before_revision
                or business.get("content_sha256") != before_content_sha256
                or _canonical(business.get("spec", {})) != _canonical(before_spec)
                or not isinstance(business.get("routes"), list)
                or not isinstance(business.get("grants"), list)
                or not business.get("routes")
                or not business.get("grants")
            ):
                raise FullGateError(
                    f"{provider} durable projection does not match the applied Topology baseline"
                )

        gateway_before = json.loads(
            self.a.command("inspect", "gateway-tls-a", timeout=30).stdout
        )[0]
        gateway_container_id = str(gateway_before.get("Id", ""))
        if (
            not re.fullmatch(r"[a-f0-9]{64}", gateway_container_id)
            or gateway_before.get("State", {}).get("Running") is not True
        ):
            raise FullGateError("TLS Gateway is not running before Worker fault injection")

        selections = [
            {
                "name": "judge_control",
                "provider_deployment_id": self.provider_deployments["judge-api"],
            },
            {
                "name": "storage_get",
                "provider_deployment_id": self.provider_deployments["storage-service"],
            },
        ]
        request = {
            "service_id": service_id,
            "catalog_source_id": service_id,
            "version": version,
            "channel": "stable",
            "target_node_id": node_id,
            "bindings": selections,
            "topology_id": self.topology_id,
            "topology_etag": topology_etag,
        }

        operation: dict[str, Any] = {}
        install_data: dict[str, Any] = {}
        stopped_before_install = False
        stopped_after_install = False
        restored_health_status = 0
        fault_started_at = int(time.time() * 1000)
        self.a.command("stop", "--time", "10", "gateway-tls-a", timeout=60)
        try:
            stopped_inspect = json.loads(
                self.a.command("inspect", "gateway-tls-a", timeout=30).stdout
            )[0]
            stopped_before_install = (
                stopped_inspect.get("State", {}).get("Running") is False
                and stopped_inspect.get("Id") == gateway_container_id
            )
            if not stopped_before_install:
                raise FullGateError("TLS Gateway did not remain stopped for fault injection")
            validated, _, _ = self._control_mutation(
                "/api/v1/store/releases:validate",
                request,
                "worker-compensation-validate",
                expected=(200,),
            )
            if validated.get("data", {}).get("valid") is not True:
                raise FullGateError(
                    "Store rejected the pre-fault Judge Worker plan: "
                    + _canonical(validated)
                )
            installed, _, _ = self._control_mutation(
                "/api/v1/store/releases:install",
                {**request, "mode": "MANAGED", "start": True},
                "worker-compensation-install",
                expected=(202,),
            )
            install_data = installed.get("data", {})
            if install_data.get("deployment_id") != deployment_id:
                raise FullGateError(
                    "failed Worker install did not use its deterministic deployment ID"
                )
            operation = self._wait_operation(
                str(install_data.get("operation_id", "")), timeout=420
            )
            stopped_after = json.loads(
                self.a.command("inspect", "gateway-tls-a", timeout=30).stdout
            )[0]
            stopped_after_install = (
                stopped_after.get("State", {}).get("Running") is False
                and stopped_after.get("Id") == gateway_container_id
            )
        finally:
            self.a.command("start", "gateway-tls-a", timeout=60)
            self._wait_json(self.gateway_client, "/health", timeout=120)
            _, _, restored_health_status = self.gateway_client.request(
                "GET", "/health", expected=(200,), timeout=10
            )

        job_binding = next(
            (
                item
                for item in operation.get("job_bindings", [])
                if isinstance(item, Mapping)
                and str(item.get("step_id", "")).startswith("install-judge-worker-")
            ),
            None,
        )
        if (
            str(operation.get("status", "")).upper() != "FAILED"
            or not stopped_after_install
            or operation.get("attention_job_ids") != []
            or not isinstance(job_binding, Mapping)
        ):
            raise FullGateError(
                "faulted Worker install did not finish FAILED without NEEDS_ATTENTION"
            )
        operation_topology = operation.get("request", {}).get("topology", {})
        proposed_revision = str(operation_topology.get("proposed_revision_id", ""))
        if (
            not proposed_revision
            or operation_topology.get("topology_id") != self.topology_id
            or operation_topology.get("selected_revision_id") != before_revision
        ):
            raise FullGateError(
                "faulted Worker Operation did not retain its selected/proposed Topology lineage"
            )
        agent_job = self._query_json(
            "ojos_orchestrator",
            "SELECT payload::text FROM orchestrator_jobs WHERE job_id="
            + self._sql(str(job_binding.get("job_id", ""))),
        )
        agent_result = agent_job.get("result", {})
        agent_failure = (
            agent_result.get("failure", {})
            if isinstance(agent_result, Mapping)
            else {}
        )
        last_health_observation = (
            agent_failure.get("last_health_observation", {})
            if isinstance(agent_failure, Mapping)
            else {}
        )
        if (
            agent_job.get("node_id") != node_id
            or str(agent_job.get("status", "")).upper() != "FAILED"
            or agent_job.get("lease_owner") != self.agent_identity.get("instance_id")
            or int(agent_job.get("attempt", 0)) < 1
            or not isinstance(agent_result, Mapping)
            or agent_result.get("action") != "install"
            or agent_result.get("compensated") is not True
            or not re.fullmatch(
                r"[a-f0-9]{64}",
                str(agent_result.get("removed_container_id", "")),
            )
            or not isinstance(agent_failure, Mapping)
            or agent_failure.get("health_gate") not in {"failed", "timeout"}
            or isinstance(agent_failure.get("probe_count"), bool)
            or not isinstance(agent_failure.get("probe_count"), int)
            or int(agent_failure.get("probe_count", 0)) < 1
            or not isinstance(last_health_observation, Mapping)
            or int(last_health_observation.get("probe", 0)) < 1
            or int(last_health_observation.get("probe", 0))
            > int(agent_failure.get("probe_count", 0))
            or str(last_health_observation.get("observed_state", "")).upper()
            != "RUNNING"
            or str(last_health_observation.get("health", "")).upper()
            not in {"STARTING", "UNHEALTHY"}
            or not str(last_health_observation.get("probe_reason", "")).strip()
        ):
            raise FullGateError(
                "faulted Worker install was not a compensated post-start health gate failure on Node B"
            )

        container_matches = self.b.command(
            "ps",
            "-a",
            "--filter",
            "label=ojos.deployment_id=" + deployment_id,
            "--format",
            "{{.ID}}",
            "--no-trunc",
            timeout=30,
        ).stdout.strip().splitlines()
        container_name = "ojos-" + deployment_id
        container_name_inspect = self.b.command(
            "inspect", container_name, timeout=30, check=False
        )
        volume_matches = self.b.command(
            "volume",
            "ls",
            "--filter",
            "label=ojos.deployment_id=" + deployment_id,
            "--format",
            "{{.Name}}",
            timeout=30,
        ).stdout.strip().splitlines()
        volume_name_inspect = self.b.command(
            "volume", "inspect", cache_volume, timeout=30, check=False
        )
        context_probe = self.root.command(
            "exec",
            self.h.b_name,
            "test",
            "-e",
            context_directory,
            timeout=30,
            check=False,
        )
        if (
            container_matches
            or container_name_inspect.returncode == 0
            or volume_matches
            or volume_name_inspect.returncode == 0
            or context_probe.returncode != 1
        ):
            raise FullGateError(
                "faulted Worker left a container, managed volume, or Agent context directory"
            )

        runtime_document, _, runtime_status = self.control_client.request(
            "GET",
            f"/api/v1/deployments/{deployment_id}",
            expected=(200, 404),
            timeout=30,
        )
        binding_document, _, binding_status = self.control_client.request(
            "GET",
            f"/api/v1/deployments/{deployment_id}/bindings",
            expected=(200, 404),
            timeout=30,
        )
        if runtime_status != 404 or binding_status != 404:
            raise FullGateError(
                "faulted Worker remained visible as a runtime or binding consumer: "
                f"runtime={runtime_status}/{runtime_document.get('code')} "
                f"bindings={binding_status}/{binding_document.get('code')}"
            )
        database_counts = self._failed_worker_database_counts(deployment_id)

        topology_deadline = time.monotonic() + 60
        after_data: dict[str, Any] = {}
        after_headers: dict[str, str] = {}
        while time.monotonic() < topology_deadline:
            after_document, after_headers, _ = self.control_client.request(
                "GET",
                f"/api/v1/topologies/{self.topology_id}",
                expected=(200,),
                timeout=30,
            )
            after_data = after_document.get("data", {})
            after_draft_candidate = after_data.get("draft", {})
            after_heads_candidate = after_data.get("heads", {})
            after_status_candidate = after_data.get("status", {})
            if (
                isinstance(after_draft_candidate, Mapping)
                and isinstance(after_heads_candidate, Mapping)
                and isinstance(after_status_candidate, Mapping)
                and after_draft_candidate.get("revision_id") == proposed_revision
                and after_heads_candidate.get("draft_revision_id")
                == proposed_revision
                and after_heads_candidate.get("applied_revision_id")
                == before_revision
                and after_heads_candidate.get("applying_revision_id") is None
                and after_status_candidate.get("desired_revision_id")
                == before_revision
                and after_status_candidate.get("observed_revision_id")
                == before_revision
                and str(after_status_candidate.get("state", "")).upper()
                == "IN_SYNC"
                and after_status_candidate.get("last_operation_id")
                == operation.get("operation_id")
                and after_status_candidate.get("drift") == []
                and str(after_headers.get("etag", "")) == f'"{proposed_revision}"'
            ):
                break
            time.sleep(1)
        else:
            raise FullGateError(
                "failed Worker Topology did not reconcile the prior applied revision while preserving its failed draft: "
                + _canonical(after_data)
            )
        after_draft = after_data.get("draft", {})
        after_heads = after_data.get("heads", {})
        after_status = after_data.get("status", {})
        after_spec = after_draft.get("spec", {})
        after_revision = str(after_draft.get("revision_id", ""))
        after_etag = str(after_headers.get("etag", ""))
        if not isinstance(after_spec, Mapping):
            raise FullGateError("Topology read-back after Worker compensation has no Spec")
        failed_endpoints = [
            endpoint
            for endpoint in after_spec.get("endpoints", [])
            if isinstance(endpoint, Mapping)
            and (
                endpoint.get("config", {}).get("deployment_id") == deployment_id
                or endpoint.get("endpoint") == logical_endpoint
            )
        ]
        failed_endpoint_names = {
            str(endpoint.get("endpoint", "")) for endpoint in failed_endpoints
        }
        failed_links = [
            link
            for link in after_spec.get("links", [])
            if isinstance(link, Mapping)
            and (
                link.get("source_endpoint")
                in failed_endpoint_names | {logical_endpoint}
                or link.get("target_endpoint")
                in failed_endpoint_names | {logical_endpoint}
            )
        ]
        desired_requirements = {
            str(binding.get("requirement", binding.get("name", "")))
            for link in failed_links
            for binding in link.get("api_bindings", [])
            if isinstance(binding, Mapping)
        }
        desired_providers = {
            str(binding.get("provider_deployment_id", ""))
            for link in failed_links
            for binding in link.get("api_bindings", [])
            if isinstance(binding, Mapping)
        }
        if (
            after_revision != proposed_revision
            or after_etag != f'"{proposed_revision}"'
            or after_heads.get("draft_revision_id") != proposed_revision
            or after_heads.get("applied_revision_id") != before_revision
            or after_heads.get("applying_revision_id") is not None
            or not isinstance(after_status, Mapping)
            or after_status.get("desired_revision_id") != before_revision
            or after_status.get("observed_revision_id") != before_revision
            or str(after_status.get("state", "")).upper() != "IN_SYNC"
            or after_status.get("last_operation_id") != operation.get("operation_id")
            or after_status.get("drift") != []
            or len(failed_endpoints) != 1
            or failed_endpoint_names != {logical_endpoint}
            or len(failed_links) != 2
            or any(
                link.get("source_endpoint") != logical_endpoint
                or link.get("enabled") is not True
                or str(link.get("auth_mode", "")).lower() != "workload"
                for link in failed_links
            )
            or desired_requirements != {"judge_control", "storage_get"}
            or desired_providers
            != {
                self.provider_deployments["judge-api"],
                self.provider_deployments["storage-service"],
            }
        ):
            raise FullGateError(
                "failed Worker install did not retain the expected immutable draft over the reconciled previous applied revision"
            )

        baseline_document, baseline_headers, _ = self.control_client.request(
            "GET",
            f"/api/v1/topologies/{self.topology_id}/revisions/{before_revision}",
            expected=(200,),
            timeout=30,
        )
        baseline_revision = baseline_document.get("data", {}).get("revision", {})
        baseline_etag = str(baseline_headers.get("etag", ""))
        baseline_number = int(baseline_revision.get("revision_number", 0))
        if (
            baseline_revision.get("revision_id") != before_revision
            or baseline_etag != before_etag
            or baseline_number < 1
            or baseline_revision.get("content_sha256") != before_content_sha256
            or _canonical(baseline_revision.get("spec", {}))
            != _canonical(before_spec)
        ):
            raise FullGateError(
                "failed Worker install mutated or hid the previous immutable revision"
            )
        failed_revision_document, failed_revision_headers, _ = (
            self.control_client.request(
                "GET",
                f"/api/v1/topologies/{self.topology_id}/revisions/{proposed_revision}",
                expected=(200,),
                timeout=30,
            )
        )
        failed_revision = failed_revision_document.get("data", {}).get(
            "revision", {}
        )
        failed_revision_etag = str(failed_revision_headers.get("etag", ""))
        failed_revision_number = int(failed_revision.get("revision_number", 0))
        failed_content_sha256 = str(failed_revision.get("content_sha256", ""))
        if (
            failed_revision.get("revision_id") != proposed_revision
            or failed_revision_etag != after_etag
            or failed_revision.get("parent_revision_id") != before_revision
            or failed_revision.get("rollback_of_revision_id") is not None
            or failed_revision_number != baseline_number + 1
            or not re.fullmatch(r"[0-9a-f]{64}", failed_content_sha256)
            or failed_content_sha256 == before_content_sha256
            or _canonical(failed_revision.get("spec", {}))
            != _canonical(after_spec)
        ):
            raise FullGateError(
                "failed Worker draft is not the expected immutable child revision"
            )

        gateway_projection_after = self._gateway_topology_projection_snapshot()
        auth_projection_after = self._auth_topology_projection_snapshot(deployment_id)
        durable_bindings_after = self._durable_topology_bindings_snapshot(
            before_revision
        )
        gateway_projection = gateway_projection_after["projection"]
        auth_projection = auth_projection_after["projection"]
        active_routes = gateway_projection.get("routes", [])
        active_grants = gateway_projection.get("grants", [])
        active_spec = gateway_projection.get("spec", {})
        if not isinstance(active_spec, Mapping):
            raise FullGateError("Gateway active Topology projection has no Spec")
        active_failed_routes = [
            route
            for route in active_routes
            if isinstance(route, Mapping)
            and route.get("consumer_deployment_id") == deployment_id
        ]
        active_failed_grants = [
            grant
            for grant in active_grants
            if isinstance(grant, Mapping)
            and grant.get("consumer_deployment_id") == deployment_id
        ]
        active_failed_endpoints = [
            endpoint
            for endpoint in active_spec.get("endpoints", [])
            if isinstance(endpoint, Mapping)
            and (
                endpoint.get("endpoint") == logical_endpoint
                or endpoint.get("config", {}).get("deployment_id") == deployment_id
            )
        ]
        auth_failed_routes = [
            route
            for route in auth_projection.get("routes", [])
            if isinstance(route, Mapping)
            and route.get("consumer_deployment_id") == deployment_id
        ]
        auth_failed_grants = [
            grant
            for grant in auth_projection.get("grants", [])
            if isinstance(grant, Mapping)
            and grant.get("consumer_deployment_id") == deployment_id
        ]
        gateway_business_before = gateway_projection_before["business"]
        gateway_business_after = gateway_projection_after["business"]
        auth_business_before = auth_projection_before["business"]
        auth_business_after = auth_projection_after["business"]
        if (
            gateway_projection.get("provider") != "gateway"
            or gateway_projection.get("topology_id") != self.topology_id
            or gateway_projection.get("revision_id") != before_revision
            or gateway_projection.get("content_sha256") != before_content_sha256
            or _canonical(active_spec) != _canonical(before_spec)
            or gateway_projection_before.get("index_member") != 1
            or gateway_projection_after.get("index_member") != 1
            or gateway_projection_before.get("key")
            != gateway_projection_after.get("key")
            or gateway_projection_before.get("index_key")
            != gateway_projection_after.get("index_key")
            or _canonical(gateway_business_before)
            != _canonical(gateway_business_after)
            or not isinstance(active_routes, list)
            or not active_routes
            or not isinstance(active_grants, list)
            or active_failed_routes
            or active_failed_grants
            or active_failed_endpoints
            or auth_projection.get("provider") != "auth"
            or auth_projection.get("topology_id") != self.topology_id
            or auth_projection.get("revision_id") != before_revision
            or auth_projection.get("content_sha256") != before_content_sha256
            or _canonical(auth_business_before) != _canonical(auth_business_after)
            or not isinstance(auth_projection.get("routes"), list)
            or not auth_projection.get("routes")
            or not isinstance(auth_projection.get("grants"), list)
            or not auth_projection.get("grants")
            or auth_failed_routes
            or auth_failed_grants
            or auth_projection_before.get("failed_consumer_grant_count") != 0
            or auth_projection_after.get("failed_consumer_grant_count") != 0
            or _canonical(auth_projection_before.get("grant_rows", []))
            != _canonical(auth_projection_after.get("grant_rows", []))
            or auth_projection_before.get("grant_count")
            != auth_projection_after.get("grant_count")
            or _canonical(durable_bindings_before.get("rows", []))
            != _canonical(durable_bindings_after.get("rows", []))
            or durable_bindings_before.get("binding_count")
            != durable_bindings_after.get("binding_count")
            or durable_bindings_after.get("active_count")
            != durable_bindings_after.get("binding_count")
            or durable_bindings_after.get("non_active_count") != 0
            or durable_bindings_after.get("wrong_revision_count") != 0
        ):
            raise FullGateError(
                "failed Worker compensation changed the prior Gateway/Auth projection or durable Binding set"
            )

        consumer_after = {
            service: self._workload_context_snapshot(service, "after")
            for service in ("problem-service", "judge-api")
        }
        consumer_rollbacks: list[dict[str, Any]] = []
        for service_id, before in consumer_before.items():
            after = consumer_after[service_id]
            stable_fields = (
                "deployment_id",
                "node_id",
                "container_id",
                "generation",
                "binding_names",
                "binding_ids",
                "binding_routes",
                "context_sha256",
            )
            before_claims = before.get("credential_claims", {})
            after_claims = after.get("credential_claims", {})
            stable_claim_fields = (
                "deployment_id",
                "service_id",
                "node_id",
                "credential_generation",
                "issuer",
                "audience",
            )
            before_expiry = before_claims.get("expires_at_unix")
            after_expiry = after_claims.get("expires_at_unix")
            if (
                any(before[field] != after[field] for field in stable_fields)
                or not isinstance(before_claims, Mapping)
                or not isinstance(after_claims, Mapping)
                or any(
                    before_claims.get(field) != after_claims.get(field)
                    for field in stable_claim_fields
                )
                or isinstance(before_expiry, bool)
                or not isinstance(before_expiry, int)
                or isinstance(after_expiry, bool)
                or not isinstance(after_expiry, int)
                or after_expiry < before_expiry - 5
            ):
                raise FullGateError(
                    f"failed Worker install changed existing {service_id} context or credential identity"
                )
            consumer_rollbacks.append(
                {
                    "service_id": service_id,
                    "deployment_id": before["deployment_id"],
                    "node_id": before["node_id"],
                    "container_id_before": before["container_id"],
                    "container_id_after": after["container_id"],
                    "context_generation_before": before["generation"],
                    "context_generation_after": after["generation"],
                    "binding_names_before": before["binding_names"],
                    "binding_names_after": after["binding_names"],
                    "binding_ids_before": before["binding_ids"],
                    "binding_ids_after": after["binding_ids"],
                    "binding_routes_before": before["binding_routes"],
                    "binding_routes_after": after["binding_routes"],
                    "context_sha256_before": before["context_sha256"],
                    "context_sha256_after": after["context_sha256"],
                    "credential_claims_before": before_claims,
                    "credential_claims_after": after_claims,
                    "workload_credential_file_sha256_before": before[
                        "workload_credential_file_sha256"
                    ],
                    "workload_credential_file_sha256_after": after[
                        "workload_credential_file_sha256"
                    ],
                    "context_content_unchanged": True,
                    "credential_claim_identity_unchanged": True,
                    "credential_expiry_non_decreasing": True,
                    "credential_refresh_during_fault_window": (
                        before["workload_credential_file_sha256"]
                        != after["workload_credential_file_sha256"]
                    ),
                }
            )

        gateway_after = json.loads(
            self.a.command("inspect", "gateway-tls-a", timeout=30).stdout
        )[0]
        node_health = self._wait_node_ready(node_id, timeout=60)
        if (
            gateway_after.get("Id") != gateway_container_id
            or gateway_after.get("State", {}).get("Running") is not True
            or restored_health_status != 200
            or int(node_health.get("unhealthy_deployments", 0)) != 0
        ):
            raise FullGateError(
                "TLS Gateway or Node B did not recover after Worker compensation proof"
            )

        rollback_operation, recovery_revision, rollback_proof = (
            self._rollback_topology_revision(
                target_revision_id=before_revision,
                parent_revision_id=proposed_revision,
                key="worker-compensation-rollback",
            )
        )
        recovered_projection_deadline = time.monotonic() + 60
        gateway_projection_recovered: dict[str, Any] = {}
        auth_projection_recovered: dict[str, Any] = {}
        while time.monotonic() < recovered_projection_deadline:
            gateway_projection_recovered = (
                self._gateway_topology_projection_snapshot()
            )
            auth_projection_recovered = self._auth_topology_projection_snapshot(
                deployment_id
            )
            if (
                gateway_projection_recovered.get("projection", {}).get(
                    "revision_id"
                )
                == recovery_revision
                and auth_projection_recovered.get("projection", {}).get(
                    "revision_id"
                )
                == recovery_revision
            ):
                break
            time.sleep(1)
        else:
            raise FullGateError(
                "Gateway/Auth projections did not observe the compensation rollback revision"
            )
        durable_bindings_recovered = self._durable_topology_bindings_snapshot(
            recovery_revision
        )
        gateway_recovered_business = gateway_projection_recovered["business"]
        auth_recovered_business = auth_projection_recovered["business"]
        gateway_stable_before = self._projection_stable_fields(
            gateway_projection_before["projection"]
        )
        gateway_stable_recovered = self._projection_stable_fields(
            gateway_projection_recovered["projection"]
        )
        auth_stable_before = self._projection_stable_fields(
            auth_projection_before["projection"]
        )
        auth_stable_recovered = self._projection_stable_fields(
            auth_projection_recovered["projection"]
        )
        gateway_generations_before = self._projection_consumer_generations(
            gateway_projection_before["projection"]
        )
        gateway_generations_recovered = self._projection_consumer_generations(
            gateway_projection_recovered["projection"]
        )
        auth_generations_before = self._projection_consumer_generations(
            auth_projection_before["projection"]
        )
        auth_generations_recovered = self._projection_consumer_generations(
            auth_projection_recovered["projection"]
        )
        auth_grant_stable_before = self._auth_grant_stable_rows(
            auth_projection_before.get("grant_rows", [])
        )
        auth_grant_stable_recovered = self._auth_grant_stable_rows(
            auth_projection_recovered.get("grant_rows", [])
        )
        auth_grant_generations_before = self._auth_grant_consumer_generations(
            auth_projection_before.get("grant_rows", [])
        )
        auth_grant_generations_recovered = (
            self._auth_grant_consumer_generations(
                auth_projection_recovered.get("grant_rows", [])
            )
        )
        durable_generations_before = durable_bindings_before.get(
            "consumer_generations", {}
        )
        durable_generations_recovered = durable_bindings_recovered.get(
            "consumer_generations", {}
        )
        expected_consumers = {
            str(snapshot["deployment_id"]) for snapshot in consumer_before.values()
        }
        for provider, recovered, baseline in (
            ("gateway", gateway_stable_recovered, gateway_stable_before),
            ("auth", auth_stable_recovered, auth_stable_before),
        ):
            if (
                recovered.get("provider") != provider
                or recovered.get("topology_id") != self.topology_id
                or recovered.get("content_sha256") != before_content_sha256
                or _canonical(recovered) != _canonical(baseline)
            ):
                raise FullGateError(
                    f"{provider} rollback projection changed stable route/grant identity"
                )
        generation_maps_before = (
            gateway_generations_before,
            auth_generations_before,
            auth_grant_generations_before,
            durable_generations_before,
        )
        generation_maps_recovered = (
            gateway_generations_recovered,
            auth_generations_recovered,
            auth_grant_generations_recovered,
            durable_generations_recovered,
        )
        if (
            any(set(generations) != expected_consumers for generations in generation_maps_before)
            or any(
                set(generations) != expected_consumers
                for generations in generation_maps_recovered
            )
            or any(
                generations != gateway_generations_before
                for generations in generation_maps_before[1:]
            )
            or any(
                generations != gateway_generations_recovered
                for generations in generation_maps_recovered[1:]
            )
            or any(
                gateway_generations_recovered[consumer]
                != gateway_generations_before[consumer] + 1
                for consumer in expected_consumers
            )
        ):
            raise FullGateError(
                "compensation rollback did not advance each consumer generation exactly once across providers"
            )
        if (
            gateway_projection_recovered.get("projection", {}).get("revision_id")
            != recovery_revision
            or auth_projection_recovered.get("projection", {}).get("revision_id")
            != recovery_revision
            or gateway_projection_recovered.get("index_member") != 1
            or auth_projection_recovered.get("failed_consumer_grant_count") != 0
            or _canonical(auth_grant_stable_recovered)
            != _canonical(auth_grant_stable_before)
            or durable_bindings_recovered.get("binding_count")
            != durable_bindings_before.get("binding_count")
            or durable_bindings_recovered.get("active_count")
            != durable_bindings_recovered.get("binding_count")
            or durable_bindings_recovered.get("non_active_count") != 0
            or durable_bindings_recovered.get("wrong_revision_count") != 0
            or _canonical(durable_bindings_recovered.get("business_rows", []))
            != _canonical(durable_bindings_before.get("business_rows", []))
        ):
            raise FullGateError(
                "compensation rollback did not preserve active Auth grants or durable Bindings"
            )

        consumer_recovered: dict[str, dict[str, Any]] = {}
        for service_id, before in consumer_before.items():
            self._wait_container_context_generation(
                self.a,
                str(before["container_id"]),
                int(before["generation"]),
                f"{service_id}-compensation-context-recovered-wait",
            )
            consumer_recovered[service_id] = self._workload_context_snapshot(
                service_id, "recovered"
            )
        rollback_by_service = {
            str(item["service_id"]): item for item in consumer_rollbacks
        }
        for service_id, before in consumer_before.items():
            fault_after = consumer_after[service_id]
            recovered = consumer_recovered[service_id]
            before_claims = before["credential_claims"]
            recovered_claims = recovered["credential_claims"]
            stable_context_fields = (
                "deployment_id",
                "node_id",
                "container_id",
                "binding_names",
                "binding_ids",
                "binding_routes",
            )
            stable_claim_fields = (
                "deployment_id",
                "service_id",
                "node_id",
                "issuer",
                "audience",
            )
            if (
                any(before[field] != recovered[field] for field in stable_context_fields)
                or recovered["generation"] != before["generation"] + 1
                or recovered["credential_generation"] != recovered["generation"]
                or any(
                    before_claims.get(field) != recovered_claims.get(field)
                    for field in stable_claim_fields
                )
                or recovered_claims.get("credential_generation")
                != recovered["generation"]
                or recovered_claims.get("expires_at_unix", 0)
                < fault_after["credential_claims"].get("expires_at_unix", 0) - 5
                or recovered["context_sha256"] == before["context_sha256"]
                or recovered["workload_credential_file_sha256"]
                == fault_after["workload_credential_file_sha256"]
            ):
                raise FullGateError(
                    f"rollback did not atomically rotate {service_id} ServiceContext and workload credential"
                )
            consumer_id = str(before["deployment_id"])
            if recovered["generation"] != gateway_generations_recovered.get(
                consumer_id
            ):
                raise FullGateError(
                    f"{service_id} ServiceContext generation does not match active route generation"
                )
            rollback_entry = rollback_by_service[service_id]
            rollback_entry.update(
                {
                    "context_generation_recovered": recovered["generation"],
                    "binding_names_recovered": recovered["binding_names"],
                    "binding_ids_recovered": recovered["binding_ids"],
                    "binding_routes_recovered": recovered["binding_routes"],
                    "context_sha256_recovered": recovered["context_sha256"],
                    "credential_claims_recovered": recovered_claims,
                    "workload_credential_file_sha256_recovered": recovered[
                        "workload_credential_file_sha256"
                    ],
                    "rollback_generation_increment": 1,
                    "context_and_credential_generation_aligned": True,
                    "context_content_rotated": True,
                    "credential_file_rotated": True,
                    "route_identity_preserved": True,
                }
            )

        evidence = {
            "fault": {
                "kind": "stop-container",
                "component": "gateway-tls-a",
                "container_id": gateway_container_id,
                "started_at_unix_ms": fault_started_at,
                "running_before_fault": True,
                "running_at_install_start": not stopped_before_install,
                "running_at_install_completion": not stopped_after_install,
            },
            "failed_deployment": {
                "deployment_id": deployment_id,
                "node_id": node_id,
                "logical_endpoint": logical_endpoint,
                "cache_volume_name": cache_volume,
                "context_directory": context_directory,
            },
            "operation": {
                "operation_id": operation.get("operation_id"),
                "status": operation.get("status"),
                "needs_attention": False,
                "attention_job_ids_count": len(operation.get("attention_job_ids", [])),
                "resource_cleanup_derived_from_operation_result": False,
            },
            "agent_attempt": {
                "job_id": agent_job.get("job_id"),
                "node_id": agent_job.get("node_id"),
                "lease_owner_instance_id": agent_job.get("lease_owner"),
                "attempt": agent_job.get("attempt"),
                "status": agent_job.get("status"),
                "result_action": agent_result.get("action"),
                "result_compensated": agent_result.get("compensated"),
                "removed_container_id": agent_result.get(
                    "removed_container_id"
                ),
                "failure_health_gate": agent_failure.get("health_gate"),
                "failure_probe_count": agent_failure.get("probe_count"),
                "last_health_observation": {
                    "probe": last_health_observation.get("probe"),
                    "observed_state": last_health_observation.get(
                        "observed_state"
                    ),
                    "health": last_health_observation.get("health"),
                    "probe_reason": last_health_observation.get(
                        "probe_reason"
                    ),
                },
                "post_start_health_gate_failure": True,
            },
            "container_readback": {
                "source": "docker-ps-by-deployment-label",
                "deployment_id": deployment_id,
                "expected_name": container_name,
                "matches": container_matches,
                "exact_name_inspect_exit_code": container_name_inspect.returncode,
                "exact_name_absent": container_name_inspect.returncode != 0,
                "absent": True,
            },
            "volume_readback": {
                "source": "docker-volume-ls-by-deployment-label",
                "deployment_id": deployment_id,
                "expected_name": cache_volume,
                "matches": volume_matches,
                "exact_name_inspect_exit_code": volume_name_inspect.returncode,
                "exact_name_absent": volume_name_inspect.returncode != 0,
                "absent": True,
            },
            "context_readback": {
                "source": "node-b-agent-host-filesystem",
                "deployment_id": deployment_id,
                "path": context_directory,
                "exists": False,
                "context_or_credential_file_present": False,
            },
            "runtime_readback": {
                "source": "GET /api/v1/deployments/{deploymentId}",
                "http_status": runtime_status,
                "problem_code": runtime_document.get("code"),
                "fake_running_projection_present": False,
            },
            "binding_readback": {
                "source": "GET /api/v1/deployments/{deploymentId}/bindings",
                "http_status": binding_status,
                "problem_code": binding_document.get("code"),
                "staged_or_active_present": False,
            },
            "control_plane_database_readback": database_counts,
            "topology_readback": {
                "source": "GET /api/v1/topologies/{topologyId}",
                "topology_id": self.topology_id,
                "selected_revision_id": before_revision,
                "baseline_status_desired_revision_id": before_status.get(
                    "desired_revision_id"
                ),
                "baseline_status_observed_revision_id": before_status.get(
                    "observed_revision_id"
                ),
                "baseline_status_state": before_status.get("state"),
                "baseline_status_drift": before_status.get("drift"),
                "operation_proposed_revision_id": proposed_revision,
                "draft_revision_id_after": after_revision,
                "draft_etag_after": after_etag,
                "applied_revision_id_after": after_heads.get(
                    "applied_revision_id"
                ),
                "observed_revision_id_after": after_status.get(
                    "observed_revision_id"
                ),
                "desired_revision_id_after": after_status.get(
                    "desired_revision_id"
                ),
                "status_state_after": after_status.get("state"),
                "status_last_operation_id": after_status.get("last_operation_id"),
                "status_drift": after_status.get("drift"),
                "status_snapshot_kind": "stable-reconciled-applied",
                "applying_revision_present": False,
                "selected_revision_readback_etag": baseline_etag,
                "selected_revision_number": baseline_number,
                "selected_content_sha256": before_content_sha256,
                "selected_spec_sha256_before": _sha256(_canonical(before_spec)),
                "selected_spec_sha256_readback": _sha256(
                    _canonical(baseline_revision.get("spec", {}))
                ),
                "failed_draft_readback_etag": failed_revision_etag,
                "failed_draft_revision_number": failed_revision_number,
                "failed_draft_parent_revision_id": failed_revision.get(
                    "parent_revision_id"
                ),
                "failed_draft_rollback_of_revision_id": failed_revision.get(
                    "rollback_of_revision_id"
                ),
                "failed_draft_content_sha256": failed_content_sha256,
                "failed_draft_spec_sha256": _sha256(_canonical(after_spec)),
                "failed_draft_endpoint_count": len(failed_endpoints),
                "failed_draft_link_count": len(failed_links),
                "failed_draft_requirements": sorted(desired_requirements),
                "failed_draft_provider_deployment_ids": sorted(desired_providers),
                "failed_draft_retained": True,
                "applied_runtime_preserved": True,
                "recovery_revision_id": recovery_revision,
                "next_retry_etag": f'"{recovery_revision}"',
            },
            "gateway_active_projection_readback": {
                "source": "redis-get-gateway-topology-projection",
                "key": gateway_projection_after.get("key"),
                "index_key": gateway_projection_after.get("index_key"),
                "index_member_before": gateway_projection_before.get(
                    "index_member"
                ),
                "index_member_after": gateway_projection_after.get(
                    "index_member"
                ),
                "provider": gateway_projection.get("provider"),
                "topology_id": gateway_projection.get("topology_id"),
                "active_revision_id": gateway_projection.get("revision_id"),
                "active_content_sha256": gateway_projection.get(
                    "content_sha256"
                ),
                "active_spec_sha256": _sha256(_canonical(active_spec)),
                "active_route_count": len(active_routes),
                "active_grant_count": len(active_grants),
                "business_sha256_before": _sha256(
                    _canonical(gateway_business_before)
                ),
                "business_sha256_after": _sha256(
                    _canonical(gateway_business_after)
                ),
                "routes_sha256_before": _sha256(
                    _canonical(gateway_business_before.get("routes", []))
                ),
                "routes_sha256_after": _sha256(_canonical(active_routes)),
                "grants_sha256_before": _sha256(
                    _canonical(gateway_business_before.get("grants", []))
                ),
                "grants_sha256_after": _sha256(_canonical(active_grants)),
                "operation_id_before": gateway_projection_before.get(
                    "projection", {}
                ).get("operation_id"),
                "operation_id_after": gateway_projection.get("operation_id"),
                "updated_at_before": gateway_projection_before.get(
                    "projection", {}
                ).get("updated_at"),
                "updated_at_after": gateway_projection.get("updated_at"),
                "failed_deployment_route_count": len(active_failed_routes),
                "failed_deployment_grant_count": len(active_failed_grants),
                "failed_deployment_endpoint_count": len(active_failed_endpoints),
                "previous_projection_preserved": True,
                "business_database_write_used": False,
            },
            "auth_active_projection_readback": {
                "source": "postgres-auth-projection-read-only-transaction",
                "provider": auth_projection.get("provider"),
                "topology_id": auth_projection.get("topology_id"),
                "active_revision_id": auth_projection.get("revision_id"),
                "active_content_sha256": auth_projection.get("content_sha256"),
                "active_spec_sha256": _sha256(
                    _canonical(auth_projection.get("spec", {}))
                ),
                "active_route_count": len(auth_projection.get("routes", [])),
                "active_grant_count": len(auth_projection.get("grants", [])),
                "business_sha256_before": _sha256(
                    _canonical(auth_business_before)
                ),
                "business_sha256_after": _sha256(
                    _canonical(auth_business_after)
                ),
                "routes_sha256_before": _sha256(
                    _canonical(auth_business_before.get("routes", []))
                ),
                "routes_sha256_after": _sha256(
                    _canonical(auth_business_after.get("routes", []))
                ),
                "grants_sha256_before": _sha256(
                    _canonical(auth_business_before.get("grants", []))
                ),
                "grants_sha256_after": _sha256(
                    _canonical(auth_business_after.get("grants", []))
                ),
                "materialized_grant_count_before": auth_projection_before.get(
                    "grant_count"
                ),
                "materialized_grant_count_after": auth_projection_after.get(
                    "grant_count"
                ),
                "materialized_grants_sha256_before": _sha256(
                    _canonical(auth_projection_before.get("grant_rows", []))
                ),
                "materialized_grants_sha256_after": _sha256(
                    _canonical(auth_projection_after.get("grant_rows", []))
                ),
                "failed_deployment_route_count": len(auth_failed_routes),
                "failed_deployment_grant_count": len(auth_failed_grants),
                "failed_deployment_materialized_grant_count": (
                    auth_projection_after.get("failed_consumer_grant_count")
                ),
                "previous_projection_preserved": True,
                "business_database_write_used": False,
            },
            "durable_binding_set_readback": {
                "source": "postgres-control-plane-bindings-read-only-transaction",
                "selected_revision_id": before_revision,
                "binding_count_before": durable_bindings_before.get(
                    "binding_count"
                ),
                "binding_count_after": durable_bindings_after.get(
                    "binding_count"
                ),
                "active_count_before": durable_bindings_before.get("active_count"),
                "active_count_after": durable_bindings_after.get("active_count"),
                "non_active_count_after": durable_bindings_after.get(
                    "non_active_count"
                ),
                "wrong_revision_count_after": durable_bindings_after.get(
                    "wrong_revision_count"
                ),
                "rows_sha256_before": _sha256(
                    _canonical(durable_bindings_before.get("rows", []))
                ),
                "rows_sha256_after": _sha256(
                    _canonical(durable_bindings_after.get("rows", []))
                ),
                "failed_deployment_binding_count": database_counts.get(
                    "binding_count"
                ),
                "exactly_preserved": True,
                "business_database_write_used": False,
            },
            "recovery_rollback": {
                **rollback_proof,
                "gateway_projection_revision_id": gateway_projection_recovered.get(
                    "projection", {}
                ).get("revision_id"),
                "gateway_projection_content_sha256": gateway_projection_recovered.get(
                    "projection", {}
                ).get("content_sha256"),
                "gateway_projection_spec_sha256": _sha256(
                    _canonical(gateway_recovered_business.get("spec", {}))
                ),
                "gateway_stable_routes_sha256_before": _sha256(
                    _canonical(gateway_stable_before.get("routes", []))
                ),
                "gateway_stable_routes_sha256_recovered": _sha256(
                    _canonical(gateway_stable_recovered.get("routes", []))
                ),
                "gateway_stable_grants_sha256_before": _sha256(
                    _canonical(gateway_stable_before.get("grants", []))
                ),
                "gateway_stable_grants_sha256_recovered": _sha256(
                    _canonical(gateway_stable_recovered.get("grants", []))
                ),
                "gateway_consumer_generations_before": gateway_generations_before,
                "gateway_consumer_generations_recovered": (
                    gateway_generations_recovered
                ),
                "gateway_index_member": gateway_projection_recovered.get(
                    "index_member"
                ),
                "auth_projection_revision_id": auth_projection_recovered.get(
                    "projection", {}
                ).get("revision_id"),
                "auth_projection_content_sha256": auth_projection_recovered.get(
                    "projection", {}
                ).get("content_sha256"),
                "auth_projection_spec_sha256": _sha256(
                    _canonical(auth_recovered_business.get("spec", {}))
                ),
                "auth_stable_routes_sha256_before": _sha256(
                    _canonical(auth_stable_before.get("routes", []))
                ),
                "auth_stable_routes_sha256_recovered": _sha256(
                    _canonical(auth_stable_recovered.get("routes", []))
                ),
                "auth_stable_grants_sha256_before": _sha256(
                    _canonical(auth_stable_before.get("grants", []))
                ),
                "auth_stable_grants_sha256_recovered": _sha256(
                    _canonical(auth_stable_recovered.get("grants", []))
                ),
                "auth_consumer_generations_before": auth_generations_before,
                "auth_consumer_generations_recovered": auth_generations_recovered,
                "auth_materialized_stable_grants_sha256_before": _sha256(
                    _canonical(auth_grant_stable_before)
                ),
                "auth_materialized_stable_grants_sha256_recovered": _sha256(
                    _canonical(auth_grant_stable_recovered)
                ),
                "auth_materialized_consumer_generations_before": (
                    auth_grant_generations_before
                ),
                "auth_materialized_consumer_generations_recovered": (
                    auth_grant_generations_recovered
                ),
                "auth_failed_deployment_grant_count": (
                    auth_projection_recovered.get("failed_consumer_grant_count")
                ),
                "durable_binding_count": durable_bindings_recovered.get(
                    "binding_count"
                ),
                "durable_binding_active_count": durable_bindings_recovered.get(
                    "active_count"
                ),
                "durable_binding_non_active_count": durable_bindings_recovered.get(
                    "non_active_count"
                ),
                "durable_binding_wrong_revision_count": (
                    durable_bindings_recovered.get("wrong_revision_count")
                ),
                "durable_binding_business_sha256_before": _sha256(
                    _canonical(durable_bindings_before.get("business_rows", []))
                ),
                "durable_binding_business_sha256_recovered": _sha256(
                    _canonical(durable_bindings_recovered.get("business_rows", []))
                ),
                "durable_consumer_generations_before": durable_generations_before,
                "durable_consumer_generations_recovered": (
                    durable_generations_recovered
                ),
                "affected_consumer_deployment_ids": sorted(expected_consumers),
                "each_consumer_generation_increment": 1,
                "all_generation_sources_aligned": True,
                "next_retry_etag": f'"{recovery_revision}"',
                "business_state_preserved": True,
            },
            "consumer_context_rollback": consumer_rollbacks,
            "gateway_recovery": {
                "component": "gateway-tls-a",
                "container_id_before": gateway_container_id,
                "container_id_after": gateway_after.get("Id"),
                "same_container": gateway_after.get("Id") == gateway_container_id,
                "running": gateway_after.get("State", {}).get("Running"),
                "public_health_status": restored_health_status,
                "node_b_ready": node_health.get("ready"),
                "node_b_unhealthy_deployments": node_health.get(
                    "unhealthy_deployments", 0
                ),
            },
            "credential_material_recorded": False,
        }
        return evidence, f'"{recovery_revision}"'

    def _install_worker(self, topology_etag: str) -> dict[str, Any]:
        selections = [
            {"name": "judge_control", "provider_deployment_id": self.provider_deployments["judge-api"]},
            {"name": "storage_get", "provider_deployment_id": self.provider_deployments["storage-service"]},
        ]
        base = {
            "service_id": "judge-worker",
            "catalog_source_id": "judge-worker",
            "version": "0.1.0",
            "channel": "stable",
            "target_node_id": "node-b",
            "bindings": selections,
            "topology_id": self.topology_id,
            "topology_etag": topology_etag,
        }
        validated, _, _ = self._control_mutation(
            "/api/v1/store/releases:validate", base, "worker-validate", expected=(200,)
        )
        validation_data = validated.get("data", {})
        if validation_data.get("valid") is not True:
            raise FullGateError(f"Store rejected Judge Worker plan: {validated}")
        install_body = {**base, "mode": "MANAGED", "start": True}
        installed, _, _ = self._control_mutation(
            "/api/v1/store/releases:install", install_body, "worker-install", expected=(202,)
        )
        install_data = installed.get("data", {})
        operation = self._wait_operation(str(install_data.get("operation_id", "")), timeout=420)
        if operation.get("status") != "SUCCEEDED":
            raise FullGateError(f"Store/Agent Judge Worker install failed: {operation}")
        self.worker_deployment_id = str(install_data.get("deployment_id", ""))
        if not self.worker_deployment_id:
            raise FullGateError("Judge Worker Store install omitted deployment_id")
        if install_data.get("endpoint") is not None:
            raise FullGateError(
                "backend-worker Store response unexpectedly published an inbound endpoint"
            )
        binding = next(
            (
                item for item in operation.get("job_bindings", [])
                if str(item.get("step_id", "")).startswith("install-judge-worker-")
            ),
            None,
        )
        if not isinstance(binding, dict):
            raise FullGateError("Worker install Operation has no Agent install Job binding")
        job = self._query_json(
            "ojos_orchestrator",
            "SELECT payload::text FROM orchestrator_jobs WHERE job_id="
            + self._sql(str(binding["job_id"])),
        )
        lease_token = str(job.get("lease_token", ""))
        if not lease_token:
            raise FullGateError("Worker install Job did not retain its completed lease evidence")
        runtime_convergence = self._managed_runtime_convergence(
            self.worker_deployment_id, "node-b", job
        )
        deployment_response = runtime_convergence["inventory_payload"]
        instance = deployment_response["instance"]
        runtime_contract = instance.get("runtime_contract", {})
        topology = self._control_get(f"/api/v1/topologies/{self.topology_id}")
        topology_spec = topology.get("data", {}).get("draft", {}).get("spec", {})
        worker_endpoints = [
            item
            for item in topology_spec.get("endpoints", [])
            if item.get("config", {}).get("deployment_id") == self.worker_deployment_id
        ]
        if len(worker_endpoints) != 1:
            raise FullGateError(
                "Store did not add exactly one logical Worker endpoint to Topology"
            )
        logical_endpoint = str(worker_endpoints[0].get("endpoint", ""))
        expected_logical_endpoint = (
            f"{self.h.evidence['engines']['b']['outer_ip']}:9101:judge-worker"
        )
        if logical_endpoint != expected_logical_endpoint:
            raise FullGateError(
                "Store automatic backend-worker endpoint did not derive from Node facts: "
                + logical_endpoint
            )
        bindings_response = self._control_get(
            f"/api/v1/deployments/{self.worker_deployment_id}/bindings"
        ).get("data", {})
        store_evidence = {
            "agent": self.agent_identity,
            "store_validate": {
                "accepted": True,
                "request_id": validated.get("meta", {}).get("request_id"),
                "topology_etag": topology_etag,
                "bindings": selections,
                "request_fields": sorted(base),
                "provider_candidates": validation_data.get("requirements", []),
            },
            "store_install": {
                "accepted": True,
                "request_id": installed.get("meta", {}).get("request_id"),
                "topology_etag": topology_etag,
                "bindings": selections,
                "request_fields": sorted(install_body),
                "request_endpoint_present": "endpoint" in install_body,
                "response_published_endpoint": install_data.get("endpoint"),
                "topology_logical_endpoint": logical_endpoint,
                "automatic_logical_endpoint": True,
                "operation_id": operation.get("operation_id"),
                "deployment_id": self.worker_deployment_id,
            },
            "operation": {
                "operation_id": operation.get("operation_id"),
                "status": operation.get("status"),
                "job_id": binding.get("job_id"),
                "revision": operation.get("revision"),
            },
            "agent_job": {
                "job_id": job.get("job_id"),
                "attempt_id": f"{job.get('job_id')}:attempt:{job.get('attempt')}",
                "lease_id": _sha256(lease_token),
                "lease_owner_instance_id": job.get("lease_owner"),
                "status": job.get("status"),
                "completed_by_agent": job.get("node_id") == "node-b" and bool(job.get("lease_owner")),
            },
            "deployment": {
                "deployment_id": instance.get("deployment_id"),
                "node_id": deployment_response.get("node_id"),
                "desired_state": instance.get("desired_state"),
                "observed_state": instance.get("observed_state"),
                "health": instance.get("health"),
                "runtime_profile": runtime_contract.get("id"),
                "runtime_attested": instance.get("runtime_attested"),
                "drift_reason": deployment_response.get("drift_reason"),
                "last_observed_at_ms": deployment_response.get("last_observed_at_ms"),
                "runtime_projection": runtime_convergence,
            },
            "bindings": bindings_response.get("items", []),
        }
        self.h.evidence["store_agent_evidence"] = store_evidence
        return {"operation": operation, "job": job, "store_evidence": store_evidence}

    def _inspect_managed_worker(self, install: Mapping[str, Any]) -> dict[str, Any]:
        output = self.b.command(
            "ps", "--filter", "label=ojos.deployment_id=" + self.worker_deployment_id,
            "--format", "{{.ID}}", "--no-trunc",
        ).stdout.strip().splitlines()
        if len(output) != 1:
            raise FullGateError(f"expected exactly one Agent-created Worker container, got {output}")
        self.worker_container_id = output[0]
        inspected = json.loads(self.b.command("inspect", self.worker_container_id).stdout)[0]
        config_user = str(inspected.get("Config", {}).get("User", ""))
        host = inspected.get("HostConfig", {})
        caps = set(host.get("CapAdd") or [])
        if (
            host.get("Privileged") is not True
            or config_user != "0:0"
            or caps != {"SYS_ADMIN", "SYS_CHROOT", "NET_ADMIN"}
            or str(host.get("CgroupnsMode", "")).lower() != "host"
            or not _judge_sandbox_security_options_are_exact(
                host.get("SecurityOpt")
            )
            or not _judge_sandbox_host_mounts_are_exact(host.get("Mounts"))
            or str(host.get("NetworkMode", "")) != "bridge"
            or (host.get("PortBindings") or {})
        ):
            raise FullGateError("Agent-created Worker HostConfig does not match judge-sandbox-v1")
        mounts = inspected.get("Mounts", [])
        context_mount = next((item for item in mounts if item.get("Destination") == "/run/ojos/service"), None)
        cgroup_mount = next((item for item in mounts if item.get("Destination") == "/sys/fs/cgroup"), None)
        if not context_mount or context_mount.get("RW") is not False:
            raise FullGateError("Agent did not mount ServiceContext read-only")
        if not cgroup_mount or cgroup_mount.get("RW") is not True:
            raise FullGateError("Agent did not mount host cgroup v2 read-write")
        context_path = self.tmp / "observed-context.json"
        self.b.command("cp", self.worker_container_id + ":/run/ojos/service/context.json", context_path)
        context = json.loads(context_path.read_text(encoding="utf-8"))
        serialized = _canonical(context).lower()
        if any(word in serialized for word in ("admin_token", "management_token", "access_token")):
            raise FullGateError("Agent ServiceContext contains management or embedded credentials")
        api_binding_ids = {
            str(item.get("binding_id", ""))
            for item in install["store_evidence"]["bindings"]
            if item.get("binding_id")
        }
        context_binding_ids = {
            str(item.get("binding_id", "")) for item in context.get("bindings", {}).values()
        }
        if context_binding_ids != api_binding_ids:
            raise FullGateError("materialized ServiceContext does not match durable ApiBindings")
        context_source = str(context_mount.get("Source", ""))
        component = hashlib.sha256(
            self.worker_deployment_id.encode("utf-8")
        ).hexdigest()[:32]
        expected_context_source = (
            f"{B_WORKLOAD_EXPORT_ROOT}/runtime-contexts/{component}/service"
        )
        if context_source != expected_context_source:
            raise FullGateError(
                "Worker ServiceContext source escaped the node-b Agent namespace"
            )
        context_file_identity = self._service_context_file_identity(
            self.b, context_source, self.worker_container_id, "0:0"
        )
        image_ref = str(inspected.get("Config", {}).get("Image", ""))
        if image_ref != self.oci["worker"]:
            raise FullGateError(f"Worker did not use exact Catalog RepoDigest: {image_ref}")
        health = inspected.get("State", {}).get("Health", {}).get("Status", "")
        if str(health).lower() != "healthy":
            raise FullGateError(f"Agent-created Worker did not pass Docker health gate: {health}")
        host_digest = _sha256(_canonical(host))
        service_context_evidence = {
            "generation": context.get("generation"),
            "deployment_id": context.get("deployment", {}).get("id"),
            "node_id": context.get("deployment", {}).get("node"),
            "binding_ids": sorted(context_binding_ids),
            "mount_read_only": True,
            "credential_embedded": False,
            "management_token_present": False,
            "gateway_origin": context.get("gateway", {}).get("origin"),
            "file_identity": context_file_identity,
        }
        runtime = {
            "created_by_agent": True,
            "context_mount_read_only": True,
            "health_gate": str(health).upper(),
            "runtime_profile": inspected.get("Config", {}).get("Labels", {}).get("ojos.runtime_profile"),
            "host_config_digest": host_digest,
            "image_repo_digest": image_ref.split("@", 1)[-1],
            "container_id": inspected.get("Id"),
            "engine_id": self.h.evidence["engines"]["b"]["engine_id"],
            "actual_host_config": {
                "privileged": host.get("Privileged"), "cap_add": sorted(caps),
                "cgroupns_mode": host.get("CgroupnsMode"), "security_opt": host.get("SecurityOpt"),
                "port_bindings": host.get("PortBindings") or {},
            },
            "config_user": config_user,
        }
        install["store_evidence"]["service_context"] = service_context_evidence
        install["store_evidence"]["runtime"] = runtime
        return {"inspect": inspected, "context": context, "runtime": runtime}

    def _worker_health_sample(self, phase: str) -> dict[str, Any]:
        if not self.worker_container_id:
            raise FullGateError("Worker health sampling requires an installed container")
        inspected = json.loads(
            self.b.command("inspect", self.worker_container_id, timeout=30).stdout
        )[0]
        actual_id = str(inspected.get("Id", ""))
        if actual_id != self.worker_container_id:
            raise FullGateError(
                "Worker container identity changed while sampling recovery health"
            )
        state = inspected.get("State", {})
        health = state.get("Health", {})
        return {
            "phase": phase,
            "observed_at_unix_ms": int(time.time() * 1000),
            "container_id": actual_id,
            "running": state.get("Running") is True,
            "restart_count": int(inspected.get("RestartCount", 0)),
            "status": str(health.get("Status", "")).upper(),
            "failing_streak": int(health.get("FailingStreak", 0)),
        }

    def _wait_worker_health_transition(
        self, expected: str, phase: str, *, timeout: float
    ) -> list[dict[str, Any]]:
        deadline = time.monotonic() + timeout
        expected = expected.upper()
        history: list[dict[str, Any]] = []
        previous: tuple[Any, ...] | None = None
        latest: dict[str, Any] = {}
        while time.monotonic() < deadline:
            latest = self._worker_health_sample(phase)
            marker = (
                latest.get("status"),
                latest.get("running"),
                latest.get("restart_count"),
                latest.get("failing_streak"),
            )
            if marker != previous:
                history.append(latest)
                previous = marker
            if latest.get("running") is True and latest.get("status") == expected:
                return history
            time.sleep(1)
        raise FullGateError(
            f"Worker did not transition to {expected} during {phase}: {latest}"
        )

    def _gateway_proxy_captures(self) -> list[dict[str, Any]]:
        assert self.gateway_client is not None
        document = self.gateway_client.request(
            "GET",
            "/__fixture/proxy-evidence",
            headers={"x-fixture-control": "cross-machine-fixture-control"},
            expected=(200,),
        )[0]
        captures = document.get("captures", [])
        if not isinstance(captures, list):
            raise FullGateError("Gateway TLS proxy returned an invalid capture list")
        return [item for item in captures if isinstance(item, dict)]

    def _wait_worker_reregistration_capture(
        self, baseline_sequence: int, *, timeout: float
    ) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        latest: list[dict[str, Any]] = []
        while time.monotonic() < deadline:
            latest = self._gateway_proxy_captures()
            matched = next(
                (
                    item
                    for item in latest
                    if int(item.get("sequence", 0)) > baseline_sequence
                    and item.get("method") == "POST"
                    and str(item.get("path", "")).split("?", 1)[0].endswith(
                        "/internal/apis/judge.worker.control/register"
                    )
                    and int(item.get("status", 0)) == 200
                    and item.get("request_body", {}).get("worker_id")
                    == self.worker_deployment_id
                ),
                None,
            )
            if isinstance(matched, dict):
                return {
                    "sequence": matched.get("sequence"),
                    "captured_at_unix_ms": matched.get("captured_at_unix_ms"),
                    "method": matched.get("method"),
                    "path": matched.get("path"),
                    "status": matched.get("status"),
                    "worker_id": matched.get("request_body", {}).get("worker_id"),
                }
            time.sleep(1)
        raise FullGateError(
            "Gateway TLS transcript did not capture Worker re-registration after recovery; "
            f"baseline={baseline_sequence}, captures={latest[-10:]}"
        )

    def _prove_worker_recovery(self, first_flow: Mapping[str, Any]) -> dict[str, Any]:
        judge_container_id = str(
            self.managed_a_runtimes.get("judge-api", {}).get("container_id", "")
        )
        gateway_container_id = self.a.command(
            "inspect", "--format", "{{.Id}}", "gateway-a", timeout=30
        ).stdout.strip()
        if not judge_container_id or not gateway_container_id:
            raise FullGateError("Worker recovery requires the A Gateway and managed Judge API")

        initial = self._worker_health_sample("before-disruption")
        if initial.get("status") != "HEALTHY" or initial.get("running") is not True:
            raise FullGateError(f"Worker was not healthy before disruption: {initial}")
        captures = self._gateway_proxy_captures()
        baseline_sequence = max(
            (int(item.get("sequence", 0)) for item in captures), default=0
        )
        disruption_started = int(time.time() * 1000)
        self.a.command(
            "stop",
            "--time",
            "10",
            "gateway-a",
            judge_container_id,
            timeout=60,
        )
        unhealthy_history = self._wait_worker_health_transition(
            "UNHEALTHY", "services-unavailable", timeout=120
        )

        restore_started = int(time.time() * 1000)
        self.a.command("start", judge_container_id, timeout=60)
        self._wait_managed_a_http(8082, timeout=120)
        self.a.command("start", "gateway-a", timeout=60)
        self._wait_a_http("gateway-a", 8080, "/health", timeout=120)
        healthy_history = self._wait_worker_health_transition(
            "HEALTHY", "services-restored", timeout=180
        )
        reregister = self._wait_worker_reregistration_capture(
            baseline_sequence, timeout=60
        )
        recovered_flow = self._run_actual_flow("recovered")
        final = self._worker_health_sample("recovered-task-complete")
        if final.get("status") != "HEALTHY" or final.get("running") is not True:
            raise FullGateError(f"Worker was not healthy after recovered task: {final}")
        if (
            recovered_flow.get("task", {}).get("task_id")
            == first_flow.get("task", {}).get("task_id")
            or recovered_flow.get("submission", {}).get("submission_id")
            == first_flow.get("submission", {}).get("submission_id")
        ):
            raise FullGateError("Worker recovery reused the pre-disruption task or submission")
        projection_integrity = self._capture_provider_projection_integrity(
            "worker-recovery"
        )

        judge_after = json.loads(
            self.a.command("inspect", judge_container_id, timeout=30).stdout
        )[0]
        gateway_after = json.loads(
            self.a.command("inspect", gateway_container_id, timeout=30).stdout
        )[0]
        return {
            "worker_deployment_id": self.worker_deployment_id,
            "worker_container_id_before": initial.get("container_id"),
            "worker_container_id_after": final.get("container_id"),
            "capture_baseline_sequence": baseline_sequence,
            "disruption_started_at_unix_ms": disruption_started,
            "restore_started_at_unix_ms": restore_started,
            "disrupted_services": [
                {"name": "gateway", "container_id": gateway_container_id},
                {"name": "judge-api", "container_id": judge_container_id},
            ],
            "restored_services": [
                {
                    "name": "gateway",
                    "container_id": str(gateway_after.get("Id", "")),
                    "running": gateway_after.get("State", {}).get("Running") is True,
                },
                {
                    "name": "judge-api",
                    "container_id": str(judge_after.get("Id", "")),
                    "running": judge_after.get("State", {}).get("Running") is True,
                    "health": str(
                        judge_after.get("State", {}).get("Health", {}).get("Status", "")
                    ).upper(),
                },
            ],
            "health_timeline": [initial, *unhealthy_history, *healthy_history, final],
            "reregistration": reregister,
            "recovered_flow": recovered_flow,
            "provider_projection_integrity": projection_integrity,
        }

    # ------------------------------------------------------------- actual flow

    @staticmethod
    def _actual_flow_username(run_id: str, suffix: str) -> str:
        """Build a readable, unique fixture username within Auth's 32 byte limit."""
        label = re.sub(r"[^a-z0-9]", "", suffix.lower())[:8] or "flow"
        digest = hashlib.sha256(
            (run_id + "\x00" + suffix).encode("utf-8")
        ).hexdigest()[:12]
        return f"cm_{label}_{digest}"

    def _run_actual_flow(self, suffix: str) -> dict[str, Any]:
        assert self.gateway_client is not None
        admin_token = self._ensure_admin_token()
        username = self._actual_flow_username(self.h.run_id, suffix)
        password = "CrossMachine-" + self.h.run_id + "-Pass9!"
        registered, _, _ = self.gateway_client.request(
            "POST", "/api/auth/register", {"username": username, "email": username + "@example.invalid", "password": password},
            expected=(200,),
        )
        user_id = int(registered.get("data", {}).get("user_id", 0))
        if user_id <= 0:
            raise FullGateError(f"Auth registration did not return user_id: {registered}")
        self.gateway_client.request(
            "POST", "/api/auth/admin/users/roles", {"user_id": user_id, "role": "super_admin"},
            headers={"authorization": "Bearer " + admin_token}, expected=(200,),
        )
        login, _, _ = self.gateway_client.request(
            "POST", "/api/auth/login", {"username": username, "password": password}, expected=(200,)
        )
        token = str(login.get("data", {}).get("token", ""))
        if not token:
            raise FullGateError("Auth login did not return a token")
        auth = {"authorization": "Bearer " + token}
        problem, _, _ = self.gateway_client.request(
            "POST", "/api/problem/problems",
            {
                "title": "Cross-machine actual " + suffix,
                "slug": "cross-machine-" + suffix + "-" + self.h.run_id,
                "statement": "Print ok.", "visibility": "public",
                "time_limit_ms": 3000, "memory_limit_mb": 256,
            },
            headers=auth, expected=(200,),
        )
        problem_id = int(problem.get("problem_id", 0))
        if problem_id <= 0:
            raise FullGateError(f"Problem API did not create a problem: {problem}")
        self.gateway_client.request(
            "POST", f"/api/problem/problems/{problem_id}/test-cases",
            {"case_no": 1, "input": "1 1\n", "answer": "ok\n", "score": 100, "group": 0, "sample": True},
            headers=auth, expected=(200,),
        )
        package_validation = self.gateway_client.request(
            "POST",
            f"/api/problem/problems/{problem_id}/package/validate",
            {},
            headers=auth,
            expected=(200,),
        )[0].get("validation", {})
        if package_validation.get("valid") is not True or package_validation.get("errors"):
            raise FullGateError(
                f"Problem published an invalid package before Judge projection: {package_validation}"
            )
        problem_evidence = self._wait_problem_projection(problem_id)
        submission, _, _ = self.gateway_client.request(
            "POST", "/api/judge/submissions",
            {
                "problem_id": problem_id,
                "language": "cpp17",
                "code": '#include <iostream>\nint main(){std::cout << "ok\\n";return 0;}\n',
            },
            headers=auth, expected=(200,),
        )
        submission_id = int(submission.get("submission_id", 0))
        if submission_id <= 0:
            raise FullGateError(f"Judge API did not create a submission: {submission}")
        final: dict[str, Any] = {}
        deadline = time.monotonic() + 240
        while time.monotonic() < deadline:
            final = self.gateway_client.request(
                "GET", f"/api/judge/submissions/{submission_id}", headers=auth, expected=(200,)
            )[0]
            if str(final.get("status", "")).upper() in {
                "ACCEPTED", "WRONG_ANSWER", "COMPILE_ERROR", "RUNTIME_ERROR", "SYSTEM_ERROR", "FAILED"
            }:
                break
            time.sleep(1)
        if str(final.get("status", "")).upper() != "ACCEPTED":
            logs = self.b.command("logs", "--tail", "300", self.worker_container_id, check=False).stdout
            raise FullGateError(f"actual Worker did not accept submission: {final}; worker logs={logs[-4000:]}")
        cases = self.gateway_client.request(
            "GET", f"/api/judge/submissions/{submission_id}/cases", headers=auth, expected=(200,)
        )[0].get("cases", [])
        if not cases or any(str(case.get("status", "")).upper() != "ACCEPTED" for case in cases):
            raise FullGateError(f"actual nsjail case evidence is incomplete: {cases}")
        task_row = self._query_json(
            "ojos_judge",
            "SELECT json_build_object('task_id',t.task_id,'submission_id',t.submission_id,'problem_id',t.problem_id,'status',t.status,'worker_id',t.worker_id,'lease_version',t.lease_version,'attempt',t.attempt,'package_sha256',s.problem_artifact_sha256)::text FROM judge_tasks t JOIN submissions s ON s.id=t.submission_id WHERE t.submission_id=" + str(submission_id),
        )
        captured_task = self._captured_task(submission_id)
        result = self._redis_result(str(task_row.get("task_id", "")), submission_id)
        source_ref = captured_task.get("source", {})
        package_ref = captured_task.get("problem_package", {})
        self._validate_production_ref(source_ref)
        self._validate_production_ref(package_ref)
        transcript = self._workload_request_transcript(
            str(task_row.get("task_id", "")),
            submission_id,
            source_ref,
            package_ref,
        )
        problem_evidence_value, projection_evidence_value, submission_package_sha256 = (
            _normalize_actual_flow_digest_evidence(
                problem_evidence, task_row.get("package_sha256")
            )
        )
        if suffix == "first":
            self.h.evidence["workload_request_transcript"] = transcript
        return {
            "same_chain": True,
            "source": "actual-services",
            "problem_created_via_http_api": True,
            "problem_package_validation": package_validation,
            "submission_created_via_http_api": True,
            "manual_judge_problem_insert": False,
            "problem": problem_evidence_value,
            "judge_projection": projection_evidence_value,
            "submission": {
                "submission_id": str(submission_id), "problem_id": str(problem_id),
                "package_sha256": submission_package_sha256, "status": final.get("status"),
            },
            "task": {
                "task_id": task_row.get("task_id"), "submission_id": str(submission_id),
                "problem_id": str(problem_id), "source": source_ref, "problem_package": package_ref,
                "wire_capture": "gateway TLS ingress claim response",
            },
            "result": result,
            "submission_cases": cases,
            "actual_components": [
                "orchestrator", "agent", "gateway", "auth", "problem-service", "judge-api",
                "storage-service", "postgresql", "redis", "minio", "rust-judge-worker", "nsjail",
            ],
            "workload_transcript_correlated": True,
        }

    def _ensure_admin_token(self) -> str:
        assert self.gateway_client is not None
        if self.admin_token:
            return self.admin_token
        bootstrap_request = {
            "username": self.admin_username,
            "email": self.admin_username + "@example.invalid",
            "password": self.admin_password,
        }
        created, _, created_status = self.gateway_client.request(
            "POST",
            "/api/auth/bootstrap/admin",
            bootstrap_request,
            headers={"x-ojos-bootstrap-secret": self.auth_bootstrap_secret},
            expected=(201,),
        )
        created_data = created.get("data", {})
        created_user_id = int(created_data.get("user_id", 0))
        if (
            created.get("code") != 0
            or created_user_id < 1
            or str(created_data.get("username", "")) != self.admin_username
        ):
            raise FullGateError(f"Auth bootstrap did not create an administrator: {created}")
        replay, _, replay_status = self.gateway_client.request(
            "POST",
            "/api/auth/bootstrap/admin",
            bootstrap_request,
            headers={"x-ojos-bootstrap-secret": self.auth_bootstrap_secret},
            expected=(409,),
        )
        if replay.get("code") != 40931:
            raise FullGateError(f"Auth bootstrap replay did not fail closed: {replay}")
        denied, _, denied_status = self.gateway_client.request(
            "POST",
            "/api/auth/bootstrap/admin",
            bootstrap_request,
            headers={"x-ojos-bootstrap-secret": secrets.token_urlsafe(48)},
            expected=(403,),
        )
        if denied.get("code") != 40331:
            raise FullGateError(f"Auth bootstrap rejected-secret response is invalid: {denied}")
        login, _, login_status = self.gateway_client.request(
            "POST",
            "/api/auth/login",
            {"username": self.admin_username, "password": self.admin_password},
            expected=(200,),
        )
        login_data = login.get("data", {})
        token = str(login_data.get("token", ""))
        login_roles = sorted(str(role) for role in login_data.get("roles", []))
        login_permissions = sorted(
            str(permission) for permission in login_data.get("permissions", [])
        )
        if (
            login.get("code") != 0
            or not token
            or int(login_data.get("user_id", 0)) != created_user_id
            or str(login_data.get("username", "")) != self.admin_username
            or "super_admin" not in login_roles
            or "system.admin" not in login_permissions
        ):
            raise FullGateError(
                "bootstrapped administrator did not obtain a matching privileged Auth login"
            )
        profile, _, profile_status = self.gateway_client.request(
            "GET",
            "/api/auth/me",
            headers={"authorization": "Bearer " + token},
            expected=(200,),
        )
        profile_data = profile.get("data", {})
        if (
            profile.get("code") != 0
            or int(profile_data.get("user_id", 0)) != created_user_id
            or str(profile_data.get("username", "")) != self.admin_username
            or "super_admin" not in profile_data.get("roles", [])
            or "system.admin" not in profile_data.get("permissions", [])
        ):
            raise FullGateError("Auth login JWT did not authenticate the bootstrapped administrator")
        database_proof = self._query_json(
            "ojos_auth",
            "SELECT json_build_object("
            "'marker_completed',s.completed_at IS NOT NULL,"
            "'marker_user_id',s.user_id::text,"
            "'super_admin_assigned',EXISTS(SELECT 1 FROM user_roles ur JOIN roles r ON r.id=ur.role_id WHERE ur.user_id=s.user_id AND r.name='super_admin'),"
            "'bootstrap_audit_count',(SELECT COUNT(*) FROM permission_audit_logs l WHERE l.action='auth.bootstrap.initial_admin' AND l.target_type='user' AND l.target_id=s.user_id)"
            ")::text FROM auth_bootstrap_state s WHERE s.bootstrap_key='initial-super-admin'",
        )
        if (
            database_proof.get("marker_completed") is not True
            or str(database_proof.get("marker_user_id", "")) != str(created_user_id)
            or database_proof.get("super_admin_assigned") is not True
            or int(database_proof.get("bootstrap_audit_count", 0)) != 1
        ):
            raise FullGateError(f"Auth bootstrap database proof is incomplete: {database_proof}")
        self.admin_token = token
        self.h.evidence["auth_admin_bootstrap"] = {
            "created_status": created_status,
            "created_code": created.get("code"),
            "created_user_id": str(created_user_id),
            "login_status": login_status,
            "login_code": login.get("code"),
            "login_user_matches_bootstrap": True,
            "login_has_super_admin": True,
            "login_has_system_admin": True,
            "profile_status": profile_status,
            "profile_code": profile.get("code"),
            "profile_authenticated_same_user": True,
            "replay_status": replay_status,
            "replay_code": replay.get("code"),
            "wrong_secret_status": denied_status,
            "wrong_secret_code": denied.get("code"),
            "database_proof": database_proof,
            "jwt_source": "auth-service-login-endpoint",
            "jwt_self_signed_by_harness": False,
            "manual_database_role_seed": False,
            "database_transactional": True,
            "secret_or_token_recorded": False,
            "credential_delivery": copy.deepcopy(
                self.auth_bootstrap_delivery_evidence
            ),
        }
        return token

    def _wait_problem_projection(self, problem_id: int) -> dict[str, Any]:
        deadline = time.monotonic() + 120
        latest: dict[str, Any] = {}
        projection: dict[str, Any] = {}
        while time.monotonic() < deadline:
            latest = self._query_json(
                "ojos_problem",
                "SELECT json_build_object('problem_id',p.id::text,'aggregate_version',p.aggregate_version,'package_revision',p.package_revision,'package_sha256',p.package_artifact_sha256,'outbox_event_id',o.event_id,'event_type',o.event_type,'published',o.published_at IS NOT NULL)::text FROM problems p JOIN LATERAL (SELECT event_id,event_type,published_at FROM integration_outbox WHERE aggregate_id='problem/' || p.id::text AND event_type='io.ojos.problem.snapshot.v1' ORDER BY aggregate_version DESC LIMIT 1) o ON TRUE WHERE p.id=" + str(problem_id),
                allow_empty=True,
            )
            projection = self._query_json(
                "ojos_judge",
                "SELECT json_build_object('problem_id',p.id::text,'aggregate_version',p.aggregate_version,'package_sha256',p.package_artifact_sha256,'event_id',p.projected_event_id)::text FROM problems p WHERE p.id=" + str(problem_id),
                allow_empty=True,
            )
            if (
                latest.get("published") is True
                and latest.get("outbox_event_id")
                and projection.get("event_id") == latest.get("outbox_event_id")
                and projection.get("aggregate_version") == latest.get("aggregate_version")
            ):
                return {"problem": latest, "projection": projection}
            time.sleep(1)
        raise FullGateError(f"Problem outbox did not project into Judge: problem={latest}, projection={projection}")

    def _captured_task(self, submission_id: int) -> dict[str, Any]:
        assert self.gateway_client is not None
        evidence = self.gateway_client.request(
            "GET", "/__fixture/proxy-evidence",
            headers={"x-fixture-control": "cross-machine-fixture-control"}, expected=(200,),
        )[0]
        for capture in reversed(evidence.get("captures", [])):
            tasks = capture.get("body", {}).get("tasks", []) if isinstance(capture, dict) else []
            for task in tasks:
                if int(task.get("submission_id", 0)) == submission_id:
                    return task
        raise FullGateError(f"TLS ingress did not capture actual claim response for submission {submission_id}")

    def _workload_request_transcript(
        self,
        task_id: str,
        submission_id: int,
        source_ref: Mapping[str, Any],
        package_ref: Mapping[str, Any],
    ) -> dict[str, Any]:
        assert self.gateway_client is not None
        evidence = self.gateway_client.request(
            "GET",
            "/__fixture/proxy-evidence",
            headers={"x-fixture-control": "cross-machine-fixture-control"},
            expected=(200,),
        )[0]
        captures = [
            item for item in evidence.get("captures", []) if isinstance(item, dict)
        ]
        claim = next(
            (
                item
                for item in reversed(captures)
                if item.get("method") == "POST"
                and str(item.get("path", "")).split("?", 1)[0].endswith(
                    "/internal/apis/judge.worker.control/tasks/claim"
                )
                and any(
                    int(task.get("submission_id", 0)) == submission_id
                    for task in item.get("body", {}).get("tasks", [])
                    if isinstance(task, dict)
                )
            ),
            None,
        )
        result = next(
            (
                item
                for item in reversed(captures)
                if item.get("method") == "POST"
                and str(item.get("path", "")).split("?", 1)[0].endswith(
                    f"/tasks/{task_id}/result"
                )
                and str(item.get("request_body", {}).get("status", "")).upper()
                == "ACCEPTED"
            ),
            None,
        )

        def resource_capture(reference: Mapping[str, Any]) -> dict[str, Any] | None:
            relative = str(reference.get("relative_path", ""))
            return next(
                (
                    item
                    for item in reversed(captures)
                    if item.get("method") == "GET"
                    and str(item.get("path", "")).split("?", 1)[0].endswith(relative)
                    and item.get("response_sha256") == reference.get("sha256")
                    and int(item.get("response_size_bytes", -1))
                    == int(reference.get("size_bytes", -2))
                ),
                None,
            )

        source = resource_capture(source_ref)
        package = resource_capture(package_ref)
        if not all(isinstance(item, dict) for item in (claim, source, package, result)):
            raise FullGateError(
                "Gateway TLS transcript does not contain the correlated claim, two resources, and result"
            )
        assert isinstance(claim, dict)
        assert isinstance(source, dict)
        assert isinstance(package, dict)
        assert isinstance(result, dict)
        if claim.get("request_headers", {}).get("prefer") != "wait=25":
            raise FullGateError("actual Worker claim did not carry Prefer: wait=25")
        if int(result.get("request_body", {}).get("lease_version", 0)) < 1:
            raise FullGateError("captured Worker result omitted lease_version")
        if any(
            "authorization" in {
                str(key).lower() for key in item.get("request_headers", {})
            }
            for item in (claim, source, package, result)
        ):
            raise FullGateError("Gateway transcript retained an Authorization credential")

        def summary(item: Mapping[str, Any]) -> dict[str, Any]:
            return {
                "method": item.get("method"),
                "path": item.get("path"),
                "status": item.get("status"),
                "request_headers": item.get("request_headers", {}),
                "request_size_bytes": item.get("request_size_bytes"),
                "request_sha256": item.get("request_sha256"),
                "response_size_bytes": item.get("response_size_bytes"),
                "response_sha256": item.get("response_sha256"),
            }

        return {
            "capture_source": "gateway-tls-ingress",
            "task_id": task_id,
            "submission_id": str(submission_id),
            "claim": summary(claim),
            "source_get": {
                **summary(source),
                "resource_ref": dict(source_ref),
            },
            "package_get": {
                **summary(package),
                "resource_ref": dict(package_ref),
            },
            "result_post": {
                **summary(result),
                "task_id": task_id,
                "status_value": result.get("request_body", {}).get("status"),
                "lease_version": result.get("request_body", {}).get("lease_version"),
            },
            "authorization_redacted": True,
            "identity_validated_by_gateway": True,
        }

    def _redis_result(
        self,
        task_id: str,
        submission_id: int,
        *,
        timeout: float = 30,
        poll_interval: float = 0.25,
    ) -> dict[str, Any]:
        # The accepted Submission and the result outbox record commit in one
        # PostgreSQL transaction, but publishing that outbox record to Redis is
        # intentionally asynchronous.  Seeing ACCEPTED immediately before this
        # method therefore does not imply that XRANGE must already contain the
        # entry; wait for the bounded relay contract instead of introducing a
        # timing-dependent false negative.
        deadline = time.monotonic() + timeout
        raw = ""
        while True:
            raw = self.a.command(
                "exec",
                "redis-a",
                "redis-cli",
                "--json",
                "XRANGE",
                "ojos:judge:result",
                "-",
                "+",
            ).stdout
            try:
                entries = json.loads(raw)
            except json.JSONDecodeError as error:
                raise FullGateError(
                    f"Redis result stream is not JSON: {raw[-2000:]}"
                ) from error
            for entry in reversed(entries):
                if not isinstance(entry, list) or len(entry) != 2:
                    continue
                fields = entry[1]
                values = (
                    dict(zip(fields[0::2], fields[1::2]))
                    if isinstance(fields, list)
                    else {}
                )
                if values.get("task_id") == task_id and str(
                    values.get("submission_id")
                ) == str(submission_id):
                    return {
                        "result_id": entry[0],
                        "task_id": task_id,
                        "submission_id": str(submission_id),
                        "status": values.get("status"),
                        "worker_id": values.get("worker_id"),
                    }
            if time.monotonic() >= deadline:
                break
            time.sleep(poll_interval)
        raise FullGateError(
            f"Redis result stream has no result for {task_id} within {timeout}s: "
            f"{raw[-2000:]}"
        )

    def _validate_production_ref(self, value: Any) -> None:
        if not isinstance(value, dict) or "url" in value:
            raise FullGateError("actual managed task serialized the retired url member")
        required = {"binding", "api_id", "relative_path", "sha256", "size_bytes"}
        if not required.issubset(value) or value.get("binding") != "storage_get":
            raise FullGateError(f"actual task resource is not an ApiResourceRef: {value}")
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", str(value.get("sha256", ""))):
            raise FullGateError("actual task resource has no immutable SHA-256")

    # ------------------------------------------------------ reconfigure/volume

    def _collect_volume_isolation(self, runtime: Mapping[str, Any]) -> None:
        judge_container = str(
            self.managed_a_runtimes.get("judge-api", {}).get("container_id", "")
        )
        if not judge_container:
            raise FullGateError("managed Judge API container is missing from A runtime evidence")
        a_info = {
            "gateway-a": json.loads(self.a.command("inspect", "gateway-a").stdout)[0],
            "judge-api-a": json.loads(self.a.command("inspect", judge_container).stdout)[0],
        }
        worker = runtime["inspect"]
        sources = {
            "gateway": [str(item.get("Source", "")) for item in a_info["gateway-a"].get("Mounts", [])],
            "judge": [str(item.get("Source", "")) for item in a_info["judge-api-a"].get("Mounts", [])],
            "worker": [str(item.get("Source", "")) for item in worker.get("Mounts", [])],
        }
        forbidden = []
        for service, values in sources.items():
            for source in values:
                lower = source.lower().replace("\\", "/")
                if "/problems" in lower or "/submissions" in lower:
                    forbidden.append({"service": service, "source": source})
        self.h.evidence["runtime_volume_isolation"] = {
            "verified": not forbidden,
            "inspection_source": "docker-inspect",
            "forbidden_shared_sources": forbidden,
            "a_engine_id": self.h.evidence["engines"]["a"]["engine_id"],
            "b_engine_id": self.h.evidence["engines"]["b"]["engine_id"],
            "gateway_mount_sources": sources["gateway"],
            "judge_mount_sources": sources["judge"],
            "worker_mount_sources": sources["worker"],
        }
        if forbidden:
            raise FullGateError(f"runtime still shares Problem/Submission paths: {forbidden}")

    def _install_storage_canary(self, topology_etag: str) -> dict[str, Any]:
        endpoint = f"{self.h.a_ip}:8086:storage-service"
        request = {
            "service_id": "storage-service",
            "catalog_source_id": "storage-service-canary",
            "version": "0.1.1",
            "channel": "stable",
            "target_node_id": "node-a",
            "endpoint": endpoint,
            "bindings": [],
            "topology_id": self.topology_id,
            "topology_etag": topology_etag,
            "start": True,
            "migration_policy": "SKIP",
            "config": {
                "STORAGE_BACKEND": "minio",
                "MINIO_ENDPOINT": f"{self.h.a_ip}:9000",
                "MINIO_USE_SSL": False,
            },
            "secret_refs": {
                "MINIO_ACCESS_KEY": "minio-access",
                "MINIO_SECRET_KEY": "minio-secret",
            },
        }
        validated, _, _ = self._control_mutation(
            "/api/v1/store/releases:validate",
            request,
            "storage-canary-validate",
            expected=(200,),
        )
        validation = validated.get("data", {})
        topology_diff = validation.get("topology_diff")
        if (
            validation.get("valid") is not True
            or not isinstance(topology_diff, dict)
            or not isinstance(topology_diff.get("changes"), list)
            or not topology_diff["changes"]
        ):
            raise FullGateError(
                "Store rejected the signed storage-service canary or omitted its Topology diff: "
                + _canonical(validated)
            )
        installed, _, _ = self._control_mutation(
            "/api/v1/store/releases:install",
            {**request, "mode": "MANAGED"},
            "storage-canary-install",
            expected=(202,),
        )
        install = installed.get("data", {})
        operation = self._wait_operation(str(install.get("operation_id", "")), timeout=420)
        if operation.get("status") != "SUCCEEDED":
            raise FullGateError(f"storage-service canary Store install failed: {operation}")
        if any(
            str(binding.get("step_id", "")).startswith("install-minio-")
            for binding in operation.get("job_bindings", [])
            if isinstance(binding, Mapping)
        ):
            raise FullGateError("storage-service canary reinstalled the healthy External MinIO")
        self.external_dependency_runtimes["minio"][
            "reused_by_managed_releases"
        ].append("storage-service@0.1.1")
        deployment_id = str(install.get("deployment_id", ""))
        if not deployment_id:
            raise FullGateError("storage-service canary Store install omitted deployment_id")
        if deployment_id == self.provider_deployments.get("storage-service"):
            raise FullGateError("storage-service canary reused the original deployment")
        observed = self._inspect_managed_a_service(
            "storage-service", deployment_id, [], 8086, operation
        )
        self.provider_deployments["storage-service-canary"] = deployment_id
        return {
            "service_id": "storage-service",
            "catalog_source_id": "storage-service-canary",
            "version": "0.1.1",
            "endpoint": endpoint,
            "validate_request_id": validated.get("meta", {}).get("request_id"),
            "validation_valid": True,
            "validation_topology_changes": len(topology_diff["changes"]),
            "install_request_id": installed.get("meta", {}).get("request_id"),
            "deployment_id": deployment_id,
            "operation_id": operation.get("operation_id"),
            "operation_status": operation.get("status"),
            "runtime": observed,
        }

    def _reconfigure_bindings_and_prove_in_place(self, runtime: Mapping[str, Any]) -> None:
        before = self._container_json(
            self.b,
            self.worker_container_id,
            "/run/ojos/service/context.json",
            "observed-context-before-storage-rebind",
        )
        current = self._control_get(f"/api/v1/topologies/{self.topology_id}")
        draft = current.get("data", {}).get("draft", {})
        revision_id = str(draft.get("revision_id", ""))
        if not revision_id:
            raise FullGateError("cannot read applied Topology before storage canary install")
        canary = self._install_storage_canary(f'"{revision_id}"')

        current = self._control_get(f"/api/v1/topologies/{self.topology_id}")
        draft = current.get("data", {}).get("draft", {})
        canary_parent_revision = str(draft.get("revision_id", ""))
        spec = draft.get("spec", {})
        if not canary_parent_revision or not isinstance(spec, dict):
            raise FullGateError("cannot read applied Store-generated Topology for reconfiguration")
        rebound_spec, rebind = _rebind_topology_requirement(
            spec,
            consumer_deployment_id=self.worker_deployment_id,
            requirement_name="storage_get",
            old_provider_deployment_id=self.provider_deployments["storage-service"],
            new_provider_deployment_id=str(canary["deployment_id"]),
        )
        if rebind.get("api_id") != "storage.object.get":
            raise FullGateError(f"Worker storage_get selected the wrong API: {rebind}")
        operation, next_id = self._apply_topology_spec(
            rebound_spec,
            canary_parent_revision,
            "worker-storage-canary-rebind",
        )
        after = self._wait_container_context_generation(
            self.b,
            self.worker_container_id,
            int(before.get("generation", 0)),
            "observed-context-after-storage-rebind",
        )
        after_container = self.b.command(
            "ps", "--filter", "label=ojos.deployment_id=" + self.worker_deployment_id,
            "--format", "{{.ID}}", "--no-trunc",
        ).stdout.strip()
        if after_container != self.worker_container_id:
            raise FullGateError("Binding reconfiguration restarted/replaced the Worker container")
        before_generation = int(before.get("generation", 0))
        after_generation = int(after.get("generation", 0))
        bindings = self._control_get(
            f"/api/v1/deployments/{self.worker_deployment_id}/bindings"
        ).get("data", {}).get("items", [])
        generations = {
            (int(item.get("credential_generation", 0)), int(item.get("context_generation", 0)))
            for item in bindings if str(item.get("desired_state", "")).upper() == "ACTIVE"
        }
        if generations != {(after_generation, after_generation)}:
            raise FullGateError(f"durable Binding generations do not match context: {generations}")
        storage_binding = next(
            (
                item for item in bindings
                if item.get("requirement_name") == "storage_get"
            ),
            None,
        )
        if (
            not isinstance(storage_binding, dict)
            or storage_binding.get("provider_deployment_id") != canary["deployment_id"]
            or storage_binding.get("provider_endpoint") != canary["endpoint"]
            or str(storage_binding.get("state", "")).upper() != "ACTIVE"
        ):
            raise FullGateError(
                "durable Worker storage_get Binding did not move to the canary provider: "
                + _canonical(bindings)
            )
        second = self._run_actual_flow("after-reconfigure")
        projection_integrity = self._capture_provider_projection_integrity(
            "binding-reconfigure"
        )
        self.h.evidence["binding_reconfiguration"] = {
            "provider_preserving": True,
            "semantic_provider_rebind": True,
            "requirement_name": rebind["requirement_name"],
            "api_id": rebind["api_id"],
            "consumer_deployment_id": rebind["consumer_deployment_id"],
            "consumer_endpoint": rebind["consumer_endpoint"],
            "old_provider_deployment_id": rebind["old_provider_deployment_id"],
            "old_provider_endpoint": rebind["old_provider_endpoint"],
            "new_provider_deployment_id": rebind["new_provider_deployment_id"],
            "new_provider_endpoint": rebind["new_provider_endpoint"],
            "canary_store": canary,
            "operation_id": operation.get("operation_id"),
            "operation_status": operation.get("status"),
            "container_id_before": self.worker_container_id,
            "container_id_after": after_container,
            "generation_before": before_generation,
            "generation_after": after_generation,
            "credential_generation_after": after_generation,
            "context_generation_after": after_generation,
            "post_update_request_succeeded": second.get("result", {}).get("status") == "ACCEPTED",
            "post_update_submission_status": second.get("submission", {}).get("status"),
            "topology_revision_id": next_id,
            "provider_projection_integrity": projection_integrity,
        }

    def _run_generic_store_topology_agent_fixture(self) -> None:
        """Prove a third-party Service Contract without product-specific hooks.

        The provider is registered through the normal External Store mode on
        Engine A and the consumer is installed by the enrolled Agent on Engine
        B.  The only relationship supplied by the harness is the manifest
        requirement selection; Store creates the Topology Link, Gateway route,
        workload grant, and Agent Service Context.
        """

        current = self._control_get(f"/api/v1/topologies/{self.topology_id}")
        draft = current.get("data", {}).get("draft", {})
        revision_id = str(draft.get("revision_id", ""))
        if not revision_id:
            raise FullGateError("generic fixture cannot read the applied Topology revision")
        provider_id = self.provider_deployments.get("contract-echo-provider", "")
        permission_provider_id = self.provider_deployments.get("auth-service", "")
        if not provider_id or not permission_provider_id:
            raise FullGateError("generic fixture providers were not registered through Store")
        selections = [
            {"name": "echo", "provider_deployment_id": provider_id},
            {
                "name": "permission_check",
                "provider_deployment_id": permission_provider_id,
            },
        ]
        base = {
            "service_id": "contract-echo-consumer",
            "catalog_source_id": "contract-echo-consumer",
            "version": "1.0.0",
            "channel": "stable",
            "target_node_id": "node-b",
            "bindings": selections,
            "topology_id": self.topology_id,
            "topology_etag": f'"{revision_id}"',
        }
        validated, _, _ = self._control_mutation(
            "/api/v1/store/releases:validate",
            base,
            "generic-consumer-validate",
            expected=(200,),
        )
        validation = validated.get("data", {})
        candidates = validation.get("requirements", [])
        recommendations = {
            str(item.get("requirement_name", "")): str(
                item.get("recommended_provider_deployment_id", "")
            )
            for item in candidates
        }
        if validation.get("valid") is not True or recommendations != {
            "echo": provider_id,
            "permission_check": permission_provider_id,
        }:
            raise FullGateError(
                "generic consumer manifest did not resolve its External provider: "
                + _canonical(validation)
            )
        installed, _, _ = self._control_mutation(
            "/api/v1/store/releases:install",
            {**base, "mode": "MANAGED", "start": True},
            "generic-consumer-install",
            expected=(202,),
        )
        install_data = installed.get("data", {})
        operation = self._wait_operation(str(install_data.get("operation_id", "")), timeout=300)
        if operation.get("status") != "SUCCEEDED":
            raise FullGateError(f"generic consumer Store install failed: {operation}")
        deployment_id = str(install_data.get("deployment_id", ""))
        if not deployment_id:
            raise FullGateError("generic consumer install omitted deployment_id")
        operation_binding = next(
            (
                item
                for item in operation.get("job_bindings", [])
                if str(item.get("step_id", "")).startswith(
                    "install-contract-echo-consumer-"
                )
            ),
            None,
        )
        if not isinstance(operation_binding, Mapping):
            raise FullGateError(
                "generic consumer Operation has no Agent install Job binding"
            )
        job = self._query_json(
            "ojos_orchestrator",
            "SELECT payload::text FROM orchestrator_jobs WHERE job_id="
            + self._sql(str(operation_binding.get("job_id", ""))),
        )
        runtime_convergence = self._managed_runtime_convergence(
            deployment_id, "node-b", job
        )
        container_id = self.b.command(
            "ps",
            "--filter",
            "label=ojos.deployment_id=" + deployment_id,
            "--format",
            "{{.ID}}",
            "--no-trunc",
        ).stdout.strip()
        if not container_id:
            raise FullGateError("generic consumer was not created by Agent on Engine B")
        inspected = json.loads(self.b.command("inspect", container_id).stdout)[0]
        config_user = str(inspected.get("Config", {}).get("User", ""))
        if config_user != AGENT_CONFIG_USER:
            raise FullGateError(
                "generic standard workload does not run as the signed 65532:65532 identity"
            )
        component = hashlib.sha256(deployment_id.encode("utf-8")).hexdigest()[:32]
        context_source = (
            f"{B_WORKLOAD_EXPORT_ROOT}/runtime-contexts/{component}/service"
        )
        context_mount = next(
            (
                item
                for item in inspected.get("Mounts", []) or []
                if item.get("Destination") == "/run/ojos/service"
            ),
            None,
        )
        if (
            not isinstance(context_mount, Mapping)
            or context_mount.get("Source") != context_source
            or context_mount.get("RW") is not False
        ):
            raise FullGateError(
                "generic standard workload does not use the exact read-only Agent context path"
            )
        context_file_identity = self._service_context_file_identity(
            self.b, context_source, container_id
        )

        result: dict[str, Any] = {}
        deadline = time.monotonic() + 120
        program = (
            "import json,urllib.request;"
            "print(json.dumps(json.load(urllib.request.urlopen("
            "'http://127.0.0.1:8080/result',timeout=2)),sort_keys=True))"
        )
        last = ""
        while time.monotonic() < deadline:
            response = self.b.command(
                "exec", container_id, "python", "-c", program, timeout=5, check=False
            )
            if response.returncode == 0:
                try:
                    result = json.loads(response.stdout.strip())
                except json.JSONDecodeError:
                    result = {}
                if result.get("response", {}).get("value") == "cross-engine-binding-ok":
                    break
            last = response.stderr or response.stdout
            time.sleep(1)
        else:
            logs = self.b.command("logs", "--tail", "200", container_id, check=False)
            raise FullGateError(
                "generic managed consumer did not cross its binding: "
                + (last + logs.stdout + logs.stderr)[-4000:]
            )

        bindings = self._control_get(
            f"/api/v1/deployments/{deployment_id}/bindings"
        ).get("data", {}).get("items", [])
        binding = next(
            (
                item
                for item in bindings
                if item.get("requirement_name") == "echo"
                and item.get("provider_deployment_id") == provider_id
            ),
            None,
        )
        permission_binding = next(
            (
                item
                for item in bindings
                if item.get("requirement_name") == "permission_check"
                and item.get("provider_deployment_id") == permission_provider_id
            ),
            None,
        )
        if (
            not isinstance(binding, dict)
            or binding.get("state") != "ACTIVE"
            or binding.get("optional") is not True
            or not isinstance(permission_binding, dict)
            or permission_binding.get("state") != "ACTIVE"
            or permission_binding.get("optional") is not False
        ):
            raise FullGateError(f"generic consumer binding was not activated: {bindings}")
        lifecycle = self._prove_generic_binding_revocation(
            deployment_id,
            container_id,
            int(result.get("success_count", 0)),
        )
        self.h.evidence["third_party_fixture"] = {
            "specialized_product_code": False,
            "provider": {
                "service_id": "contract-echo-provider",
                "deployment_id": provider_id,
                "engine": "A",
                "installed_via_store": True,
                "management_mode": "EXTERNAL",
            },
            "consumer": {
                "service_id": "contract-echo-consumer",
                "deployment_id": deployment_id,
                "engine": "B",
                "installed_via_store_agent": True,
                "container_id": container_id,
                "config_user": config_user,
                "service_context_file_identity": context_file_identity,
            },
            "manifest_only": True,
            "binding_plan": binding,
            "permission_binding_plan": permission_binding,
            "permission_provider_deployment_id": permission_provider_id,
            "workload_permission_check": result.get("response", {}).get(
                "permission_check"
            ),
            "operation_id": operation.get("operation_id"),
            "operation_status": operation.get("status"),
            "runtime_projection": runtime_convergence,
            "consumer_evidence": result,
            "binding_lifecycle": lifecycle,
        }

    def _prove_generic_binding_revocation(
        self,
        deployment_id: str,
        container_id: str,
        initial_success_count: int,
    ) -> dict[str, Any]:
        """Disconnect and restore a real Link without restarting its consumer.

        The still-active permission binding makes the negative check stronger:
        an old JWT reaches an existing virtual route and must be rejected solely
        because its deployment-wide credential generation is stale.
        """

        assert self.gateway_client is not None
        before_context = self._container_json(
            self.b,
            container_id,
            "/run/ojos/service/context.json",
            "generic-context-before-revoke",
        )
        before_generation = int(before_context.get("generation", 0))
        if before_generation < 1:
            raise FullGateError("generic consumer context has no positive generation")
        initial_binding_items = self._control_get(
            f"/api/v1/deployments/{deployment_id}/bindings"
        ).get("data", {}).get("items", [])
        initial_bindings = {
            str(item.get("requirement_name", "")): item
            for item in initial_binding_items
            if isinstance(item, Mapping)
        }
        if (
            set(initial_bindings) != {"echo", "permission_check"}
            or any(
                not str(binding.get("binding_id", "")).strip()
                or not str(binding.get("provider_deployment_id", "")).strip()
                or str(binding.get("desired_state", "")).upper() != "ACTIVE"
                or str(binding.get("state", "")).upper() != "ACTIVE"
                for binding in initial_bindings.values()
            )
        ):
            raise FullGateError(
                "generic rollback proof requires both initial ACTIVE bindings: "
                + _canonical(initial_binding_items)
            )
        old_token = self._container_text(
            self.b,
            container_id,
            "/run/ojos/service/token",
            "generic-token-before-revoke",
        )

        current = self._control_get(f"/api/v1/topologies/{self.topology_id}")
        draft = current.get("data", {}).get("draft", {})
        current_revision = str(draft.get("revision_id", ""))
        original_spec = copy.deepcopy(draft.get("spec", {}))
        if not current_revision or not isinstance(original_spec, dict):
            raise FullGateError("cannot read generic consumer Topology before Link revoke")
        consumer_endpoint = next(
            (
                str(endpoint.get("endpoint", ""))
                for endpoint in original_spec.get("endpoints", [])
                if endpoint.get("config", {}).get("deployment_id") == deployment_id
            ),
            "",
        )
        if not consumer_endpoint:
            raise FullGateError("generic consumer Topology endpoint is missing")
        revoked_spec = copy.deepcopy(original_spec)
        removed = 0
        retained_links: list[dict[str, Any]] = []
        for link in revoked_spec.get("links", []):
            selections = list(link.get("api_bindings", []))
            if link.get("source_endpoint") == consumer_endpoint:
                kept = [
                    selection
                    for selection in selections
                    if selection.get("requirement") != "echo"
                ]
                removed += len(selections) - len(kept)
                link["api_bindings"] = kept
            if link.get("api_bindings") or link.get("scope") != "api-binding":
                retained_links.append(link)
        revoked_spec["links"] = retained_links
        if removed != 1:
            raise FullGateError(
                f"expected to revoke one generic echo binding, removed {removed}"
            )
        revoke_operation, revoke_revision = self._apply_topology_spec(
            revoked_spec,
            current_revision,
            "generic-echo-revoke",
        )
        revoked_context = self._wait_container_context_generation(
            self.b,
            container_id,
            before_generation,
            "generic-context-revoked",
        )
        revoked_generation = int(revoked_context.get("generation", 0))
        revoked_token = self._container_text(
            self.b,
            container_id,
            "/run/ojos/service/token",
            "generic-token-revoked",
        )
        permission_request = {
            "user_id": 1,
            "permission": "problem.view",
            "scope_type": "global",
            "scope_id": 0,
        }
        _, _, stale_status = self.gateway_client.request(
            "POST",
            "/internal/apis/auth.user.permission.check",
            permission_request,
            headers={"authorization": "Bearer " + old_token},
            expected=(401, 403),
        )
        _, _, removed_route_status = self.gateway_client.request(
            "GET",
            "/internal/apis/fixture.contract.echo/echo",
            headers={"authorization": "Bearer " + revoked_token},
            expected=(403, 404),
            status_only=True,
        )
        permission_response, _, current_status = self.gateway_client.request(
            "POST",
            "/internal/apis/auth.user.permission.check",
            permission_request,
            headers={"authorization": "Bearer " + revoked_token},
            expected=(200,),
        )
        if permission_response.get("data", permission_response).get("allowed") is not True:
            raise FullGateError("current JWT could not use the retained permission binding")
        revoked_bindings = self._control_get(
            f"/api/v1/deployments/{deployment_id}/bindings"
        ).get("data", {}).get("items", [])
        echo_binding = next(
            (item for item in revoked_bindings if item.get("requirement_name") == "echo"),
            None,
        )
        permission_binding = next(
            (
                item
                for item in revoked_bindings
                if item.get("requirement_name") == "permission_check"
            ),
            None,
        )
        if (
            not isinstance(echo_binding, dict)
            or str(echo_binding.get("desired_state", "")).upper() != "REVOKED"
            or echo_binding.get("optional") is not True
            or not isinstance(permission_binding, dict)
            or str(permission_binding.get("state", "")).upper() != "ACTIVE"
            or str(permission_binding.get("desired_state", "")).upper()
            != "ACTIVE"
            or permission_binding.get("optional") is not False
            or int(permission_binding.get("credential_generation", 0))
            != revoked_generation
        ):
            raise FullGateError(
                f"durable bindings did not record Link revocation atomically: {revoked_bindings}"
            )
        revoked_context_bindings = revoked_context.get("bindings", {})
        revoked_permission_context = (
            revoked_context_bindings.get("permission_check", {})
            if isinstance(revoked_context_bindings, Mapping)
            else {}
        )
        if (
            not isinstance(revoked_context_bindings, Mapping)
            or sorted(revoked_context_bindings) != ["permission_check"]
            or not isinstance(revoked_permission_context, Mapping)
            or revoked_permission_context.get("binding_id")
            != permission_binding.get("binding_id")
        ):
            raise FullGateError(
                "generic Echo revocation did not retain the required permission "
                "binding in the Agent ServiceContext"
            )
        observed_error = self._wait_generic_consumer_error(container_id)

        restore_operation, restore_revision, rollback_proof = (
            self._rollback_topology_revision(
                target_revision_id=current_revision,
                parent_revision_id=revoke_revision,
                key="generic-echo-rollback",
            )
        )
        restored_context = self._wait_container_context_generation(
            self.b,
            container_id,
            revoked_generation,
            "generic-context-restored",
        )
        restored_generation = int(restored_context.get("generation", 0))
        restored_binding_items = self._control_get(
            f"/api/v1/deployments/{deployment_id}/bindings"
        ).get("data", {}).get("items", [])
        restored_bindings = {
            str(item.get("requirement_name", "")): item
            for item in restored_binding_items
            if isinstance(item, Mapping)
        }
        if (
            set(restored_bindings) != set(initial_bindings)
            or any(
                restored_bindings[name].get("binding_id")
                != initial_bindings[name].get("binding_id")
                or restored_bindings[name].get("provider_deployment_id")
                != initial_bindings[name].get("provider_deployment_id")
                or str(restored_bindings[name].get("desired_state", "")).upper()
                != "ACTIVE"
                or str(restored_bindings[name].get("state", "")).upper()
                != "ACTIVE"
                or restored_bindings[name].get("topology_revision_id")
                != restore_revision
                or int(restored_bindings[name].get("credential_generation", 0))
                != restored_generation
                for name in initial_bindings
            )
        ):
            raise FullGateError(
                "Topology rollback did not restore the target revision bindings: "
                + _canonical(restored_binding_items)
            )
        rollback_proof["restored_bindings"] = [
            {
                "requirement_name": name,
                "binding_id": restored_bindings[name].get("binding_id"),
                "provider_deployment_id": restored_bindings[name].get(
                    "provider_deployment_id"
                ),
                "desired_state": restored_bindings[name].get("desired_state"),
                "observed_state": restored_bindings[name].get("state"),
                "topology_revision_id": restored_bindings[name].get(
                    "topology_revision_id"
                ),
                "credential_generation": restored_bindings[name].get(
                    "credential_generation"
                ),
            }
            for name in sorted(restored_bindings)
        ]
        self.h.evidence["topology_rollback"] = rollback_proof
        restored_token = self._container_text(
            self.b,
            container_id,
            "/run/ojos/service/token",
            "generic-token-restored",
        )
        _, _, revoked_status = self.gateway_client.request(
            "POST",
            "/internal/apis/auth.user.permission.check",
            permission_request,
            headers={"authorization": "Bearer " + revoked_token},
            expected=(401, 403),
        )
        restored_response, _, restored_status = self.gateway_client.request(
            "GET",
            "/internal/apis/fixture.contract.echo/echo",
            headers={"authorization": "Bearer " + restored_token},
            expected=(200,),
        )
        if restored_response.get("value") != "cross-engine-binding-ok":
            raise FullGateError("restored generic Link did not reach its provider")
        recovered = self._wait_generic_consumer_recovery(
            container_id,
            restored_generation,
            initial_success_count,
        )
        after_container = self.b.command(
            "ps",
            "--filter",
            "label=ojos.deployment_id=" + deployment_id,
            "--format",
            "{{.ID}}",
            "--no-trunc",
        ).stdout.strip()
        if after_container != container_id:
            raise FullGateError("Link revoke/restore replaced the generic consumer container")
        evidence = {
            "consumer_deployment_id": deployment_id,
            "container_id_before": container_id,
            "container_id_after": after_container,
            "generation_before": before_generation,
            "generation_revoked": revoked_generation,
            "generation_restored": restored_generation,
            "rollback_target_revision_id": current_revision,
            "revoke_revision_id": revoke_revision,
            "revoke_operation_id": revoke_operation.get("operation_id"),
            "revoke_operation_status": revoke_operation.get("status"),
            "restore_revision_id": restore_revision,
            "restore_operation_id": restore_operation.get("operation_id"),
            "restore_operation_status": restore_operation.get("status"),
            "old_token_existing_route_status": stale_status,
            "current_token_removed_route_status": removed_route_status,
            "current_token_retained_route_status": current_status,
            "revoked_token_after_restore_status": revoked_status,
            "restored_token_route_status": restored_status,
            "revoked_binding_desired_state": echo_binding.get("desired_state"),
            "echo_requirement_optional": echo_binding.get("optional"),
            "permission_requirement_optional": permission_binding.get("optional"),
            "retained_permission_binding_desired_state": permission_binding.get(
                "desired_state"
            ),
            "retained_permission_binding_observed_state": permission_binding.get(
                "state"
            ),
            "revoked_context_binding_names": sorted(revoked_context_bindings),
            "revoked_context_permission_binding_id": revoked_permission_context.get(
                "binding_id"
            ),
            "durable_permission_binding_id": permission_binding.get("binding_id"),
            "consumer_observed_unbound_error": observed_error,
            "consumer_recovered": recovered.get("last_error") == "",
            "recovered_success_count": recovered.get("success_count"),
            "tokens_recorded": False,
        }
        self.h.evidence["workload_credential_lifecycle"] = evidence
        return evidence

    def _apply_topology_spec(
        self,
        spec: Mapping[str, Any],
        parent_revision_id: str,
        key: str,
    ) -> tuple[dict[str, Any], str]:
        created, headers, _ = self._control_mutation(
            f"/api/v1/topologies/{self.topology_id}/revisions",
            spec,
            key + "-revision",
            expected=(201,),
            headers={"if-match": f'"{parent_revision_id}"'},
        )
        revision_id = str(
            created.get("data", {}).get("revision", {}).get("revision_id", "")
        )
        if not revision_id:
            raise FullGateError(f"Topology {key} did not return revision_id")
        etag = str(headers.get("etag", ""))
        expected_etag = f'"{revision_id}"'
        if etag != expected_etag:
            raise FullGateError(
                f"Topology {key} revision did not return its strong ETag: "
                f"expected={expected_etag} actual={etag!r}"
            )
        applied, _, _ = self._control_mutation(
            f"/api/v1/topologies/{self.topology_id}:apply",
            {},
            key + "-apply",
            expected=(202,),
            headers={"if-match": etag},
        )
        operation = self._wait_operation(
            str(applied.get("data", {}).get("operation_id", "")),
            300,
        )
        if operation.get("status") != "SUCCEEDED":
            raise FullGateError(f"Topology {key} failed: {operation}")
        return operation, revision_id

    def _rollback_topology_revision(
        self,
        *,
        target_revision_id: str,
        parent_revision_id: str,
        key: str,
    ) -> tuple[dict[str, Any], str, dict[str, Any]]:
        """Call the public rollback API and prove its immutable applied revision."""

        assert self.control_client is not None
        if not target_revision_id or not parent_revision_id:
            raise FullGateError("Topology rollback requires target and parent revisions")
        if target_revision_id == parent_revision_id:
            raise FullGateError("Topology rollback target must precede the current draft")

        def read_revision(revision_id: str, phase: str) -> dict[str, Any]:
            document, headers, _ = self.control_client.request(
                "GET",
                f"/api/v1/topologies/{self.topology_id}/revisions/{revision_id}",
                expected=(200,),
                timeout=30,
            )
            revision = document.get("data", {}).get("revision", {})
            expected_etag = f'"{revision_id}"'
            actual_etag = str(headers.get("etag", ""))
            if (
                not isinstance(revision, Mapping)
                or revision.get("revision_id") != revision_id
                or actual_etag != expected_etag
            ):
                raise FullGateError(
                    f"Topology rollback {phase} revision/ETag mismatch: "
                    f"revision={revision} expected_etag={expected_etag} "
                    f"actual_etag={actual_etag!r}"
                )
            return dict(revision)

        target = read_revision(target_revision_id, "target")
        parent = read_revision(parent_revision_id, "parent")
        target_content = str(target.get("content_sha256", ""))
        parent_content = str(parent.get("content_sha256", ""))
        if (
            not re.fullmatch(r"[0-9a-f]{64}", target_content)
            or not re.fullmatch(r"[0-9a-f]{64}", parent_content)
            or target_content == parent_content
            or not isinstance(target.get("spec"), Mapping)
        ):
            raise FullGateError(
                "Topology rollback requires distinct immutable target/current specs"
            )

        response, _, _ = self._control_mutation(
            f"/api/v1/topologies/{self.topology_id}:rollback",
            {"revision_id": target_revision_id},
            key,
            expected=(202,),
            headers={"if-match": f'"{parent_revision_id}"'},
        )
        data = response.get("data", {})
        operation_id = str(data.get("operation_id", ""))
        rollback_revision_id = str(data.get("revision_id", ""))
        if (
            not operation_id
            or not rollback_revision_id
            or data.get("topology_id") != self.topology_id
            or rollback_revision_id in {target_revision_id, parent_revision_id}
        ):
            raise FullGateError(
                "Topology rollback API did not return a distinct revision and Operation: "
                + _canonical(data)
            )
        operation = self._wait_operation(operation_id, 300)
        if (
            str(operation.get("status", "")).upper() != "SUCCEEDED"
            or operation.get("action") != "topology.rollback"
        ):
            raise FullGateError(f"Topology rollback Operation failed: {operation}")

        rollback = read_revision(rollback_revision_id, "created")
        rollback_content = str(rollback.get("content_sha256", ""))
        target_number = int(target.get("revision_number", 0))
        parent_number = int(parent.get("revision_number", 0))
        rollback_number = int(rollback.get("revision_number", 0))
        if (
            rollback.get("parent_revision_id") != parent_revision_id
            or rollback.get("rollback_of_revision_id") != target_revision_id
            or rollback_content != target_content
            or _canonical(rollback.get("spec")) != _canonical(target.get("spec"))
            or target_number < 1
            or parent_number <= target_number
            or rollback_number != parent_number + 1
        ):
            raise FullGateError(
                "Topology rollback API did not create the expected immutable lineage: "
                + _canonical(rollback)
            )

        deadline = time.monotonic() + 60
        latest: dict[str, Any] = {}
        latest_headers: dict[str, str] = {}
        while time.monotonic() < deadline:
            document, latest_headers, _ = self.control_client.request(
                "GET",
                f"/api/v1/topologies/{self.topology_id}",
                expected=(200,),
                timeout=30,
            )
            latest = document.get("data", {})
            heads = latest.get("heads", {})
            status = latest.get("status", {})
            if (
                isinstance(heads, Mapping)
                and isinstance(status, Mapping)
                and heads.get("draft_revision_id") == rollback_revision_id
                and heads.get("applied_revision_id") == rollback_revision_id
                and heads.get("applying_revision_id") is None
                and status.get("desired_revision_id") == rollback_revision_id
                and status.get("observed_revision_id") == rollback_revision_id
                and str(status.get("state", "")).upper() == "IN_SYNC"
                and status.get("drift") == []
                and status.get("last_operation_id") == operation_id
                and str(latest_headers.get("etag", ""))
                == f'"{rollback_revision_id}"'
            ):
                break
            time.sleep(1)
        else:
            raise FullGateError(
                "Topology rollback did not converge applied head/status: "
                + _canonical(latest)
            )

        target_spec_sha256 = _sha256(_canonical(target["spec"]))
        rollback_spec_sha256 = _sha256(_canonical(rollback["spec"]))
        proof = {
            "api_path": f"/api/v1/topologies/{self.topology_id}:rollback",
            "topology_id": self.topology_id,
            "request_revision_id": target_revision_id,
            "request_if_match": f'"{parent_revision_id}"',
            "target_revision_id": target_revision_id,
            "target_revision_number": target_number,
            "target_content_sha256": target_content,
            "target_spec_sha256": target_spec_sha256,
            "parent_revision_id": parent_revision_id,
            "parent_revision_number": parent_number,
            "parent_content_sha256": parent_content,
            "created_revision_id": rollback_revision_id,
            "created_revision_number": rollback_number,
            "created_parent_revision_id": rollback.get("parent_revision_id"),
            "created_rollback_of_revision_id": rollback.get(
                "rollback_of_revision_id"
            ),
            "created_content_sha256": rollback_content,
            "created_spec_sha256": rollback_spec_sha256,
            "created_revision_etag": str(latest_headers.get("etag", "")),
            "operation_id": operation_id,
            "operation_action": operation.get("action"),
            "operation_status": operation.get("status"),
            "draft_revision_id": latest.get("heads", {}).get(
                "draft_revision_id"
            ),
            "applied_revision_id": latest.get("heads", {}).get(
                "applied_revision_id"
            ),
            "applying_revision_id": latest.get("heads", {}).get(
                "applying_revision_id"
            ),
            "status_desired_revision_id": latest.get("status", {}).get(
                "desired_revision_id"
            ),
            "status_observed_revision_id": latest.get("status", {}).get(
                "observed_revision_id"
            ),
            "status_state": latest.get("status", {}).get("state"),
            "status_drift": latest.get("status", {}).get("drift"),
            "status_last_operation_id": latest.get("status", {}).get(
                "last_operation_id"
            ),
        }
        return operation, rollback_revision_id, proof

    def _container_text(
        self,
        engine: Any,
        container_id: str,
        source: str,
        evidence_name: str,
    ) -> str:
        destination = self.tmp / (evidence_name + ".txt")
        engine.command("cp", container_id + ":" + source, destination)
        value = destination.read_text(encoding="utf-8").strip()
        if not value:
            raise FullGateError(f"container materialization {source} is empty")
        return value

    def _container_json(
        self,
        engine: Any,
        container_id: str,
        source: str,
        evidence_name: str,
    ) -> dict[str, Any]:
        value = json.loads(
            self._container_text(engine, container_id, source, evidence_name)
        )
        if not isinstance(value, dict):
            raise FullGateError(f"container materialization {source} is not an object")
        return value

    def _wait_container_context_generation(
        self,
        engine: Any,
        container_id: str,
        previous_generation: int,
        evidence_name: str,
    ) -> dict[str, Any]:
        deadline = time.monotonic() + 120
        latest: dict[str, Any] = {}
        while time.monotonic() < deadline:
            latest = self._container_json(
                engine,
                container_id,
                "/run/ojos/service/context.json",
                evidence_name,
            )
            if int(latest.get("generation", 0)) > previous_generation:
                return latest
            time.sleep(0.5)
        raise FullGateError(
            f"ServiceContext generation did not advance beyond {previous_generation}: {latest}"
        )

    def _generic_consumer_result(self, container_id: str) -> dict[str, Any]:
        program = (
            "import json,urllib.request;"
            "print(json.dumps(json.load(urllib.request.urlopen("
            "'http://127.0.0.1:8080/result',timeout=2)),sort_keys=True))"
        )
        response = self.b.command(
            "exec", container_id, "python", "-c", program, timeout=5, check=False
        )
        if response.returncode != 0:
            return {}
        try:
            value = json.loads(response.stdout.strip())
        except json.JSONDecodeError:
            return {}
        return value if isinstance(value, dict) else {}

    def _wait_generic_consumer_error(self, container_id: str) -> str:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            result = self._generic_consumer_result(container_id)
            error = str(result.get("last_error", ""))
            if error:
                return error[:512]
            time.sleep(0.25)
        raise FullGateError("generic consumer did not observe its revoked echo Link")

    def _wait_generic_consumer_recovery(
        self,
        container_id: str,
        generation: int,
        previous_success_count: int,
    ) -> dict[str, Any]:
        deadline = time.monotonic() + 60
        latest: dict[str, Any] = {}
        while time.monotonic() < deadline:
            latest = self._generic_consumer_result(container_id)
            if (
                latest.get("last_error") == ""
                and int(latest.get("context_generation", 0)) == generation
                and int(latest.get("success_count", 0)) > previous_success_count
                and latest.get("response", {}).get("value")
                == "cross-engine-binding-ok"
            ):
                return latest
            time.sleep(0.25)
        raise FullGateError(f"generic consumer did not recover after Link restore: {latest}")

    # --------------------------------------------------------------- API/SQL

    def _control_mutation(
        self,
        path: str,
        body: Mapping[str, Any],
        key: str,
        *,
        expected: Iterable[int],
        headers: Mapping[str, str] | None = None,
    ) -> tuple[dict[str, Any], dict[str, str], int]:
        assert self.control_client is not None
        combined = {"idempotency-key": self.h.run_id + "-" + key, "x-actor-id": "cross-machine-e2e"}
        combined.update(headers or {})
        return self.control_client.request(
            "POST", path, body, headers=combined, expected=expected, timeout=120
        )

    def _control_get(self, path: str) -> dict[str, Any]:
        assert self.control_client is not None
        return self.control_client.request("GET", path, expected=(200,), timeout=30)[0]

    def _operation_job_rows(
        self, operation_id: str, captured_at_ms: int
    ) -> dict[str, Any]:
        """Read the authoritative Job rows without retaining lease credentials."""

        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:/-]{0,255}", operation_id):
            raise FullGateError(
                "Operation ID is not safe for parameterized read-only diagnostics"
            )
        query = _single_line_sql(
            """
            BEGIN READ ONLY;
            SELECT json_build_object(
                'operation_id', current_setting('ojos.evidence_operation_id'),
                'transaction_read_only', current_setting('transaction_read_only')::boolean,
                'row_count', (
                    SELECT count(*)
                    FROM orchestrator_jobs
                    WHERE operation_id = current_setting('ojos.evidence_operation_id')
                ),
                'items', COALESCE((
                    SELECT json_agg(json_build_object(
                        'job_id', job_id,
                        'operation_id', operation_id,
                        'node_id', node_id,
                        'kind', payload->>'kind',
                        'status', status,
                        'payload_status', payload->>'status',
                        'payload_status_matches_column', payload->>'status' = status,
                        'attempt', (payload->>'attempt')::bigint,
                        'max_attempts', (payload->>'max_attempts')::bigint,
                        'available_at_ms', available_at_ms,
                        'lease_expires_at_ms', NULLIF(payload->>'lease_expires_at_ms', '')::bigint,
                        'created_at_ms', created_at_ms,
                        'started_at_ms', NULLIF(payload->>'started_at_ms', '')::bigint,
                        'completed_at_ms', NULLIF(payload->>'completed_at_ms', '')::bigint,
                        'updated_at_ms', NULLIF(payload->>'updated_at_ms', '')::bigint,
                        'lease_owner', payload->>'lease_owner',
                        'lease_credential_present', COALESCE(payload->>'lease_token', '') <> '',
                        'completion_fingerprint_present',
                            COALESCE(payload->>'completion_fingerprint', '') <> '',
                        'result_present',
                            payload->'result' IS NOT NULL AND payload->'result' <> 'null'::jsonb,
                        'result_phase', payload#>>'{result,phase}',
                        'result_code', payload#>>'{result,code}',
                        'result_state', payload#>>'{result,state}',
                        'error_present', COALESCE(payload->>'error_message', '') <> ''
                    ) ORDER BY created_at_ms, job_id)
                    FROM orchestrator_jobs
                    WHERE operation_id = current_setting('ojos.evidence_operation_id')
                ), '[]'::json)
            )::text;
            ROLLBACK;
            """
        )
        result = self.a.command(
            "exec",
            "--env",
            "PGOPTIONS=-cojos.evidence_operation_id=" + operation_id,
            "postgres-a",
            "psql",
            "-X",
            "-A",
            "-t",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "postgres",
            "-d",
            "ojos_orchestrator",
            "-c",
            query,
            timeout=30,
        )
        snapshot = _json_from_last_line(result.stdout)
        items = snapshot.get("items")
        row_count = snapshot.get("row_count")
        if (
            snapshot.get("operation_id") != operation_id
            or snapshot.get("transaction_read_only") is not True
            or isinstance(row_count, bool)
            or not isinstance(row_count, int)
            or not isinstance(items, list)
            or row_count != len(items)
            or any(
                not isinstance(item, Mapping)
                or item.get("operation_id") != operation_id
                or not str(item.get("job_id", "")).strip()
                for item in items
            )
        ):
            raise FullGateError(
                "Operation timeout Job readback was malformed: " + _canonical(snapshot)
            )
        normalized_items: list[dict[str, Any]] = []
        for item in items:
            normalized = dict(item)
            expiry = normalized.get("lease_expires_at_ms")
            normalized["lease_expired_at_capture"] = (
                isinstance(expiry, int)
                and not isinstance(expiry, bool)
                and expiry <= captured_at_ms
            )
            normalized_items.append(normalized)
        return {
            "source": "postgres-orchestrator_jobs",
            "query_mode": "postgres-fixed-parameterized-read-only-transaction",
            "operation_id": operation_id,
            "captured_at_ms": captured_at_ms,
            "row_count": row_count,
            "items": normalized_items,
            "lease_credentials_recorded": False,
        }

    def _operation_log_records(self, operation_id: str) -> dict[str, Any]:
        """Read bounded public Operation logs, following cursor pagination."""

        assert self.control_client is not None
        items: list[Any] = []
        cursor = ""
        seen_cursors: set[str] = set()
        pages = 0
        while pages < OPERATION_TIMEOUT_LOG_API_MAX_PAGES:
            query = {"limit": str(OPERATION_TIMEOUT_LOG_API_PAGE_SIZE)}
            if cursor:
                query["cursor"] = cursor
            path = (
                "/api/v1/operations/"
                + urllib.parse.quote(operation_id, safe="")
                + "/logs?"
                + urllib.parse.urlencode(query)
            )
            document = self.control_client.request(
                "GET", path, expected=(200,), timeout=10
            )[0]
            data = document.get("data", {})
            page_items = data.get("items", [])
            if not isinstance(page_items, list):
                raise FullGateError("Operation logs endpoint returned non-list items")
            items.extend(page_items)
            pages += 1
            next_cursor = data.get("next_cursor")
            if next_cursor in (None, ""):
                cursor = ""
                break
            if not isinstance(next_cursor, str) or next_cursor in seen_cursors:
                raise FullGateError("Operation logs endpoint repeated an invalid cursor")
            seen_cursors.add(next_cursor)
            cursor = next_cursor
        return {
            "source": "GET /api/v1/operations/{operation_id}/logs",
            "operation_id": operation_id,
            "page_count": pages,
            "item_count": len(items),
            "truncated": bool(cursor),
            "items": _redact_diagnostic_value(items),
        }

    def _operation_orchestrator_log_window(
        self, operation_id: str, latest: Mapping[str, Any]
    ) -> dict[str, Any]:
        """Capture the whole timeout window plus lines correlated to this Operation."""

        created_at_ms = latest.get("created_at_ms")
        now_seconds = int(time.time())
        since_seconds = (
            max(0, int(created_at_ms) // 1_000 - 5)
            if isinstance(created_at_ms, int) and not isinstance(created_at_ms, bool)
            else max(0, now_seconds - 600)
        )
        result = self.a.command(
            "logs",
            "--since",
            str(since_seconds),
            "--timestamps",
            "--tail",
            str(OPERATION_TIMEOUT_ORCHESTRATOR_LOG_TAIL_LINES),
            "orchestrator-a",
            timeout=30,
            check=False,
        )
        if result.returncode != 0:
            raise FullGateError(
                "orchestrator timeout log capture failed: "
                + ((result.stderr or result.stdout).strip() or f"exit {result.returncode}")
            )
        raw = result.stdout + result.stderr
        for attribute in (
            "postgres_password",
            "jwt_secret",
            "internal_token",
            "auth_internal_token",
            "workload_issuer_token",
            "auth_management_token",
            "gateway_management_token",
            "auth_contribution_ack_token",
            "gateway_contribution_ack_token",
            "auth_bootstrap_secret",
            "minio_access",
            "minio_secret",
            "admin_password",
        ):
            secret = getattr(self, attribute, "")
            if isinstance(secret, str) and secret:
                raw = raw.replace(secret, "[redacted]")
        raw = str(_redact_diagnostic_value(raw))
        job_ids = {
            str(item.get("job_id", ""))
            for item in latest.get("job_bindings", [])
            if isinstance(item, Mapping) and str(item.get("job_id", ""))
        }
        keywords = (
            operation_id,
            *sorted(job_ids),
            "topology control-plane worker error",
            "control-plane lease recovery error",
            "revision conflict",
            "FINALIZE_GROUP",
        )
        correlated = "\n".join(
            line for line in raw.splitlines() if any(keyword in line for keyword in keywords)
        )
        window, window_truncated, original_chars = _bounded_diagnostic_text(
            raw, OPERATION_TIMEOUT_ORCHESTRATOR_LOG_MAX_CHARS
        )
        correlated_window, correlated_truncated, correlated_chars = (
            _bounded_diagnostic_text(
                correlated, OPERATION_TIMEOUT_CORRELATED_LOG_MAX_CHARS
            )
        )
        return {
            "source": "docker logs orchestrator-a",
            "operation_id": operation_id,
            "since_unix": since_seconds,
            "tail_lines": OPERATION_TIMEOUT_ORCHESTRATOR_LOG_TAIL_LINES,
            "original_chars": original_chars,
            "window_truncated": window_truncated,
            "window": window,
            "correlated_original_chars": correlated_chars,
            "correlated_truncated": correlated_truncated,
            "correlated_lines": correlated_window,
        }

    def _capture_operation_timeout_diagnostics(
        self,
        operation_id: str,
        latest: Mapping[str, Any],
        timeout: float,
    ) -> dict[str, Any]:
        """Collect independent read-only evidence without masking the timeout."""

        captured_at_ms = int(time.time() * 1_000)
        diagnostic: dict[str, Any] = {
            "operation_id": operation_id,
            "wait_timeout_seconds": timeout,
            "captured_at_ms": captured_at_ms,
            "latest_operation": _redact_diagnostic_value(dict(latest)),
            "errors": [],
        }

        def capture(name: str, action: Any) -> None:
            try:
                diagnostic[name] = action()
            except Exception as error:  # diagnostics must preserve the real failure
                detail = str(error)
                diagnostic["errors"].append(
                    {
                        "source": name,
                        "error": detail[-OPERATION_TIMEOUT_DIAGNOSTIC_ERROR_MAX_CHARS:],
                    }
                )

        capture(
            "job_rows",
            lambda: self._operation_job_rows(operation_id, captured_at_ms),
        )
        capture(
            "operation_logs",
            lambda: self._operation_log_records(operation_id),
        )
        capture(
            "orchestrator_logs",
            lambda: self._operation_orchestrator_log_window(operation_id, latest),
        )
        self.h.evidence["operation_timeout_diagnostic"] = diagnostic
        checkpoint = getattr(self.h, "checkpoint", None)
        phase = self.h.evidence.get("phase")
        if callable(checkpoint) and isinstance(phase, str) and phase:
            try:
                checkpoint(phase)
            except Exception as error:
                diagnostic["errors"].append(
                    {
                        "source": "checkpoint",
                        "error": str(error)[-OPERATION_TIMEOUT_DIAGNOSTIC_ERROR_MAX_CHARS:],
                    }
                )
        return diagnostic

    def _wait_operation(self, operation_id: str, timeout: float) -> dict[str, Any]:
        if not operation_id:
            raise FullGateError("mutation did not return operation_id")
        deadline = time.monotonic() + timeout
        latest: dict[str, Any] = {}
        while time.monotonic() < deadline:
            latest = self._control_get("/api/v1/operations/" + operation_id).get("data", {}).get("operation", {})
            status = str(latest.get("status", "")).upper()
            if status in {"SUCCEEDED", "FAILED", "CANCELLED", "NEEDS_ATTENTION", "ROLLED_BACK"}:
                return latest
            time.sleep(1)
        self._capture_operation_timeout_diagnostics(operation_id, latest, timeout)
        raise FullGateError(
            f"Operation {operation_id} did not become terminal within {timeout:g} seconds: "
            f"status={latest.get('status')!r} revision={latest.get('revision')!r}; "
            "see evidence.operation_timeout_diagnostic"
        )

    @staticmethod
    def _sql(value: str) -> str:
        return "'" + value.replace("'", "''") + "'"

    def _query_json(self, database: str, sql: str, *, allow_empty: bool = False) -> dict[str, Any]:
        result = self.a.command(
            "exec", "postgres-a", "psql", "-X", "-A", "-t", "-v", "ON_ERROR_STOP=1",
            "-U", "postgres", "-d", database, "-c", sql, timeout=30,
        ).stdout.strip()
        if not result:
            if allow_empty:
                return {}
            raise FullGateError(f"read-only evidence query returned no row for {database}")
        try:
            value = json.loads(result.splitlines()[-1])
        except json.JSONDecodeError as error:
            raise FullGateError(f"evidence query returned invalid JSON: {result[-2000:]}") from error
        if not isinstance(value, dict):
            raise FullGateError("evidence query did not return a JSON object")
        return value

    # --------------------------------------------------------------- configs

    @staticmethod
    def _auth_config() -> str:
        return """Name: auth-service
Host: 0.0.0.0
Port: 8081
Database: {Url: ""}
Jaeger: {Endpoint: ""}
Jwt: {Secret: "", ExpireHours: 24}
InternalAuth: {Token: ""}
WorkloadIdentity:
  PrivateKeyFile: ""
  ControlPlaneToken: ""
  KeyID: workload-1
  Issuer: ojos-auth/workload
  Audience: ojos-gateway
  TTLSeconds: 900
"""

    def _gateway_config(self) -> str:
        return """Name: gateway-service
Host: 0.0.0.0
Port: 8080
Timeout: 600000
Middlewares:
  Timeout: false
  Recover: false
Database: {Url: ""}
Redis: {Url: ""}
Jaeger: {Endpoint: ""}
Jwt: {Secret: ""}
Storage: {ProblemsRoot: "", SubmissionsRoot: ""}
Proxy:
  TrustedServices:
    - {ServiceID: auth-service, Target: "http://auth-a:8081", StripPrefix: /api}
    - {ServiceID: problem-service, Target: "http://__A_HOST__:8083", StripPrefix: /api}
    - {ServiceID: judge-api, Target: "http://__A_HOST__:8082", StripPrefix: /api}
  Routes:
    - {Prefix: /api/auth, Target: "http://auth-a:8081", StripPrefix: /api, AuthMode: optional, TimeoutMS: 30000}
    - {Prefix: /api/problem, Target: "http://__A_HOST__:8083", StripPrefix: /api, AuthMode: required, TimeoutMS: 30000}
    - {Prefix: /api/judge, Target: "http://__A_HOST__:8082", StripPrefix: /api, AuthMode: required, TimeoutMS: 30000}
ServiceStatus: {ComposeServices: []}
InternalAuth:
  Enabled: true
  RotationIntervalSeconds: 21600
  VerifyGraceSeconds: 600
  RotateBeforeSeconds: 120
  TimestampSkewSeconds: 60
  NonceTTLSeconds: 120
Orchestrator: {Endpoint: "", InternalToken: "", NodeID: node-a}
AuthService: {Endpoint: "http://auth-a:8081"}
WorkloadIdentity:
  PublicKeyFile: ""
  KeyID: workload-1
  Issuer: ojos-auth/workload
  Audience: ojos-gateway
""".replace("__A_HOST__", self.h.a_ip)

    @staticmethod
    def _problem_config() -> str:
        return """Name: problem-service
Host: 0.0.0.0
Port: 8083
Database: {Url: ""}
Redis: {Url: ""}
Jaeger: {Endpoint: ""}
Storage:
  ProblemsRoot: /tmp/ojos-problems-unused
  ServiceEndpoint: ""
  InternalGatewayEndpoint: ""
  PutApiID: storage.object.put
  HeadApiID: storage.object.head
  Bucket: problems
  CallerService: ""
  CallerNodeID: ""
  ServiceToken: ""
AuthService: {Endpoint: "", AdminToken: "", InternalGatewayEndpoint: "", PermissionCheckApiID: auth.user.permission.check, CallerService: "", CallerNodeID: "", ServiceToken: ""}
InternalAuth: {Enabled: true, TimestampSkewSeconds: 60, NonceTTLSeconds: 120}
"""

    @staticmethod
    def _judge_config() -> str:
        return """Name: judge-api-service
Host: 0.0.0.0
Port: 8082
# Ordinary API timeout; worker claim is route-scoped to 35s in judgeapi.api.
Timeout: 3000
Database: {Url: ""}
Redis: {Url: ""}
Jaeger: {Endpoint: ""}
AuthService: {Endpoint: "", AdminToken: "", InternalGatewayEndpoint: "", PermissionCheckApiID: auth.user.permission.check, CallerService: "", CallerNodeID: "", ServiceToken: ""}
Storage:
  SubmissionsRoot: /tmp/ojos-submissions-unused
  ServiceEndpoint: ""
  InternalGatewayEndpoint: ""
  GetApiID: storage.object.get
  PutApiID: storage.object.put
  HeadApiID: storage.object.head
  Bucket: submissions
  CallerService: ""
  CallerNodeID: ""
  ServiceToken: ""
Submission: {MaxCodeBytes: 262144}
Languages:
  Items:
    - {Id: cpp17, DisplayName: C++17, Version: GCC C++17, Enabled: true, SourceFile: main.cpp}
WorkerAuth: {Token: "", LeaseTTLSeconds: 60}
WorkloadIdentity:
  PublicKeyFile: ""
  KeyID: workload-1
  Issuer: ojos-auth/workload
  Audience: ojos-gateway
  AllowLegacyWorkerToken: false
InternalAuth: {Enabled: true, TimestampSkewSeconds: 60, NonceTTLSeconds: 120}
"""

    @staticmethod
    def _storage_config() -> str:
        return """Name: storage-service
Host: 0.0.0.0
Port: 8085
Timeout: 600000
Middlewares:
  Timeout: false
  Recover: false
Storage:
  Backend: minio
  Root: /tmp/ojos-storage-unused
  Buckets: [problems, submissions, judge-artifacts]
  MinIO: {Endpoint: "minio-a:9000", AccessKey: "", SecretKey: "", UseSSL: false}
Jaeger: {Endpoint: ""}
"""
