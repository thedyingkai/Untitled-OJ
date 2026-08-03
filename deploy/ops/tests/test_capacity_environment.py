import argparse
import http.server
import importlib.util
import json
import os
import pathlib
import sys
import tempfile
import threading
import time
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[3]
MODULE_PATH = ROOT / "deploy" / "capacity" / "orchestrator-capacity-environment.py"
SPEC = importlib.util.spec_from_file_location("capacity_environment", MODULE_PATH)
capacity = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = capacity
SPEC.loader.exec_module(capacity)


def nodes():
    return {
        "schema_version": 1,
        "nodes": [
            {
                "node_id": f"capacity-node-{worker:02}-{engine:02}",
                "host_ip": f"192.0.2.{100 + worker}",
                "worker": f"capacity-worker-{worker:02}",
                "engine_ordinal": engine,
                "labels": {
                    "capacity.profile": "production",
                    "runtime": "docker",
                    "os": "linux",
                    "arch": "x86_64",
                },
            }
            for worker in range(10)
            for engine in range(10)
        ],
    }


def fixture():
    digest = "a" * 64
    return {
        "schema_version": 1,
        "catalog_source_id": "capacity-fixture",
        "version": "1.0.0",
        "channel": "stable",
        "topology_id": "capacity-primary",
        "services": [
            {
                "service_id": f"capacity-{index:02}",
                "oci_image": f"registry.example/ojos/capacity@sha256:{digest}",
            }
            for index in range(20)
        ],
    }


def engine_evidence(candidate: str = "1" * 40, observed_at: int | None = None):
    release_fixture = fixture()
    image = release_fixture["services"][0]["oci_image"]
    assert isinstance(image, str)
    timestamp = int(time.time()) if observed_at is None else observed_at
    workers = []
    worker_files = []
    for worker in range(10):
        engines = []
        for engine in range(10):
            node_id = f"capacity-node-{worker:02d}-{engine:02d}"
            image_id = "sha256:" + f"{worker:02x}{engine:02x}".ljust(64, "c")
            containers = []
            for service in release_fixture["services"]:
                service_id = service["service_id"]
                service_index = int(service_id.rsplit("-", 1)[1])
                containers.append(
                    {
                        "container_id": f"container-{worker:02d}-{engine:02d}-{service_id}",
                        "deployment_id": capacity.deployment_id(
                            service_id, "1.0.0", node_id
                        ),
                        "service_id": service_id,
                        "node_id": node_id,
                        "image_id": image_id,
                        "artifact_digest": image,
                        "state": "RUNNING",
                        "health": "HEALTHY",
                        "published_port": {
                            "container_port": 8080,
                            "host_ip": "0.0.0.0",
                            "host_port": 20_000 + engine * 20 + service_index,
                            "protocol": "tcp",
                        },
                    }
                )
            engines.append(
                {
                    "engine_ordinal": engine,
                    "node_id": node_id,
                    "image": {
                        "reference": image,
                        "image_id": image_id,
                        "repo_digest": image,
                        "oci_revision": candidate,
                    },
                    "container_count": 20,
                    "containers": containers,
                }
            )
        workers.append(
            {
                "schema_version": 1,
                "candidate_sha": candidate,
                "worker_ordinal": worker,
                "fixture_image": image,
                "engine_count": 10,
                "container_count": 200,
                "collected_at_epoch_seconds": timestamp,
                "collection_started_at_epoch_seconds": timestamp - 1,
                "collection_finished_at_epoch_seconds": timestamp,
                "collection_elapsed_seconds": 1,
                "engines": engines,
            }
        )
        worker_files.append(
            {
                "worker_ordinal": worker,
                "file": f"worker-{worker:02d}.json",
                "sha256": f"{worker:02x}".ljust(64, "a"),
            }
        )
    return {
        "schema_version": 1,
        "candidate_sha": candidate,
        "fixture_image": image,
        "worker_count": 10,
        "engine_count": 100,
        "container_count": 2_000,
        "collected_at_epoch_seconds": timestamp,
        "collection_started_at_epoch_seconds": timestamp - 1,
        "collection_finished_at_epoch_seconds": timestamp,
        "worker_collection_spread_seconds": 1,
        "worker_files": worker_files,
        "workers": workers,
    }


class CapacityEnvironmentTests(unittest.TestCase):
    def test_network_observer_rejects_redirect_oversize_and_timeout(self):
        class Handler(http.server.BaseHTTPRequestHandler):
            redirected_requests = 0

            def do_GET(self):
                if self.path == "/health":
                    body = json.dumps(
                        {
                            "status": "healthy",
                            "candidate_sha": "1" * 40,
                            "service_id": "capacity-00",
                        }
                    ).encode()
                    self.send_response(200)
                elif self.path == "/redirect":
                    body = b""
                    self.send_response(302)
                    self.send_header("Location", "/redirect-target")
                elif self.path == "/redirect-target":
                    type(self).redirected_requests += 1
                    body = b'{}'
                    self.send_response(200)
                elif self.path == "/oversize":
                    body = b"x" * 4_097
                    self.send_response(200)
                elif self.path == "/slow":
                    time.sleep(0.2)
                    body = b'{}'
                    self.send_response(200)
                else:
                    body = b'{}'
                    self.send_response(404)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except OSError:
                    pass

            def log_message(self, _format, *_args):
                return

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        endpoint = f"127.0.0.1:{server.server_port}:capacity-00"
        try:
            self.assertEqual(
                capacity.endpoint_http_json(endpoint, "/health")["service_id"],
                "capacity-00",
            )
            with self.assertRaisesRegex(
                capacity.CapacityEnvironmentError, "returned HTTP 302"
            ):
                capacity.endpoint_http_json(endpoint, "/redirect")
            self.assertEqual(Handler.redirected_requests, 0)
            with self.assertRaisesRegex(
                capacity.CapacityEnvironmentError, "4096-byte response limit"
            ):
                capacity.endpoint_http_json(endpoint, "/oversize")
            with self.assertRaisesRegex(
                capacity.CapacityEnvironmentError, "probe failed"
            ):
                capacity.endpoint_http_json(endpoint, "/slow", 0.02)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    def test_failed_operation_uses_official_retry_with_a_new_generation_key(self):
        class OperationClient:
            def __init__(self):
                self.states = [
                    {"operation_id": "operation/one", "status": "FAILED", "generation": 0},
                    {"operation_id": "operation/one", "status": "SUCCEEDED", "generation": 1},
                ]
                self.retry_calls = []

            def request(
                self,
                method,
                path,
                payload=None,
                *,
                idempotency_key=None,
                expected=(200,),
            ):
                if method == "GET":
                    return capacity.ApiResult(
                        {"operation": self.states.pop(0)}, {}, 200
                    )
                self.retry_calls.append(
                    (method, path, payload, idempotency_key, expected)
                )
                return capacity.ApiResult(
                    {
                        "operation_id": "operation/one",
                        "operation": {
                            "operation_id": "operation/one",
                            "status": "ENQUEUING",
                            "generation": 1,
                        },
                    },
                    {},
                    202,
                )

        client = OperationClient()
        result = capacity.wait_operation(client, "operation/one", 30, "1" * 40)
        self.assertEqual(result["status"], "SUCCEEDED")
        self.assertEqual(len(client.retry_calls), 1)
        retry = client.retry_calls[0]
        self.assertEqual(retry[0], "POST")
        self.assertEqual(retry[1], "/api/v1/operations/operation%2Fone:retry")
        self.assertEqual(retry[2], {})
        self.assertEqual(retry[4], (202,))
        self.assertRegex(
            retry[3], r"^capacity-retry-1{12}-[0-9a-f]{16}-g1$"
        )

    def test_operation_retry_is_single_and_ambiguous_terminals_fail_closed(self):
        class OperationClient:
            def __init__(self, states):
                self.states = list(states)
                self.retry_count = 0

            def request(
                self,
                method,
                path,
                payload=None,
                *,
                idempotency_key=None,
                expected=(200,),
            ):
                if method == "GET":
                    return capacity.ApiResult(
                        {"operation": self.states.pop(0)}, {}, 200
                    )
                self.retry_count += 1
                generation = self.states[0]["generation"]
                return capacity.ApiResult(
                    {
                        "operation_id": "operation-1",
                        "operation": {
                            "operation_id": "operation-1",
                            "generation": generation,
                        },
                    },
                    {},
                    202,
                )

        for status in ("NEEDS_ATTENTION", "CANCELLED", "ROLLED_BACK"):
            client = OperationClient(
                [{"operation_id": "operation-1", "status": status, "generation": 0}]
            )
            with self.subTest(status=status), self.assertRaisesRegex(
                capacity.CapacityEnvironmentError, status
            ):
                capacity.wait_operation(client, "operation-1", 30, "1" * 40)
            self.assertEqual(client.retry_count, 0)

        twice = OperationClient(
            [
                {"operation_id": "operation-1", "status": "FAILED", "generation": 0},
                {"operation_id": "operation-1", "status": "FAILED", "generation": 1},
            ]
        )
        with self.assertRaisesRegex(capacity.CapacityEnvironmentError, "FAILED"):
            capacity.wait_operation(twice, "operation-1", 30, "1" * 40)
        self.assertEqual(twice.retry_count, 1)

        succeeded = OperationClient(
            [{"operation_id": "operation-1", "status": "SUCCEEDED", "generation": 0}]
        )
        capacity.wait_operation(succeeded, "operation-1", 30, "1" * 40)
        self.assertEqual(succeeded.retry_count, 0)

    def test_persisted_retry_generation_survives_seed_process_restarts(self):
        class PersistedFailedClient:
            def __init__(self, generation):
                self.generation = generation
                self.retry_count = 0

            def request(
                self,
                method,
                _path,
                _payload=None,
                *,
                idempotency_key=None,
                expected=(200,),
            ):
                if method == "GET":
                    return capacity.ApiResult(
                        {
                            "operation": {
                                "operation_id": "operation-1",
                                "status": "FAILED",
                                "generation": self.generation,
                            }
                        },
                        {},
                        200,
                    )
                self.retry_count += 1
                raise AssertionError("persisted generation must prevent another retry")

        # Model independent seed processes started by Ansible's task-level
        # retries. Each process sees the same durable post-retry generation and
        # must fail closed without issuing another mutation.
        for generation in (1, 2, 30):
            for process_attempt in range(3):
                client = PersistedFailedClient(generation)
                with self.subTest(
                    generation=generation, process_attempt=process_attempt
                ), self.assertRaisesRegex(
                    capacity.CapacityEnvironmentError,
                    rf"persisted generation {generation}.*generation 0->1",
                ):
                    capacity.wait_operation(client, "operation-1", 30, "1" * 40)
                self.assertEqual(client.retry_count, 0)

    def test_ansible_enrollment_recovery_is_node_bound_and_precedes_redemption(self):
        enrollment = (
            ROOT / "deploy" / "capacity" / "tasks" / "enroll-capacity-agent.yml"
        ).read_text(encoding="utf-8")
        agent = (
            ROOT / "services" / "orchestrator" / "agent" / "src" / "main.rs"
        ).read_text(encoding="utf-8")

        self.assertIn("- --expected-node-id", enrollment)
        self.assertIn(
            'capacity-node-{{ \'%02d\' | format(capacity_worker_ordinal | int) }}-'
            "{{ '%02d' | format(capacity_engine_ordinal) }}",
            enrollment,
        )
        session_lock = agent.index("begin_enrollment_session")
        read_code = agent.index("let enrollment_code = read_enrollment_code")
        durable_request = agent.index("prepare_enrollment_attempt", read_code)
        recovery = agent.index("recover_enrollment_identity", durable_request)
        redeem = agent.index("client.redeem", recovery)
        freshness = agent.index("validate_enrollment_bundle_fresh", redeem)
        install = agent.index("identity_store.install", freshness)
        self.assertLess(session_lock, read_code)
        self.assertLess(read_code, durable_request)
        self.assertLess(durable_request, recovery)
        self.assertLess(recovery, redeem)
        self.assertLess(redeem, freshness)
        self.assertLess(freshness, install)
        self.assertIn("enrollment_attempt.as_ref()", agent[recovery:redeem])
        self.assertIn("validate_recovery_binding", agent[recovery:redeem])
        self.assertIn("HttpMtlsTransport::from_pem_files", agent[recovery:redeem])

        identity = (
            ROOT / "services" / "orchestrator" / "agent" / "src" / "identity.rs"
        ).read_text(encoding="utf-8")
        install = identity.index("fn install_with_post_publish_hook")
        generation_rename = identity.index(
            "fs::rename(&pending, &final_directory)?", install
        )
        generation_sync = identity.index(
            "sync_directory(&self.generations_dir())?", generation_rename
        )
        crash_hook = identity.index("after_generation_publish()?", generation_sync)
        self.assertLess(generation_rename, generation_sync)
        self.assertLess(generation_sync, crash_hook)
        current_rename = identity.index("fs::rename(&temporary, &current)")
        current_sync = identity.index("sync_directory(&self.root)", current_rename)
        self.assertLess(current_rename, current_sync)

    def test_engine_evidence_proves_all_real_candidate_workloads(self):
        candidate = "1" * 40
        summary = capacity.verify_engine_evidence(
            engine_evidence(candidate),
            candidate,
            capacity.validate_nodes(nodes()),
            capacity.validate_fixture(fixture()),
        )
        self.assertEqual(summary["engines"], 100)
        self.assertEqual(summary["running"], 2_000)
        self.assertEqual(summary["healthy"], 2_000)

    def test_engine_evidence_rejects_stale_unhealthy_or_incomplete_observation(self):
        candidate = "1" * 40
        node_plan = capacity.validate_nodes(nodes())
        release_fixture = capacity.validate_fixture(fixture())
        stale = engine_evidence(candidate, observed_at=1_700_000_000)
        with self.assertRaisesRegex(capacity.CapacityEnvironmentError, "stale"):
            capacity.verify_engine_evidence(
                stale,
                candidate,
                node_plan,
                release_fixture,
                now=1_700_000_301,
            )

        unhealthy = engine_evidence(candidate)
        unhealthy["workers"][0]["engines"][0]["containers"][0]["health"] = (
            "UNHEALTHY"
        )
        with self.assertRaisesRegex(capacity.CapacityEnvironmentError, "health"):
            capacity.verify_engine_evidence(
                unhealthy, candidate, node_plan, release_fixture
            )

        incomplete = engine_evidence(candidate)
        incomplete["workers"][0]["engines"][0]["containers"].pop()
        with self.assertRaisesRegex(capacity.CapacityEnvironmentError, "identity"):
            capacity.verify_engine_evidence(
                incomplete, candidate, node_plan, release_fixture
            )

    def test_topology_is_deterministic_and_has_exact_production_cardinality(self):
        node_plan = capacity.validate_nodes(nodes())
        release_fixture = capacity.validate_fixture(fixture())
        first = capacity.build_topology_spec(node_plan, release_fixture)
        second = capacity.build_topology_spec(node_plan, release_fixture)
        self.assertEqual(first, second)
        self.assertEqual(len(first["endpoints"]), 2_000)
        self.assertEqual(len(first["links"]), 8_000)
        self.assertEqual(
            len({item["endpoint"] for item in first["endpoints"]}), 2_000
        )
        self.assertEqual(
            len(
                {
                    (item["source_endpoint"], item["target_endpoint"])
                    for item in first["links"]
                }
            ),
            8_000,
        )
        encoded = json.dumps(first, sort_keys=True, separators=(",", ":")).encode()
        self.assertLess(len(encoded), 8 * 1024 * 1024)

    def test_node_plan_requires_ten_unique_hosts_with_ten_engines_each(self):
        invalid = nodes()
        invalid["nodes"][10]["host_ip"] = invalid["nodes"][0]["host_ip"]
        with self.assertRaisesRegex(
            capacity.CapacityEnvironmentError, "one unique host IP"
        ):
            capacity.validate_nodes(invalid)

        invalid = nodes()
        invalid["nodes"][0]["host_ip"] = "2001:db8::1"
        with self.assertRaisesRegex(capacity.CapacityEnvironmentError, "IPv4"):
            capacity.validate_nodes(invalid)

    def test_node_plan_requires_the_store_docker_and_platform_labels(self):
        for label in ("runtime", "os", "arch"):
            invalid = nodes()
            del invalid["nodes"][0]["labels"][label]
            with self.assertRaisesRegex(
                capacity.CapacityEnvironmentError,
                "runtime=docker, os=linux, arch=x86_64",
            ):
                capacity.validate_nodes(invalid)

        invalid = nodes()
        invalid["nodes"][0]["labels"]["arch"] = "amd64"
        with self.assertRaisesRegex(
            capacity.CapacityEnvironmentError,
            "runtime=docker, os=linux, arch=x86_64",
        ):
            capacity.validate_nodes(invalid)

    def test_enrollment_persists_store_compatible_labels_and_rejects_stale_ready_node(
        self,
    ):
        class EnrollmentClient:
            def __init__(self, existing=None):
                self.existing = existing or []
                self.requests = []

            def page(self, path):
                if path != "/api/v1/nodes":
                    raise AssertionError(f"unexpected page request: {path}")
                return self.existing

            def request(
                self,
                method,
                path,
                payload,
                *,
                idempotency_key,
                expected,
            ):
                self.requests.append(
                    (method, path, payload, idempotency_key, expected)
                )
                return capacity.ApiResult(
                    data={"enrollment_code": "one-time-code"},
                    headers={},
                    status=201,
                )

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            node_plan = {"schema_version": 1, "nodes": [nodes()["nodes"][0]]}
            nodes_file = root / "nodes.json"
            nodes_file.write_text(json.dumps(node_plan), encoding="utf-8")
            args = argparse.Namespace(
                nodes_file=nodes_file,
                expected_nodes=1,
                output_dir=root / "enrollment",
                candidate_sha="1" * 40,
                enrollment_generation="first-attempt",
            )
            client = EnrollmentClient()

            summary = capacity.issue_enrollment(args, client)

            self.assertEqual(summary["issued"], 1)
            payload = client.requests[0][2]
            self.assertEqual(
                {key: payload["labels"][key] for key in capacity.STORE_NODE_LABELS},
                capacity.STORE_NODE_LABELS,
            )
            self.assertEqual(
                (root / "enrollment" / "capacity-node-00-00.code").read_text(),
                "one-time-code",
            )

            stale = EnrollmentClient(
                [
                    {
                        "node_id": "capacity-node-00-00",
                        "status": "READY",
                        "labels": {"capacity.profile": "production"},
                    }
                ]
            )
            with self.assertRaisesRegex(
                capacity.CapacityEnvironmentError,
                "lacks the canonical Docker/Linux/x86_64 Store labels",
            ):
                capacity.issue_enrollment(args, stale)
            self.assertEqual(stale.requests, [])

    def test_fixture_rejects_mutable_or_noncanonical_images(self):
        for image in (
            "registry.example/ojos/capacity:latest",
            "registry.example/ojos/capacity@sha256:" + "A" * 64,
        ):
            invalid = fixture()
            invalid["services"][0]["oci_image"] = image
            with self.assertRaises(capacity.CapacityEnvironmentError):
                capacity.validate_fixture(invalid)

    def test_token_helper_is_shell_free_strict_and_refreshes_before_ten_minutes(self):
        helper = json.dumps(
            [
                sys.executable,
                "-c",
                (
                    "import json,time; "
                    "print(json.dumps({'access_token':'short-lived','expires_at':time.time()+601}))"
                ),
            ]
        )
        provider = capacity.TokenProvider(helper)
        self.assertEqual(provider.access_token(), "short-lived")
        self.assertGreater(provider._expires_at, time.time() + 600)

        extra_key = json.dumps(
            [
                sys.executable,
                "-c",
                (
                    "import json,time; print(json.dumps("
                    "{'access_token':'x','expires_at':time.time()+900,'extra':1}))"
                ),
            ]
        )
        with self.assertRaisesRegex(
            capacity.CapacityEnvironmentError, "exactly access_token"
        ):
            capacity.TokenProvider(extra_key).access_token()

    def test_atomic_secret_write_is_recoverable_and_not_world_readable(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "node.code"
            capacity.atomic_write(path, b"one-time", 0o600)
            self.assertEqual(path.read_bytes(), b"one-time")
            if os.name == "posix":
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_deployment_identity_matches_the_rust_contract_shape(self):
        identifier = capacity.deployment_id("capacity-00", "1.0.0", "capacity-node-00-00")
        self.assertTrue(identifier.startswith("deployment-capacity-00-"))
        self.assertEqual(len(identifier), 56)

    def test_readiness_uses_the_published_status_and_exact_build_identity(self):
        commit = "1" * 40
        readiness = {
            "status": "ready",
            "build": {
                "version": "1.0.0",
                "commit_sha": commit,
                "profile": "production",
                "target": "x86_64-unknown-linux-gnu",
            },
        }
        self.assertTrue(
            capacity.production_readiness_matches_candidate(readiness, commit)
        )
        for field, invalid in (
            ("status", "not_ready"),
            ("version", "0.2.0"),
            ("commit_sha", "2" * 40),
            ("profile", "desktop"),
            ("target", "x86_64-pc-windows-msvc"),
        ):
            changed = json.loads(json.dumps(readiness))
            if field == "status":
                changed[field] = invalid
            else:
                changed["build"][field] = invalid
            self.assertFalse(
                capacity.production_readiness_matches_candidate(changed, commit),
                field,
            )

    def test_environment_summary_uses_readiness_build_and_real_store_operation_request(self):
        commit = "1" * 40
        node_plan = capacity.validate_nodes(nodes())
        release_fixture = capacity.validate_fixture(fixture())
        client = ProductionEnvironmentClient(commit, node_plan, release_fixture)

        summary = capacity.verify_environment(
            client,
            commit,
            node_plan,
            release_fixture,
            network_verifier=lambda _spec, _sha: {
                "endpoint_checks_total": 2_000,
                "endpoint_checks_healthy": 2_000,
                "endpoint_checks_failed": 0,
                "link_probes_total": 8_000,
                "link_probes_healthy": 8_000,
                "link_probes_failed": 0,
                "drift": 0,
                "failure_samples": [],
            },
        )

        self.assertEqual(summary["build"], client.build)
        self.assertEqual(summary["nodes_ready"], 100)
        self.assertEqual(summary["deployments_running"], 2_000)
        self.assertEqual(summary["operation_target_nodes"], 50)
        self.assertEqual(summary["network_evidence"]["link_probes_healthy"], 8_000)

    def test_deployment_id_in_operation_target_id_does_not_fake_node_coverage(self):
        commit = "1" * 40
        node_plan = capacity.validate_nodes(nodes())
        release_fixture = capacity.validate_fixture(fixture())
        client = ProductionEnvironmentClient(commit, node_plan, release_fixture)
        client.operations = [
            {
                "action": "release.install",
                "status": "SUCCEEDED",
                "target_id": operation["request"]["deployment_id"],
                "request": {},
            }
            for operation in client.operations
        ]

        with self.assertRaisesRegex(
            capacity.CapacityEnvironmentError, "fewer than 50 distinct Nodes"
        ):
            capacity.verify_environment(client, commit, node_plan, release_fixture)


class ProductionEnvironmentClient:
    def __init__(
        self,
        commit: str,
        node_plan: list[dict[str, object]],
        release_fixture: dict[str, object],
    ) -> None:
        self.nodes = node_plan
        self.fixture = release_fixture
        self.build = {
            "version": "1.0.0",
            "commit_sha": commit,
            "profile": "production",
            "target": "x86_64-unknown-linux-gnu",
        }
        self.deployments = []
        services = self.fixture["services"]
        assert isinstance(services, list)
        services.sort(key=lambda service: service["service_id"])
        for node in self.nodes:
            for service_index, service in enumerate(services):
                assert isinstance(service, dict)
                deployment_id = capacity.deployment_id(
                    service["service_id"], self.fixture["version"], node["node_id"]
                )
                self.deployments.append(
                    {
                        "node_id": node["node_id"],
                        "management_mode": "MANAGED",
                        "endpoint": capacity.capacity_endpoint(
                            node, service["service_id"], service_index
                        ),
                        "instance": {
                            "deployment_id": deployment_id,
                            "service_id": service["service_id"],
                            "release_version": self.fixture["version"],
                            "container_id": f"container-{deployment_id}",
                            "artifact_digest": service["oci_image"],
                            "desired_state": "RUNNING",
                            "observed_state": "RUNNING",
                            "health": "HEALTHY",
                        },
                    }
                )
        first_service = services[0]
        assert isinstance(first_service, dict)
        self.operations = []
        for node in self.nodes[:50]:
            deployment_id = capacity.deployment_id(
                first_service["service_id"],
                self.fixture["version"],
                node["node_id"],
            )
            self.operations.append(
                {
                    "action": "release.install",
                    "status": "SUCCEEDED",
                    "target_id": f"{first_service['service_id']}@{self.fixture['version']}",
                    "request": {
                        "deployment_id": deployment_id,
                        "target_node_id": node["node_id"],
                    },
                }
            )
        topology = capacity.build_topology_spec(self.nodes, self.fixture)
        self.topology_status = {
            "state": "IN_SYNC",
            "desired_revision_id": "revision-capacity",
            "observed_revision_id": "revision-capacity",
            "drift": [],
            "endpoints": [
                {
                    "endpoint": endpoint["endpoint"],
                    "health": "HEALTHY",
                    "reachable": True,
                }
                for endpoint in topology["endpoints"]
            ],
            "links": [
                {
                    "source_endpoint": link["source_endpoint"],
                    "target_endpoint": link["target_endpoint"],
                    "health": "HEALTHY",
                }
                for link in topology["links"]
            ],
        }

    def page(self, path: str) -> list[dict[str, object]]:
        if path == "/api/v1/nodes":
            return [
                {
                    "node_id": node["node_id"],
                    "status": "READY",
                    "labels": node["labels"],
                }
                for node in self.nodes
            ]
        if path == "/api/v1/deployments":
            return self.deployments
        if path == "/api/v1/operations":
            return self.operations
        raise AssertionError(f"unexpected page request: {path}")

    def request(self, method: str, path: str) -> capacity.ApiResult:
        if method == "GET" and path == "/api/v1/healthz/ready":
            return capacity.ApiResult(
                data={"status": "ready", "build": self.build},
                headers={},
                status=200,
            )
        if method == "GET" and path.startswith("/api/v1/nodes/") and path.endswith(
            "/health"
        ):
            node_id = path.removeprefix("/api/v1/nodes/").removesuffix("/health")
            return capacity.ApiResult(
                data={"node_id": node_id, "ready": True}, headers={}, status=200
            )
        if method == "GET" and path == "/api/v1/topologies/capacity-primary/status":
            return capacity.ApiResult(
                data={"status": self.topology_status}, headers={}, status=200
            )
        raise AssertionError(f"unexpected API request: {method} {path}")


if __name__ == "__main__":
    unittest.main()
