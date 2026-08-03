from __future__ import annotations

import importlib.util
import json
import pathlib
import subprocess
import sys
import tempfile
import time
import unittest
from typing import Any


MODULE_PATH = (
    pathlib.Path(__file__).parents[2]
    / "capacity"
    / "orchestrator-capacity-engine-evidence.py"
)
SPEC = importlib.util.spec_from_file_location("capacity_engine_evidence", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
EVIDENCE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = EVIDENCE
SPEC.loader.exec_module(EVIDENCE)

CANDIDATE = "a" * 40
IMAGE = "registry.example/capacity@sha256:" + "b" * 64
IMAGE_ID = "sha256:" + "c" * 64


def fixture() -> Any:
    return EVIDENCE.Fixture(
        "1.0.0", tuple(f"capacity-{index:02d}" for index in range(20)), IMAGE
    )


def image_inspect() -> dict[str, Any]:
    return {
        "Id": IMAGE_ID,
        "RepoDigests": [IMAGE],
        "Config": {"Labels": {"org.opencontainers.image.revision": CANDIDATE}},
    }


def container_inspects(worker: int = 0, engine: int = 0) -> list[dict[str, Any]]:
    node_id = f"capacity-node-{worker:02d}-{engine:02d}"
    result = []
    for index, service_id in enumerate(fixture().services):
        deployment_id = EVIDENCE.deployment_id(service_id, "1.0.0", node_id)
        host_port = 20_000 + engine * 20 + index
        binding = [{"HostIp": "0.0.0.0", "HostPort": str(host_port)}]
        result.append(
            {
                "Id": f"container-{index:02d}",
                "Name": f"/{deployment_id}",
                "Image": IMAGE_ID,
                "Config": {
                    "Image": IMAGE,
                    "Labels": {
                        "ojos.deployment_id": deployment_id,
                        "ojos.service_id": service_id,
                        "ojos.target_node_id": node_id,
                        "ojos.artifact_digest": IMAGE,
                        "ojos.release_version": "1.0.0",
                        "ojos.generation": "1",
                    },
                },
                "State": {
                    "Running": True,
                    "Status": "running",
                    "Health": {"Status": "healthy"},
                },
                "HostConfig": {"PortBindings": {"8080/tcp": binding}},
                "NetworkSettings": {"Ports": {"8080/tcp": binding}},
            }
        )
    return result


class FakeComposeClient:
    def __init__(self) -> None:
        self.calls: list[tuple[int, tuple[str, ...]]] = []

    def run(self, engine: int, *argv: str, timeout_seconds: float = 30) -> str:
        self.assert_timeout(timeout_seconds)
        self.calls.append((engine, argv))
        if argv[:3] == ("container", "ls", "--all"):
            return "\n".join(
                container["Id"] for container in container_inspects(0, engine)
            )
        if argv[:2] == ("image", "inspect"):
            return json.dumps([image_inspect()])
        if argv[:2] == ("container", "inspect"):
            identifiers = set(argv[2:])
            matches = [
                container
                for container in container_inspects(0, engine)
                if container["Id"] in identifiers
            ]
            return json.dumps(matches)
        raise AssertionError(argv)

    @staticmethod
    def assert_timeout(timeout_seconds: float) -> None:
        if not 0 < timeout_seconds <= 30:
            raise AssertionError(f"unexpected timeout: {timeout_seconds}")


class CapacityEngineEvidenceTests(unittest.TestCase):
    def test_accepts_exactly_twenty_real_healthy_candidate_containers(self) -> None:
        observed = EVIDENCE.validate_engine_observation(
            worker_ordinal=0,
            engine_ordinal=0,
            candidate_sha=CANDIDATE,
            fixture=fixture(),
            image_inspect=image_inspect(),
            container_inspects=container_inspects(),
        )
        self.assertEqual(observed["container_count"], 20)
        self.assertEqual(len({item["container_id"] for item in observed["containers"]}), 20)

    def test_rejects_zero_nineteen_and_twenty_one_containers(self) -> None:
        all_containers = container_inspects()
        for containers in ([], all_containers[:-1], [*all_containers, all_containers[0]]):
            with self.subTest(count=len(containers)), self.assertRaisesRegex(
                EVIDENCE.EngineEvidenceError, "exactly 20"
            ):
                EVIDENCE.validate_engine_observation(
                    worker_ordinal=0,
                    engine_ordinal=0,
                    candidate_sha=CANDIDATE,
                    fixture=fixture(),
                    image_inspect=image_inspect(),
                    container_inspects=containers,
                )

    def test_rejects_stopped_unhealthy_wrong_label_and_wrong_image(self) -> None:
        mutations = (
            lambda item: item["State"].update(Running=False, Status="exited"),
            lambda item: item["State"]["Health"].update(Status="unhealthy"),
            lambda item: item["Config"]["Labels"].update(
                {"ojos.target_node_id": "capacity-node-99-99"}
            ),
            lambda item: item["Config"].update({"Image": "floating:latest"}),
            lambda item: item.update({"Image": "sha256:" + "d" * 64}),
            lambda item: item["HostConfig"]["PortBindings"]["8080/tcp"][0].update(
                {"HostPort": "29999"}
            ),
            lambda item: item["NetworkSettings"]["Ports"].update(
                {"9090/tcp": [{"HostIp": "0.0.0.0", "HostPort": "29090"}]}
            ),
        )
        for mutate in mutations:
            containers = container_inspects()
            mutate(containers[0])
            with self.subTest(mutate=mutate), self.assertRaises(
                EVIDENCE.EngineEvidenceError
            ):
                EVIDENCE.validate_engine_observation(
                    worker_ordinal=0,
                    engine_ordinal=0,
                    candidate_sha=CANDIDATE,
                    fixture=fixture(),
                    image_inspect=image_inspect(),
                    container_inspects=containers,
                )

    def test_collect_inspects_every_container_in_a_bounded_batch(self) -> None:
        client = FakeComposeClient()
        result = EVIDENCE.collect_worker(client, 0, CANDIDATE, fixture())
        self.assertEqual(result["engine_count"], 10)
        self.assertEqual(result["container_count"], 200)
        container_inspect_calls = [
            call for call in client.calls if call[1][:2] == ("container", "inspect")
        ]
        self.assertEqual(len(container_inspect_calls), 10)
        self.assertTrue(all(len(call[1][2:]) == 20 for call in container_inspect_calls))

    def test_compose_client_never_uses_a_shell(self) -> None:
        calls: list[tuple[list[str], dict[str, Any]]] = []

        def runner(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
            calls.append((argv, kwargs))
            return subprocess.CompletedProcess(argv, 0, "[]", "")

        client = EVIDENCE.ComposeEngineClient(
            pathlib.Path("compose.yml"), pathlib.Path("."), runner=runner
        )
        self.assertEqual(client.run(0, "container", "inspect", "identifier"), "[]")
        self.assertIs(calls[0][1]["shell"], False)
        self.assertEqual(calls[0][0][-4:], ["docker", "container", "inspect", "identifier"])

    def test_aggregate_requires_exactly_ten_complete_worker_documents(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for worker in range(10):
                engines = [
                    EVIDENCE.validate_engine_observation(
                        worker_ordinal=worker,
                        engine_ordinal=engine,
                        candidate_sha=CANDIDATE,
                        fixture=fixture(),
                        image_inspect=image_inspect(),
                        container_inspects=container_inspects(worker, engine),
                    )
                    for engine in range(10)
                ]
                document = {
                    "schema_version": 1,
                    "candidate_sha": CANDIDATE,
                    "worker_ordinal": worker,
                    "fixture_image": IMAGE,
                    "engine_count": 10,
                    "container_count": 200,
                    "collected_at_epoch_seconds": int(time.time()),
                    "collection_started_at_epoch_seconds": int(time.time()) - 1,
                    "collection_finished_at_epoch_seconds": int(time.time()),
                    "collection_elapsed_seconds": 1,
                    "engines": engines,
                }
                (root / f"worker-{worker:02d}.json").write_text(
                    json.dumps(document), encoding="utf-8"
                )
            aggregate = EVIDENCE.aggregate_workers(root, CANDIDATE, fixture())
            self.assertEqual(aggregate["worker_count"], 10)
            self.assertEqual(aggregate["engine_count"], 100)
            self.assertEqual(aggregate["container_count"], 2_000)
            (root / "unexpected.json").write_text("{}", encoding="utf-8")
            with self.assertRaisesRegex(EVIDENCE.EngineEvidenceError, "exactly"):
                EVIDENCE.aggregate_workers(root, CANDIDATE, fixture())

    def test_aggregate_rejects_stale_skewed_or_invalid_worker_timestamps(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            now = 1_700_000_000
            for worker in range(10):
                engines = [
                    EVIDENCE.validate_engine_observation(
                        worker_ordinal=worker,
                        engine_ordinal=engine,
                        candidate_sha=CANDIDATE,
                        fixture=fixture(),
                        image_inspect=image_inspect(),
                        container_inspects=container_inspects(worker, engine),
                    )
                    for engine in range(10)
                ]
                document = {
                    "schema_version": 1,
                    "candidate_sha": CANDIDATE,
                    "worker_ordinal": worker,
                    "fixture_image": IMAGE,
                    "engine_count": 10,
                    "container_count": 200,
                    "collected_at_epoch_seconds": now - worker,
                    "collection_started_at_epoch_seconds": now - worker - 1,
                    "collection_finished_at_epoch_seconds": now - worker,
                    "collection_elapsed_seconds": 1,
                    "engines": engines,
                }
                (root / f"worker-{worker:02d}.json").write_text(
                    json.dumps(document), encoding="utf-8"
                )
            EVIDENCE.aggregate_workers(root, CANDIDATE, fixture(), now=lambda: now)

            document = json.loads((root / "worker-09.json").read_text(encoding="utf-8"))
            document["collected_at_epoch_seconds"] = now - 91
            document["collection_started_at_epoch_seconds"] = now - 92
            document["collection_finished_at_epoch_seconds"] = now - 91
            (root / "worker-09.json").write_text(json.dumps(document), encoding="utf-8")
            with self.assertRaisesRegex(EVIDENCE.EngineEvidenceError, "window"):
                EVIDENCE.aggregate_workers(root, CANDIDATE, fixture(), now=lambda: now)

            document["collected_at_epoch_seconds"] = 0
            document["collection_finished_at_epoch_seconds"] = 0
            (root / "worker-09.json").write_text(json.dumps(document), encoding="utf-8")
            with self.assertRaisesRegex(EVIDENCE.EngineEvidenceError, "timing"):
                EVIDENCE.aggregate_workers(root, CANDIDATE, fixture(), now=lambda: now)


if __name__ == "__main__":
    unittest.main()
