#!/usr/bin/env python3
"""Provision and verify the v1 production-capacity fixture through public APIs.

The tool deliberately has no database adapter. Enrollment, Store installs,
Topology revisions and verification all use /api/v1 so the resulting evidence
exercises the same contract as Web and TUI clients.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import http.client
import ipaddress
import json
import math
import os
import pathlib
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any, Iterable


TERMINAL_OPERATION_STATES = {
    "SUCCEEDED",
    "FAILED",
    "CANCELLED",
    "NEEDS_ATTENTION",
    "ROLLED_BACK",
}
SHA40 = frozenset("0123456789abcdef")
STORE_NODE_LABELS = {
    "runtime": "docker",
    "os": "linux",
    "arch": "x86_64",
}
ENGINE_EVIDENCE_MAX_AGE_SECONDS = 300
ENGINE_EVIDENCE_MAX_SPREAD_SECONDS = 90
ENGINE_EVIDENCE_MAX_FUTURE_SKEW_SECONDS = 30


class CapacityEnvironmentError(RuntimeError):
    pass


@dataclass(frozen=True)
class ApiResult:
    data: dict[str, Any]
    headers: dict[str, str]
    status: int


class TokenProvider:
    """OIDC token helper with the same strict contract as the 24-hour gate."""

    def __init__(self, argv_json: str) -> None:
        try:
            argv = json.loads(argv_json)
        except json.JSONDecodeError as error:
            raise CapacityEnvironmentError(
                f"token helper argv is not JSON: {error}"
            ) from error
        if (
            not isinstance(argv, list)
            or not 1 <= len(argv) <= 32
            or any(not isinstance(value, str) or not value for value in argv)
        ):
            raise CapacityEnvironmentError(
                "token helper argv must contain 1-32 non-empty strings"
            )
        self._argv = tuple(argv)
        self._token = ""
        self._expires_at = 0.0
        self._lock = threading.Lock()

    def access_token(self) -> str:
        with self._lock:
            now = time.time()
            if self._token and self._expires_at - now > 600:
                return self._token
            result = subprocess.run(
                self._argv,
                shell=False,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=30,
                check=False,
            )
            if result.returncode != 0:
                raise CapacityEnvironmentError(
                    f"token helper exited with status {result.returncode}"
                )
            try:
                payload = json.loads(result.stdout)
            except json.JSONDecodeError as error:
                raise CapacityEnvironmentError(
                    "token helper stdout is not one JSON object"
                ) from error
            if not isinstance(payload, dict) or set(payload) != {
                "access_token",
                "expires_at",
            }:
                raise CapacityEnvironmentError(
                    "token helper stdout must contain exactly access_token and expires_at"
                )
            token = payload["access_token"]
            expires_at = payload["expires_at"]
            if not isinstance(token, str) or not token:
                raise CapacityEnvironmentError("token helper returned an empty access_token")
            if isinstance(expires_at, bool) or not isinstance(expires_at, (int, float)):
                raise CapacityEnvironmentError("token helper expires_at must be Unix epoch seconds")
            if float(expires_at) - now <= 600:
                raise CapacityEnvironmentError(
                    "token helper token must remain valid for more than 600 seconds"
                )
            self._token = token
            self._expires_at = float(expires_at)
            return token


class ApiClient:
    def __init__(
        self,
        base_url: str,
        ca_file: pathlib.Path,
        token_provider: TokenProvider,
        timeout_seconds: float = 30.0,
    ) -> None:
        parsed = urllib.parse.urlsplit(base_url.rstrip("/"))
        if parsed.scheme != "https" or not parsed.netloc or parsed.path not in ("", "/"):
            raise CapacityEnvironmentError(
                "base URL must be a direct HTTPS origin without a path"
            )
        if not ca_file.is_file():
            raise CapacityEnvironmentError(f"CA file does not exist: {ca_file}")
        self.base_url = base_url.rstrip("/")
        self.context = ssl.create_default_context(cafile=str(ca_file))
        self.tokens = token_provider
        self.timeout_seconds = timeout_seconds

    def request(
        self,
        method: str,
        path: str,
        payload: Any | None = None,
        *,
        idempotency_key: str | None = None,
        if_match: str | None = None,
        expected: Iterable[int] = (200,),
    ) -> ApiResult:
        if not path.startswith("/api/v1/"):
            raise CapacityEnvironmentError(f"refusing non-v1 API path: {path}")
        body = None
        headers = {
            "Accept": "application/json",
            "Authorization": f"Bearer {self.tokens.access_token()}",
            "User-Agent": "ojos-orchestrator-capacity-environment/1.0",
        }
        if payload is not None:
            body = json.dumps(
                payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False
            ).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if idempotency_key:
            headers["Idempotency-Key"] = idempotency_key
        if if_match:
            headers["If-Match"] = if_match
        request = urllib.request.Request(
            f"{self.base_url}{path}", data=body, headers=headers, method=method
        )
        try:
            with urllib.request.urlopen(
                request, context=self.context, timeout=self.timeout_seconds
            ) as response:
                response_body = response.read(16 * 1024 * 1024 + 1)
                if len(response_body) > 16 * 1024 * 1024:
                    raise CapacityEnvironmentError(f"{method} {path} response is too large")
                status = response.status
                response_headers = {
                    key.lower(): value for key, value in response.headers.items()
                }
        except urllib.error.HTTPError as error:
            response_body = error.read(1024 * 1024)
            try:
                problem = json.loads(response_body)
            except (json.JSONDecodeError, UnicodeDecodeError):
                problem = {}
            code = problem.get("code", "HTTP_ERROR") if isinstance(problem, dict) else "HTTP_ERROR"
            detail = problem.get("detail", "request rejected") if isinstance(problem, dict) else "request rejected"
            raise CapacityEnvironmentError(
                f"{method} {path} returned HTTP {error.code} {code}: {detail}"
            ) from error
        except urllib.error.URLError as error:
            raise CapacityEnvironmentError(f"{method} {path} failed: {error.reason}") from error
        if status not in set(expected):
            raise CapacityEnvironmentError(
                f"{method} {path} returned HTTP {status}; expected {sorted(expected)}"
            )
        try:
            envelope = json.loads(response_body)
        except (json.JSONDecodeError, UnicodeDecodeError) as error:
            raise CapacityEnvironmentError(f"{method} {path} returned invalid JSON") from error
        if not isinstance(envelope, dict) or not isinstance(envelope.get("data"), dict):
            raise CapacityEnvironmentError(f"{method} {path} returned an invalid v1 envelope")
        meta = envelope.get("meta")
        if not isinstance(meta, dict) or not isinstance(meta.get("request_id"), str):
            raise CapacityEnvironmentError(f"{method} {path} omitted meta.request_id")
        return ApiResult(envelope["data"], response_headers, status)

    def page(self, path: str) -> list[dict[str, Any]]:
        cursor = ""
        items: list[dict[str, Any]] = []
        seen: set[str] = set()
        while True:
            separator = "&" if "?" in path else "?"
            target = f"{path}{separator}limit=200"
            if cursor:
                target += "&cursor=" + urllib.parse.quote(cursor, safe="")
            result = self.request("GET", target)
            page_items = result.data.get("items")
            if not isinstance(page_items, list) or any(
                not isinstance(item, dict) for item in page_items
            ):
                raise CapacityEnvironmentError(f"{target} returned an invalid page")
            items.extend(page_items)
            next_cursor = result.data.get("next_cursor")
            if next_cursor is None:
                return items
            if not isinstance(next_cursor, str) or not next_cursor or next_cursor in seen:
                raise CapacityEnvironmentError(f"{target} returned an invalid cursor")
            seen.add(next_cursor)
            cursor = next_cursor


def load_json(path: pathlib.Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise CapacityEnvironmentError(f"cannot load JSON {path}: {error}") from error


def require_candidate_sha(value: str) -> str:
    normalized = value.strip().lower()
    if (
        value != normalized
        or len(normalized) != 40
        or any(char not in SHA40 for char in normalized)
    ):
        raise CapacityEnvironmentError(
            "candidate SHA must be exactly 40 lowercase hexadecimal characters"
        )
    return normalized


def validate_nodes(value: Any, expected_count: int = 100) -> list[dict[str, Any]]:
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise CapacityEnvironmentError("node plan must use schema_version 1")
    nodes = value.get("nodes")
    if not isinstance(nodes, list) or len(nodes) != expected_count:
        raise CapacityEnvironmentError(
            f"node plan must contain exactly {expected_count} nodes"
        )
    seen: set[str] = set()
    engine_slots: set[tuple[str, int]] = set()
    normalized: list[dict[str, Any]] = []
    for item in nodes:
        if not isinstance(item, dict):
            raise CapacityEnvironmentError("each node plan entry must be an object")
        node_id = item.get("node_id")
        host_ip = item.get("host_ip")
        engine_ordinal = item.get("engine_ordinal")
        worker = item.get("worker")
        labels = item.get("labels")
        if (
            not isinstance(node_id, str)
            or not node_id
            or node_id in seen
            or not isinstance(host_ip, str)
            or not isinstance(worker, str)
            or not worker
            or isinstance(engine_ordinal, bool)
            or not isinstance(engine_ordinal, int)
            or not 0 <= engine_ordinal < 10
            or not isinstance(labels, dict)
        ):
            raise CapacityEnvironmentError(f"invalid or duplicate node entry: {item!r}")
        missing_store_labels = {
            key: expected
            for key, expected in STORE_NODE_LABELS.items()
            if labels.get(key) != expected
        }
        if missing_store_labels:
            raise CapacityEnvironmentError(
                f"node {node_id} must advertise canonical Store labels "
                "runtime=docker, os=linux, arch=x86_64"
            )
        try:
            parsed_host_ip = ipaddress.ip_address(host_ip)
        except ValueError as error:
            raise CapacityEnvironmentError(f"node {node_id} has invalid host_ip") from error
        if not isinstance(parsed_host_ip, ipaddress.IPv4Address):
            raise CapacityEnvironmentError(
                f"node {node_id} host_ip must be IPv4 for the capacity endpoint contract"
            )
        slot = (worker, engine_ordinal)
        if slot in engine_slots:
            raise CapacityEnvironmentError(f"duplicate Engine slot {worker}/{engine_ordinal}")
        seen.add(node_id)
        engine_slots.add(slot)
        normalized.append(
            {
                "node_id": node_id,
                "host_ip": host_ip,
                "worker": worker,
                "engine_ordinal": engine_ordinal,
                "labels": dict(labels),
            }
        )
    workers = {item["worker"] for item in normalized}
    if expected_count == 100 and (
        len(workers) != 10
        or any(sum(node["worker"] == worker for node in normalized) != 10 for worker in workers)
    ):
        raise CapacityEnvironmentError("production node plan must be 10 workers x 10 Engines")
    worker_ips = {
        worker: {node["host_ip"] for node in normalized if node["worker"] == worker}
        for worker in workers
    }
    if any(len(addresses) != 1 for addresses in worker_ips.values()) or len(
        {next(iter(addresses)) for addresses in worker_ips.values()}
    ) != len(workers):
        raise CapacityEnvironmentError(
            "each worker must have one unique host IP shared by its 10 Engines"
        )
    return sorted(normalized, key=lambda item: item["node_id"])


def validate_fixture(value: Any, expected_services: int = 20) -> dict[str, Any]:
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise CapacityEnvironmentError("fixture manifest must use schema_version 1")
    services = value.get("services")
    if not isinstance(services, list) or len(services) != expected_services:
        raise CapacityEnvironmentError(
            f"fixture manifest must contain exactly {expected_services} services"
        )
    ids: set[str] = set()
    for service in services:
        if not isinstance(service, dict):
            raise CapacityEnvironmentError("fixture service must be an object")
        service_id = service.get("service_id")
        image = service.get("oci_image")
        digest = image.rsplit("@sha256:", 1)[1] if isinstance(image, str) and "@sha256:" in image else ""
        if (
            not isinstance(service_id, str)
            or not service_id
            or len(service_id) > 63
            or not service_id[0].isalnum()
            or any(character not in "abcdefghijklmnopqrstuvwxyz0123456789-" for character in service_id)
            or service_id in ids
            or not isinstance(image, str)
            or image.count("@sha256:") != 1
            or len(digest) != 64
            or any(character not in "0123456789abcdef" for character in digest)
        ):
            raise CapacityEnvironmentError(f"invalid fixture service: {service!r}")
        ids.add(service_id)
    for key in ("catalog_source_id", "version", "channel", "topology_id"):
        if not isinstance(value.get(key), str) or not value[key]:
            raise CapacityEnvironmentError(f"fixture manifest requires {key}")
    return value


def deployment_id(service_id: str, version: str, node_id: str) -> str:
    digest = hashlib.sha256(
        service_id.encode() + b"\0" + version.encode() + b"\0" + node_id.encode()
    ).hexdigest()
    return f"deployment-{service_id}-{digest}"[:56]


def capacity_endpoint(
    node: dict[str, Any], service_id: str, service_index: int, service_count: int = 20
) -> str:
    if not 0 <= service_index < service_count:
        raise CapacityEnvironmentError("capacity service index is outside its Engine port range")
    port = 20_000 + node["engine_ordinal"] * service_count + service_index
    return f"{node['host_ip']}:{port}:{service_id}"


def build_topology_spec(
    nodes: list[dict[str, Any]], fixture: dict[str, Any]
) -> dict[str, Any]:
    services = sorted(fixture["services"], key=lambda item: item["service_id"])
    endpoints: list[dict[str, Any]] = []
    for node in nodes:
        for service_index, service in enumerate(services):
            endpoint_id = capacity_endpoint(
                node, service["service_id"], service_index, len(services)
            )
            endpoints.append(
                {
                    "endpoint": endpoint_id,
                    "service_id": service["service_id"],
                    "protocol": "http",
                    "health_path": "/health",
                    "display_name": f"{node['node_id']} / {service['service_id']}",
                    "note": "production capacity fixture",
                    "config": {},
                }
            )
    endpoints.sort(key=lambda item: item["endpoint"])
    links: list[dict[str, Any]] = []
    offsets = (1, 3, 7, 13)
    for index, endpoint in enumerate(endpoints):
        for offset in offsets:
            target = endpoints[(index + offset) % len(endpoints)]
            links.append(
                {
                    "source_endpoint": endpoint["endpoint"],
                    "target_endpoint": target["endpoint"],
                    "protocol": "http",
                    "auth_mode": "service-identity",
                    "scope": "capacity",
                    "enabled": True,
                    "config_ref": "",
                    "secret_ref": "",
                    "policy": {},
                }
            )
    links.sort(key=lambda item: (item["source_endpoint"], item["target_endpoint"]))
    root = endpoints[0]["endpoint"]
    return {
        "api_version": "v1",
        "topology_id": fixture["topology_id"],
        "root_endpoint": root,
        "authority": {"root_endpoint": root, "exposure_policy": "private"},
        "endpoints": endpoints,
        "links": links,
    }


def parse_capacity_endpoint(endpoint: str) -> tuple[str, int, str]:
    try:
        host_and_port, service_id = endpoint.rsplit(":", 1)
        host, port_text = host_and_port.rsplit(":", 1)
        parsed_host = ipaddress.ip_address(host)
        port = int(port_text)
    except (AttributeError, ValueError) as error:
        raise CapacityEnvironmentError(
            f"invalid capacity endpoint {endpoint!r}"
        ) from error
    if not 1 <= port <= 65_535 or not service_id:
        raise CapacityEnvironmentError(f"invalid capacity endpoint {endpoint!r}")
    return str(parsed_host), port, service_id


def endpoint_http_json(
    endpoint: str, path: str, timeout_seconds: float = 2.0
) -> dict[str, Any]:
    host, port, _ = parse_capacity_endpoint(endpoint)
    connection = http.client.HTTPConnection(host, port, timeout=timeout_seconds)
    try:
        connection.request(
            "GET",
            path,
            headers={"Accept": "application/json", "Connection": "close"},
        )
        response = connection.getresponse()
        body = response.read(4_097)
        if response.status != 200:
            raise CapacityEnvironmentError(
                f"{endpoint}{path} returned HTTP {response.status}"
            )
        if len(body) > 4_096:
            raise CapacityEnvironmentError(
                f"{endpoint}{path} exceeded the 4096-byte response limit"
            )
        decoded = json.loads(body)
        if not isinstance(decoded, dict):
            raise CapacityEnvironmentError(f"{endpoint}{path} returned non-object JSON")
        return decoded
    except (OSError, http.client.HTTPException, json.JSONDecodeError) as error:
        raise CapacityEnvironmentError(f"{endpoint}{path} probe failed: {error}") from error
    finally:
        connection.close()


def verify_capacity_network(
    spec: dict[str, Any], candidate_sha: str
) -> dict[str, Any]:
    endpoints = spec.get("endpoints")
    links = spec.get("links")
    if not isinstance(endpoints, list) or not isinstance(links, list):
        raise CapacityEnvironmentError("TopologySpec network resources are invalid")
    endpoint_services = {
        endpoint["endpoint"]: endpoint["service_id"] for endpoint in endpoints
    }
    deadline = time.monotonic() + 45.0

    def endpoint_check(endpoint: dict[str, Any]) -> tuple[bool, str]:
        endpoint_id = endpoint["endpoint"]
        try:
            observed = endpoint_http_json(
                endpoint_id,
                endpoint["health_path"],
                max(0.05, min(2.0, deadline - time.monotonic())),
            )
            expected = {
                "status": "healthy",
                "candidate_sha": candidate_sha,
                "service_id": endpoint["service_id"],
            }
            if any(observed.get(key) != value for key, value in expected.items()):
                raise CapacityEnvironmentError(
                    f"{endpoint_id} health identity does not match {expected}"
                )
            return True, ""
        except CapacityEnvironmentError as error:
            return False, f"{endpoint_id}: {error}"

    def link_check(link: dict[str, Any]) -> tuple[bool, str]:
        source = link["source_endpoint"]
        target = link["target_endpoint"]
        try:
            if source not in endpoint_services or target not in endpoint_services:
                raise CapacityEnvironmentError("link references an endpoint outside the spec")
            query = urllib.parse.urlencode({"target": target})
            observed = endpoint_http_json(
                source,
                f"/probe?{query}",
                max(0.05, min(2.0, deadline - time.monotonic())),
            )
            expected = {
                "status": "healthy",
                "candidate_sha": candidate_sha,
                "source_service_id": endpoint_services[source],
                "target_endpoint": target,
                "target_service_id": endpoint_services[target],
            }
            if any(observed.get(key) != value for key, value in expected.items()):
                raise CapacityEnvironmentError(
                    f"{source} -> {target} probe identity does not match {expected}"
                )
            return True, ""
        except CapacityEnvironmentError as error:
            return False, f"{source} -> {target}: {error}"

    def run_checks(values: list[dict[str, Any]], check: Any) -> tuple[int, list[str]]:
        failures: list[str] = []
        healthy = 0
        # This observer is intentionally independent of TopologyStatus. A
        # fixed pool bounds sockets and threads while all 10,000 checks still
        # execute before successful evidence can be emitted.
        if time.monotonic() >= deadline:
            return 0, ["capacity network preflight exhausted its 45-second deadline"]
        with concurrent.futures.ThreadPoolExecutor(max_workers=64) as executor:
            futures = [executor.submit(check, value) for value in values]
            try:
                for future in concurrent.futures.as_completed(
                    futures, timeout=max(0.05, deadline - time.monotonic())
                ):
                    ok, detail = future.result()
                    if ok:
                        healthy += 1
                    elif len(failures) < 20:
                        failures.append(detail)
            except concurrent.futures.TimeoutError:
                if len(failures) < 20:
                    failures.append(
                        "capacity network preflight exhausted its 45-second deadline"
                    )
            finally:
                for future in futures:
                    future.cancel()
        return healthy, failures

    endpoints_healthy, endpoint_failures = run_checks(endpoints, endpoint_check)
    links_healthy, link_failures = run_checks(links, link_check)
    evidence = {
        "checked_at_epoch_seconds": int(time.time()),
        "endpoint_checks_total": len(endpoints),
        "endpoint_checks_healthy": endpoints_healthy,
        "endpoint_checks_failed": len(endpoints) - endpoints_healthy,
        "link_probes_total": len(links),
        "link_probes_healthy": links_healthy,
        "link_probes_failed": len(links) - links_healthy,
        "drift": 0,
        "endpoint_ids_sha256": hashlib.sha256(
            b"".join(
                endpoint_id.encode() + b"\n"
                for endpoint_id in sorted(endpoint_services)
            )
        ).hexdigest(),
        "link_ids_sha256": hashlib.sha256(
            b"".join(
                link["source_endpoint"].encode()
                + b"\0"
                + link["target_endpoint"].encode()
                + b"\n"
                for link in sorted(
                    links,
                    key=lambda item: (
                        item["source_endpoint"],
                        item["target_endpoint"],
                    ),
                )
            )
        ).hexdigest(),
        "failure_samples": endpoint_failures + link_failures,
    }
    if (
        evidence["endpoint_checks_total"] != 2_000
        or evidence["endpoint_checks_failed"] != 0
        or evidence["link_probes_total"] != 8_000
        or evidence["link_probes_failed"] != 0
    ):
        raise CapacityEnvironmentError(
            "capacity network preflight failed: "
            + json.dumps(evidence, sort_keys=True, separators=(",", ":"))
        )
    return evidence


def atomic_write(path: pathlib.Path, content: bytes, mode: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = pathlib.Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            if hasattr(os, "fchmod"):
                os.fchmod(stream.fileno(), mode)
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
        os.chmod(path, mode)
    finally:
        temporary.unlink(missing_ok=True)


def issue_enrollment(args: argparse.Namespace, client: ApiClient) -> dict[str, Any]:
    nodes = validate_nodes(load_json(args.nodes_file), args.expected_nodes)
    existing = {item["node_id"]: item for item in client.page("/api/v1/nodes")}
    issued = 0
    ready = 0
    for node in nodes:
        current = existing.get(node["node_id"])
        if current and str(current.get("status", "")).upper() == "READY":
            current_labels = current.get("labels")
            if not isinstance(current_labels, dict) or any(
                current_labels.get(key) != expected
                for key, expected in STORE_NODE_LABELS.items()
            ):
                raise CapacityEnvironmentError(
                    f"READY node {node['node_id']} lacks the canonical Docker/Linux/x86_64 "
                    "Store labels; revoke and re-enroll it"
                )
            ready += 1
            continue
        result = client.request(
            "POST",
            "/api/v1/nodes/enrollment-codes",
            {
                "node_id": node["node_id"],
                "host_ip": node["host_ip"],
                "role": "standalone",
                "parent_node_id": "",
                "labels": {
                    **(node["labels"] if isinstance(node["labels"], dict) else {}),
                    "capacity.worker": node["worker"],
                    "capacity.engine": str(node["engine_ordinal"]),
                    "capacity.candidate_sha": args.candidate_sha,
                },
                "ttl_seconds": 3600,
            },
            idempotency_key=(
                f"capacity-enroll-{args.candidate_sha[:12]}-"
                f"{args.enrollment_generation}-{node['node_id']}"
            ),
            expected=(201,),
        )
        code = result.data.get("enrollment_code")
        if not isinstance(code, str) or not code:
            raise CapacityEnvironmentError(
                f"enrollment response for {node['node_id']} omitted the one-time code"
            )
        atomic_write(args.output_dir / f"{node['node_id']}.code", code.encode(), 0o600)
        issued += 1
    return {"issued": issued, "already_ready": ready, "nodes": len(nodes)}


def wait_operation(
    client: ApiClient,
    operation_id: str,
    timeout_seconds: int,
    candidate_sha: str,
    *,
    sleep: Any = time.sleep,
    monotonic: Any = time.monotonic,
) -> dict[str, Any]:
    deadline = monotonic() + timeout_seconds
    retried = False
    while monotonic() < deadline:
        operation = client.request("GET", f"/api/v1/operations/{operation_id}").data.get(
            "operation"
        )
        if not isinstance(operation, dict):
            raise CapacityEnvironmentError(f"Operation {operation_id} response is invalid")
        status = str(operation.get("status", "")).upper()
        if status in TERMINAL_OPERATION_STATES:
            if status == "SUCCEEDED":
                return operation
            if status == "FAILED" and not retried:
                generation = operation.get("generation")
                if (
                    isinstance(generation, bool)
                    or not isinstance(generation, int)
                    or generation < 0
                ):
                    raise CapacityEnvironmentError(
                        f"FAILED Operation {operation_id} omitted a valid generation"
                    )
                # The durable Operation generation is the cross-process retry
                # ledger.  Ansible may restart this seed command after a
                # transient command failure, so an in-memory `retried` flag is
                # not sufficient to enforce the promised single automatic
                # retry.  Generation zero is the original attempt; once the
                # persisted generation is one or greater, another automatic
                # retry would silently turn an Ansible task retry into repeated
                # external side effects.
                if generation >= 1:
                    raise CapacityEnvironmentError(
                        f"Operation {operation_id} is FAILED at persisted generation "
                        f"{generation}; automatic retry is limited to generation 0->1"
                    )
                operation_hash = hashlib.sha256(operation_id.encode()).hexdigest()[:16]
                retry = client.request(
                    "POST",
                    f"/api/v1/operations/{urllib.parse.quote(operation_id, safe='')}:retry",
                    {},
                    idempotency_key=(
                        f"capacity-retry-{candidate_sha[:12]}-{operation_hash}-"
                        f"g{generation + 1}"
                    ),
                    expected=(202,),
                )
                retried_id = retry.data.get("operation_id")
                retried_operation = retry.data.get("operation")
                if (
                    retried_id != operation_id
                    or not isinstance(retried_operation, dict)
                    or retried_operation.get("operation_id") != operation_id
                    or retried_operation.get("generation") != generation + 1
                ):
                    raise CapacityEnvironmentError(
                        "Operation retry response did not preserve identity and increment generation"
                    )
                retried = True
                continue
            raise CapacityEnvironmentError(
                f"Operation {operation_id} ended in {status}"
            )
        remaining = deadline - monotonic()
        if remaining > 0:
            sleep(min(2, remaining))
    raise CapacityEnvironmentError(f"Operation {operation_id} did not finish before timeout")


def expected_deployments(
    nodes: list[dict[str, Any]], fixture: dict[str, Any]
) -> dict[str, tuple[str, dict[str, Any], str]]:
    expected: dict[str, tuple[str, dict[str, Any], str]] = {}
    services = sorted(fixture["services"], key=lambda item: item["service_id"])
    for node in nodes:
        for service_index, service in enumerate(services):
            identifier = deployment_id(
                service["service_id"], fixture["version"], node["node_id"]
            )
            expected[identifier] = (
                node["node_id"],
                service,
                capacity_endpoint(node, service["service_id"], service_index),
            )
    return expected


def verify_engine_evidence(
    value: Any,
    candidate_sha: str,
    nodes: list[dict[str, Any]],
    fixture: dict[str, Any],
    *,
    now: float | None = None,
) -> dict[str, Any]:
    """Validate the full 10x10x20 Docker observation, not API projections."""
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise CapacityEnvironmentError("Engine evidence must use schema_version 1")
    fixture_images = {service["oci_image"] for service in fixture["services"]}
    if len(fixture_images) != 1:
        raise CapacityEnvironmentError("capacity fixture must use one immutable OCI image")
    fixture_image = next(iter(fixture_images))
    if (
        value.get("candidate_sha") != candidate_sha
        or value.get("fixture_image") != fixture_image
        or value.get("worker_count") != 10
        or value.get("engine_count") != 100
        or value.get("container_count") != 2_000
    ):
        raise CapacityEnvironmentError("Engine evidence identity or cardinality is invalid")
    aggregated_at = value.get("collected_at_epoch_seconds")
    started_at = value.get("collection_started_at_epoch_seconds")
    finished_at = value.get("collection_finished_at_epoch_seconds")
    spread = value.get("worker_collection_spread_seconds")
    timestamps = (aggregated_at, started_at, finished_at, spread)
    if any(
        isinstance(item, bool)
        or not isinstance(item, (int, float))
        or not math.isfinite(float(item))
        for item in timestamps
    ):
        raise CapacityEnvironmentError("Engine evidence timestamps are invalid")
    assert isinstance(aggregated_at, (int, float))
    assert isinstance(started_at, (int, float))
    assert isinstance(finished_at, (int, float))
    assert isinstance(spread, (int, float))
    current = time.time() if now is None else now
    if (
        aggregated_at <= 0
        or started_at <= 0
        or finished_at <= 0
        or spread < 0
        or finished_at - started_at != spread
        or spread > ENGINE_EVIDENCE_MAX_SPREAD_SECONDS
        or started_at < current - ENGINE_EVIDENCE_MAX_AGE_SECONDS
        or finished_at > current + ENGINE_EVIDENCE_MAX_FUTURE_SKEW_SECONDS
        or aggregated_at < finished_at
        or aggregated_at > current + ENGINE_EVIDENCE_MAX_FUTURE_SKEW_SECONDS
    ):
        raise CapacityEnvironmentError("Engine evidence is stale or has an invalid collection window")
    workers = value.get("workers")
    worker_files = value.get("worker_files")
    if not isinstance(workers, list) or len(workers) != 10:
        raise CapacityEnvironmentError("Engine evidence must contain exactly 10 workers")
    if not isinstance(worker_files, list) or len(worker_files) != 10:
        raise CapacityEnvironmentError("Engine evidence worker file index is incomplete")
    expected_file_index = {
        ordinal: f"worker-{ordinal:02d}.json" for ordinal in range(10)
    }
    indexed_ordinals: set[int] = set()
    for entry in worker_files:
        if not isinstance(entry, dict):
            raise CapacityEnvironmentError("Engine evidence file index entry is invalid")
        ordinal = entry.get("worker_ordinal")
        digest = entry.get("sha256")
        if (
            isinstance(ordinal, bool)
            or not isinstance(ordinal, int)
            or expected_file_index.get(ordinal) != entry.get("file")
            or ordinal in indexed_ordinals
            or not isinstance(digest, str)
            or len(digest) != 64
            or any(character not in "0123456789abcdef" for character in digest)
        ):
            raise CapacityEnvironmentError("Engine evidence file index is invalid")
        indexed_ordinals.add(ordinal)

    planned_nodes = {node["node_id"]: node for node in nodes}
    ordered_services = sorted(
        service["service_id"] for service in fixture["services"]
    )
    expected_services = set(ordered_services)
    observed_nodes: set[str] = set()
    observed_deployments: set[str] = set()
    observed_containers: set[tuple[str, str]] = set()
    worker_starts: list[float] = []
    worker_finishes: list[float] = []
    for worker_ordinal, worker in enumerate(workers):
        if not isinstance(worker, dict):
            raise CapacityEnvironmentError("Engine worker evidence is invalid")
        worker_timestamp = worker.get("collected_at_epoch_seconds")
        worker_started = worker.get("collection_started_at_epoch_seconds")
        worker_finished = worker.get("collection_finished_at_epoch_seconds")
        worker_elapsed = worker.get("collection_elapsed_seconds")
        if (
            worker.get("schema_version") != 1
            or worker.get("candidate_sha") != candidate_sha
            or worker.get("worker_ordinal") != worker_ordinal
            or worker.get("fixture_image") != fixture_image
            or worker.get("engine_count") != 10
            or worker.get("container_count") != 200
            or any(
                isinstance(item, bool)
                or not isinstance(item, (int, float))
                or not math.isfinite(float(item))
                for item in (
                    worker_timestamp,
                    worker_started,
                    worker_finished,
                    worker_elapsed,
                )
            )
            or float(worker_timestamp) <= 0
            or float(worker_started) <= 0
            or float(worker_finished) < float(worker_started)
            or not 0 <= float(worker_elapsed) <= 300
            or abs(
                float(worker_finished)
                - float(worker_started)
                - float(worker_elapsed)
            )
            > 1.0
            or abs(float(worker_timestamp) - float(worker_finished)) > 1.0
        ):
            raise CapacityEnvironmentError("Engine worker evidence identity is invalid")
        worker_starts.append(float(worker_started))
        worker_finishes.append(float(worker_finished))
        engines = worker.get("engines")
        if not isinstance(engines, list) or len(engines) != 10:
            raise CapacityEnvironmentError("Engine worker evidence is incomplete")
        for engine_ordinal, engine in enumerate(engines):
            if not isinstance(engine, dict):
                raise CapacityEnvironmentError("Engine observation is invalid")
            node_id = f"capacity-node-{worker_ordinal:02d}-{engine_ordinal:02d}"
            node_plan = planned_nodes.get(node_id)
            image = engine.get("image")
            containers = engine.get("containers")
            if (
                node_plan is None
                or node_plan.get("engine_ordinal") != engine_ordinal
                or engine.get("engine_ordinal") != engine_ordinal
                or engine.get("node_id") != node_id
                or node_id in observed_nodes
                or engine.get("container_count") != 20
                or not isinstance(containers, list)
                or len(containers) != 20
                or not isinstance(image, dict)
                or image.get("reference") != fixture_image
                or image.get("repo_digest") != fixture_image
                or image.get("oci_revision") != candidate_sha
                or not isinstance(image.get("image_id"), str)
                or not image["image_id"].startswith("sha256:")
            ):
                raise CapacityEnvironmentError("Engine observation identity is invalid")
            observed_nodes.add(node_id)
            engine_services: set[str] = set()
            for container in containers:
                if not isinstance(container, dict):
                    raise CapacityEnvironmentError("container evidence is invalid")
                service_id = container.get("service_id")
                expected_id = (
                    deployment_id(service_id, fixture["version"], node_id)
                    if isinstance(service_id, str)
                    else ""
                )
                deployment = container.get("deployment_id")
                container_id = container.get("container_id")
                identity = (node_id, container_id)
                expected_port = (
                    20_000
                    + engine_ordinal * len(ordered_services)
                    + ordered_services.index(service_id)
                    if service_id in expected_services
                    else -1
                )
                if (
                    service_id not in expected_services
                    or service_id in engine_services
                    or deployment != expected_id
                    or deployment in observed_deployments
                    or container.get("node_id") != node_id
                    or container.get("artifact_digest") != fixture_image
                    or container.get("image_id") != image["image_id"]
                    or container.get("state") != "RUNNING"
                    or container.get("health") != "HEALTHY"
                    or container.get("published_port")
                    != {
                        "container_port": 8080,
                        "host_ip": "0.0.0.0",
                        "host_port": expected_port,
                        "protocol": "tcp",
                    }
                    or not isinstance(container_id, str)
                    or not container_id
                    or identity in observed_containers
                ):
                    raise CapacityEnvironmentError(
                        "container evidence identity, image or health is invalid"
                    )
                engine_services.add(service_id)
                observed_deployments.add(deployment)
                observed_containers.add(identity)
            if engine_services != expected_services:
                raise CapacityEnvironmentError("Engine service set is incomplete")
    if (
        observed_nodes != set(planned_nodes)
        or len(observed_deployments) != 2_000
        or len(observed_containers) != 2_000
        or abs(min(worker_starts) - float(started_at)) > 1.0
        or abs(max(worker_finishes) - float(finished_at)) > 1.0
    ):
        raise CapacityEnvironmentError("Engine evidence does not cover the complete plan")
    return {
        "candidate_sha": candidate_sha,
        "fixture_image": fixture_image,
        "workers": 10,
        "engines": 100,
        "containers": 2_000,
        "running": 2_000,
        "healthy": 2_000,
        "collected_at_epoch_seconds": aggregated_at,
        "worker_collection_spread_seconds": spread,
    }


def deployment_is_ready(
    stored: dict[str, Any],
    node_id: str,
    service: dict[str, Any],
    expected_endpoint: str,
    expected_release_version: str,
) -> bool:
    instance = stored.get("instance")
    return bool(
        isinstance(instance, dict)
        and stored.get("node_id") == node_id
        and str(stored.get("management_mode", "")).upper() == "MANAGED"
        and stored.get("endpoint") == expected_endpoint
        and instance.get("service_id") == service["service_id"]
        and instance.get("release_version") == expected_release_version
        and str(instance.get("observed_state", "")).upper() == "RUNNING"
        and str(instance.get("desired_state", "")).upper() == "RUNNING"
        and str(instance.get("health", "")).upper() == "HEALTHY"
        and isinstance(instance.get("container_id"), str)
        and instance["container_id"]
        and instance.get("artifact_digest") == service["oci_image"]
    )


def production_readiness_matches_candidate(
    readiness: dict[str, Any], candidate_sha: str
) -> bool:
    build = readiness.get("build")
    target = str(build.get("target", "")).lower() if isinstance(build, dict) else ""
    return bool(
        str(readiness.get("status", "")).lower() == "ready"
        and isinstance(build, dict)
        and build.get("version") == "1.0.0"
        and build.get("commit_sha") == candidate_sha
        and build.get("profile") == "production"
        and "x86_64" in target
        and "linux" in target
    )


def install_one(
    client: ApiClient,
    candidate_sha: str,
    fixture: dict[str, Any],
    node: dict[str, Any],
    service: dict[str, Any],
    service_index: int,
    timeout_seconds: int,
) -> str:
    endpoint = capacity_endpoint(node, service["service_id"], service_index)
    result = client.request(
        "POST",
        "/api/v1/store/releases:install",
        {
            "service_id": service["service_id"],
            "catalog_source_id": fixture["catalog_source_id"],
            "version": fixture["version"],
            "channel": fixture["channel"],
            "target_node_id": node["node_id"],
            "endpoint": endpoint,
            "mode": "MANAGED",
            "start": True,
            "migration_policy": "APPLY",
            "gateway_node_id": "",
            "config": {},
            "secret_refs": {},
        },
        idempotency_key=(
            f"capacity-install-{candidate_sha[:12]}-{node['node_id']}-{service['service_id']}"
        ),
        expected=(202,),
    )
    operation_id = result.data.get("operation_id")
    if not isinstance(operation_id, str) or not operation_id:
        raise CapacityEnvironmentError("Store install did not return operation_id")
    wait_operation(client, operation_id, timeout_seconds, candidate_sha)
    return operation_id


def ensure_catalog_source(client: ApiClient, fixture: dict[str, Any]) -> None:
    sources = client.page("/api/v1/store/catalogs")
    matches = [item for item in sources if item.get("id") == fixture["catalog_source_id"]]
    if len(matches) != 1 or not matches[0].get("enabled"):
        raise CapacityEnvironmentError(
            "the signed fixture catalog must already be configured as one enabled trusted source"
        )


def ready_nodes(client: ApiClient, planned: list[dict[str, Any]]) -> None:
    all_nodes = client.page("/api/v1/nodes")
    actual = {item["node_id"]: item for item in all_nodes}
    if len(all_nodes) != len(planned):
        raise CapacityEnvironmentError(
            f"dedicated capacity control plane has {len(all_nodes)} Nodes; expected exactly {len(planned)}"
        )
    missing = [node["node_id"] for node in planned if node["node_id"] not in actual]
    if missing:
        raise CapacityEnvironmentError(f"nodes are not enrolled: {missing[:5]}")
    incompatible = [
        node["node_id"]
        for node in planned
        if not isinstance(actual[node["node_id"]].get("labels"), dict)
        or any(
            actual[node["node_id"]]["labels"].get(key) != expected
            for key, expected in STORE_NODE_LABELS.items()
        )
    ]
    if incompatible:
        raise CapacityEnvironmentError(
            "Nodes lack canonical Docker/Linux/x86_64 Store labels: "
            f"{incompatible[:5]}"
        )
    with concurrent.futures.ThreadPoolExecutor(max_workers=32) as executor:
        results = list(
            executor.map(
                lambda node: client.request(
                    "GET", f"/api/v1/nodes/{urllib.parse.quote(node['node_id'], safe='')}/health"
                ).data,
                planned,
            )
        )
    failed = [value.get("node_id") for value in results if value.get("ready") is not True]
    if failed:
        raise CapacityEnvironmentError(f"nodes are not Ready and reachable: {failed[:5]}")


def ensure_topology(
    client: ApiClient,
    candidate_sha: str,
    spec: dict[str, Any],
    timeout_seconds: int,
) -> tuple[str, str | None]:
    topology_id = spec["topology_id"]
    quoted = urllib.parse.quote(topology_id, safe="")
    try:
        current = client.request("GET", f"/api/v1/topologies/{quoted}")
    except CapacityEnvironmentError as error:
        if "HTTP 404" not in str(error):
            raise
        created = client.request(
            "POST",
            "/api/v1/topologies",
            spec,
            idempotency_key=f"capacity-topology-create-{candidate_sha[:12]}",
            expected=(201,),
        )
        revision = created.data.get("revision")
        if not isinstance(revision, dict) or not isinstance(revision.get("revision_id"), str):
            raise CapacityEnvironmentError("Topology create omitted revision_id")
        revision_id = revision["revision_id"]
        etag = created.headers.get("etag", f'"{revision_id}"')
    else:
        draft = current.data.get("draft")
        if not isinstance(draft, dict) or not isinstance(draft.get("revision_id"), str):
            raise CapacityEnvironmentError("Topology draft response is invalid")
        current_spec = draft.get("spec")
        revision_id = draft["revision_id"]
        etag = current.headers.get("etag", f'"{revision_id}"')
        if current_spec != spec:
            revised = client.request(
                "POST",
                f"/api/v1/topologies/{quoted}/revisions",
                spec,
                if_match=etag,
                idempotency_key=f"capacity-topology-revision-{candidate_sha[:12]}",
                expected=(201,),
            )
            revision = revised.data.get("revision")
            if not isinstance(revision, dict) or not isinstance(
                revision.get("revision_id"), str
            ):
                raise CapacityEnvironmentError("Topology revision omitted revision_id")
            revision_id = revision["revision_id"]
            etag = revised.headers.get("etag", f'"{revision_id}"')
    status = client.request("GET", f"/api/v1/topologies/{quoted}/status").data.get(
        "status"
    )
    if (
        isinstance(status, dict)
        and status.get("observed_revision_id") == revision_id
        and str(status.get("state", "")).upper() == "IN_SYNC"
        and not status.get("drift")
    ):
        return revision_id, status.get("last_operation_id")
    applied = client.request(
        "POST",
        f"/api/v1/topologies/{quoted}:apply",
        {},
        if_match=etag,
        idempotency_key=f"capacity-topology-apply-{candidate_sha[:12]}-{revision_id}",
        expected=(202,),
    )
    operation_id = applied.data.get("operation_id")
    if not isinstance(operation_id, str) or not operation_id:
        raise CapacityEnvironmentError("Topology apply omitted operation_id")
    wait_operation(client, operation_id, timeout_seconds, candidate_sha)
    return revision_id, operation_id


def verify_environment(
    client: ApiClient,
    candidate_sha: str,
    nodes: list[dict[str, Any]],
    fixture: dict[str, Any],
    *,
    network_verifier: Any = verify_capacity_network,
    topology_status_timeout_seconds: float = 180.0,
) -> dict[str, Any]:
    ready = client.request("GET", "/api/v1/healthz/ready").data
    if not production_readiness_matches_candidate(ready, candidate_sha):
        raise CapacityEnvironmentError(
            "readiness build identity is not the Linux x86_64 v1.0.0 production candidate"
        )
    build = ready["build"]
    ready_nodes(client, nodes)
    expected = expected_deployments(nodes, fixture)
    actual = {
        item.get("instance", {}).get("deployment_id"): item
        for item in client.page("/api/v1/deployments")
        if isinstance(item.get("instance"), dict)
    }
    if len(actual) != len(expected):
        raise CapacityEnvironmentError(
            f"dedicated capacity control plane has {len(actual)} Deployments; expected exactly {len(expected)}"
        )
    bad = [
        identifier
        for identifier, (node_id, service, endpoint) in expected.items()
        if identifier not in actual
        or not deployment_is_ready(
            actual[identifier], node_id, service, endpoint, fixture["version"]
        )
    ]
    if bad:
        raise CapacityEnvironmentError(
            f"expected deployments are missing or not healthy Running: {bad[:5]}"
        )
    operations = client.page("/api/v1/operations")
    operation_nodes: set[str] = set()
    for operation in operations:
        request = operation.get("request")
        if (
            operation.get("action") != "release.install"
            or str(operation.get("status", "")).upper() != "SUCCEEDED"
            or not isinstance(request, dict)
        ):
            continue
        deployment_id = request.get("deployment_id")
        target_node_id = request.get("target_node_id")
        if not isinstance(deployment_id, str) or deployment_id not in expected:
            continue
        expected_node_id = expected[deployment_id][0]
        if target_node_id == expected_node_id:
            operation_nodes.add(expected_node_id)
    if len(operation_nodes) < 50:
        raise CapacityEnvironmentError(
            "fewer than 50 distinct Nodes have a successful real Deployment Operation target"
        )
    network_evidence = network_verifier(
        build_topology_spec(nodes, fixture), candidate_sha
    )
    topology_id = urllib.parse.quote(fixture["topology_id"], safe="")
    deadline = time.monotonic() + topology_status_timeout_seconds
    status: Any = None
    while True:
        status = client.request(
            "GET", f"/api/v1/topologies/{topology_id}/status"
        ).data.get("status")
        if isinstance(status, dict) and (
            str(status.get("state", "")).upper() == "IN_SYNC"
            and status.get("desired_revision_id") == status.get("observed_revision_id")
            and not status.get("drift")
            and len(status.get("endpoints", [])) == 2_000
            and len(status.get("links", [])) == 8_000
            and all(
                str(item.get("health", "")).upper() == "HEALTHY"
                and item.get("reachable") is True
                for item in status.get("endpoints", [])
            )
            and all(
                str(item.get("health", "")).upper() == "HEALTHY"
                for item in status.get("links", [])
            )
        ):
            break
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        time.sleep(min(2.0, remaining))
    if not isinstance(status, dict) or (
        str(status.get("state", "")).upper() != "IN_SYNC"
        or status.get("desired_revision_id") != status.get("observed_revision_id")
        or status.get("drift")
        or len(status.get("endpoints", [])) != 2_000
        or len(status.get("links", [])) != 8_000
        or any(
            str(item.get("health", "")).upper() != "HEALTHY"
            or item.get("reachable") is not True
            for item in status.get("endpoints", [])
        )
        or any(
            str(item.get("health", "")).upper() != "HEALTHY"
            for item in status.get("links", [])
        )
    ):
        raise CapacityEnvironmentError(
            "Topology did not converge within the bounded observation window to "
            "IN_SYNC with 2,000 healthy Endpoints, 8,000 healthy Links and zero drift"
        )
    return {
        "candidate_sha": candidate_sha,
        "build": build,
        "nodes_ready": len(nodes),
        "deployments_running": len(expected),
        "topology_id": fixture["topology_id"],
        "topology_revision_id": status.get("observed_revision_id"),
        "topology_endpoints": len(status["endpoints"]),
        "topology_links": len(status["links"]),
        "topology_drift": 0,
        "operation_target_nodes": len(operation_nodes),
        "network_evidence": network_evidence,
    }


def seed(args: argparse.Namespace, client: ApiClient) -> dict[str, Any]:
    nodes = validate_nodes(load_json(args.nodes_file), args.expected_nodes)
    fixture = validate_fixture(load_json(args.fixture_file), args.expected_services)
    ensure_catalog_source(client, fixture)
    ready_nodes(client, nodes)
    expected = expected_deployments(nodes, fixture)
    existing = {
        item.get("instance", {}).get("deployment_id"): item
        for item in client.page("/api/v1/deployments")
        if isinstance(item.get("instance"), dict)
    }
    installed = 0
    skipped = 0
    for service_index, service in enumerate(
        sorted(fixture["services"], key=lambda item: item["service_id"])
    ):
        pending: list[dict[str, Any]] = []
        for node in nodes:
            identifier = deployment_id(service["service_id"], fixture["version"], node["node_id"])
            if identifier in existing:
                if not deployment_is_ready(
                    existing[identifier],
                    node["node_id"],
                    service,
                    capacity_endpoint(node, service["service_id"], service_index),
                    fixture["version"],
                ):
                    raise CapacityEnvironmentError(
                        f"existing deployment {identifier} does not match the immutable "
                        "endpoint/release fixture; controlled reprovisioning with the expected "
                        "binding is required"
                    )
                skipped += 1
            else:
                pending.append(node)
        with concurrent.futures.ThreadPoolExecutor(
            max_workers=args.install_concurrency
        ) as executor:
            futures = [
                executor.submit(
                    install_one,
                    client,
                    args.candidate_sha,
                    fixture,
                    node,
                    service,
                    service_index,
                    args.operation_timeout_seconds,
                )
                for node in pending
            ]
            for future in concurrent.futures.as_completed(futures):
                future.result()
                installed += 1
    if installed + skipped != len(expected):
        raise CapacityEnvironmentError("Store seed did not account for every deployment")
    spec = build_topology_spec(nodes, fixture)
    if len(spec["endpoints"]) != 2_000 or len(spec["links"]) != 8_000:
        raise CapacityEnvironmentError("generated TopologySpec has incorrect cardinality")
    revision_id, topology_operation_id = ensure_topology(
        client,
        args.candidate_sha,
        spec,
        args.operation_timeout_seconds,
    )
    verified = verify_environment(client, args.candidate_sha, nodes, fixture)
    return {
        **verified,
        "deployments_installed": installed,
        "deployments_reused": skipped,
        "topology_revision_id": revision_id,
        "topology_operation_id": topology_operation_id,
    }


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    result.add_argument("--base-url", default=os.getenv("ORCHESTRATOR_GATE_BASE_URL", ""))
    result.add_argument(
        "--ca-file",
        type=pathlib.Path,
        default=os.getenv("ORCHESTRATOR_GATE_CA_FILE", ""),
    )
    result.add_argument(
        "--token-argv-json",
        default=os.getenv("ORCHESTRATOR_GATE_TOKEN_ARGV_JSON", ""),
    )
    result.add_argument("--candidate-sha", default=os.getenv("GITHUB_SHA", ""))
    result.add_argument("--nodes-file", type=pathlib.Path, required=True)
    result.add_argument("--expected-nodes", type=int, default=100)
    subparsers = result.add_subparsers(dest="command", required=True)
    enrollment = subparsers.add_parser("issue-enrollment")
    enrollment.add_argument("--output-dir", type=pathlib.Path, required=True)
    enrollment.add_argument("--enrollment-generation", required=True)
    for name in ("seed", "preflight"):
        command = subparsers.add_parser(name)
        command.add_argument("--fixture-file", type=pathlib.Path, required=True)
        command.add_argument("--expected-services", type=int, default=20)
        command.add_argument("--operation-timeout-seconds", type=int, default=900)
        command.add_argument("--install-concurrency", type=int, default=50)
        if name == "preflight":
            command.add_argument(
                "--engine-evidence-file", type=pathlib.Path, required=True
            )
    render = subparsers.add_parser("render-topology")
    render.add_argument("--fixture-file", type=pathlib.Path, required=True)
    render.add_argument("--expected-services", type=int, default=20)
    render.add_argument("--output", type=pathlib.Path, required=True)
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        args.candidate_sha = require_candidate_sha(args.candidate_sha)
        nodes = validate_nodes(load_json(args.nodes_file), args.expected_nodes)
        if args.command == "render-topology":
            fixture = validate_fixture(load_json(args.fixture_file), args.expected_services)
            content = json.dumps(
                build_topology_spec(nodes, fixture),
                sort_keys=True,
                separators=(",", ":"),
            ).encode()
            atomic_write(args.output, content, 0o644)
            summary = {"bytes": len(content), "endpoints": 2_000, "links": 8_000}
        else:
            if not args.base_url or not args.token_argv_json or not str(args.ca_file):
                raise CapacityEnvironmentError(
                    "base URL, CA file and token helper argv are required for API commands"
                )
            client = ApiClient(
                args.base_url, pathlib.Path(args.ca_file), TokenProvider(args.token_argv_json)
            )
            if args.command == "issue-enrollment":
                summary = issue_enrollment(args, client)
            elif args.command == "seed":
                if not 1 <= args.install_concurrency <= 50:
                    raise CapacityEnvironmentError("install concurrency must be 1-50")
                summary = seed(args, client)
            else:
                fixture = validate_fixture(
                    load_json(args.fixture_file), args.expected_services
                )
                summary = verify_environment(client, args.candidate_sha, nodes, fixture)
                engine_evidence = load_json(args.engine_evidence_file)
                summary["engine_evidence"] = {
                    **verify_engine_evidence(
                        engine_evidence, args.candidate_sha, nodes, fixture
                    ),
                    "sha256": hashlib.sha256(
                        args.engine_evidence_file.read_bytes()
                    ).hexdigest(),
                }
        print(json.dumps({"status": "ok", "data": summary}, sort_keys=True))
        return 0
    except CapacityEnvironmentError as error:
        print(f"capacity environment failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
