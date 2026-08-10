#!/usr/bin/env python3
"""Service Contract v2 cross-machine equivalence harness.

There are deliberately three different gates:

* ``validate`` is a Docker-free repository/contract gate.
* ``live`` creates two real, independent Docker-in-Docker Engines and runs the
  cross-Engine network and binding flow.  It fails (never skips) when Docker or
  a Linux daemon is unavailable.
* ``live --full-components`` runs one uninterrupted production-equivalent
  chain: actual A services plus Orchestrator, an enrolled mTLS Agent on B, and
  a Judge Worker created by Store/Agent.  No protocol fixture, hand-written
  context, direct Worker ``docker run``, or database mutation can satisfy it.

No command is executed through a shell.  All process arguments are explicit.
"""

from __future__ import annotations

import argparse
import copy
import dataclasses
import hashlib
import ipaddress
import json
import os
import platform
import re
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence


SCHEMA_VERSION = 1
SAFE_RUN_PREFIX = "ojos-cross-v2-"
WORKLOAD_TOKEN = "fixture.deployment.jwt.generation-1"
CONTROL_TOKEN = "cross-machine-fixture-control"
PRIVATE_PORTS = {
    "postgresql": 5432,
    "redis": 6379,
    "minio": 9000,
    "judge-api-direct": 8082,
}
MANAGEMENT_PORTS = {"control-plane": 8090, "oci-registry": 5000}
MAX_CAPTURED_COMMAND_OUTPUT_BYTES = 8 * 1024 * 1024
MAX_FAILURE_LOG_ENTRIES = 64
MAX_DIAGNOSTIC_ERRORS = 32
MAX_DIAGNOSTIC_ERROR_CHARS = 2_000
MAX_FAILURE_LOG_CHARS = 8_000
DIND_CONTAINER_REMOVE_TIMEOUT_SECONDS = 180
DIND_CONTAINER_REMOVE_RETRY_TIMEOUT_SECONDS = 120
DIND_VOLUME_REMOVE_TIMEOUT_SECONDS = 180
DIND_VOLUME_REMOVE_RETRY_TIMEOUT_SECONDS = 120
CLEANUP_RECONCILE_INSPECT_TIMEOUT_SECONDS = 30
DEFAULT_DIND_IMAGE = (
    "docker:29-dind@sha256:"
    "e8faad5a8dc5279dff929afc5449f2791736912fff9f99351d742db2fad01b4c"
)
CONTROL_PLANE_HEALTHCHECK_URL = (
    "https://127.0.0.1:8090/api/v1/healthz/ready"
)
CONTROL_PLANE_HEALTHCHECK_CA_CERT = "/opt/ojos-pki/ca.pem"


class GateError(RuntimeError):
    pass


def bounded_diagnostic_error(operation: str, error: BaseException | str) -> dict[str, Any]:
    """Return a small, deterministic diagnostic record safe for evidence files."""

    detail = str(error)
    if len(detail) > MAX_DIAGNOSTIC_ERROR_CHARS:
        prefix = f"[truncated {len(detail) - MAX_DIAGNOSTIC_ERROR_CHARS} chars] "
        detail = prefix + detail[-(MAX_DIAGNOSTIC_ERROR_CHARS - len(prefix)) :]
    return {"operation": operation, "error": detail}


def append_bounded_diagnostic_error(
    errors: list[dict[str, Any]], operation: str, error: BaseException | str
) -> None:
    """Append an error while keeping the complete collection strictly bounded."""

    if len(errors) < MAX_DIAGNOSTIC_ERRORS - 1:
        errors.append(bounded_diagnostic_error(operation, error))
        return
    if len(errors) == MAX_DIAGNOSTIC_ERRORS - 1:
        errors.append(
            {
                "operation": "diagnostic-error-limit",
                "error": "additional diagnostic errors omitted",
                "omitted": 1,
            }
        )
        return
    errors[-1]["omitted"] = int(errors[-1].get("omitted", 1)) + 1


def canonical_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


PROJECTION_ROUTE_FIELDS = (
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
PROJECTION_GRANT_FIELDS = (
    "binding_id",
    "requirement_name",
    "consumer_deployment_id",
    "consumer_service_id",
    "consumer_node_id",
    "credential_generation",
    "api_id",
    "permission",
)


def effective_projection_sha256(routes: Any, grants: Any) -> str:
    """Reproduce the provider's canonical routes/grants SHA-256 contract.

    This intentionally does not use ``canonical_json``: Go's projection wire
    contract preserves its declared struct-field order and only sorts each
    collection by ``binding_id``.
    """

    def normalize(items: Any, fields: tuple[str, ...], kind: str) -> list[dict[str, Any]]:
        if not isinstance(items, list):
            raise GateError(f"effective projection {kind} must be an array")
        normalized: list[dict[str, Any]] = []
        binding_ids: set[str] = set()
        for index, item in enumerate(items):
            if not isinstance(item, Mapping):
                raise GateError(f"effective projection {kind} item {index} is not an object")
            missing = [field for field in fields if field not in item]
            unknown = sorted(set(item) - set(fields))
            binding_id = item.get("binding_id")
            if missing or unknown or not isinstance(binding_id, str) or not binding_id:
                raise GateError(
                    f"effective projection {kind} item {index} is non-canonical: "
                    f"missing={missing}, unknown={unknown}"
                )
            if binding_id in binding_ids:
                raise GateError(f"effective projection {kind} has duplicate binding_id {binding_id}")
            binding_ids.add(binding_id)
            normalized.append({field: item[field] for field in fields})
        normalized.sort(key=lambda item: item["binding_id"])
        return normalized

    payload = {
        "routes": normalize(routes, PROJECTION_ROUTE_FIELDS, "routes"),
        "grants": normalize(grants, PROJECTION_GRANT_FIELDS, "grants"),
    }
    encoded = json.dumps(
        payload, ensure_ascii=False, separators=(",", ":"), sort_keys=False
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def atomic_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, path)


@dataclasses.dataclass
class Completed:
    argv: list[str]
    returncode: int
    stdout: str
    stderr: str


class Runner:
    def run(
        self,
        argv: Sequence[str | Path],
        *,
        cwd: Path | None = None,
        env: Mapping[str, str] | None = None,
        input_data: bytes | str | None = None,
        timeout: float = 120,
        check: bool = True,
    ) -> Completed:
        normalized = [str(item) for item in argv]
        if not normalized or any("\n" in item or "\r" in item for item in normalized):
            raise GateError("command arguments must be non-empty single-line values")
        process_env = os.environ.copy()
        if env:
            process_env.update({str(key): str(value) for key, value in env.items()})
        stdin_payload = (
            input_data.encode("utf-8") if isinstance(input_data, str) else input_data
        )
        # PIPE-backed capture can deadlock after a timeout when Docker (or a
        # credential helper) leaves a descendant holding the inherited pipe
        # handles.  Regular temporary files let subprocess.run wait only for
        # the process it started, and also keep large image-build output off
        # the Python heap.
        with tempfile.TemporaryFile() as stdout_file, tempfile.TemporaryFile() as stderr_file:
            try:
                result = subprocess.run(
                    normalized,
                    cwd=str(cwd) if cwd else None,
                    env=process_env,
                    stdin=subprocess.DEVNULL if stdin_payload is None else None,
                    input=stdin_payload,
                    stdout=stdout_file,
                    stderr=stderr_file,
                    timeout=timeout,
                    shell=False,
                    check=False,
                )
            except subprocess.TimeoutExpired as exc:
                stdout = read_bounded_command_output(stdout_file)
                stderr = read_bounded_command_output(stderr_file)
                detail = (stderr or stdout).strip()[-4000:]
                suffix = f": {detail}" if detail else ""
                raise GateError(
                    f"command timed out after {timeout:g} seconds: {normalized!r}{suffix}"
                ) from exc
            except OSError as exc:
                raise GateError(f"command failed to execute: {normalized!r}: {exc}") from exc
            stdout = read_bounded_command_output(stdout_file)
            stderr = read_bounded_command_output(stderr_file)
        completed = Completed(normalized, result.returncode, stdout, stderr)
        if check and result.returncode != 0:
            # subprocess output is redirected to temporary files, therefore
            # CompletedProcess.stdout/stderr are always None. Report the
            # bounded strings we just read instead of masking the real command
            # failure with an AttributeError.
            detail = (stderr or stdout).strip()[-4000:]
            raise GateError(f"command exited {result.returncode}: {normalized!r}: {detail}")
        return completed


def read_bounded_command_output(stream: Any) -> str:
    stream.flush()
    stream.seek(0, os.SEEK_END)
    length = stream.tell()
    offset = max(0, length - MAX_CAPTURED_COMMAND_OUTPUT_BYTES)
    stream.seek(offset)
    raw = stream.read(MAX_CAPTURED_COMMAND_OUTPUT_BYTES)
    text = raw.decode("utf-8", errors="replace")
    if offset:
        return f"[truncated {offset} bytes]\n{text}"
    return text


class Docker:
    def __init__(self, runner: Runner, host: str | None = None) -> None:
        self.runner = runner
        self.host = host

    def command(
        self,
        *args: str | Path,
        input_data: bytes | str | None = None,
        timeout: float = 120,
        check: bool = True,
        cwd: Path | None = None,
    ) -> Completed:
        argv: list[str | Path] = ["docker"]
        if self.host:
            argv.extend(["--host", self.host])
        argv.extend(args)
        return self.runner.run(
            argv,
            input_data=input_data,
            timeout=timeout,
            check=check,
            cwd=cwd,
        )

    def json_info(self) -> dict[str, Any]:
        raw = self.command("info", "--format", "{{json .}}", timeout=30).stdout
        return json.loads(raw)


@dataclasses.dataclass(frozen=True)
class Requirement:
    name: str
    api_id: str
    version: str
    timeout_ms: int


@dataclasses.dataclass(frozen=True)
class Provider:
    service_id: str
    api_id: str
    version: str
    path: str
    permission: str


def parse_semver(value: str) -> tuple[int, int, int]:
    match = re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:[-+].*)?", value)
    if not match:
        raise GateError(f"invalid SemVer: {value!r}")
    return tuple(int(item) for item in match.groups())  # type: ignore[return-value]


def semver_satisfies(version: str, constraint: str) -> bool:
    candidate = parse_semver(version)
    tokens = constraint.split()
    if not tokens:
        return False
    for token in tokens:
        match = re.fullmatch(r"(>=|<=|>|<|=)?(.+)", token)
        if not match:
            return False
        operator = match.group(1) or "="
        expected = parse_semver(match.group(2))
        if operator == ">=" and not candidate >= expected:
            return False
        if operator == "<=" and not candidate <= expected:
            return False
        if operator == ">" and not candidate > expected:
            return False
        if operator == "<" and not candidate < expected:
            return False
        if operator == "=" and not candidate == expected:
            return False
    return True


def load_contract(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise GateError(f"invalid contract fixture {path}: {exc}") from exc
    if not isinstance(value, dict) or value.get("schema_version") != 2:
        raise GateError(f"contract fixture is not Release v2: {path}")
    return value


def resolve_binding(
    consumer: Mapping[str, Any],
    providers: Iterable[Mapping[str, Any]],
    link: Mapping[str, Any],
) -> dict[str, Any]:
    service = consumer.get("service", {})
    consumer_id = str(service.get("id", "")).strip()
    requirements = consumer.get("requires", {}).get("apis", [])
    link_bindings = link.get("api_bindings", [])
    if not consumer_id or not isinstance(requirements, list) or not isinstance(link_bindings, list):
        raise GateError("consumer contract or topology link is incomplete")
    if len(link_bindings) != 1:
        raise GateError("fixture link must explicitly select exactly one requirement")
    selected = link_bindings[0]
    requirement_name = str(selected.get("requirement", ""))
    requirement_values = [item for item in requirements if item.get("name") == requirement_name]
    if len(requirement_values) != 1:
        raise GateError("Topology ApiBinding refers to an unknown or duplicate requirement")
    raw_requirement = requirement_values[0]
    requirement = Requirement(
        name=requirement_name,
        api_id=str(raw_requirement.get("id", "")),
        version=str(raw_requirement.get("version", "")),
        timeout_ms=int(raw_requirement.get("timeout_ms", 0)),
    )
    if selected.get("api_id") != requirement.api_id or not (1 <= requirement.timeout_ms <= 300_000):
        raise GateError("Topology ApiBinding does not match the named requirement")
    candidates: list[Provider] = []
    for manifest in providers:
        provider_service = str(manifest.get("service", {}).get("id", ""))
        for api in manifest.get("provides", {}).get("apis", []):
            if api.get("id") != requirement.api_id:
                continue
            if semver_satisfies(str(api.get("version", "")), requirement.version):
                candidates.append(
                    Provider(
                        service_id=provider_service,
                        api_id=requirement.api_id,
                        version=str(api["version"]),
                        path=str(api.get("path", "")),
                        permission=str(api.get("permission", "")),
                    )
                )
    if not candidates:
        raise GateError(f"no compatible provider for requirement {requirement.name}")
    if len(candidates) > 1:
        raise GateError(f"ambiguous provider selection for requirement {requirement.name}")
    provider = candidates[0]
    binding_id = hashlib.sha256(
        canonical_json(
            {
                "consumer": consumer_id,
                "link": link.get("link_id"),
                "provider": provider.service_id,
                "requirement": requirement.name,
                "version": provider.version,
            }
        ).encode()
    ).hexdigest()[:24]
    return {
        "binding_id": "binding-" + binding_id,
        "requirement": requirement.name,
        "api_id": requirement.api_id,
        "api_version": provider.version,
        "consumer_service": consumer_id,
        "provider_service": provider.service_id,
        "provider_path": provider.path,
        "base_path": "/internal/apis/" + requirement.api_id,
        "timeout_ms": requirement.timeout_ms,
        "permission": provider.permission,
        "topology_link_id": link.get("link_id"),
    }


def validate_resource_ref(reference: Mapping[str, Any]) -> None:
    if reference.get("url"):
        raise GateError("managed resource reference contains a URL")
    for field in ("binding", "api_id", "relative_path", "sha256", "size_bytes"):
        if not reference.get(field):
            raise GateError(f"managed resource reference is missing {field}")
    relative = str(reference["relative_path"])
    if not relative.startswith("/") or "://" in relative or "\\" in relative:
        raise GateError("managed resource reference is not relative to its binding")
    decoded_segments = relative.split("/")
    if any(segment in (".", "..") for segment in decoded_segments):
        raise GateError("managed resource reference contains dot segments")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", str(reference["sha256"])):
        raise GateError("managed resource reference has an invalid digest")


def validate_production_resource_ref(reference: Mapping[str, Any]) -> None:
    """Validate the wire shape used by an Agent-managed production Worker.

    The compatibility model still has an ``url`` member for the legacy
    development Compose path.  ``omitempty`` must remove that member entirely
    from managed task JSON: accepting ``"url": ""`` here would allow a future
    regression to silently make a stale location part of the durable protocol.
    """

    if "url" in reference:
        raise GateError("production ApiResourceRef serialized the retired url field")
    allowed = {"binding", "api_id", "relative_path", "sha256", "size_bytes", "content_type"}
    unknown = set(reference) - allowed
    if unknown:
        raise GateError(f"production ApiResourceRef contains unknown fields: {sorted(unknown)}")
    validate_resource_ref(reference)


def validate_service_context(context: Mapping[str, Any]) -> None:
    if context.get("schema_version") != 1:
        raise GateError("service context schema_version must be 1")
    encoded = canonical_json(context).lower()
    for forbidden in ("admin_token", "management_token", "gateway_admin", "auth_admin"):
        if forbidden in encoded:
            raise GateError(f"service context contains forbidden management material: {forbidden}")
    if any(key in context for key in ("token", "access_token", "workload_token")):
        raise GateError("service context must contain a credential file reference, not a token")
    origin = str(context.get("gateway", {}).get("origin", ""))
    if not origin.startswith("https://"):
        raise GateError("managed Gateway origin must use HTTPS")
    credential_file = str(context.get("credential_file", ""))
    if not credential_file.startswith("/"):
        raise GateError("credential_file must be an absolute container path")
    bindings = context.get("bindings")
    if not isinstance(bindings, dict) or not bindings:
        raise GateError("service context has no bindings")
    for name, binding in bindings.items():
        api_id = str(binding.get("api_id", ""))
        if not name or binding.get("base_path") != "/internal/apis/" + api_id:
            raise GateError(f"binding {name!r} has a non-canonical virtual path")
        if not (1 <= int(binding.get("timeout_ms", 0)) <= 300_000):
            raise GateError(f"binding {name!r} has an invalid timeout")


def validate_repository(repo_root: Path) -> dict[str, Any]:
    required = {
        "judge-worker": repo_root / "services/judge-worker/release.yaml",
        "judge-api": repo_root / "services/judge-api/release.yaml",
        "problem-service": repo_root / "services/problem-service/release.yaml",
        "storage-service": repo_root / "services/storage-service/release.yaml",
    }
    checks: dict[str, Any] = {}
    for service, path in required.items():
        if not path.is_file():
            raise GateError(f"missing checked-in Release v2 manifest: {path}")
        text = path.read_text(encoding="utf-8")
        if "schema_version: 2" not in text:
            raise GateError(f"{service} manifest is not Release v2")
        checks[service + "_release_v2"] = True
    worker_release = required["judge-worker"].read_text(encoding="utf-8")
    for forbidden in (
        "OJOS_WORKER_TOKEN",
        "OJOS_SERVICE_TOKEN",
        "OJOS_JUDGE_API_URL",
        "auth_admin_token",
        "gateway_admin_token",
        "privileged:",
        "host_path:",
    ):
        if forbidden in worker_release:
            raise GateError(f"Judge Worker production release exposes forbidden field {forbidden}")
    for required_text in (
        "name: judge_control",
        "id: judge.worker.control",
        "name: storage_get",
        "id: judge-sandbox-v1",
    ):
        if required_text not in worker_release:
            raise GateError(f"Judge Worker release is missing {required_text}")
    checks["worker_has_named_bindings_without_global_tokens"] = True

    problem_release = required["problem-service"].read_text(encoding="utf-8")
    for event_type in ("io.ojos.problem.snapshot.v1", "io.ojos.problem.deleted.v1"):
        if event_type not in problem_release:
            raise GateError(f"Problem release does not publish {event_type}")
    for required_text in ("name: storage_delete", "id: storage.object.delete"):
        if required_text not in problem_release:
            raise GateError(
                f"Problem release does not declare managed orphan GC binding {required_text}"
            )
    for forbidden in (
        "OJOS_PROBLEM_ARTIFACT_GC_JUDGE_DATABASE_URL",
        "OJOS_PROBLEM_ARTIFACT_GC_STORAGE_ENDPOINT",
        "JUDGE_DATABASE_URL",
    ):
        if forbidden in problem_release:
            raise GateError(
                f"Problem production release reintroduced direct GC dependency {forbidden}"
            )
    checks["problem_event_contracts_declared"] = True
    checks["problem_orphan_gc_is_binding_managed"] = True

    worker_common = (repo_root / "services/judge-api/internal/logic/worker_common.go").read_text(
        encoding="utf-8"
    )
    for managed_proof in (
        'Binding:      "storage_get"',
        'source.Url = ""',
        'problemPackage.Url = ""',
        "managed workers require storage-backed artifacts",
    ):
        if managed_proof not in worker_common:
            raise GateError(f"Judge managed task proof is missing: {managed_proof}")
    checks["managed_tasks_are_binding_resource_refs"] = True

    production_compose = repo_root / "deploy/compose/docker-compose.yml"
    compose_text = production_compose.read_text(encoding="utf-8")

    def service_block(service: str) -> str:
        match = re.search(
            rf"(?ms)^  {re.escape(service)}:\s*$\n(.*?)(?=^  [a-z0-9][a-z0-9-]*:\s*$|\Z)",
            compose_text,
        )
        if match is None:
            raise GateError(f"production Compose is missing service {service}")
        return match.group(1)

    shared_runtime_paths = (
        "../../storage/problems",
        "../../storage/submissions",
        "/data/ojos/problems",
        "/data/ojos/submissions",
    )
    for service in ("gateway", "judge-api", "judge-worker"):
        block = service_block(service)
        leaked = [path for path in shared_runtime_paths if path in block]
        if leaked:
            raise GateError(
                f"production Compose {service} still shares Problem/Judge data paths: {leaked}"
            )
    checks["production_runtime_has_no_problem_or_submission_shared_volume"] = True

    fixture_dir = repo_root / "deploy/cross-machine/fixture/contracts"
    for provider_manifest in (
        "judge-control-provider.release.yaml",
        "storage-get-provider.release.yaml",
        "storage-head-provenance-miss-provider.release.yaml",
    ):
        path = fixture_dir / provider_manifest
        if not path.is_file() or "schema_version: 2" not in path.read_text(encoding="utf-8"):
            raise GateError(f"missing full-components provider contract: {path}")
    checks["full_component_provider_contracts"] = True
    head_fault_contract = (
        fixture_dir / "storage-head-provenance-miss-provider.release.yaml"
    ).read_text(encoding="utf-8")
    for required_text in (
        "service_name: storage-head-provenance-miss-provider",
        "id: storage.object.head",
        "version: 1.0.0",
        "methods: [HEAD]",
        "health_path: /health",
    ):
        if required_text not in head_fault_contract:
            raise GateError(
                "storage HEAD provenance-miss provider contract is incomplete: "
                + required_text
            )
    checks["artifact_gc_fault_provider_contract"] = True
    consumer = load_contract(fixture_dir / "echo-consumer.release.json")
    provider = load_contract(fixture_dir / "echo-provider.release.json")
    link = json.loads((fixture_dir / "echo-link.json").read_text(encoding="utf-8"))
    consumer_requirements = {
        str(item.get("name", "")): item
        for item in consumer.get("requires", {}).get("apis", [])
        if isinstance(item, Mapping)
    }
    if (
        set(consumer_requirements) != {"echo", "permission_check"}
        or consumer_requirements["echo"].get("optional") is not True
        or consumer_requirements["permission_check"].get("optional") is not False
    ):
        raise GateError(
            "generic consumer must make only the revocation-test Echo edge optional "
            "while retaining the required permission authority"
        )
    checks["generic_echo_optional_permission_required"] = True
    first = resolve_binding(consumer, [provider], link)
    second = resolve_binding(copy.deepcopy(consumer), [copy.deepcopy(provider)], copy.deepcopy(link))
    if canonical_json(first) != canonical_json(second):
        raise GateError("third-party binding plan is not deterministic")
    checks["third_party_binding"] = first
    api_id = first["api_id"]
    forbidden_roots = [repo_root / "services/orchestrator", repo_root / "services/gateway"]
    specialized: list[str] = []
    for root in forbidden_roots:
        for path in root.rglob("*"):
            # Tests are allowed to name the fixture API in order to prove that
            # the generic parser/resolver accepts it.  This check is about
            # product runtime specialization, so treating test assertions as
            # runtime code makes the gate self-defeating.
            relative_parts = path.relative_to(root).parts
            is_runtime_source = "tests" not in relative_parts
            if (
                is_runtime_source
                and path.is_file()
                and path.suffix in {".rs", ".go", ".json", ".yaml", ".yml"}
            ):
                try:
                    if api_id in path.read_text(encoding="utf-8"):
                        specialized.append(str(path.relative_to(repo_root)))
                except UnicodeDecodeError:
                    continue
    if specialized:
        raise GateError(f"third-party fixture required specialized product code: {specialized}")
    checks["third_party_api_absent_from_product_code"] = True

    full_harness = repo_root / "deploy/cross-machine/full_components.py"
    full_source = full_harness.read_text(encoding="utf-8")
    for required_text in (
        '"/api/auth/bootstrap/admin"',
        '"/api/auth/login"',
        '"jwt_source": "auth-service-login-endpoint"',
        '"jwt_self_signed_by_harness": False',
        '"manual_database_role_seed": False',
        '"secret_or_token_recorded": False',
    ):
        if required_text not in full_source:
            raise GateError(
                f"full-components Auth bootstrap proof is missing {required_text}"
            )
    for pattern in (
        r"(?m)^\s*def\s+_user_jwt\s*\(",
        r"\bhmac\.new\s*\(",
        r'["\']alg["\']\s*:\s*["\']HS256["\']',
        r"\bjwt\.encode\s*\(",
    ):
        if re.search(pattern, full_source):
            raise GateError(
                "full-components harness contains a manual/forged administrator JWT path"
            )
    checks["auth_admin_bootstrap_uses_real_login_jwt"] = True
    return checks


def parse_json_line(output: str) -> dict[str, Any]:
    for line in reversed(output.splitlines()):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            return value
    raise GateError(f"command output did not contain JSON evidence: {output[-2000:]!r}")


def docker_host_port(root: Docker, container: str, port: int) -> int:
    output = root.command("port", container, f"{port}/tcp").stdout.strip().splitlines()
    if not output:
        raise GateError(f"Docker did not publish {container}:{port}")
    match = re.search(r":([0-9]+)$", output[0])
    if not match:
        raise GateError(f"could not parse Docker port output: {output[0]!r}")
    return int(match.group(1))


def container_ip(root: Docker, container: str, network: str) -> str:
    template = "{{(index .NetworkSettings.Networks \"" + network + "\").IPAddress}}"
    value = root.command("inspect", "--format", template, container).stdout.strip()
    try:
        return str(ipaddress.ip_address(value))
    except ValueError as exc:
        raise GateError(f"container {container} has no valid address on {network}: {value!r}") from exc


def required_engine_identity(info: Mapping[str, Any]) -> tuple[str, str, str, str, str, str]:
    fields = ("ID", "Name", "Driver", "ServerVersion", "OSType", "DockerRootDir")
    raw_values = tuple(info.get(field) for field in fields)
    missing = [
        field
        for field, value in zip(fields, raw_values, strict=True)
        if not isinstance(value, str) or not value.strip()
    ]
    if missing:
        raise GateError(f"nested Docker Engine info is incomplete: missing {missing}")
    values = tuple(value.strip() for value in raw_values if isinstance(value, str))
    try:
        parsed_id = uuid.UUID(values[0])
    except ValueError as error:
        raise GateError("nested Docker Engine ID is not a UUID") from error
    if parsed_id.version != 4 or str(parsed_id) != values[0]:
        raise GateError("nested Docker Engine ID is not a canonical UUIDv4")
    if values[4] != "linux":
        raise GateError(f"nested Docker Engine OSType is not linux: {values[4]!r}")
    if values[5] != "/var/lib/docker":
        raise GateError(f"nested Docker Engine root is not /var/lib/docker: {values[5]!r}")
    return values


def wait_engine(engine: Docker, timeout: float = 90) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last = ""
    stable_identity: tuple[str, str, str, str, str, str] | None = None
    stable_observations = 0
    while time.monotonic() < deadline:
        try:
            result = engine.command("info", "--format", "{{json .}}", timeout=5, check=False)
        except GateError as error:
            stable_identity = None
            stable_observations = 0
            last = str(error)
            time.sleep(1)
            continue
        if result.returncode == 0:
            try:
                info = json.loads(result.stdout)
                if not isinstance(info, dict):
                    raise GateError("nested Docker Engine info is not an object")
                identity = required_engine_identity(info)
                if identity == stable_identity:
                    stable_observations += 1
                else:
                    stable_identity = identity
                    stable_observations = 1
                if stable_observations >= 2:
                    return info
                last = "nested Docker Engine identity has not remained stable for two probes"
            except (json.JSONDecodeError, GateError) as error:
                stable_identity = None
                stable_observations = 0
                last = str(error)
        else:
            stable_identity = None
            stable_observations = 0
            last = result.stderr or result.stdout
        time.sleep(1)
    raise GateError(f"nested Docker Engine did not become ready: {last[-1000:]}")


def local_engine_info(root: Docker, container: str) -> dict[str, Any]:
    raw = root.command(
        "exec",
        container,
        "docker",
        "info",
        "--format",
        "{{json .}}",
        timeout=30,
    ).stdout
    try:
        info = json.loads(raw)
    except json.JSONDecodeError as error:
        raise GateError(f"nested Docker Engine {container} returned invalid local info JSON") from error
    if not isinstance(info, dict):
        raise GateError(f"nested Docker Engine {container} local info is not an object")
    required_engine_identity(info)
    return info


def docker_data_volume(root: Docker, container: str) -> str:
    raw = root.command("inspect", "--format", "{{json .Mounts}}", container).stdout
    try:
        mounts = json.loads(raw)
    except json.JSONDecodeError as error:
        raise GateError(f"could not decode Docker mounts for {container}") from error
    matches = [
        mount
        for mount in mounts
        if isinstance(mount, Mapping)
        and mount.get("Type") == "volume"
        and mount.get("Destination") == "/var/lib/docker"
    ]
    if len(matches) != 1:
        raise GateError(f"nested Docker Engine {container} has no unique /var/lib/docker volume")
    name = str(matches[0].get("Name", "")).strip()
    if not name:
        raise GateError(f"nested Docker Engine {container} data volume has no name")
    return name


def require_new_run_scoped_volume(root: Docker, volume: str, run_id: str) -> None:
    if not volume.startswith(f"{SAFE_RUN_PREFIX}{run_id}-"):
        raise GateError(f"nested Engine data volume is not run-scoped: {volume}")
    existing = root.command("volume", "inspect", volume, timeout=30, check=False)
    detail = (existing.stderr or existing.stdout).strip()
    if existing.returncode == 0:
        raise GateError(f"nested Engine data volume already exists: {volume}")
    if "no such volume" not in detail.casefold():
        raise GateError(
            f"could not prove nested Engine data volume is absent: {volume}: {detail}"
        )


def validate_run_scoped_volume(root: Docker, volume: str, run_id: str) -> None:
    if not volume.startswith(f"{SAFE_RUN_PREFIX}{run_id}-"):
        raise GateError(f"nested Engine data volume is not run-scoped: {volume}")
    raw = root.command("volume", "inspect", volume, timeout=30).stdout
    try:
        values = json.loads(raw)
    except json.JSONDecodeError as error:
        raise GateError(f"could not decode nested Engine data volume {volume}") from error
    if not isinstance(values, list) or len(values) != 1 or not isinstance(values[0], Mapping):
        raise GateError(f"nested Engine data volume inspect is non-canonical: {volume}")
    value = values[0]
    labels = value.get("Labels")
    if (
        value.get("Name") != volume
        or not isinstance(labels, Mapping)
        or labels.get("ojos.cross-machine.run") != run_id
    ):
        raise GateError(f"nested Engine data volume is not owned by this run: {volume}")


def outer_container_image(root: Docker, container: str) -> tuple[str, list[str]]:
    config_id = root.command(
        "inspect", "--format", "{{.Image}}", container
    ).stdout.strip()
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", config_id):
        raise GateError(
            f"nested Docker Engine {container} has no canonical image config ID"
        )
    raw = root.command(
        "image", "inspect", "--format", "{{json .RepoDigests}}", config_id
    ).stdout
    try:
        repo_digests = json.loads(raw)
    except json.JSONDecodeError as error:
        raise GateError(
            f"could not decode image RepoDigests for nested Docker Engine {container}"
        ) from error
    if not isinstance(repo_digests, list):
        raise GateError(
            f"nested Docker Engine {container} image RepoDigests is not a list"
        )
    normalized = sorted(
        {
            item.strip()
            for item in repo_digests
            if isinstance(item, str)
            and re.fullmatch(r"[^\s@]+@sha256:[0-9a-f]{64}", item.strip())
        }
    )
    if not normalized or len(normalized) != len(repo_digests):
        raise GateError(
            f"nested Docker Engine {container} image has no canonical RepoDigest proof"
        )
    return config_id, normalized


def required_local_docker_endpoint(value: Any) -> str:
    endpoint = value if isinstance(value, str) else ""
    match = re.fullmatch(r"tcp://127\.0\.0\.1:([0-9]{1,5})", endpoint)
    if match is None or not 1 <= int(match.group(1)) <= 65535:
        raise GateError(f"nested Docker Engine host endpoint is invalid: {endpoint!r}")
    return endpoint


def required_outer_container_id(value: Any) -> str:
    container_id = value if isinstance(value, str) else ""
    if not re.fullmatch(r"[0-9a-f]{64}", container_id):
        raise GateError("nested Docker Engine outer container ID is not canonical")
    return container_id


def required_image_config_id(value: Any) -> str:
    config_id = value if isinstance(value, str) else ""
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", config_id):
        raise GateError("nested Docker Engine image config ID is not canonical")
    return config_id


def required_repo_digests(value: Any) -> list[str]:
    if not isinstance(value, list) or not value:
        raise GateError("nested Docker Engine image RepoDigest proof is missing")
    digests = [item for item in value if isinstance(item, str)]
    if len(digests) != len(value) or digests != sorted(set(digests)) or any(
        re.fullmatch(r"[^\s@]+@sha256:[0-9a-f]{64}", item) is None
        for item in digests
    ):
        raise GateError("nested Docker Engine image RepoDigest proof is non-canonical")
    return digests


def no_proxy_opener(cafile: Path | None = None) -> urllib.request.OpenerDirector:
    handlers: list[Any] = [urllib.request.ProxyHandler({})]
    if cafile:
        handlers.append(urllib.request.HTTPSHandler(context=ssl.create_default_context(cafile=str(cafile))))
    return urllib.request.build_opener(*handlers)


def wait_url(url: str, *, cafile: Path | None = None, timeout: float = 60) -> None:
    opener = no_proxy_opener(cafile)
    deadline = time.monotonic() + timeout
    last: Exception | None = None
    while time.monotonic() < deadline:
        try:
            with opener.open(url, timeout=3) as response:
                if response.status < 500:
                    return
        except Exception as exc:  # noqa: PERF203
            last = exc
        time.sleep(0.5)
    raise GateError(f"URL did not become ready: {url}: {last}")


def fixture_control(
    url: str, cafile: Path, method: str = "GET", value: Any | None = None
) -> Any:
    body = canonical_json(value).encode() if value is not None else None
    request = urllib.request.Request(
        url,
        data=body,
        method=method,
        headers={
            "x-fixture-control": CONTROL_TOKEN,
            "content-type": "application/json",
        },
    )
    with no_proxy_opener(cafile).open(request, timeout=30) as response:
        return json.loads(response.read())


def safe_name(run_id: str, suffix: str) -> str:
    value = f"{SAFE_RUN_PREFIX}{run_id}-{suffix}"
    if not re.fullmatch(r"ojos-cross-v2-[a-z0-9-]{1,80}", value):
        raise GateError(f"unsafe Docker resource name: {value!r}")
    return value


def default_dind_storage_driver(host_system: str) -> str:
    # overlay-on-overlay is prone to uninterruptible nested-daemon stalls on
    # Docker Desktop.  Linux CI keeps the faster native driver; Desktop uses
    # the slower but deterministic vfs driver for this correctness gate.
    return "vfs" if host_system.casefold() == "windows" else "overlay2"


def configured_dind_storage_driver() -> str:
    driver = os.environ.get("OJOS_CROSS_MACHINE_DIND_STORAGE_DRIVER", "").strip()
    if not driver:
        driver = default_dind_storage_driver(platform.system())
    if driver not in {"overlay2", "vfs"}:
        raise GateError(
            "OJOS_CROSS_MACHINE_DIND_STORAGE_DRIVER must be exactly overlay2 or vfs"
        )
    return driver


class LiveGate:
    def __init__(self, repo_root: Path, evidence_path: Path, full_components: bool) -> None:
        self.repo_root = repo_root
        self.evidence_path = evidence_path
        self.full_components = full_components
        self.runner = Runner()
        self.root = Docker(self.runner)
        self.run_id = uuid.uuid4().hex[:10]
        self.outer_network = safe_name(self.run_id, "outer")
        self.a_name = safe_name(self.run_id, "engine-a")
        self.b_name = safe_name(self.run_id, "engine-b")
        self.a_data_volume = safe_name(self.run_id, "engine-a-data")
        self.b_data_volume = safe_name(self.run_id, "engine-b-data")
        self.a: Docker | None = None
        self.b: Docker | None = None
        self.a_ip = ""
        self.dind_storage_driver = configured_dind_storage_driver()
        self.dind_data_volumes_created: list[str] = []
        self.root_images_created: list[str] = []
        self.evidence: dict[str, Any] = {
            "schema_version": SCHEMA_VERSION,
            "gate": "cross-machine-service-contract-v2",
            "status": "RUNNING",
            "mode": "full-components" if full_components else "contract-live",
            "run_id": self.run_id,
            "started_at_unix": int(time.time()),
            "failure_log_errors": [],
            "cleanup_errors": [],
            "cleanup_completed": False,
        }

    def checkpoint(self, phase: str) -> None:
        if not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,79}", phase):
            raise GateError(f"invalid live-gate phase: {phase!r}")
        checkpoint_at = int(time.time())
        self.evidence["phase"] = phase
        self.evidence["checkpoint_at_unix"] = checkpoint_at
        atomic_json(self.evidence_path, self.evidence)
        print(
            canonical_json(
                {
                    "gate": self.evidence["gate"],
                    "mode": self.evidence["mode"],
                    "phase": phase,
                    "run_id": self.run_id,
                    "status": "RUNNING",
                }
            ),
            flush=True,
        )

    def ensure_live_prerequisites(self) -> dict[str, Any]:
        if shutil.which("docker") is None:
            raise GateError("live gate requires Docker; this is a failure, not a skip")
        info = self.root.json_info()
        if info.get("OSType") != "linux":
            raise GateError("live gate requires a Linux Docker daemon; this is a failure, not a skip")
        if not info.get("IPv4Forwarding"):
            raise GateError("live gate requires Docker IPv4 forwarding")
        return {
            "host_os": platform.platform(),
            "daemon_os": info.get("OperatingSystem"),
            "daemon_id": info.get("ID"),
            "docker_server_version": info.get("ServerVersion"),
        }

    def run(self) -> dict[str, Any]:
        original_error: Exception | None = None
        try:
            self.checkpoint("prerequisites")
            self.evidence["prerequisites"] = self.ensure_live_prerequisites()
            self.checkpoint("repository-contract")
            self.evidence["repository_contract"] = validate_repository(self.repo_root)
            self.checkpoint("nested-engines")
            self._start_engines()
            self.checkpoint("scenario")
            with tempfile.TemporaryDirectory(prefix="ojos-cross-machine-") as temporary:
                self._run_scenario(Path(temporary))
            # Validate the completed scenario before destroying its runtime,
            # but do not publish a terminal PASSED document until cleanup has
            # also succeeded.
            self.evidence["status"] = "PASSED"
            verify_evidence(
                self.evidence,
                require_full=self.full_components,
                require_cleanup=False,
            )
        except Exception as exc:
            original_error = exc
            # Failure diagnostics are best-effort. A wedged nested daemon must
            # never replace the scenario failure or prevent terminal evidence.
            try:
                self._collect_failure_logs()
            except Exception as diagnostic_error:
                errors = list(self.evidence.get("failure_log_errors", []))
                append_bounded_diagnostic_error(
                    errors, "collect-failure-logs", diagnostic_error
                )
                self.evidence["failure_log_errors"] = errors

        try:
            cleanup_errors = self.cleanup()
            self.evidence["cleanup_errors"] = cleanup_errors
        except Exception as cleanup_error:
            # cleanup() is intentionally non-throwing, but preserve correctness
            # if a future implementation or a test double violates that rule.
            cleanup_errors = list(self.evidence.get("cleanup_errors", []))
            append_bounded_diagnostic_error(cleanup_errors, "cleanup", cleanup_error)
            self.evidence["cleanup_errors"] = cleanup_errors

        if original_error is None and self.evidence["cleanup_errors"]:
            original_error = GateError("cleanup failed after the live scenario succeeded")

        self.evidence["completed_at_unix"] = int(time.time())
        self.evidence["cleanup_completed"] = not self.evidence["cleanup_errors"]
        if original_error is not None:
            self.evidence["status"] = "FAILED"
            self.evidence["failure"] = str(original_error)
            atomic_json(self.evidence_path, self.evidence)
            if isinstance(original_error, GateError):
                raise original_error
            raise GateError(str(original_error)) from original_error

        self.evidence["status"] = "PASSED"
        atomic_json(self.evidence_path, self.evidence)
        return self.evidence

    def _start_engines(self) -> None:
        self.root.command("network", "create", self.outer_network)
        dind_image = os.environ.get("OJOS_CROSS_MACHINE_DIND_IMAGE", DEFAULT_DIND_IMAGE)
        if not re.fullmatch(r"[^\s@]+@sha256:[0-9a-f]{64}", dind_image):
            raise GateError("OJOS_CROSS_MACHINE_DIND_IMAGE must be pinned by sha256 digest")
        for volume in (self.a_data_volume, self.b_data_volume):
            require_new_run_scoped_volume(self.root, volume, self.run_id)
            # Record the exact attempted target before the create request.  If
            # the client loses the response after Docker commits the volume,
            # cleanup still reconciles and removes only this run-scoped name.
            self.dind_data_volumes_created.append(volume)
            created = self.root.command(
                "volume",
                "create",
                "--label",
                "ojos.cross-machine.run=" + self.run_id,
                volume,
            ).stdout.strip()
            if created != volume:
                raise GateError(
                    f"Docker created unexpected nested Engine data volume {created!r}"
                )
            validate_run_scoped_volume(self.root, volume, self.run_id)
        for name, alias, data_volume in (
            (self.a_name, "engine-a", self.a_data_volume),
            (self.b_name, "engine-b", self.b_data_volume),
        ):
            publish = ["--publish", "127.0.0.1::2375"]
            if alias == "engine-a":
                # Docker Desktop does not route its container subnet to the
                # Windows host.  This host-only mapping is for harness control;
                # B workloads still use A's outer-network address directly.
                publish.extend(
                    [
                        "--publish",
                        "127.0.0.1::8443",
                        "--publish",
                        "127.0.0.1::5432",
                        "--publish",
                        "127.0.0.1::6379",
                        "--publish",
                        "127.0.0.1::8090",
                    ]
                )
            self.root.command(
                "run",
                "-d",
                "--name",
                name,
                "--label",
                "ojos.cross-machine.run=" + self.run_id,
                "--privileged",
                "--network",
                self.outer_network,
                "--network-alias",
                alias,
                "--env",
                "DOCKER_TLS_CERTDIR=",
                "--mount",
                f"type=volume,source={data_volume},target=/var/lib/docker",
                *publish,
                dind_image,
                "dockerd",
                "--tls=false",
                "--host=tcp://0.0.0.0:2375",
                "--host=unix:///var/run/docker.sock",
                "--insecure-registry=engine-a:5000",
                "--insecure-registry=127.0.0.1:5000",
                f"--storage-driver={self.dind_storage_driver}",
                timeout=180,
            )
        a_host = required_local_docker_endpoint(
            f"tcp://127.0.0.1:{docker_host_port(self.root, self.a_name, 2375)}"
        )
        b_host = required_local_docker_endpoint(
            f"tcp://127.0.0.1:{docker_host_port(self.root, self.b_name, 2375)}"
        )
        if a_host == b_host:
            raise GateError("the two nested Docker Engines resolved to the same host endpoint")
        self.a, self.b = Docker(self.runner, a_host), Docker(self.runner, b_host)
        a_info, b_info = wait_engine(self.a), wait_engine(self.b)
        a_local_info = local_engine_info(self.root, self.a_name)
        b_local_info = local_engine_info(self.root, self.b_name)
        self.a_ip = container_ip(self.root, self.a_name, self.outer_network)
        b_ip = container_ip(self.root, self.b_name, self.outer_network)
        a_identity = required_engine_identity(a_info)
        b_identity = required_engine_identity(b_info)
        a_local_identity = required_engine_identity(a_local_info)
        b_local_identity = required_engine_identity(b_local_info)
        a_id, b_id = a_identity[0], b_identity[0]
        a_local_id, b_local_id = a_local_identity[0], b_local_identity[0]
        outer_a_id = required_outer_container_id(
            self.root.command(
                "inspect", "--format", "{{.Id}}", self.a_name
            ).stdout.strip()
        )
        outer_b_id = required_outer_container_id(
            self.root.command(
                "inspect", "--format", "{{.Id}}", self.b_name
            ).stdout.strip()
        )
        data_volume_a = docker_data_volume(self.root, self.a_name)
        data_volume_b = docker_data_volume(self.root, self.b_name)
        if data_volume_a != self.a_data_volume or data_volume_b != self.b_data_volume:
            raise GateError("nested Docker Engine data volume does not match its run-scoped root")
        image_config_a, image_repo_digests_a = outer_container_image(
            self.root, self.a_name
        )
        image_config_b, image_repo_digests_b = outer_container_image(
            self.root, self.b_name
        )
        requested_digest = dind_image.rsplit("@", 1)[1]
        if any(
            not any(item.endswith("@" + requested_digest) for item in repo_digests)
            for repo_digests in (image_repo_digests_a, image_repo_digests_b)
        ):
            raise GateError("nested Docker Engine image RepoDigest does not match the pin")
        self.evidence["engine_probe"] = {
            "a": {
                "host_endpoint": a_host,
                "host_engine_id": a_id,
                "local_engine_id": a_local_id,
                "local_identity": {
                    "engine_id": a_local_identity[0],
                    "engine_name": a_local_identity[1],
                    "driver": a_local_identity[2],
                    "server_version": a_local_identity[3],
                    "os_type": a_local_identity[4],
                    "docker_root_dir": a_local_identity[5],
                },
                "outer_container_id": outer_a_id,
                "data_volume": data_volume_a,
                "engine_name": a_identity[1],
                "driver": a_identity[2],
                "server_version": a_identity[3],
                "os_type": a_identity[4],
                "docker_root_dir": a_identity[5],
                "image_config_id": image_config_a,
                "image_repo_digests": image_repo_digests_a,
            },
            "b": {
                "host_endpoint": b_host,
                "host_engine_id": b_id,
                "local_engine_id": b_local_id,
                "local_identity": {
                    "engine_id": b_local_identity[0],
                    "engine_name": b_local_identity[1],
                    "driver": b_local_identity[2],
                    "server_version": b_local_identity[3],
                    "os_type": b_local_identity[4],
                    "docker_root_dir": b_local_identity[5],
                },
                "outer_container_id": outer_b_id,
                "data_volume": data_volume_b,
                "engine_name": b_identity[1],
                "driver": b_identity[2],
                "server_version": b_identity[3],
                "os_type": b_identity[4],
                "docker_root_dir": b_identity[5],
                "image_config_id": image_config_b,
                "image_repo_digests": image_repo_digests_b,
            },
            "dind_image": dind_image,
        }
        atomic_json(self.evidence_path, self.evidence)
        if a_identity != a_local_identity or b_identity != b_local_identity:
            raise GateError(
                "nested Docker host endpoint identity does not match its outer container local socket"
            )
        if a_id == b_id:
            raise GateError("the two nested Docker Engines do not have distinct identities")
        if outer_a_id == outer_b_id:
            raise GateError("the two nested Docker Engines do not have distinct outer containers")
        if data_volume_a == data_volume_b:
            raise GateError("the two nested Docker Engines share /var/lib/docker storage")
        if (
            image_config_a != image_config_b
            or image_repo_digests_a != image_repo_digests_b
        ):
            raise GateError("the two nested Docker Engines did not use the same pinned image")
        for label, info in (("A", a_info), ("B", b_info)):
            if info.get("Driver") != self.dind_storage_driver:
                raise GateError(
                    f"Engine {label} uses storage driver {info.get('Driver')!r}, "
                    f"expected {self.dind_storage_driver!r}"
                )
        marker_a = safe_name(self.run_id, "only-a")
        marker_b = safe_name(self.run_id, "only-b")
        self.a.command("volume", "create", marker_a)
        if self.b.command("volume", "inspect", marker_a, check=False).returncode == 0:
            raise GateError("Engine B can see Engine A's marker volume")
        self.b.command("volume", "create", marker_b)
        if self.a.command("volume", "inspect", marker_b, check=False).returncode == 0:
            raise GateError("Engine A can see Engine B's marker volume")
        self.evidence["engines"] = {
            "a": {
                "engine_id": a_id,
                "local_engine_id": a_local_id,
                "engine_name": a_identity[1],
                "outer_container_id": outer_a_id,
                "outer_ip": self.a_ip,
                "marker_volume": marker_a,
                "storage_driver": a_identity[2],
                "server_version": a_identity[3],
                "os_type": a_identity[4],
                "docker_root_dir": a_identity[5],
                "host_endpoint": a_host,
                "host_endpoint_matches_local_socket": True,
                "data_volume": data_volume_a,
                "image_config_id": image_config_a,
                "image_repo_digests": image_repo_digests_a,
            },
            "b": {
                "engine_id": b_id,
                "local_engine_id": b_local_id,
                "engine_name": b_identity[1],
                "outer_container_id": outer_b_id,
                "outer_ip": b_ip,
                "marker_volume": marker_b,
                "storage_driver": b_identity[2],
                "server_version": b_identity[3],
                "os_type": b_identity[4],
                "docker_root_dir": b_identity[5],
                "host_endpoint": b_host,
                "host_endpoint_matches_local_socket": True,
                "data_volume": data_volume_b,
                "image_config_id": image_config_b,
                "image_repo_digests": image_repo_digests_b,
            },
            "routing_proof": "host TCP endpoint ID matches outer unix socket",
            "storage_roots_distinct": True,
            "isolation_proof": "mutually-invisible marker volumes",
            "dind_image": dind_image,
        }

    def _run_scenario(self, temporary: Path) -> None:
        assert self.a is not None and self.b is not None
        if self.full_components:
            # Keep the expensive gate structurally separate.  The full lane
            # must never fall through to the protocol fixture or its manually
            # materialized context/Worker helpers.
            from full_components import FullComponentsScenario

            FullComponentsScenario(self, temporary).run()
            return
        a_network = safe_name(self.run_id, "a-private")
        b_service_network = safe_name(self.run_id, "b-service")
        b_agent_network = safe_name(self.run_id, "b-agent")
        self.a.command("network", "create", "--subnet", "172.28.0.0/24", a_network)
        self.b.command("network", "create", "--subnet", "172.30.0.0/24", b_service_network)
        self.b.command("network", "create", "--subnet", "172.31.0.0/24", b_agent_network)
        fixture_image = "ojos/cross-machine-fixture:" + self.run_id
        fixture_dir = self.repo_root / "deploy/cross-machine/fixture"
        # Nested daemons intentionally have no dependency on public registry
        # credentials or runner proxy configuration.  Build/pull once through
        # the host daemon and stream exact images into each isolated Engine.
        self.root.command("build", "--tag", fixture_image, fixture_dir, timeout=600)
        self.root_images_created.append(fixture_image)
        self._transfer_image(fixture_image, [self.a, self.b], temporary)
        self._ensure_root_image("registry:2.8.3")
        self._transfer_image("registry:2.8.3", [self.a], temporary)
        self._start_a_private_services(fixture_image, a_network)

        consumer = load_contract(fixture_dir / "contracts/echo-consumer.release.json")
        provider = load_contract(fixture_dir / "contracts/echo-provider.release.json")
        link = json.loads((fixture_dir / "contracts/echo-link.json").read_text(encoding="utf-8"))
        third_party_binding = resolve_binding(consumer, [provider], link)
        routes = [
            {
                "binding_id": "binding-judge-control",
                "base_path": "/internal/apis/judge.worker.control",
                "api_id": "judge.worker.control",
                "kind": "judge",
                "consumer_service": "judge-worker",
            },
            {
                "binding_id": "binding-storage-get",
                "base_path": "/internal/apis/storage.object.get",
                "api_id": "storage.object.get",
                "kind": "storage",
                "consumer_service": "judge-worker",
            },
            {
                **third_party_binding,
                "kind": "proxy",
                "upstream": "http://third-party-provider:8088"
                + third_party_binding["provider_path"],
            },
        ]
        self.a.command(
            "run",
            "-d",
            "--name",
            "third-party-provider",
            "--network",
            a_network,
            fixture_image,
            "provider",
            "--port",
            "8088",
        )
        self.a.command(
            "run",
            "-d",
            "--name",
            "gateway-a",
            "--network",
            a_network,
            "--publish",
            "8443:8443",
            "--env",
            "BINDINGS_JSON=" + canonical_json(routes),
            "--env",
            "WORKLOAD_TOKEN=" + WORKLOAD_TOKEN,
            "--env",
            "GATEWAY_IP=" + self.a_ip,
            fixture_image,
            "gateway",
            "--port",
            "8443",
        )
        ca_file = temporary / "gateway-ca.pem"
        deadline = time.monotonic() + 60
        while True:
            copied = self.a.command(
                "cp",
                "gateway-a:/tmp/ojos-fixture-tls/ca.pem",
                ca_file,
                timeout=10,
                check=False,
            )
            if copied.returncode == 0 and ca_file.is_file():
                break
            if time.monotonic() >= deadline:
                raise GateError("Gateway did not publish its test CA")
            time.sleep(0.5)
        gateway_control_port = docker_host_port(self.root, self.a_name, 8443)
        gateway_outer = f"https://127.0.0.1:{gateway_control_port}"
        wait_url(gateway_outer + "/healthz/ready", cafile=ca_file)

        self._apply_b_egress_policy()
        boundary = self._run_boundary_probe(fixture_image, b_service_network)
        agent = self._run_agent_probe(fixture_image, b_agent_network)
        self.evidence["network_boundary"] = {
            "policy": "B service subnet may reach only A Gateway tcp/8443",
            "gateway_ready": True,
            "denied": boundary["denied"],
            "agent_connectivity": agent["targets"],
        }

        fixture_control(gateway_outer + "/fixture/problem", ca_file, "POST", {})
        fixture_control(gateway_outer + "/fixture/submission", ca_file, "POST", {})
        context_volume, mount_proof = self._materialize_context(
            temporary,
            fixture_image,
            ca_file,
            b_service_network,
            third_party_binding,
        )
        self.evidence["managed_context"] = mount_proof
        worker_output = self._run_fixture_worker(context_volume, fixture_image, b_service_network)
        self.evidence["worker_implementation"] = "protocol fixture (contract live gate)"
        consumer_output = self._run_generic_consumer(
            temporary, fixture_image, ca_file, b_service_network, third_party_binding
        )
        flow = fixture_control(gateway_outer + "/fixture/evidence", ca_file)
        task = flow.get("task") or {}
        validate_resource_ref(task.get("source", {}))
        validate_resource_ref(task.get("problem_package", {}))
        self.evidence["component_flow"] = {
            "gateway_evidence": flow,
            "worker_evidence": worker_output,
            "resource_refs_validated": True,
            "long_poll_prefer": flow.get("claim_prefer"),
        }
        self.evidence["third_party_fixture"] = {
            "binding_plan": third_party_binding,
            "consumer_evidence": consumer_output,
            "specialized_product_code": False,
        }

    def _ensure_root_image(self, image: str) -> None:
        if self.root.command("image", "inspect", image, check=False).returncode != 0:
            self.root.command("pull", image, timeout=600)

    def _transfer_image(self, image: str, engines: Sequence[Docker], root: Path) -> None:
        safe = re.sub(r"[^a-zA-Z0-9_.-]", "_", image)
        archive = root / (safe + ".tar")
        self.root.command("image", "save", "--output", archive, image, timeout=600)
        if not archive.is_file() or archive.stat().st_size == 0:
            raise GateError(f"host daemon did not export image {image}")
        for engine in engines:
            engine.command("image", "load", "--input", archive, timeout=600)

    def _start_a_private_services(self, image: str, network: str) -> None:
        assert self.a is not None
        for service, port in (("a-postgresql", 5432), ("a-redis", 6379)):
            self.a.command(
                "run",
                "-d",
                "--name",
                service,
                "--network",
                network,
                "--publish",
                f"{port}:{port}",
                "--env",
                "SERVICE_NAME=" + service,
                image,
                "service",
                "--port",
                str(port),
            )
        for service, port in (("a-minio", 9000), ("a-judge-direct", 8082)):
            self.a.command(
                "run",
                "-d",
                "--name",
                service,
                "--network",
                network,
                "--publish",
                f"{port}:{port}",
                "--env",
                "SERVICE_NAME=" + service,
                image,
                "service",
                "--port",
                str(port),
            )
        self.a.command(
            "run",
            "-d",
            "--name",
            "a-control-plane",
            "--network",
            network,
            "--publish",
            "8090:8090",
            "--env",
            "SERVICE_NAME=orchestrator-control-plane",
            image,
            "service",
            "--port",
            "8090",
        )
        self.a.command(
            "run",
            "-d",
            "--name",
            "a-registry",
            "--network",
            network,
            "--publish",
            "5000:5000",
            "registry:2.8.3",
            timeout=180,
        )

    def _apply_b_egress_policy(self) -> None:
        source = "172.30.0.0/24"
        self.root.command(
            "exec",
            self.b_name,
            "iptables",
            "-I",
            "DOCKER-USER",
            "1",
            "-s",
            source,
            "-d",
            self.a_ip,
            "-p",
            "tcp",
            "--dport",
            "8443",
            "-j",
            "ACCEPT",
        )
        self.root.command(
            "exec",
            self.b_name,
            "iptables",
            "-I",
            "DOCKER-USER",
            "2",
            "-s",
            source,
            "-d",
            self.a_ip,
            "-j",
            "REJECT",
        )

    def _run_boundary_probe(self, image: str, network: str) -> dict[str, Any]:
        assert self.b is not None
        targets = [
            {"name": name, "host": self.a_ip, "port": port}
            for name, port in {**PRIVATE_PORTS, **MANAGEMENT_PORTS}.items()
        ]
        result = self.b.command(
            "run",
            "--rm",
            "--network",
            network,
            "--ip",
            "172.30.0.10",
            "--env",
            "DENIED_TARGETS_JSON=" + canonical_json(targets),
            image,
            "boundary-probe",
            timeout=60,
        )
        evidence = parse_json_line(result.stdout)
        if {item["name"] for item in evidence.get("denied", [])} != set(PRIVATE_PORTS) | set(
            MANAGEMENT_PORTS
        ):
            raise GateError("boundary probe did not cover every forbidden A endpoint")
        return evidence

    def _run_agent_probe(self, image: str, network: str) -> dict[str, Any]:
        assert self.b is not None
        targets = [
            {"name": "control-plane", "url": f"http://{self.a_ip}:8090/health"},
            {"name": "oci-registry", "url": f"http://{self.a_ip}:5000/v2/"},
        ]
        result = self.b.command(
            "run",
            "--rm",
            "--network",
            network,
            "--ip",
            "172.31.0.10",
            "--env",
            "AGENT_TARGETS_JSON=" + canonical_json(targets),
            image,
            "agent-probe",
            timeout=60,
        )
        evidence = parse_json_line(result.stdout)
        if any(item.get("status") != 200 for item in evidence.get("targets", [])):
            raise GateError("Agent did not reach both the control plane and OCI registry")
        return evidence

    def _context(
        self, deployment: str, service: str, bindings: Mapping[str, Mapping[str, Any]]
    ) -> dict[str, Any]:
        return {
            "schema_version": 1,
            "deployment": {"id": deployment, "service": service, "node": "node-b"},
            "gateway": {"origin": "https://gateway-a:8443", "ca_file": "/run/ojos/service/ca.pem"},
            "bindings": bindings,
            "credential_file": "/run/ojos/service/token",
            "generation": 1,
        }

    def _materialize_volume(
        self,
        root: Path,
        image: str,
        volume: str,
        context: Mapping[str, Any],
        ca_file: Path,
    ) -> str:
        assert self.b is not None
        validate_service_context(context)
        context_root = root / volume
        context_root.mkdir(parents=True, exist_ok=True)
        (context_root / "context.json").write_text(canonical_json(context) + "\n", encoding="utf-8")
        (context_root / "token").write_text(WORKLOAD_TOKEN + "\n", encoding="utf-8")
        shutil.copyfile(ca_file, context_root / "ca.pem")
        (context_root / "Dockerfile").write_text(
            "FROM " + image + "\nCOPY context.json token ca.pem /run/ojos/service/\n",
            encoding="utf-8",
        )
        seed_image = "ojos/cross-machine-context:" + volume
        self.b.command("build", "--tag", seed_image, context_root, timeout=180)
        self.b.command("volume", "create", volume)
        self.b.command(
            "run",
            "--rm",
            "--mount",
            f"type=volume,source={volume},target=/run/ojos/service",
            seed_image,
            "noop",
        )
        return seed_image

    def _materialize_context(
        self,
        root: Path,
        fixture_image: str,
        ca_file: Path,
        network: str,
        third_party: Mapping[str, Any],
    ) -> tuple[str, dict[str, Any]]:
        assert self.b is not None
        bindings = {
            "judge_control": {
                "binding_id": "binding-judge-control",
                "api_id": "judge.worker.control",
                "base_path": "/internal/apis/judge.worker.control",
                "timeout_ms": 35000,
            },
            "storage_get": {
                "binding_id": "binding-storage-get",
                "api_id": "storage.object.get",
                "base_path": "/internal/apis/storage.object.get",
                "timeout_ms": 300000,
            },
        }
        context = self._context("worker-b", "judge-worker", bindings)
        volume = safe_name(self.run_id, "worker-context")
        self._materialize_volume(root, fixture_image, volume, context, ca_file)
        inspect_name = "context-mount-proof"
        self.b.command(
            "create",
            "--name",
            inspect_name,
            "--network",
            network,
            "--mount",
            f"type=volume,source={volume},target=/run/ojos/service,readonly",
            fixture_image,
            "noop",
        )
        inspect = json.loads(self.b.command("inspect", inspect_name).stdout)[0]
        self.b.command("rm", inspect_name)
        mounts = [item for item in inspect["Mounts"] if item.get("Destination") == "/run/ojos/service"]
        if len(mounts) != 1 or mounts[0].get("RW") is not False:
            raise GateError("Agent-equivalent service context mount is not read-only")
        return volume, {
            "validated": True,
            "credential_embedded": False,
            "management_token_present": False,
            "mount_read_only": True,
            "mount_type": mounts[0].get("Type"),
            "generation": context["generation"],
            "bindings": sorted(context["bindings"]),
        }

    def _common_b_run(self, network: str, volume: str) -> list[str]:
        return [
            "--network",
            network,
            "--add-host",
            "gateway-a:" + self.a_ip,
            "--mount",
            f"type=volume,source={volume},target=/run/ojos/service,readonly",
        ]

    def _run_fixture_worker(self, volume: str, image: str, network: str) -> dict[str, Any]:
        assert self.b is not None
        args = ["run", "--rm", *self._common_b_run(network, volume), image, "worker"]
        result = self.b.command(*args, timeout=180)
        evidence = parse_json_line(result.stdout)
        if not evidence.get("task_resource_urls_empty"):
            raise GateError("fixture Worker received an absolute or embedded resource URL")
        return evidence

    def _run_real_worker(self, volume: str, network: str) -> dict[str, Any]:
        assert self.b is not None
        image = "ojos/judge-worker-cross-machine:" + self.run_id
        self.root.command(
            "build",
            "--file",
            self.repo_root / "services/judge-worker/Dockerfile",
            "--tag",
            image,
            self.repo_root,
            timeout=2700,
        )
        self.root_images_created.append(image)
        with tempfile.TemporaryDirectory(prefix="ojos-worker-transfer-") as transfer:
            self._transfer_image(image, [self.b], Path(transfer))
        work_volume = safe_name(self.run_id, "worker-data")
        cache_volume = safe_name(self.run_id, "worker-cache")
        result = self.b.command(
            "run",
            "--rm",
            "--privileged",
            "--cgroupns=host",
            "--cap-add=SYS_ADMIN",
            "--cap-add=NET_ADMIN",
            "--cap-add=SYS_CHROOT",
            "--security-opt",
            "apparmor=unconfined",
            *self._common_b_run(network, volume),
            "--mount",
            f"type=volume,source={work_volume},target=/var/lib/ojos-worker/work",
            "--mount",
            f"type=volume,source={cache_volume},target=/var/lib/ojos-worker/cache",
            "--volume",
            "/sys/fs/cgroup:/sys/fs/cgroup:rw",
            "--tmpfs",
            "/tmp:rw,nosuid,nodev,size=268435456",
            "--env",
            "OJOS_SERVICE_CONTEXT_FILE=/run/ojos/service/context.json",
            "--env",
            "OJOS_WORKER_SMOKE_ONCE=true",
            "--env",
            "OJOS_RUNNER_MODE=nsjail",
            "--env",
            "OJOS_ALLOW_CGROUP_FALLBACK=false",
            "--env",
            "OJOS_WORK_DIR=/var/lib/ojos-worker/work",
            "--env",
            "OJOS_ARTIFACT_CACHE_DIR=/var/lib/ojos-worker/cache",
            "--env",
            "RUST_LOG=info",
            image,
            timeout=900,
        )
        if "worker smoke-once task completed" not in (result.stdout + result.stderr):
            raise GateError("real Judge Worker exited without completing its managed task")
        return {
            "status": "ok",
            "implementation": "services/judge-worker",
            "smoke_once_completed": True,
            "log_sha256": "sha256:"
            + hashlib.sha256((result.stdout + result.stderr).encode()).hexdigest(),
        }

    def _run_generic_consumer(
        self,
        root: Path,
        image: str,
        ca_file: Path,
        network: str,
        binding: Mapping[str, Any],
    ) -> dict[str, Any]:
        assert self.b is not None
        context = self._context(
            "contract-echo-consumer-b",
            "contract-echo-consumer",
            {
                "echo": {
                    "binding_id": binding["binding_id"],
                    "api_id": binding["api_id"],
                    "base_path": binding["base_path"],
                    "timeout_ms": binding["timeout_ms"],
                }
            },
        )
        volume = safe_name(self.run_id, "consumer-context")
        self._materialize_volume(root, image, volume, context, ca_file)
        result = self.b.command(
            "run",
            "--rm",
            *self._common_b_run(network, volume),
            image,
            "consumer",
            timeout=60,
        )
        return parse_json_line(result.stdout)

    def _run_real_component_probes(self, network: str) -> dict[str, Any]:
        if shutil.which("go") is None:
            raise GateError("full-components gate requires Go; this is a failure, not a skip")
        pg_port = docker_host_port(self.root, self.a_name, 5432)
        redis_port = docker_host_port(self.root, self.a_name, 6379)
        pg_url = (
            "postgres://postgres:cross-machine-postgres@127.0.0.1:"
            f"{pg_port}/ojos_cross_machine?sslmode=disable"
        )
        redis_url = f"redis://127.0.0.1:{redis_port}/0"
        probes = []
        cases = [
            (
                "/src/services/problem-service",
                "./internal/repository",
                "^TestRealProblemMutationAndOutboxShareTransaction$",
                {"OJOS_EVENTING_TEST_POSTGRES_URL": pg_url},
            ),
            (
                "/src/services/judge-api",
                "./internal/repository",
                "^TestRealProblemProjectionOutboxStreamInbox$",
                {
                    "OJOS_EVENTING_TEST_POSTGRES_URL": pg_url,
                    "OJOS_EVENTING_TEST_REDIS_URL": redis_url,
                },
            ),
        ]
        for workdir, package, test_name, env in cases:
            result = self.runner.run(
                ["go", "test", package, "-run", test_name, "-count=1", "-v"],
                cwd=self.repo_root / workdir.removeprefix("/src/"),
                env=env,
                timeout=300,
            )
            if "--- PASS:" not in result.stdout:
                raise GateError(f"real component probe did not report PASS: {test_name}")
            probes.append(
                {
                    "test": test_name.strip("^$"),
                    "real_postgresql": True,
                    "real_redis": "REDIS" in canonical_json(env),
                    "output_sha256": "sha256:" + hashlib.sha256(result.stdout.encode()).hexdigest(),
                }
            )
        return {"status": "PASSED", "tests": probes}

    def _collect_failure_logs(self) -> None:
        logs: dict[str, str] = {}
        errors: list[dict[str, Any]] = []
        truncated = False
        attempted_logs = 0
        for label, engine in (("engine_a", self.a), ("engine_b", self.b)):
            if engine is None:
                continue
            try:
                listed = engine.command(
                    "ps", "-a", "--format", "{{.Names}}", timeout=15, check=False
                )
            except Exception as error:
                append_bounded_diagnostic_error(errors, f"{label}/list-containers", error)
                continue
            if listed.returncode != 0:
                detail = (listed.stderr or listed.stdout).strip() or (
                    f"docker ps exited {listed.returncode}"
                )
                append_bounded_diagnostic_error(errors, f"{label}/list-containers", detail)
                continue
            for container in listed.stdout.splitlines():
                if not container:
                    continue
                if attempted_logs >= MAX_FAILURE_LOG_ENTRIES:
                    truncated = True
                    break
                attempted_logs += 1
                operation = label + "/" + container
                try:
                    value = engine.command(
                        "logs", "--tail", "100", container, timeout=15, check=False
                    )
                except Exception as error:
                    append_bounded_diagnostic_error(errors, operation, error)
                    continue
                if value.returncode != 0:
                    detail = (value.stderr or value.stdout).strip() or (
                        f"docker logs exited {value.returncode}"
                    )
                    append_bounded_diagnostic_error(errors, operation, detail)
                    continue
                logs[operation] = (value.stdout + value.stderr)[-MAX_FAILURE_LOG_CHARS:]
            if truncated:
                break
        self.evidence["failure_logs"] = logs
        self.evidence["failure_log_errors"] = errors
        if truncated:
            self.evidence["failure_logs_truncated"] = True

    def cleanup(self) -> list[dict[str, Any]]:
        errors: list[dict[str, Any]] = []

        def inspect_absent(
            args: tuple[str, ...], absent_markers: tuple[str, ...]
        ) -> tuple[bool | None, str]:
            try:
                inspected = self.root.command(
                    *args,
                    timeout=CLEANUP_RECONCILE_INSPECT_TIMEOUT_SECONDS,
                    check=False,
                )
            except Exception as error:
                return None, f"exact cleanup reconciliation failed: {error}"
            detail = (inspected.stderr or inspected.stdout).strip()
            if inspected.returncode == 0:
                return False, "exact cleanup target still exists"
            if any(marker in detail.casefold() for marker in absent_markers):
                return True, detail or "exact cleanup target is absent"
            return (
                None,
                detail
                or f"exact cleanup reconciliation exited {inspected.returncode}",
            )

        def remove(
            operation: str,
            args: tuple[str, ...],
            timeout: float,
            absent_markers: tuple[str, ...],
            *,
            inspect_args: tuple[str, ...] | None = None,
            retry_timeout: float | None = None,
        ) -> None:
            try:
                result = self.root.command(*args, timeout=timeout, check=False)
            except Exception as error:
                if inspect_args is None:
                    append_bounded_diagnostic_error(errors, operation, error)
                    return

                absent, reconciliation = inspect_absent(inspect_args, absent_markers)
                if absent is True:
                    return
                if absent is None:
                    append_bounded_diagnostic_error(
                        errors, operation, f"{error}; {reconciliation}"
                    )
                    return

                retry_detail = "cleanup retry was not attempted"
                try:
                    retried = self.root.command(
                        *args,
                        timeout=retry_timeout if retry_timeout is not None else timeout,
                        check=False,
                    )
                    retry_detail = (retried.stderr or retried.stdout).strip() or (
                        "cleanup retry succeeded"
                        if retried.returncode == 0
                        else f"cleanup retry exited {retried.returncode}"
                    )
                except Exception as retry_error:
                    retry_detail = f"cleanup retry failed: {retry_error}"

                absent, final_reconciliation = inspect_absent(
                    inspect_args, absent_markers
                )
                if absent is True:
                    return
                append_bounded_diagnostic_error(
                    errors,
                    operation,
                    f"{error}; {retry_detail}; {final_reconciliation}",
                )
                return
            if result.returncode == 0:
                return
            detail = (result.stderr or result.stdout).strip()
            # Cleanup is idempotent: an already absent resource is clean.
            if any(marker in detail.casefold() for marker in absent_markers):
                return
            append_bounded_diagnostic_error(
                errors,
                operation,
                detail or f"cleanup command exited {result.returncode}",
            )

        # Cleanup targets are fixed names created from the validated run ID.
        for name in (self.a_name, self.b_name):
            if name.startswith(SAFE_RUN_PREFIX):
                remove(
                    f"remove-container/{name}",
                    # --volumes also removes any unexpected anonymous volumes;
                    # the run-scoped /var/lib/docker roots are removed and
                    # reconciled explicitly below.
                    ("rm", "--force", "--volumes", name),
                    DIND_CONTAINER_REMOVE_TIMEOUT_SECONDS,
                    ("no such container",),
                    inspect_args=("container", "inspect", name),
                    retry_timeout=DIND_CONTAINER_REMOVE_RETRY_TIMEOUT_SECONDS,
                )
        expected_data_volumes = {self.a_data_volume, self.b_data_volume}
        for volume in self.dind_data_volumes_created:
            if volume not in expected_data_volumes or not volume.startswith(
                SAFE_RUN_PREFIX
            ):
                append_bounded_diagnostic_error(
                    errors,
                    f"remove-volume/{volume}",
                    "refused to remove a non-run-scoped nested Engine data volume",
                )
                continue
            try:
                ownership = self.root.command(
                    "volume",
                    "inspect",
                    "--format",
                    "{{json .Labels}}",
                    volume,
                    timeout=CLEANUP_RECONCILE_INSPECT_TIMEOUT_SECONDS,
                    check=False,
                )
            except Exception as error:
                append_bounded_diagnostic_error(
                    errors,
                    f"verify-volume-owner/{volume}",
                    error,
                )
                continue
            ownership_detail = (ownership.stderr or ownership.stdout).strip()
            if ownership.returncode != 0:
                if "no such volume" in ownership_detail.casefold():
                    continue
                append_bounded_diagnostic_error(
                    errors,
                    f"verify-volume-owner/{volume}",
                    ownership_detail
                    or f"volume ownership inspection exited {ownership.returncode}",
                )
                continue
            try:
                labels = json.loads(ownership.stdout)
            except json.JSONDecodeError as error:
                append_bounded_diagnostic_error(
                    errors,
                    f"verify-volume-owner/{volume}",
                    error,
                )
                continue
            if (
                not isinstance(labels, Mapping)
                or labels.get("ojos.cross-machine.run") != self.run_id
            ):
                append_bounded_diagnostic_error(
                    errors,
                    f"verify-volume-owner/{volume}",
                    "refused to remove a nested Engine data volume owned by another run",
                )
                continue
            remove(
                f"remove-volume/{volume}",
                ("volume", "rm", volume),
                DIND_VOLUME_REMOVE_TIMEOUT_SECONDS,
                ("no such volume",),
                inspect_args=("volume", "inspect", volume),
                retry_timeout=DIND_VOLUME_REMOVE_RETRY_TIMEOUT_SECONDS,
            )
        if self.outer_network.startswith(SAFE_RUN_PREFIX):
            remove(
                f"remove-network/{self.outer_network}",
                ("network", "rm", self.outer_network),
                30,
                ("no such network", "network " + self.outer_network.casefold() + " not found"),
            )
        for image in self.root_images_created:
            if image.endswith(":" + self.run_id):
                remove(
                    f"remove-image/{image}",
                    ("image", "rm", image),
                    60,
                    ("no such image",),
                )
        self.evidence["cleanup_errors"] = errors
        return errors


def verify_evidence(
    value: Mapping[str, Any],
    require_full: bool = False,
    *,
    require_cleanup: bool = True,
) -> None:
    if value.get("schema_version") != SCHEMA_VERSION or value.get("status") != "PASSED":
        raise GateError("cross-machine evidence is not a passed v1 document")
    if require_cleanup and (
        value.get("cleanup_completed") is not True or value.get("cleanup_errors") != []
    ):
        raise GateError("cross-machine evidence does not prove successful cleanup")
    run_id = value.get("run_id")
    if not isinstance(run_id, str) or re.fullmatch(r"[0-9a-f]{10}", run_id) is None:
        raise GateError("cross-machine evidence has no canonical run ID")
    engines = value.get("engines", {})
    if not isinstance(engines, Mapping):
        raise GateError("cross-machine evidence engines is not an object")
    a, b = engines.get("a", {}), engines.get("b", {})
    if not isinstance(a, Mapping) or not isinstance(b, Mapping):
        raise GateError("cross-machine evidence Engine entries are not objects")
    if (
        not a.get("engine_id")
        or not b.get("engine_id")
        or a.get("engine_id") == b.get("engine_id")
    ):
        raise GateError("evidence does not prove two distinct Docker Engine identities")
    if (
        not a.get("outer_container_id")
        or not b.get("outer_container_id")
        or a.get("outer_container_id") == b.get("outer_container_id")
    ):
        raise GateError("evidence reused the same outer Engine container")
    if (
        a.get("host_endpoint_matches_local_socket") is not True
        or b.get("host_endpoint_matches_local_socket") is not True
        or engines.get("routing_proof") != "host TCP endpoint ID matches outer unix socket"
    ):
        raise GateError("evidence does not prove nested Engine endpoint routing")
    if (
        not a.get("data_volume")
        or not b.get("data_volume")
        or a.get("data_volume") == b.get("data_volume")
        or engines.get("storage_roots_distinct") is not True
    ):
        raise GateError("evidence does not prove distinct nested Engine data roots")
    if engines.get("isolation_proof") != "mutually-invisible marker volumes":
        raise GateError("evidence does not prove Engine storage isolation")
    if not re.fullmatch(
        r"[^\s@]+@sha256:[0-9a-f]{64}", str(engines.get("dind_image", ""))
    ):
        raise GateError("evidence does not identify a digest-pinned DIND image")
    requested_dind_digest = str(engines.get("dind_image")).rsplit("@", 1)[1]
    probe = value.get("engine_probe", {})
    if not isinstance(probe, Mapping) or probe.get("dind_image") != engines.get(
        "dind_image"
    ):
        raise GateError("evidence Engine probe does not match the pinned DIND image")

    canonical_identities: list[tuple[str, str, str, str, str, str]] = []
    canonical_endpoints: list[str] = []
    canonical_outer_ids: list[str] = []
    canonical_image_ids: list[str] = []
    canonical_repo_digests: list[list[str]] = []
    for label, engine, volume_suffix, marker_suffix in (
        ("a", a, "engine-a-data", "only-a"),
        ("b", b, "engine-b-data", "only-b"),
    ):
        if not isinstance(engine, Mapping):
            raise GateError(f"evidence Engine {label.upper()} is not an object")
        identity = required_engine_identity(
            {
                "ID": engine.get("engine_id"),
                "Name": engine.get("engine_name"),
                "Driver": engine.get("storage_driver"),
                "ServerVersion": engine.get("server_version"),
                "OSType": engine.get("os_type"),
                "DockerRootDir": engine.get("docker_root_dir"),
            }
        )
        if engine.get("local_engine_id") != identity[0]:
            raise GateError("evidence does not prove nested Engine endpoint routing")
        outer_id = required_outer_container_id(engine.get("outer_container_id"))
        endpoint = required_local_docker_endpoint(engine.get("host_endpoint"))
        expected_volume = safe_name(run_id, volume_suffix)
        if engine.get("data_volume") != expected_volume:
            raise GateError("evidence does not prove distinct nested Engine data roots")
        if engine.get("marker_volume") != safe_name(run_id, marker_suffix):
            raise GateError("evidence does not prove Engine storage isolation")
        image_config_id = required_image_config_id(engine.get("image_config_id"))
        repo_digests = required_repo_digests(engine.get("image_repo_digests"))
        if not any(item.endswith("@" + requested_dind_digest) for item in repo_digests):
            raise GateError("evidence DIND image RepoDigest does not match the pin")

        side_probe = probe.get(label)
        if not isinstance(side_probe, Mapping):
            raise GateError(f"evidence Engine {label.upper()} probe is missing")
        probe_identity = required_engine_identity(
            {
                "ID": side_probe.get("host_engine_id"),
                "Name": side_probe.get("engine_name"),
                "Driver": side_probe.get("driver"),
                "ServerVersion": side_probe.get("server_version"),
                "OSType": side_probe.get("os_type"),
                "DockerRootDir": side_probe.get("docker_root_dir"),
            }
        )
        local_probe = side_probe.get("local_identity")
        if not isinstance(local_probe, Mapping):
            raise GateError(f"evidence Engine {label.upper()} local probe is missing")
        local_probe_identity = required_engine_identity(
            {
                "ID": local_probe.get("engine_id"),
                "Name": local_probe.get("engine_name"),
                "Driver": local_probe.get("driver"),
                "ServerVersion": local_probe.get("server_version"),
                "OSType": local_probe.get("os_type"),
                "DockerRootDir": local_probe.get("docker_root_dir"),
            }
        )
        if (
            probe_identity != identity
            or local_probe_identity != identity
            or side_probe.get("local_engine_id") != identity[0]
            or side_probe.get("host_endpoint") != endpoint
            or side_probe.get("outer_container_id") != outer_id
            or side_probe.get("data_volume") != expected_volume
            or side_probe.get("image_config_id") != image_config_id
            or side_probe.get("image_repo_digests") != repo_digests
        ):
            raise GateError(
                f"evidence Engine {label.upper()} probe does not match final identity"
            )
        canonical_identities.append(identity)
        canonical_endpoints.append(endpoint)
        canonical_outer_ids.append(outer_id)
        canonical_image_ids.append(image_config_id)
        canonical_repo_digests.append(repo_digests)

    if (
        canonical_identities[0][0] == canonical_identities[1][0]
        or canonical_endpoints[0] == canonical_endpoints[1]
        or canonical_outer_ids[0] == canonical_outer_ids[1]
    ):
        raise GateError("evidence does not prove two independent nested Docker Engines")
    if (
        canonical_image_ids[0] != canonical_image_ids[1]
        or canonical_repo_digests[0] != canonical_repo_digests[1]
    ):
        raise GateError("evidence nested Engines did not use the same pinned image")
    boundary = value.get("network_boundary", {})
    if not boundary.get("gateway_ready"):
        raise GateError("B did not prove A Gateway reachability")
    denied = boundary.get("denied", [])
    expected_denied = set(PRIVATE_PORTS) | set(MANAGEMENT_PORTS)
    if {item.get("name") for item in denied if item.get("denied") is True} != expected_denied:
        raise GateError("B business boundary did not deny every private/management endpoint")
    agent = boundary.get("agent_connectivity", [])
    if {item.get("name") for item in agent if item.get("status") == 200} != set(MANAGEMENT_PORTS):
        raise GateError("B Agent did not prove control-plane and registry reachability")
    # The generic provider/consumer proof is a release gate in both lanes.  A
    # full Judge run must not bypass it merely because its product-specific
    # checks are stricter.
    third_party = value.get("third_party_fixture", {})
    if third_party.get("specialized_product_code") is not False:
        raise GateError("third-party fixture used specialized product code")
    response = third_party.get("consumer_evidence", {}).get("response", {})
    if (
        response.get("value") != "cross-engine-binding-ok"
        or response.get("provider") != "contract-echo-provider"
        or response.get("caller") != "contract-echo-consumer"
        or response.get("path") != "/echo"
    ):
        raise GateError("third-party consumer did not cross the manifest-generated binding")
    if require_full:
        _verify_full_evidence(value, engines, require_completion=require_cleanup)
        return

    context = value.get("managed_context", {})
    for proof in ("validated", "mount_read_only"):
        if context.get(proof) is not True:
            raise GateError(f"managed context proof is missing: {proof}")
    if context.get("credential_embedded") is not False or context.get("management_token_present") is not False:
        raise GateError("managed context contains embedded or management credentials")
    flow = value.get("component_flow", {})
    gateway = flow.get("gateway_evidence", {})
    required_transitions = {
        "problem.transaction_committed_with_outbox",
        "problem.snapshot_published",
        "judge.inbox_recorded",
        "judge.problem_projection_applied",
        "judge.submission_froze_problem_revision",
        "judge.task_queued",
        "worker.long_poll_claimed",
        "worker.result_reported",
        "judge.submission_completed",
    }
    if not required_transitions.issubset(set(gateway.get("transitions", []))):
        raise GateError("component flow evidence is missing a required transition")
    if gateway.get("task_state") != "succeeded" or gateway.get("result", {}).get("status") != "ACCEPTED":
        raise GateError("cross-machine task did not complete successfully")
    if gateway.get("claim_prefer") != "wait=25" or int(gateway.get("claim_wait_ms", 0)) < 250:
        raise GateError("Worker did not use the cancellable long-poll protocol")
    task = gateway.get("task", {})
    validate_resource_ref(task.get("source", {}))
    validate_resource_ref(task.get("problem_package", {}))
    if not gateway.get("identity_headers_removed") or int(gateway.get("workload_requests", 0)) < 1:
        raise GateError("Gateway workload identity evidence is incomplete")


def _verify_full_evidence(
    value: Mapping[str, Any],
    engines: Mapping[str, Any],
    *,
    require_completion: bool,
) -> None:
    """Validate the one actual Store -> Agent -> Judge execution chain.

    A full result is deliberately stricter than the protocol fixture.
    Component probes, a hand-written ServiceContext, or a direct ``docker
    run`` of the Worker are never accepted as substitutes.
    """

    if (
        value.get("gate") != "cross-machine-service-contract-v2"
        or value.get("mode") != "full-components"
        or not re.fullmatch(r"[0-9a-f]{10}", str(value.get("run_id", "")))
        or int(value.get("started_at_unix", 0)) <= 0
        or (
            require_completion
            and int(value.get("completed_at_unix", 0))
            < int(value.get("started_at_unix", 0))
        )
    ):
        raise GateError("full-components evidence identity or completion timestamps are invalid")
    build = value.get("build_identity", {})
    if (
        not isinstance(build, Mapping)
        or not str(build.get("version", "")).strip()
        or not re.fullmatch(r"[0-9a-f]{40}", str(build.get("commit_sha", "")))
        or build.get("profile") != "production"
        or not str(build.get("target", "")).strip()
    ):
        raise GateError("full-components evidence is not tied to a production build identity")
    control_plane = value.get("control_plane_runtime", {})
    if (
        not isinstance(control_plane, Mapping)
        or control_plane.get("evidence_source") != "docker-inspect"
        or not re.fullmatch(
            r"[0-9a-f]{64}", str(control_plane.get("container_id", ""))
        )
        or control_plane.get("engine_id") != engines.get("a", {}).get("engine_id")
        or control_plane.get("running") is not True
        or control_plane.get("docker_health") != "HEALTHY"
        or control_plane.get("healthcheck_url")
        != CONTROL_PLANE_HEALTHCHECK_URL
        or control_plane.get("healthcheck_ca_cert")
        != CONTROL_PLANE_HEALTHCHECK_CA_CERT
        or control_plane.get("tls_enabled") is not True
    ):
        raise GateError(
            "full-components evidence does not prove a Docker-healthy TLS control-plane"
        )
    if value.get("deployment_via_store_agent") is not True:
        raise GateError(
            "full-components evidence did not deploy the Worker through Store and an enrolled Agent"
        )
    if value.get("worker_implementation") != "repository Rust judge-worker image (Agent-created)":
        raise GateError("full-components evidence did not run the Agent-created repository Judge Worker")

    worker_bridge_denied = value.get("network_boundary", {}).get(
        "worker_bridge_denied", []
    )
    expected_denied = set(PRIVATE_PORTS) | set(MANAGEMENT_PORTS)
    if {
        item.get("name")
        for item in worker_bridge_denied
        if isinstance(item, Mapping) and item.get("denied") is True
    } != expected_denied:
        raise GateError(
            "full-components evidence did not deny every private/management endpoint "
            "from the Worker default bridge"
        )

    flow = value.get("component_flow", {})
    if not isinstance(flow, Mapping) or flow.get("same_chain") is not True or flow.get("source") != "actual-services":
        raise GateError("full-components evidence is not one uninterrupted actual-service chain")
    if flow.get("problem_created_via_http_api") is not True:
        raise GateError("full-components evidence did not create the problem through the HTTP API")
    if flow.get("submission_created_via_http_api") is not True:
        raise GateError("full-components evidence did not create the submission through the HTTP API")
    if flow.get("manual_judge_problem_insert") is not False:
        raise GateError("full-components evidence permits a manual Judge problem INSERT")
    actual_components = set(flow.get("actual_components", []))
    required_components = {
        "orchestrator",
        "agent",
        "gateway",
        "auth",
        "problem-service",
        "judge-api",
        "storage-service",
        "postgresql",
        "redis",
        "minio",
        "rust-judge-worker",
        "nsjail",
    }
    if not required_components.issubset(actual_components):
        missing = ", ".join(sorted(required_components - actual_components))
        raise GateError(f"full-components evidence is missing actual components: {missing}")
    _verify_actual_flow_correlation(flow)

    _verify_auth_admin_bootstrap(value)
    _verify_no_secret_material(value)

    a_agent = _verify_managed_a_stack(value, engines)
    _verify_problem_artifact_gc(value)

    store_agent = value.get("store_agent_evidence", {})
    if not isinstance(store_agent, Mapping):
        raise GateError("full-components Store/Agent evidence is missing")
    enrolled = store_agent.get("agent", {})
    enrolled_health = (
        enrolled.get("runtime_health", {}) if isinstance(enrolled, Mapping) else {}
    )
    enrolled_observation_age = (
        int(enrolled_health.get("observation_age_ms", -1))
        if isinstance(enrolled_health, Mapping)
        else -1
    )
    enrolled_freshness = (
        int(enrolled_health.get("freshness_threshold_ms", 0))
        if isinstance(enrolled_health, Mapping)
        else 0
    )
    if (
        not isinstance(enrolled, Mapping)
        or enrolled.get("enrolled") is not True
        or enrolled.get("mtls") is not True
        or not str(enrolled.get("node_id", "")).strip()
        or not str(enrolled.get("instance_id", "")).strip()
        or not str(enrolled.get("certificate_serial", "")).strip()
        or enrolled.get("runtime_health_sample") != "final-agent-report"
        or not isinstance(enrolled_health, Mapping)
        or enrolled_health.get("node_id") != "node-b"
        or str(enrolled_health.get("status", "")).upper() != "READY"
        or enrolled_health.get("ready") is not True
        or enrolled_health.get("accepting_jobs") is not True
        or enrolled_health.get("agent_reachable") is not True
        or not str(enrolled_health.get("last_observed_at", "")).startswith("unix-ms:")
        or int(enrolled_health.get("unhealthy_deployments", -1)) != 0
        or enrolled_observation_age < 0
        or enrolled_freshness <= 0
        or enrolled_freshness > 60_000
        or enrolled_observation_age > enrolled_freshness
    ):
        raise GateError(
            "full-components evidence does not prove an enrolled mTLS Agent with a final runtime report"
        )

    validation = store_agent.get("store_validate", {})
    if (
        not isinstance(validation, Mapping)
        or validation.get("accepted") is not True
        or not str(validation.get("request_id", "")).strip()
        or not _strong_etag(validation.get("topology_etag"))
        or "endpoint" in validation.get("request_fields", [])
    ):
        raise GateError("full-components evidence is missing accepted Store validation with topology_etag")
    validated_bindings = _binding_requirements(validation.get("bindings"))
    if validated_bindings != {"judge_control", "storage_get"}:
        raise GateError("Store validation did not confirm both explicit Judge Worker bindings")

    install = store_agent.get("store_install", {})
    if (
        not isinstance(install, Mapping)
        or install.get("accepted") is not True
        or not str(install.get("request_id", "")).strip()
        or not str(install.get("operation_id", "")).strip()
        or not str(install.get("deployment_id", "")).strip()
        or install.get("topology_etag") != validation.get("topology_etag")
        or _binding_requirements(install.get("bindings")) != validated_bindings
        or "endpoint" in install.get("request_fields", [])
        or install.get("request_endpoint_present") is not False
        or install.get("response_published_endpoint") is not None
        or install.get("automatic_logical_endpoint") is not True
    ):
        raise GateError("full-components evidence is missing the confirmed Store install request")
    node_b_host = str(engines.get("b", {}).get("outer_ip", "")).strip()
    if (
        not node_b_host
        or install.get("topology_logical_endpoint")
        != f"{node_b_host}:9101:judge-worker"
    ):
        raise GateError(
            "Store did not derive the backend-worker logical endpoint from enrolled Node facts"
        )

    operation = store_agent.get("operation", {})
    if (
        not isinstance(operation, Mapping)
        or operation.get("operation_id") != install.get("operation_id")
        or str(operation.get("status", "")).upper() != "SUCCEEDED"
        or not str(operation.get("job_id", "")).strip()
    ):
        raise GateError("Store install Operation did not reach SUCCEEDED with a durable Job")
    job = store_agent.get("agent_job", {})
    if (
        not isinstance(job, Mapping)
        or job.get("job_id") != operation.get("job_id")
        or not str(job.get("attempt_id", "")).strip()
        or not str(job.get("lease_id", "")).strip()
        or job.get("lease_owner_instance_id") != enrolled.get("instance_id")
        or str(job.get("status", "")).upper() != "SUCCEEDED"
        or job.get("completed_by_agent") is not True
    ):
        raise GateError("full-components evidence is missing the enrolled Agent lease/attempt completion")

    deployment = store_agent.get("deployment", {})
    if (
        not isinstance(deployment, Mapping)
        or deployment.get("deployment_id") != install.get("deployment_id")
        or deployment.get("node_id") != enrolled.get("node_id")
        or str(deployment.get("desired_state", "")).upper() != "RUNNING"
        or str(deployment.get("observed_state", "")).upper() != "RUNNING"
        or str(deployment.get("health", "")).upper() != "HEALTHY"
        or deployment.get("runtime_profile") != "judge-sandbox-v1"
        or deployment.get("runtime_attested") is not True
        or str(deployment.get("drift_reason", "")).strip()
    ):
        raise GateError("Agent-created Judge Worker Deployment is not Running/Healthy on enrolled Node B")
    _verify_runtime_inventory_takeover(
        deployment.get("runtime_projection"),
        deployment_id=str(install.get("deployment_id", "")),
        node_id=str(enrolled.get("node_id", "")),
    )

    observed_bindings = store_agent.get("bindings", [])
    if _active_binding_requirements(observed_bindings) != validated_bindings:
        raise GateError("persisted deployment bindings are not ACTIVE for both requirements")
    binding_ids = _binding_ids(observed_bindings)
    if len(binding_ids) != 2:
        raise GateError("persisted deployment binding IDs are missing or duplicated")

    service_context = store_agent.get("service_context", {})
    if (
        not isinstance(service_context, Mapping)
        or int(service_context.get("generation", 0)) < 1
        or service_context.get("deployment_id") != install.get("deployment_id")
        or service_context.get("node_id") != enrolled.get("node_id")
        or service_context.get("mount_read_only") is not True
        or service_context.get("credential_embedded") is not False
        or service_context.get("management_token_present") is not False
        or set(service_context.get("binding_ids", [])) != binding_ids
    ):
        raise GateError("Agent ServiceContext evidence is incomplete or does not match active bindings")

    runtime = store_agent.get("runtime", {})
    if (
        not isinstance(runtime, Mapping)
        or runtime.get("created_by_agent") is not True
        or runtime.get("context_mount_read_only") is not True
        or str(runtime.get("health_gate", "")).upper() != "HEALTHY"
        or runtime.get("runtime_profile") != "judge-sandbox-v1"
        or not _sha256_digest(runtime.get("host_config_digest"))
        or not _sha256_digest(runtime.get("image_repo_digest"))
        or not str(runtime.get("container_id", "")).strip()
        or runtime.get("engine_id") != engines.get("b", {}).get("engine_id")
    ):
        raise GateError("runtime evidence does not prove Agent-created digest-pinned judge-sandbox-v1 on Engine B")

    _verify_worker_install_failure_compensation(
        value,
        worker_deployment_id=str(install.get("deployment_id", "")),
        successful_operation_id=str(operation.get("operation_id", "")),
        logical_endpoint=str(install.get("topology_logical_endpoint", "")),
        node_b_instance_id=str(enrolled.get("instance_id", "")),
    )
    _verify_worker_recovery(
        value,
        first_flow=flow,
        worker_deployment_id=str(install.get("deployment_id", "")),
        worker_container_id=str(runtime.get("container_id", "")),
    )

    _verify_workload_credential_lifecycle(value)
    _verify_workload_request_transcript(
        value,
        flow=flow,
        worker_deployment_id=str(install.get("deployment_id", "")),
        bindings=observed_bindings,
    )

    volume_isolation = value.get("runtime_volume_isolation", {})
    if (
        not isinstance(volume_isolation, Mapping)
        or volume_isolation.get("verified") is not True
        or volume_isolation.get("inspection_source") != "docker-inspect"
        or volume_isolation.get("forbidden_shared_sources") != []
        or volume_isolation.get("a_engine_id") != engines.get("a", {}).get("engine_id")
        or volume_isolation.get("b_engine_id") != engines.get("b", {}).get("engine_id")
    ):
        raise GateError(
            "runtime evidence does not prove Gateway/Judge and Worker avoid shared Problem/Submission volumes"
        )

    reconfiguration = value.get("binding_reconfiguration", {})
    before_generation = int(reconfiguration.get("generation_before", 0)) if isinstance(reconfiguration, Mapping) else 0
    after_generation = int(reconfiguration.get("generation_after", 0)) if isinstance(reconfiguration, Mapping) else 0
    if (
        not isinstance(reconfiguration, Mapping)
        or reconfiguration.get("provider_preserving") is not True
        or reconfiguration.get("semantic_provider_rebind") is not True
        or str(reconfiguration.get("operation_status", "")).upper() != "SUCCEEDED"
        or not str(reconfiguration.get("operation_id", "")).strip()
        or reconfiguration.get("container_id_before") != runtime.get("container_id")
        or reconfiguration.get("container_id_after") != runtime.get("container_id")
        or before_generation < 1
        or after_generation <= before_generation
        or int(reconfiguration.get("credential_generation_after", 0)) != after_generation
        or int(reconfiguration.get("context_generation_after", 0)) != after_generation
        or reconfiguration.get("post_update_request_succeeded") is not True
        or str(reconfiguration.get("post_update_submission_status", "")).upper() != "ACCEPTED"
    ):
        raise GateError(
            "Topology Binding reconfiguration did not rotate context in place and preserve service traffic"
        )

    managed_a_deployments = value.get("managed_a_deployments", {})
    original_storage = (
        managed_a_deployments.get("storage-service", {})
        if isinstance(managed_a_deployments, Mapping)
        else {}
    )
    canary = reconfiguration.get("canary_store", {})
    canary_runtime = canary.get("runtime", {}) if isinstance(canary, Mapping) else {}
    old_provider_id = str(reconfiguration.get("old_provider_deployment_id", "")).strip()
    new_provider_id = str(reconfiguration.get("new_provider_deployment_id", "")).strip()
    old_provider_endpoint = str(reconfiguration.get("old_provider_endpoint", "")).strip()
    new_provider_endpoint = str(reconfiguration.get("new_provider_endpoint", "")).strip()
    if (
        reconfiguration.get("requirement_name") != "storage_get"
        or reconfiguration.get("api_id") != "storage.object.get"
        or reconfiguration.get("consumer_deployment_id") != install.get("deployment_id")
        or not str(reconfiguration.get("consumer_endpoint", "")).strip()
        or old_provider_id != original_storage.get("deployment_id")
        or not new_provider_id
        or new_provider_id == old_provider_id
        or not old_provider_endpoint
        or not new_provider_endpoint
        or new_provider_endpoint == old_provider_endpoint
        or not str(reconfiguration.get("topology_revision_id", "")).strip()
        or not isinstance(canary, Mapping)
        or canary.get("service_id") != "storage-service"
        or canary.get("catalog_source_id") != "storage-service-canary"
        or canary.get("version") != "0.1.1"
        or canary.get("deployment_id") != new_provider_id
        or canary.get("endpoint") != new_provider_endpoint
        or not str(canary.get("validate_request_id", "")).strip()
        or canary.get("validation_valid") is not True
        or int(canary.get("validation_topology_changes", 0)) < 1
        or not str(canary.get("install_request_id", "")).strip()
        or not str(canary.get("operation_id", "")).strip()
        or canary.get("operation_id") == reconfiguration.get("operation_id")
        or str(canary.get("operation_status", "")).upper() != "SUCCEEDED"
        or not isinstance(canary_runtime, Mapping)
        or canary_runtime.get("deployment_id") != new_provider_id
        or canary_runtime.get("deployment_id") == original_storage.get("deployment_id")
        or canary_runtime.get("container_id") == original_storage.get("container_id")
        or canary_runtime.get("image_repo_digest")
        != original_storage.get("image_repo_digest")
    ):
        raise GateError(
            "Topology Binding reconfiguration does not prove a distinct signed Store canary provider"
        )
    _verify_provider_projection_integrity(
        reconfiguration.get("provider_projection_integrity"),
        phase="binding-reconfigure",
        expected_revision_id=str(reconfiguration.get("topology_revision_id", "")),
    )
    _verify_managed_a_deployment(
        service_id="storage-service",
        evidence=canary_runtime,
        required_bindings=set(),
        deployments=managed_a_deployments,
        agent=a_agent,
        engine_id=str(engines.get("a", {}).get("engine_id", "")),
        auth_deployment_id="",
    )
    generic = value.get("third_party_fixture", {})
    provider = generic.get("provider", {}) if isinstance(generic, Mapping) else {}
    consumer = generic.get("consumer", {}) if isinstance(generic, Mapping) else {}
    generic_binding = generic.get("binding_plan", {}) if isinstance(generic, Mapping) else {}
    permission_binding = (
        generic.get("permission_binding_plan", {}) if isinstance(generic, Mapping) else {}
    )
    permission_result = (
        generic.get("workload_permission_check", {}) if isinstance(generic, Mapping) else {}
    )
    permission_data = (
        permission_result.get("data", permission_result)
        if isinstance(permission_result, Mapping)
        else {}
    )
    if (
        not isinstance(generic, Mapping)
        or generic.get("specialized_product_code") is not False
        or generic.get("manifest_only") is not True
        or not isinstance(provider, Mapping)
        or provider.get("engine") != "A"
        or provider.get("installed_via_store") is not True
        or provider.get("management_mode") != "EXTERNAL"
        or not str(provider.get("deployment_id", "")).strip()
        or not isinstance(consumer, Mapping)
        or consumer.get("engine") != "B"
        or consumer.get("installed_via_store_agent") is not True
        or not str(consumer.get("deployment_id", "")).strip()
        or not str(consumer.get("container_id", "")).strip()
        or not isinstance(generic_binding, Mapping)
        or generic_binding.get("requirement_name") != "echo"
        or generic_binding.get("api_id") != "fixture.contract.echo"
        or generic_binding.get("provider_deployment_id") != provider.get("deployment_id")
        or str(generic_binding.get("state", "")).upper() != "ACTIVE"
        or generic_binding.get("optional") is not True
        or not isinstance(permission_binding, Mapping)
        or permission_binding.get("requirement_name") != "permission_check"
        or permission_binding.get("api_id") != "auth.user.permission.check"
        or permission_binding.get("provider_deployment_id")
        != generic.get("permission_provider_deployment_id")
        or str(permission_binding.get("state", "")).upper() != "ACTIVE"
        or permission_binding.get("optional") is not False
        or not isinstance(permission_result, Mapping)
        or not isinstance(permission_data, Mapping)
        or permission_data.get("allowed") is not True
        or str(generic.get("operation_status", "")).upper() != "SUCCEEDED"
        or not str(generic.get("operation_id", "")).strip()
    ):
        raise GateError(
            "full-components evidence does not prove a generic manifest-only Store/Topology/Agent binding"
        )
    _verify_runtime_inventory_takeover(
        generic.get("runtime_projection"),
        deployment_id=str(consumer.get("deployment_id", "")),
        node_id="node-b",
    )
    _verify_topology_rollback(value)
    rollback = value.get("topology_rollback", {})
    _verify_provider_projection_integrity(
        value.get("final_provider_projection_integrity"),
        phase="final-applied",
        expected_revision_id=(
            str(rollback.get("created_revision_id", ""))
            if isinstance(rollback, Mapping)
            else ""
        ),
        expected_content_sha256=(
            str(rollback.get("created_content_sha256", ""))
            if isinstance(rollback, Mapping)
            else ""
        ),
    )

    if a_agent.get("node_id") == enrolled.get("node_id"):
        raise GateError("A and B managed workloads reused one enrolled Node identity")


def _verify_problem_artifact_gc(value: Mapping[str, Any]) -> None:
    evidence = value.get("problem_artifact_gc", {})
    if not isinstance(evidence, Mapping):
        raise GateError("Problem artifact GC evidence is missing")
    if set(evidence) != {
        "setup",
        "intent_count",
        "intents",
        "all_objects_observed_before_gc",
        "storage_head_probe",
        "failure_recovery",
        "latest_topology_etag",
        "ledger_removed",
        "ledger_rows_remaining",
        "all_objects_removed",
        "gateway_storage_head_paths",
        "gateway_storage_head_observed",
        "gateway_storage_delete_paths",
        "gateway_storage_delete_observed",
        "judge_database_connection_used",
        "direct_storage_management_credential_used",
        "runtime_health_fabricated",
    }:
        raise GateError("Problem artifact GC evidence has a non-canonical shape")
    setup = evidence.get("setup", {})
    probe = evidence.get("storage_head_probe", {})
    recovery = evidence.get("failure_recovery", {})
    intents = evidence.get("intents", [])
    expected_probe_fields = {
        "status",
        "sha256_header",
        "size_bytes",
        "storage_result_header",
    }
    if (
        not isinstance(setup, Mapping)
        or set(setup)
        != {
            "method",
            "request_marker",
            "problem_no",
            "seed_problem_id",
            "seed_status",
            "failure_status",
            "baseline_pending_count",
            "new_intent_count",
            "package_intent_count",
            "content_intent_count",
            "business_database_write_used",
            "intent_rows_fabricated",
            "storage_objects_fabricated",
        }
        or setup.get("method") != "duplicate-problem-no-http-conflict"
        or setup.get("request_marker")
        != "artifact-gc-recovery-" + str(value.get("run_id", ""))
        or not re.fullmatch(r"GC[a-f0-9]{1,30}", str(setup.get("problem_no", "")))
        or not str(setup.get("seed_problem_id", "")).isdigit()
        or int(str(setup.get("seed_problem_id", "0"))) <= 0
        or setup.get("seed_status") != 200
        or setup.get("failure_status") != 500
        or isinstance(setup.get("baseline_pending_count"), bool)
        or not isinstance(setup.get("baseline_pending_count"), int)
        or int(setup.get("baseline_pending_count", -1)) < 0
        or not isinstance(intents, list)
        or not 1 <= len(intents) <= 100
        or evidence.get("intent_count") != len(intents)
        or setup.get("new_intent_count") != len(intents)
        or isinstance(setup.get("package_intent_count"), bool)
        or not isinstance(setup.get("package_intent_count"), int)
        or int(setup.get("package_intent_count", 0)) < 1
        or isinstance(setup.get("content_intent_count"), bool)
        or not isinstance(setup.get("content_intent_count"), int)
        or int(setup.get("content_intent_count", 0)) < 1
        or int(setup.get("package_intent_count", 0))
        + int(setup.get("content_intent_count", 0))
        != len(intents)
        or setup.get("business_database_write_used") is not False
        or setup.get("intent_rows_fabricated") is not False
        or setup.get("storage_objects_fabricated") is not False
        or evidence.get("all_objects_observed_before_gc") is not True
        or not isinstance(probe, Mapping)
        or set(probe)
        != {
            "role",
            "binding",
            "api_id",
            "service_context_mount_read_only",
            "problem_network_namespace_reused",
            "deployment_jwt_used",
            "credential_recorded",
            "provider_result_header_recorded",
        }
        or probe.get("role") != "binding-head"
        or probe.get("binding") != "storage_head"
        or probe.get("api_id") != "storage.object.head"
        or probe.get("service_context_mount_read_only") is not True
        or probe.get("problem_network_namespace_reused") is not True
        or probe.get("deployment_jwt_used") is not True
        or probe.get("credential_recorded") is not False
        or probe.get("provider_result_header_recorded") is not True
        or evidence.get("ledger_removed") is not True
        or evidence.get("ledger_rows_remaining") != 0
        or evidence.get("all_objects_removed") is not True
        or evidence.get("gateway_storage_head_observed") is not True
        or evidence.get("gateway_storage_delete_observed") is not True
        or evidence.get("judge_database_connection_used") is not False
        or evidence.get("direct_storage_management_credential_used") is not False
        or evidence.get("runtime_health_fabricated") is not False
    ):
        raise GateError(
            "Problem artifact GC evidence does not prove natural-conflict operator recovery"
        )
    expected_intent_fields = {
        "artifact_uri",
        "sha256",
        "size_bytes",
        "kind",
        "initial_status",
        "upload_completed_at",
        "relative_path",
        "head_before",
        "recovery_action",
        "recovery_action_id",
        "head_after",
    }
    uris: set[str] = set()
    expected_head_paths: list[str] = []
    expected_delete_paths: list[str] = []
    observed_kinds: dict[str, int] = {"package": 0, "content": 0}
    for intent in intents:
        if not isinstance(intent, Mapping) or set(intent) != expected_intent_fields:
            raise GateError(
                "Problem artifact GC evidence does not contain the complete strict intent set"
            )
        digest = str(intent.get("sha256", ""))
        uri = str(intent.get("artifact_uri", ""))
        relative_path = str(intent.get("relative_path", ""))
        size = intent.get("size_bytes")
        kind = str(intent.get("kind", ""))
        before = intent.get("head_before", {})
        after = intent.get("head_after", {})
        package_uri = f"storage://problems/package-sha256-{digest}.zip"
        content_uri = re.fullmatch(
            rf"storage://problems/problem-[1-9][0-9]*-objects-sha256-{re.escape(digest)}",
            uri,
        )
        if (
            uri in uris
            or not re.fullmatch(r"[a-f0-9]{64}", digest)
            or (
                kind == "package"
                and (uri != package_uri or not isinstance(size, int) or size <= 0)
            )
            or (
                kind == "content"
                and (content_uri is None or not isinstance(size, int) or size < 0)
            )
            or kind not in observed_kinds
            or relative_path != "/" + uri.removeprefix("storage://")
            or isinstance(size, bool)
            or not isinstance(size, int)
            or intent.get("initial_status") != "PENDING"
            or not _rfc3339_timestamp(intent.get("upload_completed_at"))
            or not isinstance(before, Mapping)
            or set(before) != expected_probe_fields
            or before.get("status") != 200
            or before.get("sha256_header") != digest
            or before.get("size_bytes") != size
            or str(before.get("storage_result_header", "")).lower() != "present"
            or intent.get("recovery_action") not in {"reconcile", "retry"}
            or not str(intent.get("recovery_action_id", "")).strip()
            or not isinstance(after, Mapping)
            or set(after) != expected_probe_fields
            or after.get("status") != 404
            or str(after.get("storage_result_header", "")).lower()
            != "object-not-found"
        ):
            raise GateError(
                "Problem artifact GC evidence does not prove every correlated object was reclaimed"
            )
        uris.add(uri)
        observed_kinds[kind] += 1
        expected_head_paths.append(
            "/internal/apis/storage.object.head" + relative_path
        )
        expected_delete_paths.append(
            "/internal/apis/storage.object.delete" + relative_path
        )
    if (
        observed_kinds["package"] != setup.get("package_intent_count")
        or observed_kinds["content"] != setup.get("content_intent_count")
        or observed_kinds["package"] < 1
        or observed_kinds["content"] < 1
        or evidence.get("gateway_storage_head_paths") != sorted(expected_head_paths)
        or evidence.get("gateway_storage_delete_paths")
        != sorted(expected_delete_paths)
    ):
        raise GateError(
            "Problem artifact GC evidence does not cover every correlated Gateway Binding path"
        )

    _verify_problem_artifact_gc_failure_recovery(
        evidence,
        recovery,
        intents,
        expected_probe_fields,
    )


def _verify_problem_artifact_gc_failure_recovery(
    evidence: Mapping[str, Any],
    recovery: Any,
    intents: Sequence[Any],
    expected_probe_fields: set[str],
) -> None:
    if not isinstance(recovery, Mapping) or set(recovery) != {
        "target_uri",
        "target_sha256",
        "target_size_bytes",
        "state_chain",
        "binding_context_proof",
        "route_fault_injection",
        "fault_provider",
        "targeted_reconcile",
        "needs_attention",
        "route_restore",
        "object_before_operator_retry",
        "operator_retry",
        "ledger_absent_after_retry",
        "object_absent_after_retry",
    }:
        raise GateError("Problem artifact GC failure-recovery evidence is non-canonical")
    target_uri = str(recovery.get("target_uri", ""))
    target = next(
        (
            item
            for item in intents
            if isinstance(item, Mapping) and item.get("artifact_uri") == target_uri
        ),
        None,
    )
    if (
        not isinstance(target, Mapping)
        or target.get("kind") != "content"
        or recovery.get("target_sha256") != target.get("sha256")
        or recovery.get("target_size_bytes") != target.get("size_bytes")
        or recovery.get("state_chain")
        != ["PENDING", "NEEDS_ATTENTION", "PENDING", "ABSENT"]
        or recovery.get("ledger_absent_after_retry") is not True
        or recovery.get("object_absent_after_retry") is not True
        or target.get("recovery_action") != "retry"
    ):
        raise GateError("Problem artifact GC recovery target/state chain is invalid")

    fault_route = recovery.get("route_fault_injection", {})
    restore = recovery.get("route_restore", {})
    route_fields = {
        "requirement_name",
        "api_id",
        "api_version",
        "old_provider_deployment_id",
        "old_provider_endpoint",
        "new_provider_deployment_id",
        "new_provider_endpoint",
        "consumer_deployment_id",
        "consumer_endpoint",
        "required_binding_preserved",
        "revision_id",
        "operation_id",
        "operation_status",
        "context_generation_before",
        "context_generation_after",
        "binding_desired_state",
        "binding_observed_state",
    }
    if (
        not isinstance(fault_route, Mapping)
        or not isinstance(restore, Mapping)
        or set(fault_route) != route_fields
        or set(restore) != route_fields
    ):
        raise GateError(
            "Problem artifact GC provider fault-injection/restore evidence is non-canonical"
        )
    stable_route_fields = {
        "requirement_name",
        "api_id",
        "api_version",
        "consumer_deployment_id",
        "consumer_endpoint",
    }
    if (
        any(
            fault_route.get(field) != restore.get(field)
            for field in stable_route_fields
        )
        or fault_route.get("requirement_name") != "storage_head"
        or fault_route.get("api_id") != "storage.object.head"
        or fault_route.get("api_version") != "1.0.0"
        or not str(fault_route.get("old_provider_deployment_id", "")).strip()
        or not str(fault_route.get("new_provider_deployment_id", "")).strip()
        or fault_route.get("old_provider_deployment_id")
        == fault_route.get("new_provider_deployment_id")
        or not str(fault_route.get("consumer_deployment_id", "")).strip()
        or not str(fault_route.get("revision_id", "")).strip()
        or not str(restore.get("revision_id", "")).strip()
        or fault_route.get("revision_id") == restore.get("revision_id")
        or not str(fault_route.get("operation_id", "")).strip()
        or not str(restore.get("operation_id", "")).strip()
        or fault_route.get("operation_id") == restore.get("operation_id")
        or str(fault_route.get("operation_status", "")).upper() != "SUCCEEDED"
        or str(restore.get("operation_status", "")).upper() != "SUCCEEDED"
        or fault_route.get("required_binding_preserved") is not True
        or restore.get("required_binding_preserved") is not True
        or str(fault_route.get("binding_desired_state", "")).upper() != "ACTIVE"
        or str(fault_route.get("binding_observed_state", "")).upper() != "ACTIVE"
        or str(restore.get("binding_desired_state", "")).upper() != "ACTIVE"
        or str(restore.get("binding_observed_state", "")).upper() != "ACTIVE"
        or fault_route.get("old_provider_deployment_id")
        != restore.get("new_provider_deployment_id")
        or fault_route.get("new_provider_deployment_id")
        != restore.get("old_provider_deployment_id")
        or fault_route.get("old_provider_endpoint")
        != restore.get("new_provider_endpoint")
        or fault_route.get("new_provider_endpoint")
        != restore.get("old_provider_endpoint")
        or not _strictly_increasing_positive_ints(
            fault_route.get("context_generation_before"),
            fault_route.get("context_generation_after"),
            restore.get("context_generation_after"),
        )
        or restore.get("context_generation_before")
        != fault_route.get("context_generation_after")
        or evidence.get("latest_topology_etag")
        != f'"{restore.get("revision_id", "")}"'
    ):
        raise GateError(
            "Problem artifact GC did not preserve the required Binding across "
            "compatible provider fault injection and restore"
        )

    context_proof = recovery.get("binding_context_proof", {})
    context_phase_fields = {
        "source",
        "required_binding_names",
        "required_bindings_complete",
        "storage_head_binding_id",
        "storage_head_api_id",
        "storage_head_provider_deployment_id",
        "binding_desired_state",
        "binding_observed_state",
        "context_generation",
    }
    expected_problem_bindings = [
        "permission_check",
        "storage_delete",
        "storage_head",
        "storage_put",
    ]
    if (
        not isinstance(context_proof, Mapping)
        or set(context_proof)
        != {"expected_required_bindings", "initial", "fault_provider", "restored"}
        or context_proof.get("expected_required_bindings")
        != expected_problem_bindings
    ):
        raise GateError(
            "Problem artifact GC required ServiceContext proof is non-canonical"
        )
    context_expectations = (
        (
            "initial",
            fault_route.get("old_provider_deployment_id"),
            fault_route.get("context_generation_before"),
        ),
        (
            "fault_provider",
            fault_route.get("new_provider_deployment_id"),
            fault_route.get("context_generation_after"),
        ),
        (
            "restored",
            restore.get("new_provider_deployment_id"),
            restore.get("context_generation_after"),
        ),
    )
    context_binding_ids: set[str] = set()
    for phase, expected_provider_id, expected_generation in context_expectations:
        phase_proof = context_proof.get(phase, {})
        if (
            not isinstance(phase_proof, Mapping)
            or set(phase_proof) != context_phase_fields
            or phase_proof.get("source")
            != "agent-materialized-service-context+deployment-binding-api"
            or phase_proof.get("required_binding_names")
            != expected_problem_bindings
            or phase_proof.get("required_bindings_complete") is not True
            or phase_proof.get("storage_head_api_id") != "storage.object.head"
            or not str(phase_proof.get("storage_head_binding_id", "")).strip()
            or phase_proof.get("storage_head_provider_deployment_id")
            != expected_provider_id
            or str(phase_proof.get("binding_desired_state", "")).upper()
            != "ACTIVE"
            or str(phase_proof.get("binding_observed_state", "")).upper()
            != "ACTIVE"
            or phase_proof.get("context_generation") != expected_generation
        ):
            raise GateError(
                "Problem artifact GC did not derive required Binding preservation "
                "from the Agent ServiceContext and durable binding API"
            )
        context_binding_ids.add(str(phase_proof["storage_head_binding_id"]))
    if len(context_binding_ids) != 1:
        raise GateError(
            "Problem artifact GC changed storage_head binding identity during provider rebind"
        )

    fault_provider = recovery.get("fault_provider", {})
    fault_probe = (
        fault_provider.get("head_probe", {})
        if isinstance(fault_provider, Mapping)
        else {}
    )
    expected_fault_path = "/api/storage/objects" + str(target.get("relative_path", ""))
    if (
        not isinstance(fault_provider, Mapping)
        or set(fault_provider)
        != {
            "service_id",
            "deployment_id",
            "endpoint",
            "api_id",
            "api_version",
            "management_mode",
            "observed_state",
            "health",
            "head_path",
            "head_request_observed",
            "head_probe",
            "storage_result_header_present",
        }
        or fault_provider.get("service_id")
        != "storage-head-provenance-miss-provider"
        or fault_provider.get("deployment_id")
        != fault_route.get("new_provider_deployment_id")
        or fault_provider.get("endpoint") != fault_route.get("new_provider_endpoint")
        or fault_provider.get("api_id") != "storage.object.head"
        or fault_provider.get("api_version") != "1.0.0"
        or str(fault_provider.get("management_mode", "")).upper() != "EXTERNAL"
        or str(fault_provider.get("observed_state", "")).upper() != "RUNNING"
        or str(fault_provider.get("health", "")).upper() != "HEALTHY"
        or fault_provider.get("head_path") != expected_fault_path
        or fault_provider.get("head_request_observed") is not True
        or fault_provider.get("storage_result_header_present") is not False
        or not isinstance(fault_probe, Mapping)
        or set(fault_probe) != expected_probe_fields
        or fault_probe.get("status") != 404
        or str(fault_probe.get("storage_result_header", ""))
    ):
        raise GateError(
            "Problem artifact GC fault provider is not a healthy compatible provider "
            "with an observed unproven HEAD 404"
        )

    reconcile = recovery.get("targeted_reconcile", {})
    retry = recovery.get("operator_retry", {})
    action_fields = {
        "endpoint",
        "first_http_status",
        "replay_http_status",
        "action_id",
        "request_id",
        "artifact_uri",
        "queued",
        "first_request_replay",
        "duplicate_request_replay",
        "duplicate_action_id_matched",
        "duplicate_request_id_matched",
        "idempotency_key_used",
        "idempotency_key_recorded",
        "reason_recorded",
        "from_status",
        "to_status",
    }
    if (
        not isinstance(reconcile, Mapping)
        or set(reconcile) != action_fields | {"operator_reason", "exact_identity_submitted"}
        or not isinstance(retry, Mapping)
        or set(retry) != action_fields | {"operator_reason", "expected_failure_count"}
    ):
        raise GateError("Problem artifact GC operator action evidence is non-canonical")
    for action, expected_from, expected_endpoint in (
        (
            reconcile,
            "PENDING",
            "/api/problem/admin/artifact-gc/intents:reconcile",
        ),
        (
            retry,
            "NEEDS_ATTENTION",
            "/api/problem/admin/artifact-gc/intents:retry",
        ),
    ):
        if (
            action.get("endpoint") != expected_endpoint
            or isinstance(action.get("first_http_status"), bool)
            or not isinstance(action.get("first_http_status"), int)
            or action.get("first_http_status") != 202
            or isinstance(action.get("replay_http_status"), bool)
            or not isinstance(action.get("replay_http_status"), int)
            or action.get("replay_http_status") != 202
            or action.get("artifact_uri") != target_uri
            or not str(action.get("action_id", "")).strip()
            or not str(action.get("request_id", "")).strip()
            or action.get("queued") is not True
            or action.get("first_request_replay") is not False
            or action.get("duplicate_request_replay") is not True
            or action.get("duplicate_action_id_matched") is not True
            or action.get("duplicate_request_id_matched") is not True
            or action.get("idempotency_key_used") is not True
            or action.get("idempotency_key_recorded") is not False
            or action.get("reason_recorded") is not True
            or action.get("from_status") != expected_from
            or action.get("to_status") != "PENDING"
            or not str(action.get("operator_reason", "")).strip()
        ):
            raise GateError("Problem artifact GC operator action/idempotency proof is invalid")
    if (
        reconcile.get("exact_identity_submitted") is not True
        or retry.get("action_id") != target.get("recovery_action_id")
        or reconcile.get("action_id") == retry.get("action_id")
    ):
        raise GateError("Problem artifact GC actions are not causally tied to the target")

    attention = recovery.get("needs_attention", {})
    last_failure = attention.get("last_failure", {}) if isinstance(attention, Mapping) else {}
    if (
        not isinstance(attention, Mapping)
        or set(attention)
        != {
            "status",
            "failure_count",
            "last_failure",
            "upload_completed_at",
            "manual_reconcile_requested_at",
            "manual_reconcile_marker_consumed",
            "needs_attention_at",
            "ledger_preserved",
            "claim_credential_exposed",
        }
        or attention.get("status") != "NEEDS_ATTENTION"
        or isinstance(attention.get("failure_count"), bool)
        or not isinstance(attention.get("failure_count"), int)
        or int(attention.get("failure_count", 0)) < 1
        or retry.get("expected_failure_count") != attention.get("failure_count")
        or attention.get("upload_completed_at") != target.get("upload_completed_at")
        or not _rfc3339_timestamp(attention.get("upload_completed_at"))
        or str(attention.get("manual_reconcile_requested_at", "")).strip()
        or attention.get("manual_reconcile_marker_consumed") is not True
        or not _rfc3339_timestamp(attention.get("needs_attention_at"))
        or attention.get("ledger_preserved") is not True
        or attention.get("claim_credential_exposed") is not False
        or not isinstance(last_failure, Mapping)
        or set(last_failure)
        != {
            "message",
            "stage",
            "kind",
            "http_status",
            "provider_result",
            "deterministic",
        }
        or not str(last_failure.get("message", "")).strip()
        or last_failure.get("stage") != "inspect"
        or last_failure.get("kind") != "PROVIDER_HTTP"
        or isinstance(last_failure.get("http_status"), bool)
        or not isinstance(last_failure.get("http_status"), int)
        or last_failure.get("http_status") != 404
        or last_failure.get("provider_result") != "HTTP_404"
        or last_failure.get("deterministic") is not True
    ):
        raise GateError(
            "Problem artifact GC does not prove an unproven Gateway 404 retained the ledger"
        )

    before_retry = recovery.get("object_before_operator_retry", {})
    if (
        not isinstance(before_retry, Mapping)
        or set(before_retry) != expected_probe_fields
        or before_retry.get("status") != 200
        or before_retry.get("sha256_header") != target.get("sha256")
        or before_retry.get("size_bytes") != target.get("size_bytes")
        or str(before_retry.get("storage_result_header", "")).lower() != "present"
    ):
        raise GateError("Problem artifact GC route 404 did not preserve the exact object")


def _strictly_increasing_positive_ints(*values: Any) -> bool:
    return all(
        not isinstance(item, bool) and isinstance(item, int) and item > 0
        for item in values
    ) and all(left < right for left, right in zip(values, values[1:]))


def _rfc3339_timestamp(value: Any) -> bool:
    return isinstance(value, str) and re.fullmatch(
        r"[0-9]{4}-[0-9]{2}-[0-9]{2}T"
        r"[0-9]{2}:[0-9]{2}:[0-9]{2}"
        r"(?:\.[0-9]{1,9})?(?:Z|[+-][0-9]{2}:[0-9]{2})",
        value,
    ) is not None


def _verify_actual_flow_correlation(flow: Mapping[str, Any]) -> None:
    problem = flow.get("problem", {})
    projection = flow.get("judge_projection", {})
    submission = flow.get("submission", {})
    task = flow.get("task", {})
    result = flow.get("result", {})
    if not all(isinstance(item, Mapping) for item in (problem, projection, submission, task, result)):
        raise GateError("actual-service chain evidence is missing a correlation section")
    problem_id = str(problem.get("problem_id", "")).strip()
    event_id = str(problem.get("outbox_event_id", "")).strip()
    if (
        not problem_id
        or int(problem.get("aggregate_version", 0)) < 1
        or int(problem.get("package_revision", 0)) < 1
        or not event_id
        or problem.get("event_type") != "io.ojos.problem.snapshot.v1"
        or not _sha256_digest(problem.get("package_sha256"))
    ):
        raise GateError("Problem aggregate/outbox evidence is incomplete")
    if (
        str(projection.get("problem_id", "")) != problem_id
        or projection.get("event_id") != event_id
        or int(projection.get("aggregate_version", 0)) != int(problem.get("aggregate_version", 0))
        or projection.get("package_sha256") != problem.get("package_sha256")
    ):
        raise GateError("Judge projection is not correlated to the Problem outbox event")
    submission_id = str(submission.get("submission_id", "")).strip()
    task_id = str(task.get("task_id", "")).strip()
    result_id = str(result.get("result_id", "")).strip()
    if (
        not submission_id
        or str(submission.get("problem_id", "")) != problem_id
        or submission.get("package_sha256") != problem.get("package_sha256")
        or not task_id
        or str(task.get("submission_id", "")) != submission_id
        or str(task.get("problem_id", "")) != problem_id
        or not result_id
        or str(result.get("task_id", "")) != task_id
        or str(result.get("submission_id", "")) != submission_id
        or str(result.get("status", "")).upper() != "ACCEPTED"
    ):
        raise GateError("submission/task/result IDs do not prove one actual Judge execution")
    validate_production_resource_ref(task.get("source", {}))
    validate_production_resource_ref(task.get("problem_package", {}))


def _verify_worker_install_failure_compensation(
    value: Mapping[str, Any],
    *,
    worker_deployment_id: str,
    successful_operation_id: str,
    logical_endpoint: str,
    node_b_instance_id: str,
) -> None:
    proof = value.get("worker_install_failure_compensation", {})
    fault = proof.get("fault", {}) if isinstance(proof, Mapping) else {}
    failed = proof.get("failed_deployment", {}) if isinstance(proof, Mapping) else {}
    operation = proof.get("operation", {}) if isinstance(proof, Mapping) else {}
    attempt = proof.get("agent_attempt", {}) if isinstance(proof, Mapping) else {}
    container = proof.get("container_readback", {}) if isinstance(proof, Mapping) else {}
    volume = proof.get("volume_readback", {}) if isinstance(proof, Mapping) else {}
    context = proof.get("context_readback", {}) if isinstance(proof, Mapping) else {}
    runtime = proof.get("runtime_readback", {}) if isinstance(proof, Mapping) else {}
    bindings = proof.get("binding_readback", {}) if isinstance(proof, Mapping) else {}
    database = (
        proof.get("control_plane_database_readback", {})
        if isinstance(proof, Mapping)
        else {}
    )
    topology = proof.get("topology_readback", {}) if isinstance(proof, Mapping) else {}
    gateway_projection = (
        proof.get("gateway_active_projection_readback", {})
        if isinstance(proof, Mapping)
        else {}
    )
    auth_projection = (
        proof.get("auth_active_projection_readback", {})
        if isinstance(proof, Mapping)
        else {}
    )
    durable_bindings = (
        proof.get("durable_binding_set_readback", {})
        if isinstance(proof, Mapping)
        else {}
    )
    recovery = (
        proof.get("recovery_rollback", {})
        if isinstance(proof, Mapping)
        else {}
    )
    gateway = proof.get("gateway_recovery", {}) if isinstance(proof, Mapping) else {}
    component = hashlib.sha256(worker_deployment_id.encode("utf-8")).hexdigest()[:32]
    expected_volume = "ojos-judge-cache-" + component
    expected_context = "/var/lib/ojos-agent/runtime-contexts/" + component
    gateway_container_id = str(fault.get("container_id", ""))

    if (
        not isinstance(proof, Mapping)
        or fault.get("kind") != "stop-container"
        or fault.get("component") != "gateway-tls-a"
        or not re.fullmatch(r"[a-f0-9]{64}", gateway_container_id)
        or int(fault.get("started_at_unix_ms", 0)) <= 0
        or fault.get("running_before_fault") is not True
        or fault.get("running_at_install_start") is not False
        or fault.get("running_at_install_completion") is not False
        or failed.get("deployment_id") != worker_deployment_id
        or failed.get("node_id") != "node-b"
        or failed.get("logical_endpoint") != logical_endpoint
        or failed.get("cache_volume_name") != expected_volume
        or failed.get("context_directory") != expected_context
    ):
        raise GateError(
            "Worker install compensation evidence does not prove the real TLS Gateway fault"
        )
    failed_operation_id = str(operation.get("operation_id", ""))
    removed_container_id = str(attempt.get("removed_container_id", ""))
    last_health_observation = attempt.get("last_health_observation", {})
    if (
        not failed_operation_id
        or failed_operation_id == successful_operation_id
        or str(operation.get("status", "")).upper() != "FAILED"
        or operation.get("needs_attention") is not False
        or int(operation.get("attention_job_ids_count", -1)) != 0
        or operation.get("resource_cleanup_derived_from_operation_result") is not False
        or not str(attempt.get("job_id", "")).strip()
        or attempt.get("node_id") != "node-b"
        or attempt.get("lease_owner_instance_id") != node_b_instance_id
        or isinstance(attempt.get("attempt"), bool)
        or not isinstance(attempt.get("attempt"), int)
        or int(attempt.get("attempt", 0)) < 1
        or str(attempt.get("status", "")).upper() != "FAILED"
        or attempt.get("result_action") != "install"
        or attempt.get("result_compensated") is not True
        or not re.fullmatch(r"[a-f0-9]{64}", removed_container_id)
        or attempt.get("failure_health_gate") not in {"failed", "timeout"}
        or isinstance(attempt.get("failure_probe_count"), bool)
        or not isinstance(attempt.get("failure_probe_count"), int)
        or int(attempt.get("failure_probe_count", 0)) < 1
        or not isinstance(last_health_observation, Mapping)
        or int(last_health_observation.get("probe", 0)) < 1
        or int(last_health_observation.get("probe", 0))
        > int(attempt.get("failure_probe_count", 0))
        or str(last_health_observation.get("observed_state", "")).upper()
        != "RUNNING"
        or str(last_health_observation.get("health", "")).upper()
        not in {"STARTING", "UNHEALTHY"}
        or not str(last_health_observation.get("probe_reason", "")).strip()
        or attempt.get("post_start_health_gate_failure") is not True
    ):
        raise GateError(
            "Worker compensation fault was not a distinct FAILED Agent attempt without NEEDS_ATTENTION"
        )
    if (
        container.get("source") != "docker-ps-by-deployment-label"
        or container.get("deployment_id") != worker_deployment_id
        or container.get("expected_name") != "ojos-" + worker_deployment_id
        or container.get("matches") != []
        or int(container.get("exact_name_inspect_exit_code", 0)) == 0
        or container.get("exact_name_absent") is not True
        or container.get("absent") is not True
        or volume.get("source") != "docker-volume-ls-by-deployment-label"
        or volume.get("deployment_id") != worker_deployment_id
        or volume.get("expected_name") != expected_volume
        or volume.get("matches") != []
        or int(volume.get("exact_name_inspect_exit_code", 0)) == 0
        or volume.get("exact_name_absent") is not True
        or volume.get("absent") is not True
        or context.get("source") != "node-b-agent-host-filesystem"
        or context.get("deployment_id") != worker_deployment_id
        or context.get("path") != expected_context
        or context.get("exists") is not False
        or context.get("context_or_credential_file_present") is not False
    ):
        raise GateError(
            "Worker compensation evidence left a container, cache volume, or Agent context"
        )
    if (
        runtime.get("source") != "GET /api/v1/deployments/{deploymentId}"
        or int(runtime.get("http_status", 0)) != 404
        or runtime.get("problem_code") != "DEPLOYMENT_NOT_FOUND"
        or runtime.get("fake_running_projection_present") is not False
        or bindings.get("source")
        != "GET /api/v1/deployments/{deploymentId}/bindings"
        or int(bindings.get("http_status", 0)) != 404
        or bindings.get("problem_code") != "DEPLOYMENT_NOT_FOUND"
        or bindings.get("staged_or_active_present") is not False
    ):
        raise GateError(
            "Worker compensation public API read-back retained a runtime or binding projection"
        )
    if (
        database.get("query_mode")
        != "postgres-fixed-parameterized-read-only-transaction"
        or database.get("control_plane_database_read_only_verification") is not True
        or database.get("business_database_write_used") is not False
        or database.get("row_payload_recorded") is not False
        or int(database.get("runtime_instance_count", -1)) != 0
        or int(database.get("binding_count", -1)) != 0
        or int(database.get("active_or_staged_binding_count", -1)) != 0
    ):
        raise GateError(
            "Worker compensation did not independently prove zero control-plane runtime/binding rows"
        )
    selected_revision = str(topology.get("selected_revision_id", ""))
    proposed_revision = str(topology.get("operation_proposed_revision_id", ""))
    recovery_revision = str(topology.get("recovery_revision_id", ""))
    draft_etag = topology.get("draft_etag_after")
    selected_etag = topology.get("selected_revision_readback_etag")
    retry_etag = (
        value.get("store_agent_evidence", {})
        .get("store_validate", {})
        .get("topology_etag")
    )
    selected_spec_sha = topology.get("selected_spec_sha256_before")
    expected_providers = {
        str(binding.get("provider_deployment_id", ""))
        for binding in value.get("store_agent_evidence", {})
        .get("store_validate", {})
        .get("bindings", [])
        if isinstance(binding, Mapping) and binding.get("provider_deployment_id")
    }
    status_tuple = (
        str(topology.get("status_state_after", "")).upper(),
        topology.get("desired_revision_id_after"),
        topology.get("observed_revision_id_after"),
        topology.get("status_last_operation_id"),
    )
    accepted_status_tuples = (
        ("FAILED", proposed_revision, selected_revision, failed_operation_id),
        ("IN_SYNC", selected_revision, selected_revision, failed_operation_id),
    )
    if (
        topology.get("source") != "GET /api/v1/topologies/{topologyId}"
        or not str(topology.get("topology_id", "")).strip()
        or not selected_revision
        or not proposed_revision
        or proposed_revision == selected_revision
        or topology.get("baseline_status_desired_revision_id")
        != selected_revision
        or topology.get("baseline_status_observed_revision_id")
        != selected_revision
        or str(topology.get("baseline_status_state", "")).upper() != "IN_SYNC"
        or topology.get("baseline_status_drift") != []
        or topology.get("draft_revision_id_after") != proposed_revision
        or topology.get("applied_revision_id_after") != selected_revision
        or status_tuple not in accepted_status_tuples
        or topology.get("status_drift") != []
        or not _strong_etag(draft_etag)
        or draft_etag != f'"{proposed_revision}"'
        or not recovery_revision
        or recovery_revision in {selected_revision, proposed_revision}
        or topology.get("next_retry_etag") != f'"{recovery_revision}"'
        or retry_etag != f'"{recovery_revision}"'
        or topology.get("applying_revision_present") is not False
        or not _strong_etag(selected_etag)
        or selected_etag != f'"{selected_revision}"'
        or isinstance(topology.get("selected_revision_number"), bool)
        or not isinstance(topology.get("selected_revision_number"), int)
        or int(topology.get("selected_revision_number", 0)) < 1
        or not re.fullmatch(
            r"[0-9a-f]{64}", str(topology.get("selected_content_sha256", ""))
        )
        or not _sha256_digest(selected_spec_sha)
        or topology.get("selected_spec_sha256_readback") != selected_spec_sha
        or topology.get("failed_draft_readback_etag") != draft_etag
        or topology.get("failed_draft_parent_revision_id") != selected_revision
        or topology.get("failed_draft_rollback_of_revision_id") is not None
        or topology.get("failed_draft_revision_number")
        != int(topology.get("selected_revision_number", 0)) + 1
        or not re.fullmatch(
            r"[0-9a-f]{64}",
            str(topology.get("failed_draft_content_sha256", "")),
        )
        or topology.get("failed_draft_content_sha256")
        == topology.get("selected_content_sha256")
        or not _sha256_digest(topology.get("failed_draft_spec_sha256"))
        or int(topology.get("failed_draft_endpoint_count", -1)) != 1
        or int(topology.get("failed_draft_link_count", -1)) != 2
        or set(topology.get("failed_draft_requirements", []))
        != {"judge_control", "storage_get"}
        or set(topology.get("failed_draft_provider_deployment_ids", []))
        != expected_providers
        or len(expected_providers) != 2
        or topology.get("failed_draft_retained") is not True
        or topology.get("applied_runtime_preserved") is not True
    ):
        raise GateError(
            "Worker compensation did not preserve the failed immutable draft over the prior applied Topology"
        )
    if (
        gateway_projection.get("source")
        != "redis-get-gateway-topology-projection"
        or gateway_projection.get("key")
        != "ojos:gateway:topology-projection:v1:"
        + str(topology.get("topology_id", ""))
        or gateway_projection.get("index_key")
        != "ojos:gateway:topology-projections:v1"
        or int(gateway_projection.get("index_member_before", 0)) != 1
        or int(gateway_projection.get("index_member_after", 0)) != 1
        or gateway_projection.get("provider") != "gateway"
        or gateway_projection.get("topology_id") != topology.get("topology_id")
        or gateway_projection.get("active_revision_id") != selected_revision
        or gateway_projection.get("active_content_sha256")
        != topology.get("selected_content_sha256")
        or gateway_projection.get("active_spec_sha256") != selected_spec_sha
        or int(gateway_projection.get("active_route_count", 0)) < 1
        or int(gateway_projection.get("active_grant_count", 0)) < 1
        or not _sha256_digest(gateway_projection.get("business_sha256_before"))
        or gateway_projection.get("business_sha256_after")
        != gateway_projection.get("business_sha256_before")
        or not _sha256_digest(gateway_projection.get("routes_sha256_before"))
        or gateway_projection.get("routes_sha256_after")
        != gateway_projection.get("routes_sha256_before")
        or not _sha256_digest(gateway_projection.get("grants_sha256_before"))
        or gateway_projection.get("grants_sha256_after")
        != gateway_projection.get("grants_sha256_before")
        or int(gateway_projection.get("failed_deployment_route_count", -1)) != 0
        or int(gateway_projection.get("failed_deployment_grant_count", -1)) != 0
        or int(gateway_projection.get("failed_deployment_endpoint_count", -1)) != 0
        or gateway_projection.get("previous_projection_preserved") is not True
        or gateway_projection.get("business_database_write_used") is not False
    ):
        raise GateError(
            "Gateway active projection retained the failed Worker or lost the prior applied routes"
        )
    if (
        auth_projection.get("source")
        != "postgres-auth-projection-read-only-transaction"
        or auth_projection.get("provider") != "auth"
        or auth_projection.get("topology_id") != topology.get("topology_id")
        or auth_projection.get("active_revision_id") != selected_revision
        or auth_projection.get("active_content_sha256")
        != topology.get("selected_content_sha256")
        or auth_projection.get("active_spec_sha256") != selected_spec_sha
        or int(auth_projection.get("active_route_count", 0)) < 1
        or int(auth_projection.get("active_grant_count", 0)) < 1
        or not _sha256_digest(auth_projection.get("business_sha256_before"))
        or auth_projection.get("business_sha256_after")
        != auth_projection.get("business_sha256_before")
        or not _sha256_digest(auth_projection.get("routes_sha256_before"))
        or auth_projection.get("routes_sha256_after")
        != auth_projection.get("routes_sha256_before")
        or not _sha256_digest(auth_projection.get("grants_sha256_before"))
        or auth_projection.get("grants_sha256_after")
        != auth_projection.get("grants_sha256_before")
        or int(auth_projection.get("materialized_grant_count_before", 0)) < 1
        or auth_projection.get("materialized_grant_count_after")
        != auth_projection.get("materialized_grant_count_before")
        or not _sha256_digest(
            auth_projection.get("materialized_grants_sha256_before")
        )
        or auth_projection.get("materialized_grants_sha256_after")
        != auth_projection.get("materialized_grants_sha256_before")
        or int(auth_projection.get("failed_deployment_route_count", -1)) != 0
        or int(auth_projection.get("failed_deployment_grant_count", -1)) != 0
        or int(
            auth_projection.get("failed_deployment_materialized_grant_count", -1)
        )
        != 0
        or auth_projection.get("previous_projection_preserved") is not True
        or auth_projection.get("business_database_write_used") is not False
    ):
        raise GateError(
            "Auth active projection or materialized grant set changed during Worker compensation"
        )
    binding_count = durable_bindings.get("binding_count_before")
    if (
        durable_bindings.get("source")
        != "postgres-control-plane-bindings-read-only-transaction"
        or durable_bindings.get("selected_revision_id") != selected_revision
        or isinstance(binding_count, bool)
        or not isinstance(binding_count, int)
        or binding_count < 1
        or durable_bindings.get("binding_count_after") != binding_count
        or durable_bindings.get("active_count_before") != binding_count
        or durable_bindings.get("active_count_after") != binding_count
        or int(durable_bindings.get("non_active_count_after", -1)) != 0
        or int(durable_bindings.get("wrong_revision_count_after", -1)) != 0
        or not _sha256_digest(durable_bindings.get("rows_sha256_before"))
        or durable_bindings.get("rows_sha256_after")
        != durable_bindings.get("rows_sha256_before")
        or int(durable_bindings.get("failed_deployment_binding_count", -1)) != 0
        or durable_bindings.get("exactly_preserved") is not True
        or durable_bindings.get("business_database_write_used") is not False
    ):
        raise GateError(
            "durable applied Binding set changed or retained the failed Worker"
        )

    rollback_operation_id = str(recovery.get("operation_id", ""))
    affected_consumers = recovery.get("affected_consumer_deployment_ids", [])
    generation_map_names = (
        "gateway_consumer_generations",
        "auth_consumer_generations",
        "auth_materialized_consumer_generations",
        "durable_consumer_generations",
    )
    generation_maps_before = {
        name: recovery.get(name + "_before", {}) for name in generation_map_names
    }
    generation_maps_recovered = {
        name: recovery.get(name + "_recovered", {})
        for name in generation_map_names
    }
    affected_consumer_set = (
        set(affected_consumers) if isinstance(affected_consumers, list) else set()
    )
    generation_maps_valid = (
        len(affected_consumer_set) == 2
        and len(affected_consumer_set) == len(affected_consumers)
        and all(
            isinstance(generations, Mapping)
            and set(generations) == affected_consumer_set
            and all(
                not isinstance(generation, bool)
                and isinstance(generation, int)
                and generation >= 1
                for generation in generations.values()
            )
            for generations in (
                *generation_maps_before.values(),
                *generation_maps_recovered.values(),
            )
        )
    )
    gateway_generations_before = generation_maps_before[
        "gateway_consumer_generations"
    ]
    gateway_generations_recovered = generation_maps_recovered[
        "gateway_consumer_generations"
    ]
    if generation_maps_valid:
        generation_maps_valid = (
            all(
                generations == gateway_generations_before
                for generations in generation_maps_before.values()
            )
            and all(
                generations == gateway_generations_recovered
                for generations in generation_maps_recovered.values()
            )
            and all(
                gateway_generations_recovered[consumer]
                == gateway_generations_before[consumer] + 1
                for consumer in affected_consumer_set
            )
        )
    if (
        recovery.get("api_path")
        != f"/api/v1/topologies/{topology.get('topology_id')}:rollback"
        or recovery.get("topology_id") != topology.get("topology_id")
        or recovery.get("request_revision_id") != selected_revision
        or recovery.get("request_if_match") != f'"{proposed_revision}"'
        or recovery.get("target_revision_id") != selected_revision
        or recovery.get("target_revision_number")
        != topology.get("selected_revision_number")
        or recovery.get("target_content_sha256")
        != topology.get("selected_content_sha256")
        or recovery.get("target_spec_sha256") != selected_spec_sha
        or recovery.get("parent_revision_id") != proposed_revision
        or recovery.get("parent_revision_number")
        != topology.get("failed_draft_revision_number")
        or recovery.get("parent_content_sha256")
        != topology.get("failed_draft_content_sha256")
        or recovery.get("created_revision_id") != recovery_revision
        or recovery.get("created_revision_number")
        != int(topology.get("failed_draft_revision_number", 0)) + 1
        or recovery.get("created_parent_revision_id") != proposed_revision
        or recovery.get("created_rollback_of_revision_id") != selected_revision
        or recovery.get("created_content_sha256")
        != topology.get("selected_content_sha256")
        or recovery.get("created_spec_sha256") != selected_spec_sha
        or recovery.get("created_revision_etag") != f'"{recovery_revision}"'
        or not rollback_operation_id
        or rollback_operation_id
        in {failed_operation_id, successful_operation_id}
        or recovery.get("operation_action") != "topology.rollback"
        or str(recovery.get("operation_status", "")).upper() != "SUCCEEDED"
        or recovery.get("draft_revision_id") != recovery_revision
        or recovery.get("applied_revision_id") != recovery_revision
        or recovery.get("applying_revision_id") is not None
        or recovery.get("status_desired_revision_id") != recovery_revision
        or recovery.get("status_observed_revision_id") != recovery_revision
        or str(recovery.get("status_state", "")).upper() != "IN_SYNC"
        or recovery.get("status_drift") != []
        or recovery.get("status_last_operation_id") != rollback_operation_id
        or recovery.get("gateway_projection_revision_id") != recovery_revision
        or recovery.get("gateway_projection_content_sha256")
        != topology.get("selected_content_sha256")
        or recovery.get("gateway_projection_spec_sha256") != selected_spec_sha
        or not _sha256_digest(
            recovery.get("gateway_stable_routes_sha256_before")
        )
        or recovery.get("gateway_stable_routes_sha256_recovered")
        != recovery.get("gateway_stable_routes_sha256_before")
        or not _sha256_digest(
            recovery.get("gateway_stable_grants_sha256_before")
        )
        or recovery.get("gateway_stable_grants_sha256_recovered")
        != recovery.get("gateway_stable_grants_sha256_before")
        or int(recovery.get("gateway_index_member", 0)) != 1
        or recovery.get("auth_projection_revision_id") != recovery_revision
        or recovery.get("auth_projection_content_sha256")
        != topology.get("selected_content_sha256")
        or recovery.get("auth_projection_spec_sha256") != selected_spec_sha
        or not _sha256_digest(recovery.get("auth_stable_routes_sha256_before"))
        or recovery.get("auth_stable_routes_sha256_recovered")
        != recovery.get("auth_stable_routes_sha256_before")
        or not _sha256_digest(recovery.get("auth_stable_grants_sha256_before"))
        or recovery.get("auth_stable_grants_sha256_recovered")
        != recovery.get("auth_stable_grants_sha256_before")
        or not _sha256_digest(
            recovery.get("auth_materialized_stable_grants_sha256_before")
        )
        or recovery.get("auth_materialized_stable_grants_sha256_recovered")
        != recovery.get("auth_materialized_stable_grants_sha256_before")
        or int(recovery.get("auth_failed_deployment_grant_count", -1)) != 0
        or recovery.get("durable_binding_count") != binding_count
        or recovery.get("durable_binding_active_count") != binding_count
        or int(recovery.get("durable_binding_non_active_count", -1)) != 0
        or int(recovery.get("durable_binding_wrong_revision_count", -1)) != 0
        or not _sha256_digest(
            recovery.get("durable_binding_business_sha256_before")
        )
        or recovery.get("durable_binding_business_sha256_recovered")
        != recovery.get("durable_binding_business_sha256_before")
        or generation_maps_valid is not True
        or int(recovery.get("each_consumer_generation_increment", 0)) != 1
        or recovery.get("all_generation_sources_aligned") is not True
        or recovery.get("next_retry_etag") != f'"{recovery_revision}"'
        or recovery.get("business_state_preserved") is not True
    ):
        raise GateError(
            "Worker compensation rollback did not create and apply the recovery revision"
        )

    rollbacks = proof.get("consumer_context_rollback", [])
    managed = value.get("managed_a_deployments", {})
    if not isinstance(rollbacks, list) or not isinstance(managed, Mapping):
        raise GateError("Worker compensation consumer context rollback proof is missing")
    by_service = {
        str(item.get("service_id", "")): item
        for item in rollbacks
        if isinstance(item, Mapping)
    }
    if len(by_service) != len(rollbacks) or set(by_service) != {
        "problem-service",
        "judge-api",
    }:
        raise GateError(
            "Worker compensation must prove both existing consumer contexts unchanged"
        )
    for service_id, rollback in by_service.items():
        deployed = managed.get(service_id, {})
        expected_binding_names = set(deployed.get("binding_requirements", []))
        expected_binding_ids = {
            str(item.get("binding_id", ""))
            for item in deployed.get("bindings", [])
            if isinstance(item, Mapping) and item.get("binding_id")
        }
        before_generation = rollback.get("context_generation_before")
        after_generation = rollback.get("context_generation_after")
        recovered_generation = rollback.get("context_generation_recovered")
        before_context_sha = rollback.get("context_sha256_before")
        after_context_sha = rollback.get("context_sha256_after")
        recovered_context_sha = rollback.get("context_sha256_recovered")
        before_credential_sha = rollback.get(
            "workload_credential_file_sha256_before"
        )
        after_credential_sha = rollback.get(
            "workload_credential_file_sha256_after"
        )
        recovered_credential_sha = rollback.get(
            "workload_credential_file_sha256_recovered"
        )
        before_claims = rollback.get("credential_claims_before", {})
        after_claims = rollback.get("credential_claims_after", {})
        recovered_claims = rollback.get("credential_claims_recovered", {})
        stable_claim_fields = (
            "deployment_id",
            "service_id",
            "node_id",
            "issuer",
            "audience",
        )
        routes_before = rollback.get("binding_routes_before", [])
        routes_after = rollback.get("binding_routes_after", [])
        routes_recovered = rollback.get("binding_routes_recovered", [])
        route_items = routes_before if isinstance(routes_before, list) else []
        route_by_binding_id = {
            str(route.get("binding_id", "")): route
            for route in route_items
            if isinstance(route, Mapping) and route.get("binding_id")
        }
        claims_valid = (
            isinstance(before_claims, Mapping)
            and isinstance(after_claims, Mapping)
            and isinstance(recovered_claims, Mapping)
            and all(
                before_claims.get(field) == after_claims.get(field)
                for field in (*stable_claim_fields, "credential_generation")
            )
            and all(
                before_claims.get(field) == recovered_claims.get(field)
                for field in stable_claim_fields
            )
            and before_claims.get("deployment_id") == deployed.get("deployment_id")
            and before_claims.get("service_id") == service_id
            and before_claims.get("node_id") == "node-a"
            and before_claims.get("credential_generation") == before_generation
            and recovered_claims.get("credential_generation")
            == recovered_generation
            and all(
                isinstance(claims.get("expires_at_unix"), int)
                and not isinstance(claims.get("expires_at_unix"), bool)
                and int(claims.get("expires_at_unix", 0)) > 0
                and _sha256_digest(claims.get("jti_sha256"))
                and isinstance(claims.get("issuer"), str)
                and bool(claims.get("issuer"))
                and isinstance(claims.get("audience"), list)
                and bool(claims.get("audience"))
                for claims in (before_claims, after_claims, recovered_claims)
            )
            and int(after_claims.get("expires_at_unix", 0))
            >= int(before_claims.get("expires_at_unix", 0)) - 5
            and int(recovered_claims.get("expires_at_unix", 0))
            >= int(after_claims.get("expires_at_unix", 0)) - 5
        )
        if (
            rollback.get("deployment_id") != deployed.get("deployment_id")
            or rollback.get("node_id") != "node-a"
            or rollback.get("container_id_before") != deployed.get("container_id")
            or rollback.get("container_id_after") != deployed.get("container_id")
            or isinstance(before_generation, bool)
            or not isinstance(before_generation, int)
            or before_generation < 1
            or after_generation != before_generation
            or recovered_generation != before_generation + 1
            or set(rollback.get("binding_names_before", []))
            != expected_binding_names
            or rollback.get("binding_names_after")
            != rollback.get("binding_names_before")
            or rollback.get("binding_names_recovered")
            != rollback.get("binding_names_before")
            or set(rollback.get("binding_ids_before", [])) != expected_binding_ids
            or rollback.get("binding_ids_after") != rollback.get("binding_ids_before")
            or rollback.get("binding_ids_recovered")
            != rollback.get("binding_ids_before")
            or not isinstance(routes_before, list)
            or len(route_by_binding_id) != len(routes_before)
            or set(route_by_binding_id) != expected_binding_ids
            or {
                str(route.get("requirement_name", ""))
                for route in route_items
                if isinstance(route, Mapping)
            }
            != expected_binding_names
            or any(
                not str(route.get("api_id", "")).strip()
                or not str(route.get("base_path", "")).startswith(
                    "/internal/apis/"
                )
                or isinstance(route.get("timeout_ms"), bool)
                or not isinstance(route.get("timeout_ms"), int)
                or int(route.get("timeout_ms", 0)) < 1
                for route in route_by_binding_id.values()
            )
            or routes_after != routes_before
            or routes_recovered != routes_before
            or not _sha256_digest(before_context_sha)
            or after_context_sha != before_context_sha
            or not _sha256_digest(recovered_context_sha)
            or recovered_context_sha == before_context_sha
            or not _sha256_digest(before_credential_sha)
            or not _sha256_digest(after_credential_sha)
            or not _sha256_digest(recovered_credential_sha)
            or recovered_credential_sha == after_credential_sha
            or claims_valid is not True
            or rollback.get("context_content_unchanged") is not True
            or rollback.get("credential_claim_identity_unchanged") is not True
            or rollback.get("credential_expiry_non_decreasing") is not True
            or not isinstance(
                rollback.get("credential_refresh_during_fault_window"), bool
            )
            or rollback.get("credential_refresh_during_fault_window")
            is not (before_credential_sha != after_credential_sha)
            or int(rollback.get("rollback_generation_increment", 0)) != 1
            or rollback.get("context_and_credential_generation_aligned") is not True
            or rollback.get("context_content_rotated") is not True
            or rollback.get("credential_file_rotated") is not True
            or rollback.get("route_identity_preserved") is not True
            or gateway_generations_before.get(deployed.get("deployment_id"))
            != before_generation
            or gateway_generations_recovered.get(deployed.get("deployment_id"))
            != recovered_generation
        ):
            raise GateError(
                f"Worker compensation did not preserve then atomically rotate {service_id} context and credential"
            )
    if {
        str(item.get("deployment_id", ""))
        for item in by_service.values()
    } != affected_consumer_set:
        raise GateError(
            "Worker compensation generation map does not match managed consumers"
        )
    if (
        gateway.get("component") != "gateway-tls-a"
        or gateway.get("container_id_before") != gateway_container_id
        or gateway.get("container_id_after") != gateway_container_id
        or gateway.get("same_container") is not True
        or gateway.get("running") is not True
        or int(gateway.get("public_health_status", 0)) != 200
        or gateway.get("node_b_ready") is not True
        or int(gateway.get("node_b_unhealthy_deployments", -1)) != 0
        or proof.get("credential_material_recorded") is not False
    ):
        raise GateError(
            "TLS Gateway or Node B did not recover cleanly after Worker compensation"
        )


def _verify_provider_projection_integrity(
    evidence: Any,
    *,
    phase: str,
    expected_revision_id: str | None = None,
    expected_content_sha256: str | None = None,
    not_before_unix_ms: int | None = None,
) -> Mapping[str, Any]:
    if not isinstance(evidence, Mapping):
        raise GateError(f"{phase} provider projection integrity evidence is missing")
    captured_at = int(evidence.get("captured_at_unix_ms", 0))
    topology_id = str(evidence.get("topology_id", ""))
    revision_id = str(evidence.get("applied_revision_id", ""))
    content_sha256 = str(evidence.get("applied_content_sha256", ""))
    expected_digest = str(evidence.get("expected_projection_sha256", ""))
    if (
        evidence.get("phase") != phase
        or captured_at < 1
        or (not_before_unix_ms is not None and captured_at < not_before_unix_ms)
        or not topology_id
        or not revision_id
        or (expected_revision_id is not None and revision_id != expected_revision_id)
        or not re.fullmatch(r"[0-9a-f]{64}", content_sha256)
        or (
            expected_content_sha256 is not None
            and content_sha256 != expected_content_sha256
        )
        or evidence.get("topology_etag") != f'"{revision_id}"'
        or str(evidence.get("topology_status_state", "")).upper() != "IN_SYNC"
        or evidence.get("topology_status_drift") != []
        or not re.fullmatch(r"[0-9a-f]{64}", expected_digest)
        or evidence.get("all_match") is not True
    ):
        raise GateError(f"{phase} projection digest checkpoint is not a converged applied Topology")

    providers = evidence.get("providers", {})
    if not isinstance(providers, Mapping) or set(providers) != {"gateway", "auth"}:
        raise GateError(f"{phase} projection digest checkpoint must cover Gateway and Auth")
    canonical_projections: list[str] = []
    for provider in ("gateway", "auth"):
        proof = providers.get(provider, {})
        projection = proof.get("projection", {}) if isinstance(proof, Mapping) else {}
        routes = projection.get("routes") if isinstance(projection, Mapping) else None
        grants = projection.get("grants") if isinstance(projection, Mapping) else None
        if not isinstance(routes, list) or not isinstance(grants, list):
            raise GateError(f"{phase} {provider} durable projection rows are missing")
        recomputed = effective_projection_sha256(routes, grants)
        observed_digest = str(proof.get("observed_projection_sha256", ""))
        recorded_recomputed = str(proof.get("recomputed_projection_sha256", ""))
        if (
            proof.get("source")
            != "provider-present-status-and-durable-projection"
            or proof.get("api_version") != "v1"
            or proof.get("provider") != provider
            or proof.get("topology_id") != topology_id
            or proof.get("absent") is not False
            or proof.get("observed_revision_id") != revision_id
            or proof.get("observed_content_sha256") != content_sha256
            or not re.fullmatch(r"[0-9a-f]{64}", observed_digest)
            or not re.fullmatch(r"[0-9a-f]{64}", recorded_recomputed)
            or observed_digest != expected_digest
            or recorded_recomputed != expected_digest
            or recomputed != expected_digest
            or proof.get("route_count") != len(routes)
            or proof.get("grant_count") != len(grants)
            or len(routes) < 1
            or len(grants) < 1
            or proof.get("matches_expected") is not True
        ):
            raise GateError(
                f"{phase} {provider} present Status digest does not match its canonical routes/grants"
            )
        canonical_projections.append(canonical_json(projection))
    if canonical_projections[0] != canonical_projections[1]:
        raise GateError(f"{phase} Gateway/Auth durable effective projections diverged")
    return evidence


def _verify_worker_recovery(
    value: Mapping[str, Any],
    *,
    first_flow: Mapping[str, Any],
    worker_deployment_id: str,
    worker_container_id: str,
) -> None:
    recovery = value.get("worker_recovery", {})
    if not isinstance(recovery, Mapping):
        raise GateError("Worker recovery evidence is missing")
    baseline = int(recovery.get("capture_baseline_sequence", 0))
    disruption_started = int(recovery.get("disruption_started_at_unix_ms", 0))
    restore_started = int(recovery.get("restore_started_at_unix_ms", 0))
    if (
        recovery.get("worker_deployment_id") != worker_deployment_id
        or recovery.get("worker_container_id_before") != worker_container_id
        or recovery.get("worker_container_id_after") != worker_container_id
        or baseline < 1
        or disruption_started < 1
        or restore_started <= disruption_started
    ):
        raise GateError(
            "Worker recovery is not correlated to the Store-created container or disruption window"
        )

    disrupted = recovery.get("disrupted_services", [])
    restored = recovery.get("restored_services", [])
    if not isinstance(disrupted, list) or not isinstance(restored, list):
        raise GateError("Worker recovery service lifecycle evidence is invalid")
    disrupted_by_name = {
        str(item.get("name", "")): item
        for item in disrupted
        if isinstance(item, Mapping)
    }
    restored_by_name = {
        str(item.get("name", "")): item
        for item in restored
        if isinstance(item, Mapping)
    }
    if set(disrupted_by_name) != {"gateway", "judge-api"} or set(restored_by_name) != {
        "gateway",
        "judge-api",
    }:
        raise GateError("Worker recovery did not disrupt and restore both Gateway and Judge API")
    for name in ("gateway", "judge-api"):
        stopped = disrupted_by_name[name]
        running = restored_by_name[name]
        if (
            not str(stopped.get("container_id", "")).strip()
            or running.get("container_id") != stopped.get("container_id")
            or running.get("running") is not True
        ):
            raise GateError(f"Worker recovery did not restore the same {name} container")
    if str(restored_by_name["judge-api"].get("health", "")).upper() != "HEALTHY":
        raise GateError("Worker recovery restored Judge API without a healthy Docker gate")

    timeline = recovery.get("health_timeline", [])
    if not isinstance(timeline, list) or len(timeline) < 3:
        raise GateError("Worker recovery health transition timeline is incomplete")
    samples = [item for item in timeline if isinstance(item, Mapping)]
    if len(samples) != len(timeline):
        raise GateError("Worker recovery health timeline contains an invalid sample")
    timestamps = [int(item.get("observed_at_unix_ms", 0)) for item in samples]
    if (
        any(timestamp < 1 for timestamp in timestamps)
        or timestamps != sorted(timestamps)
        or any(item.get("container_id") != worker_container_id for item in samples)
        or any(item.get("running") is not True for item in samples)
        or str(samples[0].get("status", "")).upper() != "HEALTHY"
        or str(samples[-1].get("status", "")).upper() != "HEALTHY"
    ):
        raise GateError(
            "Worker recovery health timeline changed container identity or is not ordered"
        )
    unhealthy_index = next(
        (
            index
            for index, item in enumerate(samples)
            if str(item.get("status", "")).upper() == "UNHEALTHY"
            and int(item.get("observed_at_unix_ms", 0)) >= disruption_started
        ),
        None,
    )
    recovered_index = (
        next(
            (
                index
                for index, item in enumerate(samples)
                if unhealthy_index is not None
                and index > unhealthy_index
                and str(item.get("status", "")).upper() == "HEALTHY"
                and int(item.get("observed_at_unix_ms", 0)) >= restore_started
            ),
            None,
        )
        if unhealthy_index is not None
        else None
    )
    if unhealthy_index is None or recovered_index is None:
        raise GateError("Worker recovery did not prove HEALTHY -> UNHEALTHY -> HEALTHY")

    registration = recovery.get("reregistration", {})
    if (
        not isinstance(registration, Mapping)
        or int(registration.get("sequence", 0)) <= baseline
        or int(registration.get("captured_at_unix_ms", 0)) < restore_started
        or registration.get("method") != "POST"
        or not str(registration.get("path", "")).split("?", 1)[0].endswith(
            "/internal/apis/judge.worker.control/register"
        )
        or int(registration.get("status", 0)) != 200
        or registration.get("worker_id") != worker_deployment_id
    ):
        raise GateError(
            "Worker recovery lacks a post-restore Gateway TLS re-registration capture"
        )

    recovered_flow = recovery.get("recovered_flow", {})
    if (
        not isinstance(recovered_flow, Mapping)
        or recovered_flow.get("same_chain") is not True
        or recovered_flow.get("source") != "actual-services"
        or recovered_flow.get("problem_created_via_http_api") is not True
        or recovered_flow.get("submission_created_via_http_api") is not True
        or recovered_flow.get("manual_judge_problem_insert") is not False
        or recovered_flow.get("workload_transcript_correlated") is not True
    ):
        raise GateError("Worker recovery did not execute a new actual-service flow")
    _verify_actual_flow_correlation(recovered_flow)
    for section, identity_field in (
        ("problem", "problem_id"),
        ("submission", "submission_id"),
        ("task", "task_id"),
        ("result", "result_id"),
    ):
        if str(recovered_flow.get(section, {}).get(identity_field, "")) == str(
            first_flow.get(section, {}).get(identity_field, "")
        ):
            raise GateError(
                f"Worker recovery reused the pre-disruption {section} identity"
            )
    _verify_provider_projection_integrity(
        recovery.get("provider_projection_integrity"),
        phase="worker-recovery",
        not_before_unix_ms=restore_started,
    )


def _verify_auth_admin_bootstrap(value: Mapping[str, Any]) -> None:
    bootstrap = value.get("auth_admin_bootstrap", {})
    database = bootstrap.get("database_proof", {}) if isinstance(bootstrap, Mapping) else {}
    created_user_id = str(bootstrap.get("created_user_id", "")) if isinstance(bootstrap, Mapping) else ""
    if (
        not isinstance(bootstrap, Mapping)
        or int(bootstrap.get("created_status", 0)) != 201
        or int(bootstrap.get("created_code", -1)) != 0
        or not created_user_id.isdigit()
        or int(created_user_id) < 1
        or int(bootstrap.get("login_status", 0)) != 200
        or int(bootstrap.get("login_code", -1)) != 0
        or bootstrap.get("login_user_matches_bootstrap") is not True
        or bootstrap.get("login_has_super_admin") is not True
        or bootstrap.get("login_has_system_admin") is not True
        or int(bootstrap.get("profile_status", 0)) != 200
        or int(bootstrap.get("profile_code", -1)) != 0
        or bootstrap.get("profile_authenticated_same_user") is not True
        or int(bootstrap.get("replay_status", 0)) != 409
        or int(bootstrap.get("replay_code", 0)) != 40931
        or int(bootstrap.get("wrong_secret_status", 0)) != 403
        or int(bootstrap.get("wrong_secret_code", 0)) != 40331
        or not isinstance(database, Mapping)
        or database.get("marker_completed") is not True
        or str(database.get("marker_user_id", "")) != created_user_id
        or database.get("super_admin_assigned") is not True
        or int(database.get("bootstrap_audit_count", 0)) != 1
        or bootstrap.get("jwt_source") != "auth-service-login-endpoint"
        or bootstrap.get("jwt_self_signed_by_harness") is not False
        or bootstrap.get("manual_database_role_seed") is not False
        or bootstrap.get("database_transactional") is not True
        or bootstrap.get("secret_or_token_recorded") is not False
    ):
        raise GateError(
            "full-components evidence does not prove one-time Auth admin bootstrap, "
            "replay/wrong-secret rejection, and a real login JWT"
        )


def _verify_no_secret_material(value: Mapping[str, Any]) -> None:
    """Fail closed if a PASSED evidence document retained credential material."""

    exact_sensitive_keys = {
        "authorization",
        "cookie",
        "set_cookie",
        "x_ojos_bootstrap_secret",
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
    sensitive_suffixes = ("_token", "_secret", "_password", "_private_key")
    compact_jwt = re.compile(
        r"(?<![A-Za-z0-9_-])eyJ[A-Za-z0-9_-]{5,}\."
        r"[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}(?![A-Za-z0-9_-])"
    )

    def contains_material(item: Any) -> bool:
        return item is not None and item is not False and item != "" and item != [] and item != {}

    def visit(item: Any, path: str) -> None:
        if isinstance(item, Mapping):
            for raw_key, child in item.items():
                key = str(raw_key).casefold().replace("-", "_")
                child_path = f"{path}.{raw_key}" if path else str(raw_key)
                if (
                    key in exact_sensitive_keys or key.endswith(sensitive_suffixes)
                ) and contains_material(child):
                    raise GateError(
                        f"full-components evidence retained secret/token material at {child_path}"
                    )
                visit(child, child_path)
            return
        if isinstance(item, (list, tuple)):
            for index, child in enumerate(item):
                visit(child, f"{path}[{index}]")
            return
        if not isinstance(item, str):
            return
        if (
            compact_jwt.search(item)
            or re.search(r"(?i)(?:^|\s)Bearer\s+[A-Za-z0-9._~+/=-]+", item)
            or "-----BEGIN PRIVATE KEY-----" in item
            or "-----BEGIN RSA PRIVATE KEY-----" in item
            or "-----BEGIN EC PRIVATE KEY-----" in item
        ):
            raise GateError(
                f"full-components evidence retained secret/token material at {path}"
            )

    visit(value, "evidence")


def _verify_runtime_inventory_takeover(
    evidence: Any, *, deployment_id: str, node_id: str
) -> None:
    if not isinstance(evidence, Mapping):
        raise GateError(
            f"deployment {deployment_id} is missing lifecycle-to-inventory convergence evidence"
        )
    watermark = evidence.get("completion_watermark_ms")
    immediate = evidence.get("immediate_payload", {})
    inventory = evidence.get("inventory_payload", {})
    if isinstance(watermark, bool) or not isinstance(watermark, int) or watermark <= 0:
        raise GateError(f"deployment {deployment_id} has an invalid lifecycle watermark")

    def verify_projection(payload: Any, phase: str) -> int:
        instance = payload.get("instance", {}) if isinstance(payload, Mapping) else {}
        observed_at = (
            payload.get("last_observed_at_ms") if isinstance(payload, Mapping) else None
        )
        if (
            not isinstance(payload, Mapping)
            or not isinstance(instance, Mapping)
            or instance.get("deployment_id") != deployment_id
            or payload.get("node_id") != node_id
            or str(instance.get("desired_state", "")).upper() != "RUNNING"
            or str(instance.get("observed_state", "")).upper() != "RUNNING"
            or str(instance.get("health", "")).upper() != "HEALTHY"
            or instance.get("runtime_attested") is not True
            or str(payload.get("drift_reason", "")).strip()
            or isinstance(observed_at, bool)
            or not isinstance(observed_at, int)
            or observed_at <= 0
        ):
            raise GateError(
                f"deployment {deployment_id} {phase} projection is not "
                "Running/Healthy/runtime-attested without drift"
            )
        return observed_at

    verify_projection(immediate, "immediate lifecycle")
    inventory_observed_at = verify_projection(inventory, "Agent inventory")
    if inventory_observed_at <= watermark:
        raise GateError(
            f"deployment {deployment_id} Agent inventory did not supersede its lifecycle watermark"
        )


def _verify_managed_a_stack(
    value: Mapping[str, Any], engines: Mapping[str, Any]
) -> Mapping[str, Any]:
    if value.get("a_business_stack_mode") != "production-service-contract-v2":
        raise GateError("A business services were not installed as the production Service Contract v2 stack")

    agent = value.get("a_agent_evidence", {})
    health = agent.get("runtime_health", {}) if isinstance(agent, Mapping) else {}
    observation_age = int(health.get("observation_age_ms", -1)) if isinstance(health, Mapping) else -1
    freshness = int(health.get("freshness_threshold_ms", 0)) if isinstance(health, Mapping) else 0
    if (
        not isinstance(agent, Mapping)
        or agent.get("enrolled") is not True
        or agent.get("mtls") is not True
        or agent.get("node_id") != "node-a"
        or not str(agent.get("instance_id", "")).strip()
        or not str(agent.get("certificate_serial", "")).strip()
        or agent.get("management_credentials_present") is not False
        or agent.get("management_environment_inspected") is not True
        or agent.get("forbidden_management_environment") != []
        or not re.fullmatch(r"[0-9a-f]{12,64}", str(agent.get("container_id", "")))
        or agent.get("engine_id") != engines.get("a", {}).get("engine_id")
        or agent.get("runtime_health_sample") != "final-agent-report"
        or not isinstance(health, Mapping)
        or health.get("node_id") != "node-a"
        or str(health.get("status", "")).upper() != "READY"
        or health.get("ready") is not True
        or health.get("accepting_jobs") is not True
        or health.get("agent_reachable") is not True
        or not str(health.get("last_observed_at", "")).startswith("unix-ms:")
        or int(health.get("unhealthy_deployments", -1)) != 0
        or observation_age < 0
        or freshness <= 0
        or freshness > 60_000
        or observation_age > freshness
    ):
        raise GateError("A stack evidence does not prove a fresh, enrolled mTLS node-a Agent")

    network = value.get("managed_a_network", {})
    tls = network.get("postgres_tls", {}) if isinstance(network, Mapping) else {}
    targets = network.get("targets", []) if isinstance(network, Mapping) else []
    expected_targets = {
        "postgresql-tls",
        "redis-events",
        "minio-s3",
        "gateway-workload",
        "control-plane",
        "oci-registry",
    }
    connected_targets = {
        str(item.get("name", ""))
        for item in targets
        if isinstance(item, Mapping) and item.get("connected") is True
    }
    if (
        not isinstance(network, Mapping)
        or network.get("source_network") != "engine-a-default-bridge"
        or network.get("engine_id") != engines.get("a", {}).get("engine_id")
        or connected_targets != expected_targets
        or len(targets) != len(expected_targets)
        or not isinstance(tls, Mapping)
        or tls.get("verify_full_succeeded") is not True
        or tls.get("server_ssl_enabled") is not True
        or tls.get("plaintext_rejected") is not True
        or not _sha256_digest(tls.get("ca_sha256"))
        or network.get("postgres_plaintext_rejected") is not True
    ):
        raise GateError("A managed-service network evidence lacks live connectivity and verify-full PostgreSQL TLS proof")

    deployments = value.get("managed_a_deployments", {})
    expected_bindings = {
        "storage-service": set(),
        "problem-service": {
            "permission_check",
            "storage_put",
            "storage_head",
            "storage_delete",
        },
        "judge-api": {"permission_check", "storage_get", "storage_put", "storage_head"},
    }
    if not isinstance(deployments, Mapping) or set(deployments) != set(expected_bindings):
        raise GateError("A managed stack must contain exactly Storage, Problem, and Judge deployments")

    for service_id, required_bindings in expected_bindings.items():
        _verify_managed_a_deployment(
            service_id=service_id,
            evidence=deployments.get(service_id, {}),
            required_bindings=required_bindings,
            deployments=deployments,
            agent=agent,
            engine_id=str(engines.get("a", {}).get("engine_id", "")),
            auth_deployment_id=str(
                value.get("third_party_fixture", {}).get(
                    "permission_provider_deployment_id", ""
                )
            ),
        )
    return agent


def _verify_managed_a_deployment(
    *,
    service_id: str,
    evidence: Any,
    required_bindings: set[str],
    deployments: Mapping[str, Any],
    agent: Mapping[str, Any],
    engine_id: str,
    auth_deployment_id: str,
) -> None:
    if not isinstance(evidence, Mapping):
        raise GateError(f"managed A deployment evidence is missing for {service_id}")
    job = evidence.get("agent_job", {})
    if (
        evidence.get("service_id") != service_id
        or evidence.get("node_id") != "node-a"
        or evidence.get("created_by_agent") is not True
        or not str(evidence.get("operation_id", "")).strip()
        or str(evidence.get("operation_status", "")).upper() != "SUCCEEDED"
        or not isinstance(job, Mapping)
        or not str(job.get("job_id", "")).strip()
        or not str(job.get("attempt_id", "")).strip()
        or not _sha256_digest(job.get("lease_id"))
        or job.get("lease_owner_instance_id") != agent.get("instance_id")
        or str(job.get("status", "")).upper() != "SUCCEEDED"
        or job.get("completed_by_agent") is not True
        or not str(evidence.get("deployment_id", "")).strip()
        or str(evidence.get("desired_state", "")).upper() != "RUNNING"
        or str(evidence.get("observed_state", "")).upper() != "RUNNING"
        or str(evidence.get("health", "")).upper() != "HEALTHY"
        or evidence.get("runtime_attested") is not True
        or str(evidence.get("drift_reason", "")).strip()
        or evidence.get("engine_id") != engine_id
        or not re.fullmatch(r"[0-9a-f]{12,64}", str(evidence.get("container_id", "")))
        or not _sha256_digest(evidence.get("image_repo_digest"))
        or not _sha256_digest(evidence.get("host_config_digest"))
        or evidence.get("legacy_environment_present") is not False
    ):
        raise GateError(
            f"managed A {service_id} evidence does not prove one digest-pinned Agent Job and Running/Healthy container"
        )
    _verify_runtime_inventory_takeover(
        evidence.get("runtime_projection"),
        deployment_id=str(evidence.get("deployment_id", "")),
        node_id="node-a",
    )

    binding_map = _strict_active_binding_map(evidence.get("bindings", []))
    if (
        set(binding_map) != required_bindings
        or evidence.get("binding_requirements") != sorted(required_bindings)
        or (not required_bindings and evidence.get("bindings") != [])
    ):
        raise GateError(f"managed A {service_id} has incomplete or non-active ApiBindings")
    storage_deployment_id = str(
        deployments.get("storage-service", {}).get("deployment_id", "")
    )
    for requirement, binding in binding_map.items():
        expected_provider = (
            auth_deployment_id if requirement == "permission_check" else storage_deployment_id
        )
        if not expected_provider or binding.get("provider_deployment_id") != expected_provider:
            raise GateError(f"managed A {service_id} binding {requirement} selected the wrong provider")

    binding_ids = {str(item.get("binding_id", "")) for item in binding_map.values()}
    context = evidence.get("service_context", {})
    context_required = bool(required_bindings)
    if not isinstance(context, Mapping):
        raise GateError(f"managed A {service_id} context evidence is missing")
    if context_required:
        if (
            context.get("required") is not True
            or context.get("present") is not True
            or int(context.get("generation", 0)) < 1
            or context.get("mount_read_only") is not True
            or context.get("credential_embedded") is not False
            or context.get("management_token_present") is not False
            or set(context.get("binding_ids", [])) != binding_ids
            or len(context.get("binding_ids", [])) != len(binding_ids)
        ):
            raise GateError(f"managed A {service_id} ServiceContext proof is incomplete")
    elif (
        context.get("required") is not False
        or context.get("present") is not False
        or context.get("binding_ids") != []
        or context.get("generation") is not None
        or context.get("mount_read_only") is not False
    ):
        raise GateError("Storage context absence must be explicit and justified")

    events = evidence.get("event_context", {})
    expected_publishes = (
        {"io.ojos.problem.snapshot.v1", "io.ojos.problem.deleted.v1"}
        if service_id == "problem-service"
        else set()
    )
    expected_subscribes = (
        {"io.ojos.problem.snapshot.v1", "io.ojos.problem.deleted.v1"}
        if service_id == "judge-api"
        else set()
    )
    event_required = bool(expected_publishes or expected_subscribes)
    if not isinstance(events, Mapping):
        raise GateError(f"managed A {service_id} event context evidence is missing")
    if event_required:
        subscriptions = events.get("subscriptions", [])
        subscribed_types = {
            str(item.get("event_type", ""))
            for item in subscriptions
            if isinstance(item, Mapping)
            and item.get("consumer_group") == "judge-api"
        }
        if (
            events.get("required") is not True
            or events.get("present") is not True
            or int(events.get("generation", 0)) < 1
            or events.get("connection_id") != "a-events"
            or events.get("stream") != "ojos:events:v1"
            or set(events.get("publish_types", [])) != expected_publishes
            or len(events.get("publish_types", [])) != len(expected_publishes)
            or subscribed_types != expected_subscribes
            or len(subscriptions) != len(expected_subscribes)
            or events.get("connection_secret_recorded") is not False
            or int(events.get("generation", 0)) != int(context.get("generation", 0))
        ):
            raise GateError(f"managed A {service_id} EventContext proof is incomplete")
    elif (
        events.get("required") is not False
        or events.get("present") is not False
        or events.get("publish_types") != []
        or events.get("subscriptions") != []
        or events.get("generation") is not None
        or events.get("connection_secret_recorded") is not False
    ):
        raise GateError(f"managed A {service_id} must explicitly report no EventContext")


def _verify_workload_credential_lifecycle(value: Mapping[str, Any]) -> None:
    lifecycle = value.get("workload_credential_lifecycle", {})
    generic = value.get("third_party_fixture", {})
    consumer = generic.get("consumer", {}) if isinstance(generic, Mapping) else {}
    mirrored = generic.get("binding_lifecycle", {}) if isinstance(generic, Mapping) else {}
    before = int(lifecycle.get("generation_before", 0)) if isinstance(lifecycle, Mapping) else 0
    revoked = int(lifecycle.get("generation_revoked", 0)) if isinstance(lifecycle, Mapping) else 0
    restored = int(lifecycle.get("generation_restored", 0)) if isinstance(lifecycle, Mapping) else 0
    if (
        not isinstance(lifecycle, Mapping)
        or not isinstance(consumer, Mapping)
        or lifecycle.get("consumer_deployment_id") != consumer.get("deployment_id")
        or lifecycle.get("container_id_before") != consumer.get("container_id")
        or lifecycle.get("container_id_after") != consumer.get("container_id")
        or before < 1
        or revoked <= before
        or restored <= revoked
        or not str(lifecycle.get("rollback_target_revision_id", "")).strip()
        or not str(lifecycle.get("revoke_revision_id", "")).strip()
        or not str(lifecycle.get("restore_revision_id", "")).strip()
        or not str(lifecycle.get("revoke_operation_id", "")).strip()
        or str(lifecycle.get("revoke_operation_status", "")).upper() != "SUCCEEDED"
        or not str(lifecycle.get("restore_operation_id", "")).strip()
        or str(lifecycle.get("restore_operation_status", "")).upper() != "SUCCEEDED"
        or int(lifecycle.get("old_token_existing_route_status", 0)) not in {401, 403}
        or int(lifecycle.get("current_token_removed_route_status", 0)) not in {403, 404}
        or int(lifecycle.get("current_token_retained_route_status", 0)) != 200
        or int(lifecycle.get("revoked_token_after_restore_status", 0)) not in {401, 403}
        or int(lifecycle.get("restored_token_route_status", 0)) != 200
        or str(lifecycle.get("revoked_binding_desired_state", "")).upper() != "REVOKED"
        or lifecycle.get("echo_requirement_optional") is not True
        or lifecycle.get("permission_requirement_optional") is not False
        or str(
            lifecycle.get("retained_permission_binding_desired_state", "")
        ).upper()
        != "ACTIVE"
        or str(
            lifecycle.get("retained_permission_binding_observed_state", "")
        ).upper()
        != "ACTIVE"
        or lifecycle.get("revoked_context_binding_names") != ["permission_check"]
        or not str(
            lifecycle.get("revoked_context_permission_binding_id", "")
        ).strip()
        or lifecycle.get("revoked_context_permission_binding_id")
        != lifecycle.get("durable_permission_binding_id")
        or not str(lifecycle.get("consumer_observed_unbound_error", "")).strip()
        or lifecycle.get("consumer_recovered") is not True
        or int(lifecycle.get("recovered_success_count", 0)) < 1
        or lifecycle.get("tokens_recorded") is not False
        or canonical_json(lifecycle) != canonical_json(mirrored)
    ):
        raise GateError(
            "workload credential lifecycle does not prove real Link revocation, stale-token rejection, and fresh-token recovery"
        )


def _verify_topology_rollback(value: Mapping[str, Any]) -> None:
    proof = value.get("topology_rollback", {})
    lifecycle = value.get("workload_credential_lifecycle", {})
    generic = value.get("third_party_fixture", {})
    binding_plan = generic.get("binding_plan", {}) if isinstance(generic, Mapping) else {}
    permission_plan = (
        generic.get("permission_binding_plan", {})
        if isinstance(generic, Mapping)
        else {}
    )
    expected_fields = {
        "api_path",
        "topology_id",
        "request_revision_id",
        "request_if_match",
        "target_revision_id",
        "target_revision_number",
        "target_content_sha256",
        "target_spec_sha256",
        "parent_revision_id",
        "parent_revision_number",
        "parent_content_sha256",
        "created_revision_id",
        "created_revision_number",
        "created_parent_revision_id",
        "created_rollback_of_revision_id",
        "created_content_sha256",
        "created_spec_sha256",
        "created_revision_etag",
        "operation_id",
        "operation_action",
        "operation_status",
        "draft_revision_id",
        "applied_revision_id",
        "applying_revision_id",
        "status_desired_revision_id",
        "status_observed_revision_id",
        "status_state",
        "status_drift",
        "status_last_operation_id",
        "restored_bindings",
    }
    if (
        not isinstance(proof, Mapping)
        or set(proof) != expected_fields
        or not isinstance(lifecycle, Mapping)
        or not isinstance(binding_plan, Mapping)
        or not isinstance(permission_plan, Mapping)
    ):
        raise GateError("Topology rollback evidence is missing or non-canonical")

    topology_id = str(proof.get("topology_id", ""))
    target = str(proof.get("target_revision_id", ""))
    parent = str(proof.get("parent_revision_id", ""))
    created = str(proof.get("created_revision_id", ""))
    target_number = int(proof.get("target_revision_number", 0))
    parent_number = int(proof.get("parent_revision_number", 0))
    created_number = int(proof.get("created_revision_number", 0))
    target_content = str(proof.get("target_content_sha256", ""))
    parent_content = str(proof.get("parent_content_sha256", ""))
    created_content = str(proof.get("created_content_sha256", ""))
    raw_sha256 = lambda item: bool(re.fullmatch(r"[0-9a-f]{64}", item))
    if (
        not topology_id
        or proof.get("api_path")
        != f"/api/v1/topologies/{topology_id}:rollback"
        or proof.get("request_revision_id") != target
        or proof.get("request_if_match") != f'"{parent}"'
        or lifecycle.get("rollback_target_revision_id") != target
        or lifecycle.get("revoke_revision_id") != parent
        or lifecycle.get("restore_revision_id") != created
        or lifecycle.get("restore_operation_id") != proof.get("operation_id")
        or target in {"", parent, created}
        or parent in {"", created}
        or target_number < 1
        or parent_number <= target_number
        or created_number != parent_number + 1
        or target != f"{topology_id}:r{target_number}:{target_content}"
        or parent != f"{topology_id}:r{parent_number}:{parent_content}"
        or created != f"{topology_id}:r{created_number}:{created_content}"
        or not raw_sha256(target_content)
        or not raw_sha256(parent_content)
        or not raw_sha256(created_content)
        or target_content != created_content
        or target_content == parent_content
        or not _sha256_digest(proof.get("target_spec_sha256"))
        or proof.get("target_spec_sha256") != proof.get("created_spec_sha256")
        or proof.get("created_parent_revision_id") != parent
        or proof.get("created_rollback_of_revision_id") != target
        or proof.get("created_revision_etag") != f'"{created}"'
        or proof.get("operation_action") != "topology.rollback"
        or str(proof.get("operation_status", "")).upper() != "SUCCEEDED"
        or str(lifecycle.get("restore_operation_status", "")).upper()
        != "SUCCEEDED"
        or proof.get("draft_revision_id") != created
        or proof.get("applied_revision_id") != created
        or proof.get("applying_revision_id") is not None
        or proof.get("status_desired_revision_id") != created
        or proof.get("status_observed_revision_id") != created
        or str(proof.get("status_state", "")).upper() != "IN_SYNC"
        or proof.get("status_drift") != []
        or proof.get("status_last_operation_id") != proof.get("operation_id")
    ):
        raise GateError(
            "Topology rollback did not create and apply a new immutable revision"
        )

    restored = proof.get("restored_bindings")
    if not isinstance(restored, list) or len(restored) != 2:
        raise GateError("Topology rollback did not restore both durable bindings")
    by_requirement: dict[str, Mapping[str, Any]] = {}
    binding_ids: set[str] = set()
    binding_fields = {
        "requirement_name",
        "binding_id",
        "provider_deployment_id",
        "desired_state",
        "observed_state",
        "topology_revision_id",
        "credential_generation",
    }
    for binding in restored:
        if not isinstance(binding, Mapping) or set(binding) != binding_fields:
            raise GateError("Topology rollback binding evidence is non-canonical")
        requirement = str(binding.get("requirement_name", ""))
        binding_id = str(binding.get("binding_id", ""))
        if (
            requirement in by_requirement
            or not requirement
            or not binding_id
            or binding_id in binding_ids
            or str(binding.get("provider_deployment_id", "")) == ""
            or str(binding.get("desired_state", "")).upper() != "ACTIVE"
            or str(binding.get("observed_state", "")).upper() != "ACTIVE"
            or binding.get("topology_revision_id") != created
            or int(binding.get("credential_generation", 0))
            != int(lifecycle.get("generation_restored", 0))
        ):
            raise GateError("Topology rollback restored binding state is invalid")
        by_requirement[requirement] = binding
        binding_ids.add(binding_id)
    if (
        set(by_requirement) != {"echo", "permission_check"}
        or by_requirement["echo"].get("provider_deployment_id")
        != binding_plan.get("provider_deployment_id")
        or by_requirement["permission_check"].get("provider_deployment_id")
        != permission_plan.get("provider_deployment_id")
        or by_requirement["permission_check"].get("binding_id")
        != lifecycle.get("durable_permission_binding_id")
    ):
        raise GateError("Topology rollback bindings do not match the target revision")


def _verify_workload_request_transcript(
    value: Mapping[str, Any],
    *,
    flow: Mapping[str, Any],
    worker_deployment_id: str,
    bindings: Any,
) -> None:
    transcript = value.get("workload_request_transcript", {})
    if (
        not isinstance(transcript, Mapping)
        or transcript.get("capture_source") != "gateway-tls-ingress"
        or transcript.get("authorization_redacted") is not True
        or transcript.get("identity_validated_by_gateway") is not True
        or flow.get("workload_transcript_correlated") is not True
    ):
        raise GateError("Gateway workload request transcript is missing or is not a redacted live TLS capture")

    binding_map = _strict_active_binding_map(bindings)
    if set(binding_map) != {"judge_control", "storage_get"}:
        raise GateError("Gateway transcript cannot be correlated to both active Worker bindings")
    task = flow.get("task", {})
    submission = flow.get("submission", {})
    result = flow.get("result", {})
    claim = transcript.get("claim", {})
    source_get = transcript.get("source_get", {})
    package_get = transcript.get("package_get", {})
    result_capture = transcript.get("result_post", {})
    if not all(
        isinstance(item, Mapping)
        for item in (claim, source_get, package_get, result_capture)
    ):
        raise GateError("Gateway transcript is missing claim, both storage GETs, or result POST")
    if (
        transcript.get("task_id") != task.get("task_id")
        or str(transcript.get("submission_id", ""))
        != str(submission.get("submission_id", ""))
        or task.get("wire_capture") != "gateway TLS ingress claim response"
    ):
        raise GateError("Gateway transcript task/submission correlation is invalid")
    headers = _normalized_headers(claim.get("request_headers"))
    if (
        str(claim.get("method", "")).upper() != "POST"
        or claim.get("path") != "/internal/apis/judge.worker.control/tasks/claim"
        or int(claim.get("status", 0)) != 200
        or headers.get("prefer") != "wait=25"
        or "authorization" in headers
        or not _sha256_digest(claim.get("request_sha256"))
        or not _sha256_digest(claim.get("response_sha256"))
        or int(claim.get("response_size_bytes", 0)) <= 0
    ):
        raise GateError("Gateway transcript does not prove the real Prefer=wait=25 claim response")

    for kind, role, capture in (
        ("source_get", "source", source_get),
        ("package_get", "problem_package", package_get),
    ):
        resource = task.get(role, {})
        expected_path = "/internal/apis/storage.object.get" + str(resource.get("relative_path", ""))
        capture_headers = _normalized_headers(capture.get("request_headers"))
        if (
            str(capture.get("method", "")).upper() != "GET"
            or capture.get("path") != expected_path
            or int(capture.get("status", 0)) != 200
            or capture.get("resource_ref") != resource
            or "authorization" in capture_headers
            or not _sha256_digest(capture.get("request_sha256"))
            or capture.get("response_sha256") != resource.get("sha256")
            or int(capture.get("response_size_bytes", -1)) != int(resource.get("size_bytes", -2))
        ):
            raise GateError(f"Gateway transcript {kind} is not correlated to the claimed resource reference")

    expected_result_path = (
        "/internal/apis/judge.worker.control/tasks/"
        + str(task.get("task_id", ""))
        + "/result"
    )
    result_headers = _normalized_headers(result_capture.get("request_headers"))
    if (
        str(result_capture.get("method", "")).upper() != "POST"
        or result_capture.get("path") != expected_result_path
        or int(result_capture.get("status", 0)) != 200
        or result_capture.get("task_id") != task.get("task_id")
        or str(result_capture.get("status_value", "")).upper()
        != str(result.get("status", "")).upper()
        or int(result_capture.get("lease_version", 0)) < 1
        or "authorization" in result_headers
        or not _sha256_digest(result_capture.get("request_sha256"))
        or not _sha256_digest(result_capture.get("response_sha256"))
        or int(result_capture.get("request_size_bytes", 0)) <= 0
        or int(result_capture.get("response_size_bytes", 0)) <= 0
    ):
        raise GateError("Gateway transcript result POST is not correlated to the accepted Judge result")


def _normalized_headers(value: Any) -> dict[str, str]:
    if not isinstance(value, Mapping):
        return {}
    return {str(key).lower(): str(item) for key, item in value.items()}


def _strict_active_binding_map(value: Any) -> dict[str, Mapping[str, Any]]:
    if not isinstance(value, list):
        return {}
    bindings: dict[str, Mapping[str, Any]] = {}
    ids: set[str] = set()
    for item in value:
        if not isinstance(item, Mapping):
            return {}
        requirement = str(item.get("requirement_name", item.get("requirement", ""))).strip()
        binding_id = str(item.get("binding_id", "")).strip()
        provider = str(item.get("provider_deployment_id", "")).strip()
        if (
            not requirement
            or requirement in bindings
            or not binding_id
            or binding_id in ids
            or not provider
            or str(item.get("desired_state", "")).upper() != "ACTIVE"
            or str(item.get("observed_state", "")).upper() != "ACTIVE"
            or str(item.get("health", "")).upper() not in {"HEALTHY", "READY"}
        ):
            return {}
        bindings[requirement] = item
        ids.add(binding_id)
    return bindings


def _strong_etag(value: Any) -> bool:
    return isinstance(value, str) and bool(re.fullmatch(r'"[^"\s]+"', value))


def _sha256_digest(value: Any) -> bool:
    return isinstance(value, str) and bool(re.fullmatch(r"sha256:[0-9a-f]{64}", value))


def _binding_requirements(value: Any) -> set[str]:
    if not isinstance(value, list):
        return set()
    requirements: set[str] = set()
    for item in value:
        if not isinstance(item, Mapping):
            return set()
        requirement = str(item.get("requirement", item.get("name", ""))).strip()
        provider = str(item.get("provider_deployment_id", "")).strip()
        if not requirement or not provider:
            return set()
        requirements.add(requirement)
    return requirements


def _active_binding_requirements(value: Any) -> set[str]:
    if not isinstance(value, list):
        return set()
    active: set[str] = set()
    for item in value:
        if not isinstance(item, Mapping):
            return set()
        if (
            str(item.get("desired_state", "")).upper() != "ACTIVE"
            or str(item.get("observed_state", "")).upper() != "ACTIVE"
            or str(item.get("health", "")).upper() not in {"HEALTHY", "READY"}
        ):
            continue
        requirement = str(item.get("requirement", item.get("requirement_name", ""))).strip()
        if requirement:
            active.add(requirement)
    return active


def _binding_ids(value: Any) -> set[str]:
    if not isinstance(value, list):
        return set()
    ids = {
        str(item.get("binding_id", "")).strip()
        for item in value
        if isinstance(item, Mapping) and str(item.get("binding_id", "")).strip()
    }
    return ids


def command_validate(args: argparse.Namespace) -> int:
    checks = validate_repository(args.repo_root.resolve())
    output = {"schema_version": SCHEMA_VERSION, "status": "PASSED", "kind": "contract-static", "checks": checks}
    if args.evidence:
        atomic_json(args.evidence, output)
    print(json.dumps(output, indent=2, sort_keys=True))
    return 0


def command_live(args: argparse.Namespace) -> int:
    gate = LiveGate(args.repo_root.resolve(), args.evidence.resolve(), args.full_components)
    evidence = gate.run()
    print(json.dumps({"status": evidence["status"], "evidence": str(args.evidence)}, sort_keys=True))
    return 0


def command_verify(args: argparse.Namespace) -> int:
    try:
        value = json.loads(args.evidence.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise GateError(f"could not read evidence: {exc}") from exc
    verify_evidence(value, require_full=args.require_full)
    print(canonical_json({"status": "PASSED", "evidence": str(args.evidence)}))
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    validate = subparsers.add_parser("validate", help="Docker-free fail-closed contract gate")
    validate.add_argument("--repo-root", type=Path, default=Path.cwd())
    validate.add_argument("--evidence", type=Path)
    validate.set_defaults(function=command_validate)

    live = subparsers.add_parser("live", help="run two real independent Linux Docker Engines")
    live.add_argument("--repo-root", type=Path, default=Path.cwd())
    live.add_argument("--evidence", type=Path, required=True)
    live.add_argument("--full-components", action="store_true")
    live.set_defaults(function=command_live)

    verify = subparsers.add_parser("verify-evidence", help="fail closed on incomplete live evidence")
    verify.add_argument("evidence", type=Path)
    verify.add_argument("--require-full", action="store_true")
    verify.set_defaults(function=command_verify)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return int(args.function(args))
    except GateError as exc:
        print(f"cross-machine gate failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
