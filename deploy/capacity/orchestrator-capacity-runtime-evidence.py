#!/usr/bin/env python3
"""Collect fail-closed runtime identity/configuration evidence for capacity hosts.

The collector deliberately emits only public configuration and structural facts.
Passwords, enrollment material, OIDC tokens, and private-key fingerprints never
enter the evidence document.  Docker and PostgreSQL probes are argv-only and do
not invoke a shell.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import socket
import ssl
import stat
import struct
import subprocess
import sys
import tempfile
import time
from typing import Any, Callable, Sequence
from urllib.parse import parse_qsl, unquote, urlsplit


SCHEMA_VERSION = 2
SHA40 = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
IMAGE_ID = re.compile(r"^sha256:[0-9a-f]{64}$")
OCI_DIGEST = re.compile(r"^[^\s@]+@sha256:[0-9a-f]{64}$")
BOOT_ID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
ENV_NAME = re.compile(r"^[A-Z][A-Z0-9_]*$")
DAEMON_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9:._-]{15,255}$")
MAX_DOCKER_OUTPUT_BYTES = 16 * 1024 * 1024
MAX_INPUT_BYTES = 16 * 1024 * 1024
POSTGRES_SSL_REQUEST_CODE = 80_877_103


class RuntimeEvidenceError(RuntimeError):
    pass


def canonical_bytes(value: Any) -> bytes:
    return json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")


def canonical_sha256(value: Any) -> str:
    return hashlib.sha256(canonical_bytes(value)).hexdigest()


def bounded_bytes(path: pathlib.Path, maximum: int = MAX_INPUT_BYTES) -> bytes:
    try:
        if path.is_symlink() or not path.is_file():
            raise RuntimeEvidenceError(f"input is not a regular file: {path}")
        with path.open("rb") as stream:
            raw = stream.read(maximum + 1)
    except OSError as error:
        raise RuntimeEvidenceError(
            f"cannot read runtime evidence input {path}"
        ) from error
    if len(raw) > maximum:
        raise RuntimeEvidenceError(f"runtime evidence input is oversized: {path}")
    return raw


def bounded_text(path: pathlib.Path, maximum: int = 4096) -> str:
    try:
        return bounded_bytes(path, maximum).decode("ascii").strip().lower()
    except UnicodeDecodeError as error:
        raise RuntimeEvidenceError(
            f"host identity input is not ASCII: {path}"
        ) from error


def load_json(path: pathlib.Path, maximum: int = MAX_INPUT_BYTES) -> Any:
    try:
        return json.loads(bounded_bytes(path, maximum))
    except json.JSONDecodeError as error:
        raise RuntimeEvidenceError(
            f"runtime evidence input is invalid JSON: {path}"
        ) from error


def load_env_file(path: pathlib.Path) -> dict[str, str]:
    try:
        text = bounded_bytes(path, 1024 * 1024).decode("utf-8")
    except UnicodeDecodeError as error:
        raise RuntimeEvidenceError("control-plane env file is not UTF-8") from error
    result: dict[str, str] = {}
    for line_number, raw_line in enumerate(text.splitlines(), start=1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export ") or "=" not in line:
            raise RuntimeEvidenceError(f"invalid env-file line {line_number}")
        name, value = line.split("=", 1)
        name = name.strip()
        value = value.strip()
        if not ENV_NAME.fullmatch(name) or name in result:
            raise RuntimeEvidenceError(
                f"invalid or duplicate env name on line {line_number}"
            )
        if len(value) >= 2 and value[0] == value[-1] and value[0] in ("'", '"'):
            value = value[1:-1]
        if "\x00" in value or "\r" in value or "\n" in value:
            raise RuntimeEvidenceError(f"invalid env value on line {line_number}")
        result[name] = value
    return result


def parse_container_env(config: dict[str, Any]) -> dict[str, str]:
    raw = config.get("Env")
    if not isinstance(raw, list) or any(not isinstance(item, str) for item in raw):
        raise RuntimeEvidenceError("container Env is missing or malformed")
    result: dict[str, str] = {}
    for item in raw:
        name, separator, value = item.partition("=")
        if not separator or not ENV_NAME.fullmatch(name) or name in result:
            raise RuntimeEvidenceError("container Env has an invalid or duplicate name")
        result[name] = value
    return result


def required_env(environment: dict[str, str], name: str) -> str:
    value = environment.get(name)
    if not isinstance(value, str) or not value or value != value.strip():
        raise RuntimeEvidenceError(f"{name} is required and must be canonical")
    return value


def normalize_https_origin(value: str, label: str) -> str:
    parsed = urlsplit(value)
    if (
        parsed.scheme != "https"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or parsed.path not in ("", "/")
    ):
        raise RuntimeEvidenceError(f"{label} must be an HTTPS origin")
    try:
        port = parsed.port
    except ValueError as error:
        raise RuntimeEvidenceError(f"{label} has an invalid port") from error
    host = parsed.hostname.lower()
    if ":" in host:
        host = f"[{host}]"
    return f"https://{host}" + (f":{port}" if port and port != 443 else "")


def database_projection(value: str) -> dict[str, Any]:
    parsed = urlsplit(value)
    if parsed.scheme not in ("postgres", "postgresql") or not parsed.hostname:
        raise RuntimeEvidenceError("database URL is not a PostgreSQL URL")
    try:
        port = parsed.port or 5432
    except ValueError as error:
        raise RuntimeEvidenceError("database URL has an invalid port") from error
    if parsed.fragment or not parsed.username or parsed.password is None:
        raise RuntimeEvidenceError(
            "database URL must carry one password-authenticated identity"
        )
    pairs = parse_qsl(parsed.query, keep_blank_values=True, strict_parsing=True)
    parameters: dict[str, str] = {}
    for name, item in pairs:
        if name in parameters:
            raise RuntimeEvidenceError("database URL contains duplicate parameters")
        parameters[name] = item
    if (
        parameters.get("sslmode") != "verify-full"
        or parameters.get("sslrootcert") != "/run/secrets/orchestrator-postgres-ca.crt"
        or any(
            not name
            or len(name) > 64
            or len(item) > 1024
            or any(token in name.lower() for token in ("password", "secret", "token"))
            for name, item in parameters.items()
        )
    ):
        raise RuntimeEvidenceError(
            "database URL must use sslmode=verify-full and the mounted CA"
        )
    database = unquote(parsed.path.removeprefix("/"))
    username = unquote(parsed.username)
    if not database or "/" in database or not username:
        raise RuntimeEvidenceError("database URL database/user identity is invalid")
    return {
        "scheme": "postgresql",
        "host": parsed.hostname.lower(),
        "port": port,
        "database": database,
        "username": username,
        "sslmode": "verify-full",
        "sslrootcert": "/run/secrets/orchestrator-postgres-ca.crt",
        "parameters": dict(sorted(parameters.items())),
        "password_present": True,
    }


def oidc_projection(environment: dict[str, str]) -> dict[str, Any]:
    scopes = required_env(environment, "ORCHESTRATOR_OIDC_SCOPES").split()
    if "openid" not in scopes or len(scopes) != len(set(scopes)):
        raise RuntimeEvidenceError("OIDC scopes must be unique and include openid")
    return {
        "issuer": normalize_https_origin(
            required_env(environment, "ORCHESTRATOR_OIDC_ISSUER"), "OIDC issuer"
        ),
        "audience": required_env(environment, "ORCHESTRATOR_OIDC_AUDIENCE"),
        "client_id": required_env(environment, "ORCHESTRATOR_OIDC_CLIENT_ID"),
        "public_base_url": normalize_https_origin(
            required_env(environment, "ORCHESTRATOR_PUBLIC_BASE_URL"),
            "public base URL",
        ),
        "scopes": scopes,
        "role_claim": required_env(environment, "ORCHESTRATOR_OIDC_ROLE_CLAIM"),
        "viewer_role": required_env(environment, "ORCHESTRATOR_OIDC_VIEWER_ROLE"),
        "operator_role": required_env(environment, "ORCHESTRATOR_OIDC_OPERATOR_ROLE"),
        "admin_role": required_env(environment, "ORCHESTRATOR_OIDC_ADMIN_ROLE"),
        "jwks_cache_seconds": int(
            required_env(environment, "ORCHESTRATOR_OIDC_JWKS_CACHE_SECONDS")
        ),
        "http_timeout_seconds": int(
            required_env(environment, "ORCHESTRATOR_OIDC_HTTP_TIMEOUT_SECONDS")
        ),
    }


def control_plane_environment_projection(
    environment: dict[str, str], control_plane_origin: str
) -> dict[str, Any]:
    result = {
        "profile": required_env(environment, "OJOS_ENVIRONMENT"),
        "legacy_api_mode": required_env(environment, "ORCHESTRATOR_LEGACY_API_MODE"),
        "database": database_projection(
            required_env(environment, "ORCHESTRATOR_DATABASE_URL")
        ),
        "oidc": oidc_projection(environment),
        "paths": {
            name: required_env(environment, name)
            for name in (
                "ORCHESTRATOR_POSTGRES_CA_CERT",
                "ORCHESTRATOR_TLS_CERT",
                "ORCHESTRATOR_TLS_KEY",
                "ORCHESTRATOR_NODE_CA_CERT",
                "ORCHESTRATOR_NODE_CA_KEY",
                "ORCHESTRATOR_HEALTHCHECK_CA_CERT",
            )
        },
        "healthcheck_url": required_env(environment, "ORCHESTRATOR_HEALTHCHECK_URL"),
    }
    expected_paths = {
        "ORCHESTRATOR_POSTGRES_CA_CERT": "/run/secrets/orchestrator-postgres-ca.crt",
        "ORCHESTRATOR_TLS_CERT": "/run/secrets/orchestrator-tls.crt",
        "ORCHESTRATOR_TLS_KEY": "/run/secrets/orchestrator-tls.key",
        "ORCHESTRATOR_NODE_CA_CERT": "/run/secrets/orchestrator-node-ca.crt",
        "ORCHESTRATOR_NODE_CA_KEY": "/run/secrets/orchestrator-node-ca.key",
        "ORCHESTRATOR_HEALTHCHECK_CA_CERT": "/run/secrets/orchestrator-health-ca.crt",
    }
    if (
        result["profile"] != "production"
        or result["legacy_api_mode"] != "gone"
        or result["paths"] != expected_paths
        or result["healthcheck_url"] != f"{control_plane_origin}/api/v1/healthz/ready"
    ):
        raise RuntimeEvidenceError(
            "control-plane production/TLS environment is invalid"
        )
    return result


def host_identity(
    role: str,
    *,
    machine_id_path: pathlib.Path = pathlib.Path("/etc/machine-id"),
    boot_id_path: pathlib.Path = pathlib.Path("/proc/sys/kernel/random/boot_id"),
) -> dict[str, str]:
    machine_id = bounded_text(machine_id_path)
    boot_id = bounded_text(boot_id_path)
    if (
        not re.fullmatch(r"(?:control-plane|postgres|runner|worker-[0-9]{2})", role)
        or not re.fullmatch(r"[0-9a-f]{32}", machine_id)
        or not BOOT_ID.fullmatch(boot_id)
    ):
        raise RuntimeEvidenceError("host role, machine-id or boot-id is invalid")
    return {
        "role": role,
        "machine_id_sha256": hashlib.sha256(machine_id.encode()).hexdigest(),
        "boot_id": boot_id,
    }


class DockerClient:
    def __init__(
        self,
        *,
        runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    ) -> None:
        self._runner = runner

    def run(self, argv: Sequence[str], timeout: float = 20.0) -> str:
        result = self._runner(
            list(argv),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout,
            check=False,
            shell=False,
        )
        if result.returncode != 0:
            raise RuntimeEvidenceError(
                f"Docker evidence argv exited with {result.returncode}: {list(argv)!r}"
            )
        if (
            not isinstance(result.stdout, str)
            or len(result.stdout) > MAX_DOCKER_OUTPUT_BYTES
        ):
            raise RuntimeEvidenceError("Docker evidence output is missing or oversized")
        return result.stdout

    def compose_ids(
        self,
        compose_file: pathlib.Path,
        project_directory: pathlib.Path,
        services: Sequence[str],
    ) -> list[str]:
        raw = self.run(
            (
                "docker",
                "compose",
                "--project-directory",
                str(project_directory),
                "-f",
                str(compose_file),
                "ps",
                "--all",
                "--quiet",
                *services,
            )
        )
        identifiers = [line.strip() for line in raw.splitlines() if line.strip()]
        if len(identifiers) != len(services) or len(set(identifiers)) != len(services):
            raise RuntimeEvidenceError(
                f"Compose returned {len(identifiers)} containers for {len(services)} services"
            )
        return identifiers

    def inspect_containers(self, identifiers: Sequence[str]) -> list[dict[str, Any]]:
        raw = self.run(("docker", "container", "inspect", *identifiers))
        try:
            values = json.loads(raw)
        except json.JSONDecodeError as error:
            raise RuntimeEvidenceError(
                "container inspect returned invalid JSON"
            ) from error
        if not isinstance(values, list) or any(
            not isinstance(item, dict) for item in values
        ):
            raise RuntimeEvidenceError(
                "container inspect did not return an object array"
            )
        by_id = {item.get("Id"): item for item in values}
        if len(by_id) != len(identifiers) or set(by_id) != set(identifiers):
            raise RuntimeEvidenceError(
                "container inspect did not cover the exact requested set"
            )
        return [by_id[identifier] for identifier in identifiers]

    def compose_inspections(
        self,
        compose_file: pathlib.Path,
        project_directory: pathlib.Path,
        services: Sequence[str],
    ) -> dict[str, dict[str, Any]]:
        identifiers = self.compose_ids(compose_file, project_directory, services)
        inspections = self.inspect_containers(identifiers)
        by_service: dict[str, dict[str, Any]] = {}
        for inspection in inspections:
            config = inspection.get("Config")
            labels = config.get("Labels") if isinstance(config, dict) else None
            service = (
                labels.get("com.docker.compose.service")
                if isinstance(labels, dict)
                else None
            )
            if service not in services or service in by_service:
                raise RuntimeEvidenceError(
                    "Compose inspection service identity is invalid"
                )
            by_service[service] = inspection
        if set(by_service) != set(services):
            raise RuntimeEvidenceError("Compose inspection omitted an expected service")
        return by_service

    def inspect_image(self, reference: str) -> dict[str, Any]:
        raw = self.run(("docker", "image", "inspect", reference))
        try:
            values = json.loads(raw)
        except json.JSONDecodeError as error:
            raise RuntimeEvidenceError("image inspect returned invalid JSON") from error
        if (
            not isinstance(values, list)
            or len(values) != 1
            or not isinstance(values[0], dict)
        ):
            raise RuntimeEvidenceError(
                "image inspect did not return exactly one object"
            )
        return values[0]


def validate_image(
    inspection: dict[str, Any], reference: str, candidate_sha: str | None = None
) -> dict[str, Any]:
    image_id = inspection.get("Id")
    repo_digests = inspection.get("RepoDigests")
    config = inspection.get("Config")
    labels = config.get("Labels") if isinstance(config, dict) else None
    if (
        not OCI_DIGEST.fullmatch(reference)
        or not isinstance(image_id, str)
        or not IMAGE_ID.fullmatch(image_id)
        or not isinstance(repo_digests, list)
        or reference not in repo_digests
    ):
        raise RuntimeEvidenceError("deployed image identity is not digest-pinned")
    revision = (
        labels.get("org.opencontainers.image.revision")
        if isinstance(labels, dict)
        else None
    )
    if candidate_sha is not None and revision != candidate_sha:
        raise RuntimeEvidenceError(
            "deployed image revision does not match the candidate"
        )
    return {
        "reference": reference,
        "repo_digest": reference,
        "image_id": image_id,
        "oci_revision": revision,
    }


def running_container(
    inspection: dict[str, Any],
    image: dict[str, Any],
    compose_service: str,
    compose_project: str,
    *,
    healthy: bool = False,
) -> tuple[dict[str, Any], dict[str, Any], list[dict[str, Any]]]:
    state = inspection.get("State")
    config = inspection.get("Config")
    labels = config.get("Labels") if isinstance(config, dict) else None
    mounts = inspection.get("Mounts")
    health = state.get("Health") if isinstance(state, dict) else None
    if (
        not isinstance(state, dict)
        or state.get("Running") is not True
        or str(state.get("Status", "")).lower() != "running"
        or not isinstance(state.get("StartedAt"), str)
        or not state["StartedAt"]
        or (
            healthy
            and (not isinstance(health, dict) or health.get("Status") != "healthy")
        )
        or not isinstance(config, dict)
        or config.get("Image") != image["reference"]
        or inspection.get("Image") != image["image_id"]
        or not isinstance(labels, dict)
        or labels.get("com.docker.compose.service") != compose_service
        or labels.get("com.docker.compose.project") != compose_project
        or not isinstance(mounts, list)
    ):
        raise RuntimeEvidenceError(
            f"Compose service {compose_service} is not the expected Running image"
        )
    return state, config, mounts


def expected_port_binding(
    inspection: dict[str, Any],
    container_port: int,
    host_ip: str,
    host_port: int,
    *,
    exact_set: bool = True,
) -> dict[str, Any]:
    key = f"{container_port}/tcp"
    expected = [{"HostIp": host_ip, "HostPort": str(host_port)}]
    host_config = inspection.get("HostConfig")
    network = inspection.get("NetworkSettings")
    host_bindings = (
        host_config.get("PortBindings") if isinstance(host_config, dict) else None
    )
    network_bindings = network.get("Ports") if isinstance(network, dict) else None
    if (
        not isinstance(host_bindings, dict)
        or (exact_set and set(host_bindings) != {key})
        or host_bindings.get(key) != expected
        or not isinstance(network_bindings, dict)
        or network_bindings.get(key) != expected
    ):
        raise RuntimeEvidenceError(f"container has an unexpected {key} port binding")
    return {
        "container_port": container_port,
        "protocol": "tcp",
        "host_ip": host_ip,
        "host_port": host_port,
    }


def exact_mount(
    mounts: Sequence[dict[str, Any]],
    *,
    destination: str,
    mount_type: str,
    read_only: bool,
    source: str | None = None,
    name_suffix: str | None = None,
) -> dict[str, Any]:
    matches = [item for item in mounts if item.get("Destination") == destination]
    if len(matches) != 1:
        raise RuntimeEvidenceError(f"expected exactly one mount at {destination}")
    mount = matches[0]
    actual_source = mount.get("Source")
    actual_name = mount.get("Name")
    if (
        mount.get("Type") != mount_type
        or mount.get("RW") is not (not read_only)
        or (
            source is not None
            and (
                (mount_type == "volume" and actual_name != source)
                or (mount_type != "volume" and actual_source != source)
            )
        )
        or (
            name_suffix is not None
            and (
                not isinstance(actual_name, str)
                or not actual_name.endswith(name_suffix)
            )
        )
    ):
        raise RuntimeEvidenceError(
            f"mount at {destination} does not match provisioning"
        )
    return {
        "type": mount_type,
        "source": (
            source
            if source is not None
            else str(actual_name if mount_type == "volume" else actual_source)
        ),
        "destination": destination,
        "read_only": read_only,
    }


def map_container_path(mount: dict[str, Any], container_path: str) -> pathlib.Path:
    destination = pathlib.PurePosixPath(
        str(mount.get("Destination", mount.get("destination", "")))
    )
    target = pathlib.PurePosixPath(container_path)
    try:
        relative = target.relative_to(destination)
    except ValueError as error:
        raise RuntimeEvidenceError(
            "container path is outside the expected mount"
        ) from error
    source = pathlib.Path(str(mount.get("Source", mount.get("source", ""))))
    return source.joinpath(*relative.parts)


def pem_certificate_fingerprints(path: pathlib.Path) -> list[str]:
    try:
        text = bounded_bytes(path, 1024 * 1024).decode("ascii")
    except UnicodeDecodeError as error:
        raise RuntimeEvidenceError(
            f"certificate bundle is not ASCII PEM: {path}"
        ) from error
    blocks = re.findall(
        r"-----BEGIN CERTIFICATE-----\s+.+?\s+-----END CERTIFICATE-----",
        text,
        flags=re.DOTALL,
    )
    if not blocks:
        raise RuntimeEvidenceError(f"certificate bundle is empty: {path}")
    fingerprints: list[str] = []
    try:
        for block in blocks:
            der = ssl.PEM_cert_to_DER_cert(block)
            fingerprints.append(hashlib.sha256(der).hexdigest())
    except ValueError as error:
        raise RuntimeEvidenceError(
            f"certificate bundle contains invalid PEM: {path}"
        ) from error
    return fingerprints


def postgres_tls_identity(
    projection: dict[str, Any], ca_file: pathlib.Path, timeout: float = 5.0
) -> dict[str, Any]:
    context = ssl.create_default_context(cafile=str(ca_file))
    context.check_hostname = True
    context.verify_mode = ssl.CERT_REQUIRED
    try:
        with socket.create_connection(
            (projection["host"], projection["port"]), timeout=timeout
        ) as connection:
            connection.sendall(struct.pack("!II", 8, POSTGRES_SSL_REQUEST_CODE))
            if connection.recv(1) != b"S":
                raise RuntimeEvidenceError(
                    "PostgreSQL endpoint refused TLS negotiation"
                )
            with context.wrap_socket(
                connection, server_hostname=projection["host"]
            ) as secure:
                peer = secure.getpeercert(binary_form=True)
                if not peer or not secure.version():
                    raise RuntimeEvidenceError(
                        "PostgreSQL TLS peer identity is incomplete"
                    )
                return {
                    "verified_hostname": projection["host"],
                    "port": projection["port"],
                    "peer_leaf_sha256": hashlib.sha256(peer).hexdigest(),
                    "root_certificates_sha256": pem_certificate_fingerprints(ca_file),
                    "tls_version": secure.version(),
                }
    except (OSError, ssl.SSLError) as error:
        raise RuntimeEvidenceError(
            "verify-full PostgreSQL TLS identity failed"
        ) from error


def generate_manifest(
    *,
    candidate_sha: str,
    control_plane_image: str,
    postgres_image: str,
    agent_image: str,
    engine_image: str,
    control_plane_origin: str,
    control_plane_listen_address: str,
    database_listen_address: str,
    postgres_database: str,
    postgres_user: str,
    control_plane_env_file: pathlib.Path,
) -> dict[str, Any]:
    if not SHA40.fullmatch(candidate_sha):
        raise RuntimeEvidenceError("candidate SHA must be canonical lowercase")
    for reference in (control_plane_image, postgres_image, agent_image, engine_image):
        if not OCI_DIGEST.fullmatch(reference):
            raise RuntimeEvidenceError("manifest images must be digest pinned")
    origin = normalize_https_origin(control_plane_origin, "control-plane origin")
    environment = load_env_file(control_plane_env_file)
    # Compose overrides these values; model the effective container environment,
    # not only the protected env-file contents.
    environment.update(
        {
            "OJOS_ENVIRONMENT": "production",
            "ORCHESTRATOR_LEGACY_API_MODE": "gone",
            "ORCHESTRATOR_POSTGRES_CA_CERT": "/run/secrets/orchestrator-postgres-ca.crt",
            "ORCHESTRATOR_TLS_CERT": "/run/secrets/orchestrator-tls.crt",
            "ORCHESTRATOR_TLS_KEY": "/run/secrets/orchestrator-tls.key",
            "ORCHESTRATOR_NODE_CA_CERT": "/run/secrets/orchestrator-node-ca.crt",
            "ORCHESTRATOR_NODE_CA_KEY": "/run/secrets/orchestrator-node-ca.key",
            "ORCHESTRATOR_HEALTHCHECK_URL": f"{origin}/api/v1/healthz/ready",
            "ORCHESTRATOR_HEALTHCHECK_CA_CERT": "/run/secrets/orchestrator-health-ca.crt",
        }
    )
    cp_configuration = {
        "compose_project": "ojos-capacity-control-plane",
        "compose_service": "orchestrator",
        "environment": control_plane_environment_projection(environment, origin),
        "secret_mount": {
            "type": "bind",
            "source": "/etc/ojos/capacity/control-plane",
            "destination": "/run/secrets",
            "read_only": True,
        },
        "port": {
            "container_port": 8090,
            "protocol": "tcp",
            "host_ip": control_plane_listen_address,
            "host_port": 8090,
        },
    }
    postgres_configuration = {
        "compose_project": "ojos-capacity-postgres",
        "compose_service": "postgres",
        "environment": {
            "database": postgres_database,
            "username": postgres_user,
            "password_file": "/run/secrets/postgres-password",
        },
        "command": [
            "postgres",
            "-c",
            "ssl=on",
            "-c",
            "ssl_cert_file=/run/secrets/server.crt",
            "-c",
            "ssl_key_file=/run/secrets/server.key",
            "-c",
            "ssl_ca_file=/run/secrets/root.crt",
            "-c",
            "max_connections=300",
        ],
        "secret_mount": {
            "type": "bind",
            "source": "/etc/ojos/capacity/postgres",
            "destination": "/run/secrets",
            "read_only": True,
        },
        "data_mount": {
            "type": "volume",
            "source": "ojos-capacity-postgres_postgres-data",
            "destination": "/var/lib/postgresql/data",
            "read_only": False,
        },
        "port": {
            "container_port": 5432,
            "protocol": "tcp",
            "host_ip": database_listen_address,
            "host_port": 5432,
        },
    }
    return {
        "schema_version": SCHEMA_VERSION,
        "candidate_sha": candidate_sha,
        "control_plane_origin": origin,
        "control_plane": {
            "image": control_plane_image,
            "configuration": cp_configuration,
        },
        "postgres": {"image": postgres_image, "configuration": postgres_configuration},
        "agent": {
            "image": agent_image,
            "command_prefix": [
                "run",
                "--control-plane",
                origin,
                "--identity-dir",
                "/var/lib/ojos-agent/identity",
                "--ledger",
                "/var/lib/ojos-agent/execution-ledger.sqlite3",
                "--instance",
            ],
            "socket_destination": "/var/run",
            "ledger_destination": "/var/lib/ojos-agent",
            "ledger_root": "/var/lib/ojos/capacity/agents",
            "ca_destination": "/run/secrets/control-plane-ca.pem",
            "ca_source": "/etc/ojos/capacity/control-plane-ca.pem",
        },
        "engine": {
            "image": engine_image,
            "command": [
                "--host=unix:///var/run/docker.sock",
                "--group=10004",
                "--storage-driver=overlay2",
                "--log-driver=json-file",
                "--log-opt=max-size=10m",
                "--log-opt=max-file=3",
            ],
            "privileged": True,
            "socket_destination": "/var/run",
            "data_destination": "/var/lib/docker",
            "host_ip": "0.0.0.0",
            "first_host_port": 20_000,
            "engines_per_worker": 10,
            "services_per_engine": 20,
        },
    }


def validate_manifest(document: Any) -> dict[str, Any]:
    if not isinstance(document, dict) or set(document) != {
        "schema_version",
        "candidate_sha",
        "control_plane_origin",
        "control_plane",
        "postgres",
        "agent",
        "engine",
    }:
        raise RuntimeEvidenceError("provision manifest fields are invalid")
    if document.get("schema_version") != SCHEMA_VERSION or not SHA40.fullmatch(
        str(document.get("candidate_sha", ""))
    ):
        raise RuntimeEvidenceError("provision manifest identity is invalid")
    if document.get("control_plane_origin") != normalize_https_origin(
        str(document.get("control_plane_origin", "")), "manifest control-plane origin"
    ):
        raise RuntimeEvidenceError("provision manifest origin is not canonical")
    control_plane = document.get("control_plane")
    postgres = document.get("postgres")
    agent = document.get("agent")
    engine = document.get("engine")
    if (
        not isinstance(control_plane, dict)
        or set(control_plane) != {"image", "configuration"}
        or not OCI_DIGEST.fullmatch(str(control_plane.get("image", "")))
        or not isinstance(control_plane.get("configuration"), dict)
        or not isinstance(postgres, dict)
        or set(postgres) != {"image", "configuration"}
        or not OCI_DIGEST.fullmatch(str(postgres.get("image", "")))
        or not isinstance(postgres.get("configuration"), dict)
        or not isinstance(agent, dict)
        or not OCI_DIGEST.fullmatch(str(agent.get("image", "")))
        or not isinstance(engine, dict)
        or not OCI_DIGEST.fullmatch(str(engine.get("image", "")))
    ):
        raise RuntimeEvidenceError("provision manifest runtime sections are invalid")
    cp_configuration = control_plane["configuration"]
    pg_configuration = postgres["configuration"]
    if set(cp_configuration) != {
        "compose_project",
        "compose_service",
        "environment",
        "secret_mount",
        "port",
    } or set(pg_configuration) != {
        "compose_project",
        "compose_service",
        "environment",
        "command",
        "secret_mount",
        "data_mount",
        "port",
    }:
        raise RuntimeEvidenceError("provision manifest configuration schema is invalid")
    cp_environment = cp_configuration.get("environment")
    database = (
        cp_environment.get("database") if isinstance(cp_environment, dict) else None
    )
    oidc = cp_environment.get("oidc") if isinstance(cp_environment, dict) else None
    if (
        not isinstance(cp_environment, dict)
        or set(cp_environment)
        != {
            "profile",
            "legacy_api_mode",
            "database",
            "oidc",
            "paths",
            "healthcheck_url",
        }
        or not isinstance(database, dict)
        or set(database)
        != {
            "scheme",
            "host",
            "port",
            "database",
            "username",
            "sslmode",
            "sslrootcert",
            "parameters",
            "password_present",
        }
        or database.get("scheme") != "postgresql"
        or database.get("sslmode") != "verify-full"
        or database.get("sslrootcert") != "/run/secrets/orchestrator-postgres-ca.crt"
        or database.get("password_present") is not True
        or not isinstance(oidc, dict)
        or set(oidc)
        != {
            "issuer",
            "audience",
            "client_id",
            "public_base_url",
            "scopes",
            "role_claim",
            "viewer_role",
            "operator_role",
            "admin_role",
            "jwks_cache_seconds",
            "http_timeout_seconds",
        }
    ):
        raise RuntimeEvidenceError("provision manifest production identity is invalid")
    expected_paths = {
        "ORCHESTRATOR_POSTGRES_CA_CERT": "/run/secrets/orchestrator-postgres-ca.crt",
        "ORCHESTRATOR_TLS_CERT": "/run/secrets/orchestrator-tls.crt",
        "ORCHESTRATOR_TLS_KEY": "/run/secrets/orchestrator-tls.key",
        "ORCHESTRATOR_NODE_CA_CERT": "/run/secrets/orchestrator-node-ca.crt",
        "ORCHESTRATOR_NODE_CA_KEY": "/run/secrets/orchestrator-node-ca.key",
        "ORCHESTRATOR_HEALTHCHECK_CA_CERT": "/run/secrets/orchestrator-health-ca.crt",
    }
    pg_environment = pg_configuration.get("environment")
    if (
        cp_configuration.get("compose_project") != "ojos-capacity-control-plane"
        or cp_configuration.get("compose_service") != "orchestrator"
        or cp_environment.get("profile") != "production"
        or cp_environment.get("legacy_api_mode") != "gone"
        or cp_environment.get("paths") != expected_paths
        or cp_environment.get("healthcheck_url")
        != f"{document['control_plane_origin']}/api/v1/healthz/ready"
        or cp_configuration.get("secret_mount")
        != {
            "type": "bind",
            "source": "/etc/ojos/capacity/control-plane",
            "destination": "/run/secrets",
            "read_only": True,
        }
        or not isinstance(cp_configuration.get("port"), dict)
        or set(cp_configuration["port"])
        != {"container_port", "protocol", "host_ip", "host_port"}
        or cp_configuration["port"].get("container_port") != 8090
        or cp_configuration["port"].get("protocol") != "tcp"
        or cp_configuration["port"].get("host_port") != 8090
        or pg_configuration.get("compose_project") != "ojos-capacity-postgres"
        or pg_configuration.get("compose_service") != "postgres"
        or not isinstance(pg_environment, dict)
        or set(pg_environment) != {"database", "username", "password_file"}
        or pg_environment.get("password_file") != "/run/secrets/postgres-password"
        or pg_configuration.get("command")
        != [
            "postgres",
            "-c",
            "ssl=on",
            "-c",
            "ssl_cert_file=/run/secrets/server.crt",
            "-c",
            "ssl_key_file=/run/secrets/server.key",
            "-c",
            "ssl_ca_file=/run/secrets/root.crt",
            "-c",
            "max_connections=300",
        ]
        or pg_configuration.get("secret_mount")
        != {
            "type": "bind",
            "source": "/etc/ojos/capacity/postgres",
            "destination": "/run/secrets",
            "read_only": True,
        }
        or pg_configuration.get("data_mount")
        != {
            "type": "volume",
            "source": "ojos-capacity-postgres_postgres-data",
            "destination": "/var/lib/postgresql/data",
            "read_only": False,
        }
        or not isinstance(pg_configuration.get("port"), dict)
        or set(pg_configuration["port"])
        != {"container_port", "protocol", "host_ip", "host_port"}
        or pg_configuration["port"].get("container_port") != 5432
        or pg_configuration["port"].get("protocol") != "tcp"
        or pg_configuration["port"].get("host_port") != 5432
    ):
        raise RuntimeEvidenceError("provision manifest container structure is invalid")
    expected_agent_keys = {
        "image",
        "command_prefix",
        "socket_destination",
        "ledger_destination",
        "ledger_root",
        "ca_destination",
        "ca_source",
    }
    expected_engine_keys = {
        "image",
        "command",
        "privileged",
        "socket_destination",
        "data_destination",
        "host_ip",
        "first_host_port",
        "engines_per_worker",
        "services_per_engine",
    }
    if (
        set(agent) != expected_agent_keys
        or agent.get("command_prefix")
        != [
            "run",
            "--control-plane",
            document["control_plane_origin"],
            "--identity-dir",
            "/var/lib/ojos-agent/identity",
            "--ledger",
            "/var/lib/ojos-agent/execution-ledger.sqlite3",
            "--instance",
        ]
        or agent.get("socket_destination") != "/var/run"
        or agent.get("ledger_destination") != "/var/lib/ojos-agent"
        or agent.get("ledger_root") != "/var/lib/ojos/capacity/agents"
        or agent.get("ca_destination") != "/run/secrets/control-plane-ca.pem"
        or agent.get("ca_source") != "/etc/ojos/capacity/control-plane-ca.pem"
        or set(engine) != expected_engine_keys
        or engine.get("command")
        != [
            "--host=unix:///var/run/docker.sock",
            "--group=10004",
            "--storage-driver=overlay2",
            "--log-driver=json-file",
            "--log-opt=max-size=10m",
            "--log-opt=max-file=3",
        ]
        or engine.get("privileged") is not True
        or engine.get("socket_destination") != "/var/run"
        or engine.get("data_destination") != "/var/lib/docker"
        or engine.get("first_host_port") != 20_000
        or engine.get("engines_per_worker") != 10
        or engine.get("services_per_engine") != 20
    ):
        raise RuntimeEvidenceError(
            "provision manifest Agent/Engine contract is invalid"
        )
    return document


def collect_control_plane(
    client: DockerClient,
    compose_file: pathlib.Path,
    project_directory: pathlib.Path,
    manifest: dict[str, Any],
    identity: dict[str, str],
) -> dict[str, Any]:
    expected = manifest["control_plane"]
    image = validate_image(
        client.inspect_image(expected["image"]),
        expected["image"],
        manifest["candidate_sha"],
    )
    configuration = expected["configuration"]
    inspection = client.compose_inspections(
        compose_file, project_directory, ("orchestrator",)
    )["orchestrator"]
    state, config, mounts = running_container(
        inspection,
        image,
        configuration["compose_service"],
        configuration["compose_project"],
    )
    if {item.get("Destination") for item in mounts if isinstance(item, dict)} != {
        "/run/secrets",
        "/var/lib/ojos/orchestrator/artifacts",
    }:
        raise RuntimeEvidenceError("control-plane mount set differs from provisioning")
    environment = parse_container_env(config)
    secret_mount = exact_mount(
        mounts,
        destination=configuration["secret_mount"]["destination"],
        mount_type="bind",
        read_only=True,
        source=configuration["secret_mount"]["source"],
    )
    exact_mount(
        mounts,
        destination="/var/lib/ojos/orchestrator/artifacts",
        mount_type="volume",
        read_only=False,
        source="ojos-capacity-control-plane_orchestrator-artifacts",
    )
    port = expected_port_binding(
        inspection,
        configuration["port"]["container_port"],
        configuration["port"]["host_ip"],
        configuration["port"]["host_port"],
    )
    observed_configuration = {
        "compose_project": configuration["compose_project"],
        "compose_service": configuration["compose_service"],
        "environment": control_plane_environment_projection(
            environment, manifest["control_plane_origin"]
        ),
        "secret_mount": secret_mount,
        "port": port,
    }
    if observed_configuration != configuration:
        raise RuntimeEvidenceError(
            "effective control-plane configuration differs from provisioning"
        )
    database = observed_configuration["environment"]["database"]
    ca_file = map_container_path(secret_mount, database["sslrootcert"])
    tls_identity = postgres_tls_identity(database, ca_file)
    return {
        "schema_version": SCHEMA_VERSION,
        "candidate_sha": manifest["candidate_sha"],
        "provision_manifest_sha256": canonical_sha256(manifest),
        "host": identity,
        "image": image,
        "container": {
            "container_id": inspection["Id"],
            "container_name": str(inspection.get("Name", "")).removeprefix("/"),
            "started_at": state["StartedAt"],
            "state": "RUNNING",
        },
        "configuration": {
            "effective_sha256": canonical_sha256(observed_configuration),
            "provisioned_sha256": canonical_sha256(configuration),
            "non_sensitive": observed_configuration,
        },
        "database_tls_identity": tls_identity,
    }


def parse_postgres_settings(raw: str) -> dict[str, str]:
    lines = [line.strip() for line in raw.splitlines() if line.strip()]
    if len(lines) != 1:
        raise RuntimeEvidenceError("PostgreSQL settings query returned unexpected rows")
    fields = lines[0].split("\t")
    if len(fields) != 7:
        raise RuntimeEvidenceError(
            "PostgreSQL settings query returned unexpected fields"
        )
    return dict(
        zip(
            (
                "ssl",
                "ssl_cert_file",
                "ssl_key_file",
                "ssl_ca_file",
                "data_directory",
                "port",
                "postmaster_started_at",
            ),
            fields,
        )
    )


def collect_postgres(
    client: DockerClient,
    compose_file: pathlib.Path,
    project_directory: pathlib.Path,
    manifest: dict[str, Any],
    identity: dict[str, str],
) -> dict[str, Any]:
    expected = manifest["postgres"]
    image = validate_image(client.inspect_image(expected["image"]), expected["image"])
    configuration = expected["configuration"]
    inspection = client.compose_inspections(
        compose_file, project_directory, ("postgres",)
    )["postgres"]
    state, config, mounts = running_container(
        inspection,
        image,
        configuration["compose_service"],
        configuration["compose_project"],
        healthy=True,
    )
    if {item.get("Destination") for item in mounts if isinstance(item, dict)} != {
        "/run/secrets",
        "/var/lib/postgresql/data",
    }:
        raise RuntimeEvidenceError("PostgreSQL mount set differs from provisioning")
    environment = parse_container_env(config)
    observed_environment = {
        "database": required_env(environment, "POSTGRES_DB"),
        "username": required_env(environment, "POSTGRES_USER"),
        "password_file": required_env(environment, "POSTGRES_PASSWORD_FILE"),
    }
    secret_mount = exact_mount(
        mounts,
        destination=configuration["secret_mount"]["destination"],
        mount_type="bind",
        read_only=True,
        source=configuration["secret_mount"]["source"],
    )
    data_mount = exact_mount(
        mounts,
        destination=configuration["data_mount"]["destination"],
        mount_type="volume",
        read_only=False,
        source=configuration["data_mount"]["source"],
    )
    port = expected_port_binding(
        inspection,
        configuration["port"]["container_port"],
        configuration["port"]["host_ip"],
        configuration["port"]["host_port"],
    )
    observed_configuration = {
        "compose_project": configuration["compose_project"],
        "compose_service": configuration["compose_service"],
        "environment": observed_environment,
        "command": config.get("Cmd"),
        "secret_mount": secret_mount,
        "data_mount": {
            **data_mount,
            "source": data_mount["source"],
        },
        "port": port,
    }
    if observed_configuration != configuration:
        raise RuntimeEvidenceError(
            "effective PostgreSQL configuration differs from provisioning"
        )
    query = (
        "SELECT current_setting('ssl'),current_setting('ssl_cert_file'),"
        "current_setting('ssl_key_file'),current_setting('ssl_ca_file'),"
        "current_setting('data_directory'),current_setting('port'),"
        "pg_postmaster_start_time()::text"
    )
    settings = parse_postgres_settings(
        client.run(
            (
                "docker",
                "container",
                "exec",
                inspection["Id"],
                "psql",
                "--no-psqlrc",
                "--tuples-only",
                "--no-align",
                "--field-separator",
                "\t",
                "--username",
                observed_environment["username"],
                "--dbname",
                observed_environment["database"],
                "--command",
                query,
            )
        )
    )
    if (
        settings
        != {
            **{
                "ssl": "on",
                "ssl_cert_file": "/run/secrets/server.crt",
                "ssl_key_file": "/run/secrets/server.key",
                "ssl_ca_file": "/run/secrets/root.crt",
                "data_directory": "/var/lib/postgresql/data",
                "port": "5432",
            },
            "postmaster_started_at": settings["postmaster_started_at"],
        }
        or not settings["postmaster_started_at"]
    ):
        raise RuntimeEvidenceError("PostgreSQL effective TLS/data settings are invalid")
    server_certificate = pem_certificate_fingerprints(
        map_container_path(secret_mount, "/run/secrets/server.crt")
    )
    root_certificates = pem_certificate_fingerprints(
        map_container_path(secret_mount, "/run/secrets/root.crt")
    )
    return {
        "schema_version": SCHEMA_VERSION,
        "candidate_sha": manifest["candidate_sha"],
        "provision_manifest_sha256": canonical_sha256(manifest),
        "host": identity,
        "image": image,
        "container": {
            "container_id": inspection["Id"],
            "container_name": str(inspection.get("Name", "")).removeprefix("/"),
            "started_at": state["StartedAt"],
            "state": "RUNNING",
            "health": "HEALTHY",
        },
        "configuration": {
            "effective_sha256": canonical_sha256(observed_configuration),
            "provisioned_sha256": canonical_sha256(configuration),
            "non_sensitive": observed_configuration,
        },
        "server_leaf_sha256": server_certificate[0],
        "root_certificates_sha256": root_certificates,
        "settings": settings,
    }


def require_owned_regular_file(
    path: pathlib.Path, *, expected_uid: int = 10_004, private: bool = False
) -> os.stat_result:
    try:
        if path.is_symlink() or not path.is_file():
            raise RuntimeEvidenceError(f"Agent state is not a regular file: {path}")
        information = path.stat()
    except OSError as error:
        raise RuntimeEvidenceError(f"cannot inspect Agent state: {path}") from error
    mode = stat.S_IMODE(information.st_mode)
    if information.st_uid != expected_uid or (private and mode != 0o600):
        raise RuntimeEvidenceError(f"Agent state ownership/mode is invalid: {path}")
    return information


def collect_agent_state(state_root: pathlib.Path, node_id: str) -> dict[str, Any]:
    try:
        if state_root.is_symlink() or not state_root.is_dir():
            raise RuntimeEvidenceError("Agent state root is not a real directory")
    except OSError as error:
        raise RuntimeEvidenceError("cannot inspect Agent state root") from error
    identity_root = state_root / "identity"
    current_path = identity_root / "current.json"
    require_owned_regular_file(current_path)
    current = load_json(current_path, 16 * 1024)
    if (
        not isinstance(current, dict)
        or set(current) != {"schema_version", "generation"}
        or current.get("schema_version") != 1
        or not isinstance(current.get("generation"), str)
        or not re.fullmatch(r"[0-9a-f]{1,128}", current["generation"])
    ):
        raise RuntimeEvidenceError("Agent current identity pointer is invalid")
    generation = current["generation"]
    generation_root = identity_root / "generations" / generation
    if generation_root.is_symlink() or not generation_root.is_dir():
        raise RuntimeEvidenceError("Agent current identity generation is missing")
    metadata_path = generation_root / "identity.json"
    certificate_path = generation_root / "certificate.pem"
    private_key_path = generation_root / "private-key.pem"
    node_ca_path = generation_root / "node-ca.pem"
    server_ca_path = generation_root / "server-ca.pem"
    for path in (metadata_path, certificate_path, node_ca_path, server_ca_path):
        require_owned_regular_file(path)
    private_key = require_owned_regular_file(private_key_path, private=True)
    if not 64 <= private_key.st_size <= 64 * 1024:
        raise RuntimeEvidenceError("Agent private key size is invalid")
    metadata = load_json(metadata_path, 64 * 1024)
    if (
        not isinstance(metadata, dict)
        or set(metadata)
        != {
            "schema_version",
            "node_id",
            "spiffe_id",
            "serial_hex",
            "not_after_ms",
            "renew_after_ms",
            "installed_at_ms",
        }
        or metadata.get("schema_version") != 1
        or metadata.get("node_id") != node_id
        or metadata.get("spiffe_id") != f"spiffe://ojos.local/node/{node_id}"
        or metadata.get("serial_hex") != generation
        or not isinstance(metadata.get("not_after_ms"), int)
        or not isinstance(metadata.get("renew_after_ms"), int)
        or not isinstance(metadata.get("installed_at_ms"), int)
        or metadata["installed_at_ms"] <= 0
        or metadata["renew_after_ms"] <= metadata["installed_at_ms"]
        or metadata["not_after_ms"] <= metadata["renew_after_ms"]
        or metadata["not_after_ms"] <= int(time.time() * 1000)
    ):
        raise RuntimeEvidenceError("Agent mTLS identity metadata is invalid")
    try:
        decoded = ssl._ssl._test_decode_cert(str(certificate_path))  # type: ignore[attr-defined]
    except (OSError, ValueError, ssl.SSLError) as error:
        raise RuntimeEvidenceError(
            "Agent client certificate cannot be decoded"
        ) from error
    sans = [
        value
        for kind, value in decoded.get("subjectAltName", ())
        if kind == "URI" and value.startswith("spiffe://ojos.local/node/")
    ]
    decoded_serial = str(decoded.get("serialNumber", "")).replace(":", "").lower()
    decoded_not_after = decoded.get("notAfter")
    try:
        decoded_not_after_ms = int(ssl.cert_time_to_seconds(decoded_not_after) * 1000)
    except (TypeError, ValueError) as error:
        raise RuntimeEvidenceError(
            "Agent client certificate expiry is invalid"
        ) from error
    if (
        sans != [metadata["spiffe_id"]]
        or decoded_serial.lstrip("0") != generation.lstrip("0")
        or abs(decoded_not_after_ms - metadata["not_after_ms"]) > 1000
    ):
        raise RuntimeEvidenceError(
            "Agent client certificate identity does not match metadata"
        )
    certificate_fingerprints = pem_certificate_fingerprints(certificate_path)
    if len(certificate_fingerprints) != 1:
        raise RuntimeEvidenceError(
            "Agent identity must contain exactly one client certificate"
        )
    ledger_path = state_root / "execution-ledger.sqlite3"
    ledger = require_owned_regular_file(ledger_path)
    try:
        with ledger_path.open("rb") as stream:
            header = stream.read(16)
    except OSError as error:
        raise RuntimeEvidenceError("cannot inspect Agent SQLite ledger") from error
    if header != b"SQLite format 3\x00" or ledger.st_size < 512:
        raise RuntimeEvidenceError(
            "Agent execution ledger is not a persistent SQLite database"
        )
    return {
        "identity": {
            "node_id": node_id,
            "spiffe_id": metadata["spiffe_id"],
            "serial_hex": generation,
            "certificate_sha256": certificate_fingerprints[0],
            "not_after_ms": metadata["not_after_ms"],
            "renew_after_ms": metadata["renew_after_ms"],
            "node_ca_certificates_sha256": pem_certificate_fingerprints(node_ca_path),
            "server_ca_certificates_sha256": pem_certificate_fingerprints(
                server_ca_path
            ),
            "private_key_present": True,
            "private_key_mode": "0600",
        },
        "ledger": {
            "path": str(ledger_path),
            "format": "sqlite3",
            "device": ledger.st_dev,
            "inode": ledger.st_ino,
            "size_bytes": ledger.st_size,
        },
    }


def validate_agent_mounts(
    mounts: Sequence[dict[str, Any]],
    worker_ordinal: int,
    engine_ordinal: int,
    expected: dict[str, Any],
) -> dict[str, str]:
    if {item.get("Destination") for item in mounts if isinstance(item, dict)} != {
        expected["socket_destination"],
        expected["ledger_destination"],
        expected["ca_destination"],
    }:
        raise RuntimeEvidenceError("Agent mount set differs from provisioning")
    volume = f"ojos-capacity-{worker_ordinal:02d}_engine-{engine_ordinal:02d}-socket"
    socket_mount = exact_mount(
        mounts,
        destination=expected["socket_destination"],
        mount_type="volume",
        read_only=False,
        source=volume,
    )
    exact_mount(
        mounts,
        destination=expected["ledger_destination"],
        mount_type="bind",
        read_only=False,
        source=f"{expected['ledger_root']}/{engine_ordinal:02d}",
    )
    source = str(
        next(
            item
            for item in mounts
            if item.get("Destination") == expected["ledger_destination"]
        ).get("Source", "")
    ).replace("\\", "/")
    if source != f"{expected['ledger_root']}/{engine_ordinal:02d}":
        raise RuntimeEvidenceError(
            "Agent ledger bind does not match its Engine ordinal"
        )
    exact_mount(
        mounts,
        destination=expected["ca_destination"],
        mount_type="bind",
        read_only=True,
        source=expected["ca_source"],
    )
    return {
        "socket_volume": socket_mount["source"],
        "ledger_source": source,
        "ca_source": str(
            next(
                item
                for item in mounts
                if item.get("Destination") == expected["ca_destination"]
            ).get("Source", "")
        ),
    }


def parse_inner_engine_info(raw: str) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise RuntimeEvidenceError("inner Docker info returned invalid JSON") from error
    if not isinstance(value, dict):
        raise RuntimeEvidenceError("inner Docker info is not an object")
    daemon_id = value.get("ID")
    if (
        not isinstance(daemon_id, str)
        or not DAEMON_ID.fullmatch(daemon_id)
        or value.get("DockerRootDir") != "/var/lib/docker"
        or value.get("Driver") != "overlay2"
        or value.get("OSType") != "linux"
        or value.get("Architecture") not in ("x86_64", "amd64")
        or value.get("Containers") != 20
        or value.get("ContainersRunning") != 20
        or value.get("ContainersPaused") != 0
        or value.get("ContainersStopped") != 0
        or not isinstance(value.get("ServerVersion"), str)
        or not value["ServerVersion"]
    ):
        raise RuntimeEvidenceError("inner Docker daemon identity/state is invalid")
    return {
        "daemon_id": daemon_id,
        "docker_root_dir": "/var/lib/docker",
        "storage_driver": "overlay2",
        "os_type": "linux",
        "architecture": value["Architecture"],
        "server_version": value["ServerVersion"],
        "containers": 20,
        "containers_running": 20,
    }


def collect_agents(
    client: DockerClient,
    compose_file: pathlib.Path,
    project_directory: pathlib.Path,
    manifest: dict[str, Any],
    worker_ordinal: int,
    identity: dict[str, str],
) -> dict[str, Any]:
    if not 0 <= worker_ordinal < 10:
        raise RuntimeEvidenceError("worker ordinal must be 0..9")
    expected_agent = manifest["agent"]
    expected_engine = manifest["engine"]
    agent_image = validate_image(
        client.inspect_image(expected_agent["image"]),
        expected_agent["image"],
        manifest["candidate_sha"],
    )
    engine_image = validate_image(
        client.inspect_image(expected_engine["image"]), expected_engine["image"]
    )
    agent_services = tuple(f"agent-{ordinal:02d}" for ordinal in range(10))
    engine_services = tuple(f"engine-{ordinal:02d}" for ordinal in range(10))
    inspections = client.compose_inspections(
        compose_file, project_directory, (*agent_services, *engine_services)
    )
    compose_project = f"ojos-capacity-{worker_ordinal:02d}"
    agents: list[dict[str, Any]] = []
    engines: list[dict[str, Any]] = []
    for engine_ordinal in range(10):
        agent_service = f"agent-{engine_ordinal:02d}"
        engine_service = f"engine-{engine_ordinal:02d}"
        agent_inspection = inspections[agent_service]
        agent_state, agent_config, agent_mounts = running_container(
            agent_inspection,
            agent_image,
            agent_service,
            compose_project,
        )
        node_id = f"capacity-node-{worker_ordinal:02d}-{engine_ordinal:02d}"
        instance = f"{node_id}-{manifest['candidate_sha'][:12]}"
        command = [*expected_agent["command_prefix"], instance]
        if agent_config.get("Cmd") != command:
            raise RuntimeEvidenceError(f"{agent_service} command is not exact")
        agent_mount_identity = validate_agent_mounts(
            agent_mounts, worker_ordinal, engine_ordinal, expected_agent
        )
        durable_state = collect_agent_state(
            pathlib.Path(agent_mount_identity["ledger_source"]), node_id
        )
        transport_ca = pem_certificate_fingerprints(
            pathlib.Path(agent_mount_identity["ca_source"])
        )
        if durable_state["identity"]["server_ca_certificates_sha256"] != transport_ca:
            raise RuntimeEvidenceError(
                f"{agent_service} persisted server CA differs from its mounted transport CA"
            )
        agents.append(
            {
                "node_id": node_id,
                "instance": instance,
                "control_plane_origin": manifest["control_plane_origin"],
                "container_id": agent_inspection["Id"],
                "started_at": agent_state["StartedAt"],
                "image_id": agent_image["image_id"],
                "repo_digest": agent_image["repo_digest"],
                "oci_revision": manifest["candidate_sha"],
                "state": "RUNNING",
                "mount_identity": agent_mount_identity,
                "transport_ca_certificates_sha256": transport_ca,
                **durable_state,
            }
        )

        engine_inspection = inspections[engine_service]
        engine_state, engine_config, engine_mounts = running_container(
            engine_inspection,
            engine_image,
            engine_service,
            compose_project,
            healthy=True,
        )
        host_config = engine_inspection.get("HostConfig")
        if (
            not isinstance(host_config, dict)
            or host_config.get("Privileged") is not True
            or engine_config.get("Cmd") != expected_engine["command"]
        ):
            raise RuntimeEvidenceError(
                f"{engine_service} isolation configuration is invalid"
            )
        if {
            item.get("Destination") for item in engine_mounts if isinstance(item, dict)
        } != {
            expected_engine["socket_destination"],
            expected_engine["data_destination"],
        }:
            raise RuntimeEvidenceError(f"{engine_service} mount set is invalid")
        volume_prefix = (
            f"ojos-capacity-{worker_ordinal:02d}_engine-{engine_ordinal:02d}"
        )
        socket_mount = exact_mount(
            engine_mounts,
            destination=expected_engine["socket_destination"],
            mount_type="volume",
            read_only=False,
            source=f"{volume_prefix}-socket",
        )
        data_mount = exact_mount(
            engine_mounts,
            destination=expected_engine["data_destination"],
            mount_type="volume",
            read_only=False,
            source=f"{volume_prefix}-data",
        )
        if socket_mount["source"] != agent_mount_identity["socket_volume"]:
            raise RuntimeEvidenceError(
                "Agent is not bound to its exact isolated Engine socket"
            )
        first_port = expected_engine["first_host_port"] + engine_ordinal * 20
        observed_ports = [
            expected_port_binding(
                engine_inspection,
                first_port + service,
                expected_engine["host_ip"],
                first_port + service,
                exact_set=False,
            )
            for service in range(expected_engine["services_per_engine"])
        ]
        # expected_port_binding checks one-key exactness for normal services; an
        # Engine publishes 20 ports, so verify the complete dictionaries here.
        expected_bindings = {
            f"{first_port + service}/tcp": [
                {
                    "HostIp": expected_engine["host_ip"],
                    "HostPort": str(first_port + service),
                }
            ]
            for service in range(expected_engine["services_per_engine"])
        }
        host_bindings = host_config.get("PortBindings")
        network = engine_inspection.get("NetworkSettings")
        network_bindings = network.get("Ports") if isinstance(network, dict) else None
        if (
            host_bindings != expected_bindings
            or not isinstance(network_bindings, dict)
            or any(
                network_bindings.get(key) != value
                for key, value in expected_bindings.items()
            )
        ):
            raise RuntimeEvidenceError(
                f"{engine_service} published port set is invalid"
            )
        inner = parse_inner_engine_info(
            client.run(
                (
                    "docker",
                    "container",
                    "exec",
                    engine_inspection["Id"],
                    "docker",
                    "info",
                    "--format",
                    "{{json .}}",
                )
            )
        )
        engines.append(
            {
                "engine_ordinal": engine_ordinal,
                "node_id": node_id,
                "container_id": engine_inspection["Id"],
                "started_at": engine_state["StartedAt"],
                "state": "RUNNING",
                "health": "HEALTHY",
                "image_id": engine_image["image_id"],
                "repo_digest": engine_image["repo_digest"],
                "socket_volume": socket_mount["source"],
                "data_volume": data_mount["source"],
                "published_ports": observed_ports,
                "inner_daemon": inner,
            }
        )
    return {
        "schema_version": SCHEMA_VERSION,
        "candidate_sha": manifest["candidate_sha"],
        "provision_manifest_sha256": canonical_sha256(manifest),
        "worker_ordinal": worker_ordinal,
        "host": identity,
        "agent_image": agent_image,
        "engine_image": engine_image,
        "agent_count": 10,
        "engine_count": 10,
        "agents": agents,
        "engines": engines,
    }


def sha256_values(values: Sequence[str]) -> str:
    digest = hashlib.sha256()
    for value in sorted(values):
        digest.update(value.encode())
        digest.update(b"\n")
    return digest.hexdigest()


def aggregate(input_dir: pathlib.Path, manifest: dict[str, Any]) -> dict[str, Any]:
    manifest = validate_manifest(manifest)
    expected_names = {
        "control-plane.json",
        "postgres.json",
        "runner.json",
        *(f"worker-{ordinal:02d}.json" for ordinal in range(10)),
    }
    actual_names = {path.name for path in input_dir.iterdir() if path.is_file()}
    if actual_names != expected_names:
        raise RuntimeEvidenceError(
            "runtime evidence must contain the exact 13-host file set"
        )
    candidate_sha = manifest["candidate_sha"]
    manifest_sha = canonical_sha256(manifest)
    control_plane = load_json(input_dir / "control-plane.json")
    postgres = load_json(input_dir / "postgres.json")
    expected_cp_configuration = manifest["control_plane"]["configuration"]
    expected_pg_configuration = manifest["postgres"]["configuration"]
    expected_cp_configuration_sha = canonical_sha256(expected_cp_configuration)
    expected_pg_configuration_sha = canonical_sha256(expected_pg_configuration)
    if (
        not isinstance(control_plane, dict)
        or control_plane.get("schema_version") != SCHEMA_VERSION
        or control_plane.get("candidate_sha") != candidate_sha
        or control_plane.get("provision_manifest_sha256") != manifest_sha
        or control_plane.get("image", {}).get("repo_digest")
        != manifest["control_plane"]["image"]
        or control_plane.get("container", {}).get("state") != "RUNNING"
        or control_plane.get("configuration", {}).get("effective_sha256")
        != control_plane.get("configuration", {}).get("provisioned_sha256")
        or control_plane.get("configuration", {}).get("provisioned_sha256")
        != expected_cp_configuration_sha
        or control_plane.get("configuration", {}).get("non_sensitive")
        != expected_cp_configuration
        or not isinstance(control_plane.get("container", {}).get("container_id"), str)
        or not control_plane["container"]["container_id"]
        or not isinstance(control_plane.get("container", {}).get("container_name"), str)
        or not control_plane["container"]["container_name"]
        or not isinstance(control_plane.get("container", {}).get("started_at"), str)
        or not control_plane["container"]["started_at"]
    ):
        raise RuntimeEvidenceError("control-plane runtime evidence is invalid")
    if (
        not isinstance(postgres, dict)
        or postgres.get("schema_version") != SCHEMA_VERSION
        or postgres.get("candidate_sha") != candidate_sha
        or postgres.get("provision_manifest_sha256") != manifest_sha
        or postgres.get("image", {}).get("repo_digest") != manifest["postgres"]["image"]
        or postgres.get("container", {}).get("state") != "RUNNING"
        or postgres.get("container", {}).get("health") != "HEALTHY"
        or postgres.get("configuration", {}).get("effective_sha256")
        != postgres.get("configuration", {}).get("provisioned_sha256")
        or postgres.get("configuration", {}).get("provisioned_sha256")
        != expected_pg_configuration_sha
        or postgres.get("configuration", {}).get("non_sensitive")
        != expected_pg_configuration
        or control_plane.get("database_tls_identity", {}).get("peer_leaf_sha256")
        != postgres.get("server_leaf_sha256")
        or control_plane.get("database_tls_identity", {}).get(
            "root_certificates_sha256"
        )
        != postgres.get("root_certificates_sha256")
        or not SHA256.fullmatch(str(postgres.get("server_leaf_sha256", "")))
        or not isinstance(postgres.get("root_certificates_sha256"), list)
        or not postgres["root_certificates_sha256"]
        or any(
            not isinstance(value, str) or not SHA256.fullmatch(value)
            for value in postgres.get("root_certificates_sha256", [])
        )
        or control_plane.get("database_tls_identity", {}).get("verified_hostname")
        != expected_cp_configuration["environment"]["database"]["host"]
        or control_plane.get("database_tls_identity", {}).get("port")
        != expected_cp_configuration["environment"]["database"]["port"]
        or not isinstance(postgres.get("container", {}).get("container_id"), str)
        or not postgres["container"]["container_id"]
        or not isinstance(postgres.get("container", {}).get("started_at"), str)
        or not postgres["container"]["started_at"]
    ):
        raise RuntimeEvidenceError(
            "PostgreSQL runtime/TLS identity evidence is invalid"
        )
    hosts = [control_plane.get("host"), postgres.get("host")]
    runner = load_json(input_dir / "runner.json")
    if (
        not isinstance(runner, dict)
        or runner.get("schema_version") != SCHEMA_VERSION
        or runner.get("host", {}).get("role") != "runner"
    ):
        raise RuntimeEvidenceError("runner host evidence is invalid")
    hosts.append(runner["host"])
    agents: list[dict[str, Any]] = []
    engines: list[dict[str, Any]] = []
    agent_image_ids: set[str] = set()
    engine_image_ids: set[str] = set()
    for ordinal in range(10):
        document = load_json(input_dir / f"worker-{ordinal:02d}.json")
        if (
            not isinstance(document, dict)
            or document.get("schema_version") != SCHEMA_VERSION
            or document.get("candidate_sha") != candidate_sha
            or document.get("provision_manifest_sha256") != manifest_sha
            or document.get("worker_ordinal") != ordinal
            or document.get("host", {}).get("role") != f"worker-{ordinal:02d}"
            or document.get("agent_image", {}).get("repo_digest")
            != manifest["agent"]["image"]
            or document.get("engine_image", {}).get("repo_digest")
            != manifest["engine"]["image"]
            or document.get("agent_count") != 10
            or document.get("engine_count") != 10
            or not isinstance(document.get("agents"), list)
            or not isinstance(document.get("engines"), list)
            or len(document["agents"]) != 10
            or len(document["engines"]) != 10
        ):
            raise RuntimeEvidenceError(
                f"worker-{ordinal:02d} runtime evidence is invalid"
            )
        hosts.append(document["host"])
        for engine_ordinal, (agent, engine) in enumerate(
            zip(document["agents"], document["engines"])
        ):
            expected_node = f"capacity-node-{ordinal:02d}-{engine_ordinal:02d}"
            expected_instance = f"{expected_node}-{candidate_sha[:12]}"
            expected_socket = (
                f"ojos-capacity-{ordinal:02d}_engine-{engine_ordinal:02d}-socket"
            )
            expected_data = (
                f"ojos-capacity-{ordinal:02d}_engine-{engine_ordinal:02d}-data"
            )
            first_port = manifest["engine"]["first_host_port"] + engine_ordinal * 20
            expected_ports = [
                {
                    "container_port": first_port + service,
                    "protocol": "tcp",
                    "host_ip": manifest["engine"]["host_ip"],
                    "host_port": first_port + service,
                }
                for service in range(manifest["engine"]["services_per_engine"])
            ]
            inner = engine.get("inner_daemon", {}) if isinstance(engine, dict) else {}
            node_identity = agent.get("identity", {}) if isinstance(agent, dict) else {}
            ledger = agent.get("ledger", {}) if isinstance(agent, dict) else {}
            if (
                not isinstance(agent, dict)
                or agent.get("node_id") != expected_node
                or agent.get("instance") != expected_instance
                or agent.get("control_plane_origin") != manifest["control_plane_origin"]
                or agent.get("repo_digest") != manifest["agent"]["image"]
                or agent.get("oci_revision") != candidate_sha
                or agent.get("state") != "RUNNING"
                or not isinstance(agent.get("container_id"), str)
                or not agent["container_id"]
                or not isinstance(agent.get("started_at"), str)
                or not agent["started_at"]
                or not IMAGE_ID.fullmatch(str(agent.get("image_id", "")))
                or agent.get("mount_identity")
                != {
                    "socket_volume": expected_socket,
                    "ledger_source": f"{manifest['agent']['ledger_root']}/{engine_ordinal:02d}",
                    "ca_source": manifest["agent"]["ca_source"],
                }
                or agent.get("transport_ca_certificates_sha256")
                != node_identity.get("server_ca_certificates_sha256")
                or not isinstance(node_identity, dict)
                or node_identity.get("node_id") != expected_node
                or node_identity.get("spiffe_id")
                != f"spiffe://ojos.local/node/{expected_node}"
                or not re.fullmatch(
                    r"[0-9a-f]{1,128}", str(node_identity.get("serial_hex", ""))
                )
                or not SHA256.fullmatch(
                    str(node_identity.get("certificate_sha256", ""))
                )
                or node_identity.get("private_key_present") is not True
                or node_identity.get("private_key_mode") != "0600"
                or not isinstance(node_identity.get("not_after_ms"), int)
                or not isinstance(node_identity.get("renew_after_ms"), int)
                or node_identity["not_after_ms"] <= node_identity["renew_after_ms"]
                or any(
                    not isinstance(values, list)
                    or not values
                    or any(
                        not isinstance(value, str) or not SHA256.fullmatch(value)
                        for value in values
                    )
                    for values in (
                        node_identity.get("node_ca_certificates_sha256"),
                        node_identity.get("server_ca_certificates_sha256"),
                    )
                )
                or not isinstance(ledger, dict)
                or ledger.get("path")
                != f"{manifest['agent']['ledger_root']}/{engine_ordinal:02d}/execution-ledger.sqlite3"
                or ledger.get("format") != "sqlite3"
                or not isinstance(ledger.get("device"), int)
                or not isinstance(ledger.get("inode"), int)
                or ledger.get("inode", 0) <= 0
                or not isinstance(ledger.get("size_bytes"), int)
                or ledger.get("size_bytes", 0) < 512
                or not isinstance(engine, dict)
                or engine.get("engine_ordinal") != engine_ordinal
                or engine.get("node_id") != expected_node
                or engine.get("repo_digest") != manifest["engine"]["image"]
                or engine.get("state") != "RUNNING"
                or engine.get("health") != "HEALTHY"
                or not IMAGE_ID.fullmatch(str(engine.get("image_id", "")))
                or engine.get("socket_volume") != expected_socket
                or engine.get("data_volume") != expected_data
                or engine.get("published_ports") != expected_ports
                or not isinstance(engine.get("container_id"), str)
                or not engine["container_id"]
                or not isinstance(engine.get("started_at"), str)
                or not engine["started_at"]
                or not isinstance(inner, dict)
                or not DAEMON_ID.fullmatch(str(inner.get("daemon_id", "")))
                or inner.get("docker_root_dir") != "/var/lib/docker"
                or inner.get("storage_driver") != "overlay2"
                or inner.get("os_type") != "linux"
                or inner.get("architecture") not in ("x86_64", "amd64")
                or not isinstance(inner.get("server_version"), str)
                or not inner["server_version"]
                or inner.get("containers") != 20
                or inner.get("containers_running") != 20
            ):
                raise RuntimeEvidenceError(
                    f"Node/Engine {expected_node} evidence is invalid"
                )
            agents.append(agent)
            engines.append(engine)
            agent_image_ids.add(agent["image_id"])
            engine_image_ids.add(engine["image_id"])
    if len(hosts) != 13 or any(not isinstance(host, dict) for host in hosts):
        raise RuntimeEvidenceError("runtime evidence does not cover 13 hosts")
    roles = [host.get("role") for host in hosts]
    machine_hashes = [host.get("machine_id_sha256") for host in hosts]
    boot_ids = [host.get("boot_id") for host in hosts]
    daemon_ids = [engine.get("inner_daemon", {}).get("daemon_id") for engine in engines]
    certificate_ids = [agent["identity"]["certificate_sha256"] for agent in agents]
    serials = [agent["identity"]["serial_hex"] for agent in agents]
    spiffe_ids = [agent["identity"]["spiffe_id"] for agent in agents]
    ledger_ids = [
        f"{agent['node_id']}\0{agent['ledger']['device']}\0{agent['ledger']['inode']}"
        for agent in agents
    ]
    if (
        len(set(roles)) != 13
        or any(
            not isinstance(value, str) or not SHA256.fullmatch(value)
            for value in machine_hashes
        )
        or len(set(machine_hashes)) != 13
        or any(
            not isinstance(value, str) or not BOOT_ID.fullmatch(value)
            for value in boot_ids
        )
        or len(agents) != 100
        or len(engines) != 100
        or len({agent["node_id"] for agent in agents}) != 100
        or len({agent["container_id"] for agent in agents}) != 100
        or len(set(certificate_ids)) != 100
        or len(set(serials)) != 100
        or len(set(spiffe_ids)) != 100
        or len(set(ledger_ids)) != 100
        or len({engine["container_id"] for engine in engines}) != 100
        or len({engine["socket_volume"] for engine in engines}) != 100
        or len({engine["data_volume"] for engine in engines}) != 100
        or any(not isinstance(value, str) for value in daemon_ids)
        or len(set(daemon_ids)) != 100
        or len(agent_image_ids) != 1
        or len(engine_image_ids) != 1
    ):
        raise RuntimeEvidenceError(
            "runtime host/Agent/Engine identity set is incomplete or duplicated"
        )
    return {
        "schema_version": SCHEMA_VERSION,
        "candidate_sha": candidate_sha,
        "provision_manifest_sha256": manifest_sha,
        "host_count": 13,
        "host_identity_sha256": sha256_values(
            [
                f"{host['role']}\0{host['machine_id_sha256']}\0{host['boot_id']}"
                for host in hosts
            ]
        ),
        "hosts": sorted(hosts, key=lambda host: host["role"]),
        "control_plane": control_plane,
        "postgres": postgres,
        "restart_identity": {
            "container_id": control_plane["container"]["container_id"],
            "container_name": control_plane["container"]["container_name"],
            "started_at": control_plane["container"]["started_at"],
            "image_id": control_plane["image"]["image_id"],
            "repo_digest": control_plane["image"]["repo_digest"],
        },
        "agents": {
            "count": 100,
            "running": 100,
            "control_plane_origin": manifest["control_plane_origin"],
            "image": {
                "reference": manifest["agent"]["image"],
                "repo_digest": manifest["agent"]["image"],
                "image_ids": sorted(agent_image_ids),
                "oci_revision": candidate_sha,
            },
            "node_ids_sha256": sha256_values([agent["node_id"] for agent in agents]),
            "container_ids_sha256": sha256_values(
                [agent["container_id"] for agent in agents]
            ),
            "started_at_sha256": sha256_values(
                [f"{agent['node_id']}\0{agent['started_at']}" for agent in agents]
            ),
            "spiffe_ids_sha256": sha256_values(spiffe_ids),
            "certificate_fingerprints_sha256": sha256_values(certificate_ids),
            "ledger_identities_sha256": sha256_values(ledger_ids),
            "independent_mtls_identities": 100,
            "independent_sqlite_ledgers": 100,
        },
        "engines": {
            "count": 100,
            "running": 100,
            "healthy": 100,
            "inner_daemon_count": 100,
            "container_count": 2_000,
            "image": {
                "reference": manifest["engine"]["image"],
                "repo_digest": manifest["engine"]["image"],
                "image_ids": sorted(engine_image_ids),
            },
            "outer_container_ids_sha256": sha256_values(
                [engine["container_id"] for engine in engines]
            ),
            "inner_daemon_ids_sha256": sha256_values(daemon_ids),
            "socket_volumes_sha256": sha256_values(
                [engine["socket_volume"] for engine in engines]
            ),
            "data_volumes_sha256": sha256_values(
                [engine["data_volume"] for engine in engines]
            ),
        },
    }


def atomic_write(path: pathlib.Path, value: Any) -> None:
    content = canonical_bytes(value) + b"\n"
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = pathlib.Path(name)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        if hasattr(os, "O_DIRECTORY"):
            directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
    finally:
        temporary.unlink(missing_ok=True)


def add_manifest_argument(command: argparse.ArgumentParser) -> None:
    command.add_argument("--expected-manifest", type=pathlib.Path, required=True)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    commands = result.add_subparsers(dest="command", required=True)
    manifest = commands.add_parser("manifest")
    manifest.add_argument("--candidate-sha", required=True)
    manifest.add_argument("--control-plane-image", required=True)
    manifest.add_argument("--postgres-image", required=True)
    manifest.add_argument("--agent-image", required=True)
    manifest.add_argument("--engine-image", required=True)
    manifest.add_argument("--control-plane-origin", required=True)
    manifest.add_argument("--control-plane-listen-address", required=True)
    manifest.add_argument("--database-listen-address", required=True)
    manifest.add_argument("--postgres-database", required=True)
    manifest.add_argument("--postgres-user", required=True)
    manifest.add_argument("--control-plane-env-file", type=pathlib.Path, required=True)
    manifest.add_argument("--output", type=pathlib.Path, required=True)
    for name in ("control-plane", "postgres", "agents"):
        command = commands.add_parser(name)
        command.add_argument("--compose-file", type=pathlib.Path, required=True)
        command.add_argument("--project-directory", type=pathlib.Path, required=True)
        add_manifest_argument(command)
        command.add_argument("--output", type=pathlib.Path, required=True)
        if name == "agents":
            command.add_argument("--worker-ordinal", type=int, required=True)
    host = commands.add_parser("host")
    host.add_argument("--role", choices=("runner",), required=True)
    host.add_argument("--output", type=pathlib.Path, required=True)
    aggregate_command = commands.add_parser("aggregate")
    aggregate_command.add_argument("--input-dir", type=pathlib.Path, required=True)
    add_manifest_argument(aggregate_command)
    aggregate_command.add_argument("--output", type=pathlib.Path, required=True)
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "manifest":
            evidence = generate_manifest(
                candidate_sha=args.candidate_sha,
                control_plane_image=args.control_plane_image,
                postgres_image=args.postgres_image,
                agent_image=args.agent_image,
                engine_image=args.engine_image,
                control_plane_origin=args.control_plane_origin,
                control_plane_listen_address=args.control_plane_listen_address,
                database_listen_address=args.database_listen_address,
                postgres_database=args.postgres_database,
                postgres_user=args.postgres_user,
                control_plane_env_file=args.control_plane_env_file,
            )
        elif args.command == "host":
            evidence = {
                "schema_version": SCHEMA_VERSION,
                "host": host_identity(args.role),
            }
        else:
            manifest = validate_manifest(load_json(args.expected_manifest))
            client = DockerClient()
            if args.command == "aggregate":
                evidence = aggregate(args.input_dir, manifest)
            else:
                role = (
                    "control-plane"
                    if args.command == "control-plane"
                    else "postgres"
                    if args.command == "postgres"
                    else f"worker-{args.worker_ordinal:02d}"
                )
                identity = host_identity(role)
                if args.command == "control-plane":
                    evidence = collect_control_plane(
                        client,
                        args.compose_file,
                        args.project_directory,
                        manifest,
                        identity,
                    )
                elif args.command == "postgres":
                    evidence = collect_postgres(
                        client,
                        args.compose_file,
                        args.project_directory,
                        manifest,
                        identity,
                    )
                else:
                    evidence = collect_agents(
                        client,
                        args.compose_file,
                        args.project_directory,
                        manifest,
                        args.worker_ordinal,
                        identity,
                    )
        atomic_write(args.output, evidence)
        print(json.dumps({"status": "ok", "output": str(args.output)}, sort_keys=True))
        return 0
    except (RuntimeEvidenceError, OSError, ValueError) as error:
        print(f"capacity runtime evidence failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
