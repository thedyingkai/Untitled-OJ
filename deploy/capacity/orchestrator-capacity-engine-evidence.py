#!/usr/bin/env python3
"""Collect and aggregate real container evidence from isolated DinD Engines.

Every Docker invocation is an argv-only subprocess with ``shell=False``.  The
collector lists every container in a dedicated Engine and then inspects each
container individually, so a cached image or a stale control-plane projection
cannot stand in for a real Running workload.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import pathlib
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from typing import Any, Callable, Sequence


SCHEMA_VERSION = 1
ENGINE_COUNT_PER_WORKER = 10
SERVICE_COUNT_PER_ENGINE = 20
WORKER_COUNT = 10
WORKER_COLLECTION_BUDGET_SECONDS = 300
ENGINE_COLLECTION_BUDGET_SECONDS = 30
MAX_WORKER_COLLECTION_SPREAD_SECONDS = 90
MAX_EVIDENCE_AGE_SECONDS = 300
MAX_FUTURE_CLOCK_SKEW_SECONDS = 30
SHA40 = frozenset("0123456789abcdef")


class EngineEvidenceError(RuntimeError):
    pass


@dataclass(frozen=True)
class Fixture:
    version: str
    services: tuple[str, ...]
    image: str


def require_candidate_sha(value: str) -> str:
    normalized = value.strip().lower()
    if (
        value != normalized
        or len(normalized) != 40
        or any(character not in SHA40 for character in normalized)
    ):
        raise EngineEvidenceError(
            "candidate SHA must be exactly 40 lowercase hexadecimal characters"
        )
    return normalized


def deployment_id(service_id: str, version: str, node_id: str) -> str:
    digest = hashlib.sha256(
        service_id.encode("utf-8")
        + b"\0"
        + version.encode("utf-8")
        + b"\0"
        + node_id.encode("utf-8")
    ).hexdigest()
    return f"deployment-{service_id}-{digest}"[:56]


def load_json(path: pathlib.Path, maximum_bytes: int = 32 * 1024 * 1024) -> Any:
    try:
        with path.open("rb") as stream:
            raw = stream.read(maximum_bytes + 1)
    except OSError as error:
        raise EngineEvidenceError(f"cannot read {path}: {error}") from error
    if len(raw) > maximum_bytes:
        raise EngineEvidenceError(f"JSON document is too large: {path}")
    try:
        return json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise EngineEvidenceError(f"invalid JSON in {path}: {error}") from error


def load_fixture(path: pathlib.Path, expected_image: str) -> Fixture:
    document = load_json(path)
    if not isinstance(document, dict) or document.get("schema_version") != 1:
        raise EngineEvidenceError("fixture must use schema_version 1")
    version = document.get("version")
    services = document.get("services")
    if not isinstance(version, str) or not version:
        raise EngineEvidenceError("fixture version is missing")
    if not isinstance(services, list) or len(services) != SERVICE_COUNT_PER_ENGINE:
        raise EngineEvidenceError("fixture must contain exactly 20 services")
    service_ids: list[str] = []
    for service in services:
        if not isinstance(service, dict):
            raise EngineEvidenceError("fixture service must be an object")
        service_id = service.get("service_id")
        image = service.get("oci_image")
        if (
            not isinstance(service_id, str)
            or not service_id
            or service_id in service_ids
            or image != expected_image
        ):
            raise EngineEvidenceError(
                "fixture services must be unique and use the expected immutable image"
            )
        service_ids.append(service_id)
    return Fixture(version, tuple(sorted(service_ids)), expected_image)


def parse_single_inspect(raw: str, label: str) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise EngineEvidenceError(f"{label} returned invalid JSON") from error
    if not isinstance(value, list) or len(value) != 1 or not isinstance(value[0], dict):
        raise EngineEvidenceError(f"{label} must return exactly one inspect object")
    return value[0]


def parse_container_inspects(
    raw: str, expected_ids: Sequence[str], label: str
) -> list[dict[str, Any]]:
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise EngineEvidenceError(f"{label} returned invalid JSON") from error
    if not isinstance(value, list) or any(not isinstance(item, dict) for item in value):
        raise EngineEvidenceError(f"{label} must return an inspect object array")
    observed_ids = [item.get("Id") for item in value]
    if (
        len(value) != len(expected_ids)
        or any(not isinstance(item, str) or not item for item in observed_ids)
        or len(set(observed_ids)) != len(observed_ids)
        or set(observed_ids) != set(expected_ids)
    ):
        raise EngineEvidenceError(
            f"{label} must return exactly one object for every requested container"
        )
    by_id = {item["Id"]: item for item in value}
    return [by_id[identifier] for identifier in expected_ids]


def validate_engine_observation(
    *,
    worker_ordinal: int,
    engine_ordinal: int,
    candidate_sha: str,
    fixture: Fixture,
    image_inspect: dict[str, Any],
    container_inspects: Sequence[dict[str, Any]],
) -> dict[str, Any]:
    if not 0 <= worker_ordinal < WORKER_COUNT:
        raise EngineEvidenceError("worker ordinal must be between 0 and 9")
    if not 0 <= engine_ordinal < ENGINE_COUNT_PER_WORKER:
        raise EngineEvidenceError("Engine ordinal must be between 0 and 9")
    if len(container_inspects) != SERVICE_COUNT_PER_ENGINE:
        raise EngineEvidenceError(
            f"engine-{engine_ordinal:02d} has {len(container_inspects)} containers; expected exactly 20"
        )

    image_id = image_inspect.get("Id")
    repo_digests = image_inspect.get("RepoDigests")
    image_config = image_inspect.get("Config")
    image_labels = image_config.get("Labels") if isinstance(image_config, dict) else None
    if (
        not isinstance(image_id, str)
        or not image_id.startswith("sha256:")
        or not isinstance(repo_digests, list)
        or fixture.image not in repo_digests
        or not isinstance(image_labels, dict)
        or image_labels.get("org.opencontainers.image.revision") != candidate_sha
    ):
        raise EngineEvidenceError(
            f"engine-{engine_ordinal:02d} fixture image identity does not match the candidate"
        )

    node_id = f"capacity-node-{worker_ordinal:02d}-{engine_ordinal:02d}"
    expected = {
        deployment_id(service_id, fixture.version, node_id): service_id
        for service_id in fixture.services
    }
    observed: dict[str, dict[str, Any]] = {}
    for inspection in container_inspects:
        container_id = inspection.get("Id")
        config = inspection.get("Config")
        state = inspection.get("State")
        labels = config.get("Labels") if isinstance(config, dict) else None
        health = state.get("Health") if isinstance(state, dict) else None
        host_config = inspection.get("HostConfig")
        network_settings = inspection.get("NetworkSettings")
        if not isinstance(container_id, str) or not container_id:
            raise EngineEvidenceError(
                f"engine-{engine_ordinal:02d} returned a container without an ID"
            )
        if not isinstance(config, dict) or not isinstance(labels, dict):
            raise EngineEvidenceError(f"container {container_id} has no labels")
        if (
            not isinstance(state, dict)
            or state.get("Running") is not True
            or str(state.get("Status", "")).lower() != "running"
        ):
            raise EngineEvidenceError(f"container {container_id} is not Running")
        if not isinstance(health, dict) or str(health.get("Status", "")).lower() != "healthy":
            raise EngineEvidenceError(f"container {container_id} is not Docker healthy")
        if config.get("Image") != fixture.image or inspection.get("Image") != image_id:
            raise EngineEvidenceError(
                f"container {container_id} does not use the inspected fixture image ID"
            )

        observed_deployment = labels.get("ojos.deployment_id")
        observed_service = labels.get("ojos.service_id")
        service_index = (
            fixture.services.index(observed_service)
            if isinstance(observed_service, str)
            and observed_service in fixture.services
            else -1
        )
        expected_host_port = (
            20_000 + engine_ordinal * SERVICE_COUNT_PER_ENGINE + service_index
        )
        expected_binding = [{"HostIp": "0.0.0.0", "HostPort": str(expected_host_port)}]
        host_bindings = (
            host_config.get("PortBindings") if isinstance(host_config, dict) else None
        )
        network_ports = (
            network_settings.get("Ports")
            if isinstance(network_settings, dict)
            else None
        )
        expected_labels = {
            "ojos.target_node_id": node_id,
            "ojos.artifact_digest": fixture.image,
            "ojos.release_version": fixture.version,
            "ojos.generation": "1",
        }
        if (
            not isinstance(observed_deployment, str)
            or observed_deployment not in expected
            or observed_service != expected[observed_deployment]
            or any(labels.get(key) != value for key, value in expected_labels.items())
            or service_index < 0
        ):
            raise EngineEvidenceError(
                f"container {container_id} has an unexpected deployment/service/node identity"
            )
        if (
            not isinstance(host_bindings, dict)
            or set(host_bindings) != {"8080/tcp"}
            or host_bindings.get("8080/tcp") != expected_binding
            or not isinstance(network_ports, dict)
            or set(network_ports) != {"8080/tcp"}
            or network_ports.get("8080/tcp") != expected_binding
        ):
            raise EngineEvidenceError(
                f"container {container_id} is not exclusively bound from 8080/tcp "
                f"to 0.0.0.0:{expected_host_port}"
            )
        if observed_deployment in observed:
            raise EngineEvidenceError(
                f"engine-{engine_ordinal:02d} has duplicate deployment {observed_deployment}"
            )
        observed[observed_deployment] = {
            "container_id": container_id,
            "container_name": str(inspection.get("Name", "")).removeprefix("/"),
            "deployment_id": observed_deployment,
            "service_id": observed_service,
            "node_id": node_id,
            "image_id": image_id,
            "artifact_digest": fixture.image,
            "state": "RUNNING",
            "health": "HEALTHY",
            "published_port": {
                "container_port": 8080,
                "host_ip": "0.0.0.0",
                "host_port": expected_host_port,
                "protocol": "tcp",
            },
        }

    missing = sorted(set(expected) - set(observed))
    unexpected = sorted(set(observed) - set(expected))
    if missing or unexpected:
        raise EngineEvidenceError(
            f"engine-{engine_ordinal:02d} deployment set mismatch; "
            f"missing={missing[:3]} unexpected={unexpected[:3]}"
        )
    return {
        "engine_ordinal": engine_ordinal,
        "node_id": node_id,
        "image": {
            "reference": fixture.image,
            "image_id": image_id,
            "repo_digest": fixture.image,
            "oci_revision": candidate_sha,
        },
        "container_count": len(observed),
        "containers": [observed[key] for key in sorted(observed)],
    }


class ComposeEngineClient:
    def __init__(
        self,
        compose_file: pathlib.Path,
        project_directory: pathlib.Path,
        *,
        runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    ) -> None:
        self.prefix = (
            "docker",
            "compose",
            "--project-directory",
            str(project_directory),
            "-f",
            str(compose_file),
            "exec",
            "--no-tty",
        )
        self._runner = runner

    def run(
        self,
        engine_ordinal: int,
        *docker_argv: str,
        timeout_seconds: float = ENGINE_COLLECTION_BUDGET_SECONDS,
    ) -> str:
        argv = [*self.prefix, f"engine-{engine_ordinal:02d}", "docker", *docker_argv]
        completed = self._runner(
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=max(0.1, timeout_seconds),
            check=False,
            shell=False,
        )
        if completed.returncode != 0:
            raise EngineEvidenceError(
                f"Engine Docker argv failed with exit {completed.returncode}: {argv!r}"
            )
        if not isinstance(completed.stdout, str) or len(completed.stdout) > 16 * 1024 * 1024:
            raise EngineEvidenceError("Engine Docker output is missing or oversized")
        return completed.stdout


def collect_worker(
    client: ComposeEngineClient,
    worker_ordinal: int,
    candidate_sha: str,
    fixture: Fixture,
    *,
    monotonic: Callable[[], float] = time.monotonic,
) -> dict[str, Any]:
    wall_started_at = time.time()
    collection_started = monotonic()
    overall_deadline = collection_started + WORKER_COLLECTION_BUDGET_SECONDS
    engines: list[dict[str, Any]] = []
    for engine_ordinal in range(ENGINE_COUNT_PER_WORKER):
        engine_deadline = min(
            overall_deadline, monotonic() + ENGINE_COLLECTION_BUDGET_SECONDS
        )

        def remaining() -> float:
            seconds = engine_deadline - monotonic()
            if seconds <= 0:
                raise EngineEvidenceError(
                    f"engine-{engine_ordinal:02d} evidence deadline expired"
                )
            return seconds

        raw_ids = client.run(
            engine_ordinal,
            "container",
            "ls",
            "--all",
            "--no-trunc",
            "--quiet",
            timeout_seconds=remaining(),
        )
        container_ids = [line.strip() for line in raw_ids.splitlines() if line.strip()]
        if len(container_ids) != len(set(container_ids)):
            raise EngineEvidenceError(
                f"engine-{engine_ordinal:02d} returned duplicate container IDs"
            )
        if len(container_ids) != SERVICE_COUNT_PER_ENGINE:
            raise EngineEvidenceError(
                f"engine-{engine_ordinal:02d} has {len(container_ids)} containers; expected exactly 20"
            )
        image_inspect = parse_single_inspect(
            client.run(
                engine_ordinal,
                "image",
                "inspect",
                fixture.image,
                timeout_seconds=remaining(),
            ),
            f"engine-{engine_ordinal:02d} image inspect",
        )
        container_inspects = parse_container_inspects(
            client.run(
                engine_ordinal,
                "container",
                "inspect",
                *container_ids,
                timeout_seconds=remaining(),
            ),
            container_ids,
            f"engine-{engine_ordinal:02d} container inspect",
        )
        engines.append(
            validate_engine_observation(
                worker_ordinal=worker_ordinal,
                engine_ordinal=engine_ordinal,
                candidate_sha=candidate_sha,
                fixture=fixture,
                image_inspect=image_inspect,
                container_inspects=container_inspects,
            )
        )
    collection_elapsed = monotonic() - collection_started
    wall_finished_at = wall_started_at + collection_elapsed
    return {
        "schema_version": SCHEMA_VERSION,
        "candidate_sha": candidate_sha,
        "worker_ordinal": worker_ordinal,
        "fixture_image": fixture.image,
        "engine_count": len(engines),
        "container_count": sum(engine["container_count"] for engine in engines),
        "collected_at_epoch_seconds": wall_finished_at,
        "collection_started_at_epoch_seconds": wall_started_at,
        "collection_finished_at_epoch_seconds": wall_finished_at,
        "collection_elapsed_seconds": collection_elapsed,
        "engines": engines,
    }


def validate_worker_evidence(
    document: Any,
    *,
    worker_ordinal: int,
    candidate_sha: str,
    fixture: Fixture,
) -> dict[str, Any]:
    if not isinstance(document, dict) or document.get("schema_version") != SCHEMA_VERSION:
        raise EngineEvidenceError("worker evidence must use schema_version 1")
    if (
        document.get("candidate_sha") != candidate_sha
        or document.get("worker_ordinal") != worker_ordinal
        or document.get("fixture_image") != fixture.image
    ):
        raise EngineEvidenceError("worker evidence identity does not match the candidate")
    collected_at = document.get("collected_at_epoch_seconds")
    started_at = document.get("collection_started_at_epoch_seconds")
    finished_at = document.get("collection_finished_at_epoch_seconds")
    elapsed = document.get("collection_elapsed_seconds")
    if (
        any(
            isinstance(item, bool)
            or not isinstance(item, (int, float))
            or not math.isfinite(float(item))
            for item in (collected_at, started_at, finished_at, elapsed)
        )
        or float(collected_at) <= 0
        or float(started_at) <= 0
        or float(finished_at) < float(started_at)
        or not 0 <= float(elapsed) <= WORKER_COLLECTION_BUDGET_SECONDS
        or abs(float(finished_at) - float(started_at) - float(elapsed)) > 1.0
        or abs(float(collected_at) - float(finished_at)) > 1.0
    ):
        raise EngineEvidenceError("worker evidence collection timing is invalid")
    engines = document.get("engines")
    if not isinstance(engines, list) or len(engines) != ENGINE_COUNT_PER_WORKER:
        raise EngineEvidenceError("worker evidence must contain exactly 10 Engines")
    expected_nodes = {
        f"capacity-node-{worker_ordinal:02d}-{engine:02d}"
        for engine in range(ENGINE_COUNT_PER_WORKER)
    }
    actual_nodes: set[str] = set()
    total = 0
    for expected_ordinal, engine in enumerate(engines):
        if not isinstance(engine, dict) or engine.get("engine_ordinal") != expected_ordinal:
            raise EngineEvidenceError("worker Engine evidence is not ordered or complete")
        node_id = engine.get("node_id")
        image = engine.get("image")
        containers = engine.get("containers")
        if (
            node_id not in expected_nodes
            or node_id in actual_nodes
            or not isinstance(image, dict)
            or image.get("reference") != fixture.image
            or image.get("repo_digest") != fixture.image
            or image.get("oci_revision") != candidate_sha
            or not isinstance(containers, list)
            or len(containers) != SERVICE_COUNT_PER_ENGINE
            or engine.get("container_count") != SERVICE_COUNT_PER_ENGINE
        ):
            raise EngineEvidenceError("worker Engine evidence is incomplete or inconsistent")
        actual_nodes.add(node_id)
        expected_deployments = {
            deployment_id(service, fixture.version, node_id): service
            for service in fixture.services
        }
        actual_deployments: dict[str, str] = {}
        for container in containers:
            if not isinstance(container, dict):
                raise EngineEvidenceError("container evidence must be an object")
            identifier = container.get("deployment_id")
            service_id = container.get("service_id")
            if (
                not isinstance(identifier, str)
                or identifier in actual_deployments
                or expected_deployments.get(identifier) != service_id
                or container.get("node_id") != node_id
                or container.get("artifact_digest") != fixture.image
                or container.get("image_id") != image.get("image_id")
                or container.get("state") != "RUNNING"
                or container.get("health") != "HEALTHY"
                or container.get("published_port")
                != {
                    "container_port": 8080,
                    "host_ip": "0.0.0.0",
                    "host_port": 20_000
                    + expected_ordinal * SERVICE_COUNT_PER_ENGINE
                    + fixture.services.index(service_id),
                    "protocol": "tcp",
                }
                or not isinstance(container.get("container_id"), str)
                or not container["container_id"]
            ):
                raise EngineEvidenceError("container evidence identity or health is invalid")
            actual_deployments[identifier] = service_id
        if actual_deployments != expected_deployments:
            raise EngineEvidenceError("worker evidence deployment set does not match the plan")
        total += len(containers)
    if actual_nodes != expected_nodes or total != 200:
        raise EngineEvidenceError("worker evidence does not prove 10 Engines x 20 containers")
    if document.get("engine_count") != 10 or document.get("container_count") != 200:
        raise EngineEvidenceError("worker evidence summary counters are inconsistent")
    return document


def aggregate_workers(
    input_dir: pathlib.Path,
    candidate_sha: str,
    fixture: Fixture,
    *,
    now: Callable[[], float] = time.time,
) -> dict[str, Any]:
    expected_names = {f"worker-{ordinal:02d}.json" for ordinal in range(WORKER_COUNT)}
    try:
        actual_names = {path.name for path in input_dir.iterdir() if path.is_file()}
    except OSError as error:
        raise EngineEvidenceError(f"cannot enumerate worker evidence: {error}") from error
    if actual_names != expected_names:
        raise EngineEvidenceError(
            "worker evidence directory must contain exactly worker-00.json through worker-09.json"
        )
    workers: list[dict[str, Any]] = []
    worker_files: list[dict[str, Any]] = []
    collection_starts: list[float] = []
    collection_finishes: list[float] = []
    for worker_ordinal in range(WORKER_COUNT):
        path = input_dir / f"worker-{worker_ordinal:02d}.json"
        raw = path.read_bytes()
        document = validate_worker_evidence(
            json.loads(raw),
            worker_ordinal=worker_ordinal,
            candidate_sha=candidate_sha,
            fixture=fixture,
        )
        workers.append(document)
        collection_starts.append(float(document["collection_started_at_epoch_seconds"]))
        collection_finishes.append(float(document["collection_finished_at_epoch_seconds"]))
        worker_files.append(
            {
                "worker_ordinal": worker_ordinal,
                "file": path.name,
                "sha256": hashlib.sha256(raw).hexdigest(),
            }
        )
    aggregated_at = now()
    oldest = min(collection_starts)
    newest = max(collection_finishes)
    if newest - oldest > MAX_WORKER_COLLECTION_SPREAD_SECONDS:
        raise EngineEvidenceError("worker evidence collection window exceeds 90 seconds")
    if oldest < aggregated_at - MAX_EVIDENCE_AGE_SECONDS:
        raise EngineEvidenceError("worker evidence is older than five minutes")
    if newest > aggregated_at + MAX_FUTURE_CLOCK_SKEW_SECONDS:
        raise EngineEvidenceError("worker evidence timestamp is in the future")
    return {
        "schema_version": SCHEMA_VERSION,
        "candidate_sha": candidate_sha,
        "fixture_image": fixture.image,
        "worker_count": WORKER_COUNT,
        "engine_count": WORKER_COUNT * ENGINE_COUNT_PER_WORKER,
        "container_count": WORKER_COUNT
        * ENGINE_COUNT_PER_WORKER
        * SERVICE_COUNT_PER_ENGINE,
        "collected_at_epoch_seconds": aggregated_at,
        "collection_started_at_epoch_seconds": oldest,
        "collection_finished_at_epoch_seconds": newest,
        "worker_collection_spread_seconds": newest - oldest,
        "worker_files": worker_files,
        "workers": workers,
    }


def atomic_write_json(path: pathlib.Path, value: Any) -> None:
    content = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = pathlib.Path(name)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        os.chmod(path, 0o644)
    finally:
        temporary.unlink(missing_ok=True)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    subparsers = result.add_subparsers(dest="command", required=True)
    collect = subparsers.add_parser("collect")
    collect.add_argument("--compose-file", type=pathlib.Path, required=True)
    collect.add_argument("--project-directory", type=pathlib.Path, required=True)
    collect.add_argument("--fixture-file", type=pathlib.Path, required=True)
    collect.add_argument("--fixture-image", required=True)
    collect.add_argument("--candidate-sha", required=True)
    collect.add_argument("--worker-ordinal", type=int, required=True)
    collect.add_argument("--output", type=pathlib.Path, required=True)

    aggregate = subparsers.add_parser("aggregate")
    aggregate.add_argument("--input-dir", type=pathlib.Path, required=True)
    aggregate.add_argument("--fixture-file", type=pathlib.Path, required=True)
    aggregate.add_argument("--fixture-image", required=True)
    aggregate.add_argument("--candidate-sha", required=True)
    aggregate.add_argument("--output", type=pathlib.Path, required=True)
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        candidate_sha = require_candidate_sha(args.candidate_sha)
        fixture = load_fixture(args.fixture_file, args.fixture_image)
        if args.command == "collect":
            client = ComposeEngineClient(args.compose_file, args.project_directory)
            evidence = collect_worker(client, args.worker_ordinal, candidate_sha, fixture)
        else:
            evidence = aggregate_workers(args.input_dir, candidate_sha, fixture)
        atomic_write_json(args.output, evidence)
        print(
            json.dumps(
                {
                    "status": "ok",
                    "output": str(args.output),
                    "engine_count": evidence["engine_count"],
                    "container_count": evidence["container_count"],
                },
                sort_keys=True,
            )
        )
        return 0
    except (EngineEvidenceError, OSError, json.JSONDecodeError) as error:
        print(f"capacity Engine evidence failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
