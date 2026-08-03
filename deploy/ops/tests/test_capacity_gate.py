from __future__ import annotations

import importlib.util
import hashlib
import io
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
from unittest import mock
from typing import Any


MODULE_PATH = pathlib.Path(__file__).parents[1] / "orchestrator-capacity-gate.py"
SPEC = importlib.util.spec_from_file_location("orchestrator_capacity_gate", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
CAPACITY = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CAPACITY
SPEC.loader.exec_module(CAPACITY)


def write_fake_process(
    proc_root: pathlib.Path,
    *,
    pid: int,
    parent_pid: int,
    executable: str,
    start_ticks: int = 100_000,
    control_group: str | None = None,
) -> pathlib.Path:
    process = proc_root / str(pid)
    process.mkdir(parents=True, exist_ok=True)
    (process / "cmdline").write_bytes(executable.encode("utf-8") + b"\0run\0")
    if control_group is not None:
        (process / "cgroup").write_text(
            f"0::{control_group}\n", encoding="utf-8"
        )
    stat_fields = ["S", str(parent_pid), *("0" for _ in range(17)), str(start_ticks)]
    (process / "stat").write_text(
        f"{pid} ({pathlib.Path(executable).name}) {' '.join(stat_fields)}\n",
        encoding="utf-8",
    )
    return process


def write_fake_listener(
    proc_root: pathlib.Path,
    control_group: str,
    *,
    pid: int = 9_876,
    start_ticks: int = 100_000,
) -> pathlib.Path:
    return write_fake_process(
        proc_root,
        pid=pid,
        parent_pid=0,
        executable="/opt/actions-runner/bin/Runner.Listener",
        start_ticks=start_ticks,
        control_group=control_group,
    )


def environment_observation(
    candidate: str = "a" * 40,
    observed_at: float = 1_700_000_000.0,
) -> dict[str, Any]:
    fixture_image = "registry.example/capacity@sha256:" + "2" * 64
    control_plane_image = "registry.example/control-plane@sha256:" + "a" * 64
    agent_image = "registry.example/agent@sha256:" + "b" * 64
    postgres_image = "registry.example/postgres@sha256:" + "3" * 64
    engine_image = "registry.example/docker@sha256:" + "4" * 64
    hosts = [
        {
            "role": role,
            "machine_id_sha256": f"{index + 1:064x}",
            "boot_id": f"{index + 1:08x}-2222-3333-4444-555555555555",
        }
        for index, role in enumerate(
            [
                "control-plane",
                "postgres",
                "runner",
                *(f"worker-{ordinal:02d}" for ordinal in range(10)),
            ]
        )
    ]
    return {
        "schema_version": 1,
        "candidate_sha": candidate,
        "started_at_epoch_seconds": observed_at - 10,
        "completed_at_epoch_seconds": observed_at,
        "configuration_fingerprint_sha256": "1" * 64,
        "observer_identity": {
            "program_sha256": "1" * 64,
            "config_sha256": "2" * 64,
            "applied_manifest_sha256": "3" * 64,
            "helper_manifest_sha256": "4" * 64,
            "helper_files_sha256": "5" * 64,
            "ansible_playbook_sha256": "6" * 64,
        },
        "provenance_identity": {
            "record_sha256": "7" * 64,
            "repository": "owner/repo",
            "source_workflow": ".github/workflows/orchestrator-candidate-images.yml",
            "source_workflow_run_id": "123",
            "source_workflow_run_attempt": 1,
            "github_oidc_issuer": "https://token.actions.githubusercontent.com",
            "control_plane_reference": control_plane_image,
            "control_plane_digest": "sha256:" + "a" * 64,
            "agent_reference": agent_image,
            "agent_digest": "sha256:" + "b" * 64,
            "fixture_reference": fixture_image,
            "fixture_digest": "sha256:" + "2" * 64,
        },
        "deployment_identity": {
            "control_plane_origin_sha256": "8" * 64,
            "restart_argv_sha256": "9" * 64,
            "topology_id": "topology-capacity",
            "topology_revision_id": "revision-capacity",
            "topology_identity_sha256": "a" * 64,
        },
        "engine_evidence": {
            "fixture_image": fixture_image,
            "worker_count": 10,
            "engine_count": 100,
            "container_count": 2_000,
            "running_containers": 2_000,
            "healthy_containers": 2_000,
            "oldest_worker_observed_at_epoch_seconds": observed_at - 9,
            "newest_worker_observed_at_epoch_seconds": observed_at - 1,
            "worker_collection_spread_seconds": 8,
            "aggregate_sha256": "3" * 64,
            "node_ids_sha256": "4" * 64,
            "deployment_ids_sha256": "5" * 64,
            "container_ids_sha256": "6" * 64,
        },
        "network_evidence": {
            "checked_at_epoch_seconds": observed_at - 1,
            "endpoint_checks_total": 2_000,
            "endpoint_checks_healthy": 2_000,
            "endpoint_checks_failed": 0,
            "link_probes_total": 8_000,
            "link_probes_healthy": 8_000,
            "link_probes_failed": 0,
            "drift": 0,
            "endpoint_ids_sha256": "7" * 64,
            "link_ids_sha256": "8" * 64,
        },
        "runtime_evidence": {
            "schema_version": 2,
            "candidate_sha": candidate,
            "provision_manifest_sha256": "9" * 64,
            "host_count": 13,
            "host_identity_sha256": "b" * 64,
            "hosts": hosts,
            "control_plane": {
                "schema_version": 2,
                "candidate_sha": candidate,
                "provision_manifest_sha256": "9" * 64,
                "host": hosts[0],
                "image": {
                    "reference": control_plane_image,
                    "repo_digest": control_plane_image,
                    "image_id": "sha256:" + "c" * 64,
                    "oci_revision": candidate,
                },
                "container": {
                    "container_id": "d" * 64,
                    "container_name": "orchestrator",
                    "started_at": "2026-08-03T00:00:00Z",
                    "state": "RUNNING",
                },
                "configuration": {
                    "effective_sha256": "2" * 64,
                    "provisioned_sha256": "2" * 64,
                    "non_sensitive": {},
                },
                "database_tls_identity": {
                    "verified_hostname": "postgres.capacity.internal",
                    "port": 5432,
                    "peer_leaf_sha256": "3" * 64,
                    "root_certificates_sha256": ["4" * 64],
                    "tls_version": "TLSv1.3",
                },
            },
            "postgres": {
                "schema_version": 2,
                "candidate_sha": candidate,
                "provision_manifest_sha256": "9" * 64,
                "host": hosts[1],
                "image": {
                    "reference": postgres_image,
                    "repo_digest": postgres_image,
                    "image_id": "sha256:" + "5" * 64,
                    "oci_revision": None,
                },
                "container": {
                    "container_id": "6" * 64,
                    "container_name": "postgres",
                    "started_at": "2026-08-02T00:00:00Z",
                    "state": "RUNNING",
                    "health": "HEALTHY",
                },
                "configuration": {
                    "effective_sha256": "7" * 64,
                    "provisioned_sha256": "7" * 64,
                    "non_sensitive": {},
                },
                "server_leaf_sha256": "3" * 64,
                "root_certificates_sha256": ["4" * 64],
                "settings": {},
            },
            "restart_identity": {
                "container_id": "d" * 64,
                "container_name": "orchestrator",
                "started_at": "2026-08-03T00:00:00Z",
                "image_id": "sha256:" + "c" * 64,
                "repo_digest": control_plane_image,
            },
            "agents": {
                "count": 100,
                "running": 100,
                "control_plane_origin": "https://capacity.example.test:8090",
                "image": {
                    "reference": agent_image,
                    "repo_digest": agent_image,
                    "image_ids": ["sha256:" + "e" * 64],
                    "oci_revision": candidate,
                },
                "node_ids_sha256": "f" * 64,
                "container_ids_sha256": "0" * 64,
                "started_at_sha256": "1" * 64,
                "spiffe_ids_sha256": "2" * 64,
                "certificate_fingerprints_sha256": "3" * 64,
                "ledger_identities_sha256": "4" * 64,
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
                    "reference": engine_image,
                    "repo_digest": engine_image,
                    "image_ids": ["sha256:" + "8" * 64],
                },
                "outer_container_ids_sha256": "a" * 64,
                "inner_daemon_ids_sha256": "b" * 64,
                "socket_volumes_sha256": "c" * 64,
                "data_volumes_sha256": "d" * 64,
            },
        },
    }


def operation_event_cursor(revision: int) -> str:
    return json.dumps(
        {"operation_revision": revision, "job_sequences": {}},
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8").hex()


def operation_sse(operation_id: str, revision: int) -> bytes:
    cursor = operation_event_cursor(revision)
    data = json.dumps(
        {
            "request_id": f"request-{revision}",
            "operation": {
                "operation_id": operation_id,
                "revision": revision,
                "status": "RUNNING",
            },
        },
        sort_keys=True,
        separators=(",", ":"),
    )
    return f"id: {cursor}\nevent: operation\ndata: {data}\n\nretry: 1000\n\n".encode()


class OperationClient:
    def __init__(
        self,
        cancel_status: int,
        observed_status: str = "",
        event_mode: str = "new",
    ) -> None:
        self.cancel_status = cancel_status
        self.observed_status = observed_status
        self.event_mode = event_mode
        self.calls: list[tuple[str, str, Any, str]] = []
        self.event_requests: list[dict[str, Any]] = []
        self.plan: dict[str, Any] | None = None

    def json(
        self, method: str, path: str, body: Any = None, idem: str = ""
    ) -> tuple[int, Any, float, dict[str, str]]:
        self.calls.append((method, path, body, idem))
        if method == "POST" and path == "/api/v1/operations:plan":
            self.plan = body
            return 201, {"data": {"operation": {"operation_id": "operation-1"}}}, 1.0, {}
        if path.endswith(":confirm"):
            return 200, {"data": {"operation_id": "operation-1"}}, 1.0, {}
        if path.endswith(":apply"):
            return 202, {"data": {"operation_id": "operation-1"}}, 2.0, {}
        if path.endswith(":cancel"):
            return (
                self.cancel_status,
                {"detail": "Operation is already terminal"},
                1.5,
                {},
            )
        if method == "GET" and path == "/api/v1/operations/operation-1":
            return (
                200,
                {"data": {"operation": {"status": self.observed_status}}},
                1.0,
                {},
            )
        raise AssertionError(f"unexpected request: {method} {path}")

    def call(
        self,
        method: str,
        path: str,
        body: Any = None,
        idem: str = "",
        **kwargs: Any,
    ) -> tuple[int, bytes, float, dict[str, str]]:
        self.calls.append((method, path, body, idem))
        if method == "GET" and path == "/api/v1/operations/operation-1/events":
            self.event_requests.append(kwargs)
            last_event_id = kwargs.get("request_headers", {}).get("Last-Event-ID")
            headers = {"content-type": "text/event-stream; charset=utf-8"}
            if not last_event_id:
                return 200, operation_sse("operation-1", 2), 1.0, headers
            if self.event_mode == "old":
                return 200, operation_sse("operation-1", 2), 1.0, headers
            if self.event_mode == "reconnect" and len(self.event_requests) == 2:
                return 200, b": keep-alive\nretry: 1000\n\n", 1.0, headers
            return 200, operation_sse("operation-1", 3), 1.0, headers
        raise AssertionError(f"unexpected request: {method} {path}")


class TopologyClient:
    def json(
        self, method: str, path: str, body: Any = None, idem: str = ""
    ) -> tuple[int, Any, float, dict[str, str]]:
        if method == "GET" and path == "/api/v1/topologies/topology-1":
            return (
                200,
                {
                    "data": {
                        "heads": {
                            "topology_id": "topology-1",
                            "applied_revision_id": "revision-applied",
                        },
                        "draft": {"spec": {"endpoints": [], "links": []}},
                    }
                },
                2.0,
                {},
            )
        if (
            method == "GET"
            and path == "/api/v1/topologies/topology-1/revisions/revision-applied"
        ):
            return (
                200,
                {
                    "data": {
                        "revision": {
                            "spec": {
                                "endpoints": [{}, {}],
                                "links": [{}, {}, {}],
                            }
                        }
                    }
                },
                3.0,
                {},
            )
        raise AssertionError(f"unexpected request: {method} {path}")


class CapacityGateTests(unittest.TestCase):
    def test_production_arguments_lock_identity_authentication_and_gate_strength(self) -> None:
        commit = "a" * 40
        environment = {
            "GITHUB_REPOSITORY": "owner/repo",
            "GITHUB_WORKFLOW": "capacity",
            "GITHUB_RUN_ID": "1",
            "GITHUB_RUN_ATTEMPT": "1",
            "GITHUB_JOB": "production-soak",
            "GITHUB_REF": "refs/heads/main",
            "GITHUB_SHA": commit,
            "RUNNER_NAME": "soak-1",
            "RUNNER_OS": "Linux",
            "RUNNER_ARCH": "X64",
            "RUNNER_ENVIRONMENT": "self-hosted",
            "ORCHESTRATOR_GATE_RUNNER_LABELS": "self-hosted,linux,x64,orchestrator-soak",
            "ORCHESTRATOR_GATE_GITHUB_TOKEN": "actions-api-token",
            "ORCHESTRATOR_GATE_BASE_URL": "https://orchestrator.example.test",
            "ORCHESTRATOR_GATE_TOKEN_ARGV_JSON": '["token-helper"]',
            "ORCHESTRATOR_GATE_RESTART_ARGV_JSON": '["restart-helper"]',
            "ORCHESTRATOR_GATE_ENVIRONMENT_ARGV_JSON": json.dumps(
                CAPACITY.REPOSITORY_ENVIRONMENT_OBSERVER_ARGV
            ),
            "ORCHESTRATOR_GATE_OCI_REVISION": commit,
            "ORCHESTRATOR_GATE_PROVENANCE_COMMIT": commit,
            "ORCHESTRATOR_GATE_CONTROL_PLANE_IMAGE": (
                "registry.example/control-plane@sha256:" + "1" * 64
            ),
            "ORCHESTRATOR_GATE_AGENT_IMAGE": (
                "registry.example/agent@sha256:" + "2" * 64
            ),
            "ORCHESTRATOR_GATE_FIXTURE_IMAGE": (
                "registry.example/fixture@sha256:" + "3" * 64
            ),
            "ORCHESTRATOR_GATE_IMAGE_WORKFLOW_RUN_ID": "123",
            "ORCHESTRATOR_GATE_IMAGE_PROVENANCE_RECORD_SHA256": "4" * 64,
            "ORCHESTRATOR_GATE_EXPECTED_RUNNER_UNIT": (
                "actions.runner.owner-repo.soak-1.service"
            ),
        }
        argv = [
            "orchestrator-capacity-gate.py",
            "--profile",
            "production",
            "--soak-seconds",
            "86400",
        ]
        with mock.patch.dict(os.environ, environment, clear=True), mock.patch.object(
            sys, "argv", argv
        ):
            args = CAPACITY.arguments()
        self.assertEqual(args.warmup_seconds, 600)
        self.assertEqual(args.sample_seconds, 30)
        self.assertEqual(args.operation_interval_seconds, 300)
        self.assertEqual(args.minimum_valid_samples, 2_736)

        weakened = argv + ["--nodes", "99"]
        with mock.patch.dict(os.environ, environment, clear=True), mock.patch.object(
            sys, "argv", weakened
        ), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            CAPACITY.arguments()

        static_environment = {**environment, "ORCHESTRATOR_GATE_OIDC_TOKEN": "static"}
        with mock.patch.dict(os.environ, static_environment, clear=True), mock.patch.object(
            sys, "argv", argv
        ), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            CAPACITY.arguments()

        rerun_environment = {**environment, "GITHUB_RUN_ATTEMPT": "2"}
        with mock.patch.dict(os.environ, rerun_environment, clear=True), mock.patch.object(
            sys, "argv", argv
        ), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            CAPACITY.arguments()

    def test_token_helper_is_no_shell_cached_and_refreshed_ten_minutes_early(self) -> None:
        now = [1_700_000_000.0]
        calls: list[tuple[list[str], dict[str, Any]]] = []

        def runner(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
            calls.append((argv, kwargs))
            token_number = len(calls)
            return subprocess.CompletedProcess(
                argv,
                0,
                json.dumps(
                    {
                        "access_token": f"token-{token_number}",
                        "expires_at": now[0] + 3_600,
                    }
                ),
                "",
            )

        provider = CAPACITY.TokenProvider(
            '["/opt/ojos/get-token", "--audience", "orchestrator"]',
            now=lambda: now[0],
            runner=runner,
        )

        self.assertEqual(provider.token(), "token-1")
        self.assertEqual(provider.token(), "token-1")
        now[0] += 3_001
        self.assertEqual(provider.token(), "token-2")
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0][0][0], "/opt/ojos/get-token")
        self.assertIs(calls[0][1]["shell"], False)
        self.assertEqual(provider.refresh_count, 2)

    def test_token_helper_rejects_extra_fields_and_short_lived_tokens(self) -> None:
        def result(payload: dict[str, Any]) -> Any:
            def runner(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
                return subprocess.CompletedProcess(argv, 0, json.dumps(payload), "")

            return runner

        with self.assertRaisesRegex(RuntimeError, "exactly"):
            CAPACITY.TokenProvider(
                '["token-helper"]',
                now=lambda: 1_000,
                runner=result(
                    {"access_token": "token", "expires_at": 2_000, "scope": "extra"}
                ),
            ).token()
        with self.assertRaisesRegex(RuntimeError, "10-minute"):
            CAPACITY.TokenProvider(
                '["token-helper"]',
                now=lambda: 1_000,
                runner=result({"access_token": "token", "expires_at": 1_600}),
            ).token()
        with self.assertRaisesRegex(RuntimeError, "longer than 2 hours"):
            CAPACITY.TokenProvider(
                '["token-helper"]',
                max_lifetime_seconds=7_200,
                now=lambda: 1_000,
                runner=result({"access_token": "token", "expires_at": 8_201}),
            ).token()

    def test_environment_helper_is_no_shell_strict_and_candidate_bound(self) -> None:
        now = 1_700_000_000.0
        calls: list[tuple[list[str], dict[str, Any]]] = []

        def runner(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
            calls.append((argv, kwargs))
            return subprocess.CompletedProcess(
                argv, 0, json.dumps(environment_observation(observed_at=now)), ""
            )

        provider = CAPACITY.EnvironmentEvidenceProvider(
            '["/opt/actions-runner/protected/environment-evidence"]',
            "a" * 40,
            runner=runner,
            now=lambda: now,
        )
        observed = provider.observe()
        self.assertEqual(observed["engine_evidence"]["container_count"], 2_000)
        self.assertEqual(observed["network_evidence"]["link_probes_healthy"], 8_000)
        self.assertIs(calls[0][1]["shell"], False)
        self.assertEqual(calls[0][1]["timeout"], 85)

        extra = environment_observation(observed_at=now)
        extra["access_token"] = "must-not-be-reportable"
        with self.assertRaisesRegex(RuntimeError, "unexpected top-level"):
            CAPACITY.validate_environment_observation(
                extra,
                "a" * 40,
                local_started_at=now,
                local_completed_at=now,
            )

        unhealthy = environment_observation(observed_at=now)
        unhealthy["engine_evidence"]["healthy_containers"] = 1_999
        with self.assertRaisesRegex(RuntimeError, "healthy_containers"):
            CAPACITY.validate_environment_observation(
                unhealthy,
                "a" * 40,
                local_started_at=now,
                local_completed_at=now,
            )

        redirected_or_fake_network = environment_observation(observed_at=now)
        redirected_or_fake_network["network_evidence"]["link_probes_failed"] = 1
        with self.assertRaisesRegex(RuntimeError, "link_probes_failed"):
            CAPACITY.validate_environment_observation(
                redirected_or_fake_network,
                "a" * 40,
                local_started_at=now,
                local_completed_at=now,
            )

    def test_environment_identity_cannot_change_between_observations(self) -> None:
        now = 1_700_000_000.0
        payloads = [environment_observation(observed_at=now) for _ in range(2)]
        payloads[1]["engine_evidence"]["deployment_ids_sha256"] = "9" * 64

        def runner(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
            return subprocess.CompletedProcess(argv, 0, json.dumps(payloads.pop(0)), "")

        provider = CAPACITY.EnvironmentEvidenceProvider(
            '["environment-helper"]', "a" * 40, runner=runner, now=lambda: now
        )
        provider.observe()
        with self.assertRaisesRegex(RuntimeError, "identity changed"):
            provider.observe()

    def test_runtime_runner_host_is_bound_to_the_executing_gate_machine(self) -> None:
        now = 1_700_000_000.0
        observation = environment_observation(observed_at=now)

        def runner(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
            return subprocess.CompletedProcess(argv, 0, json.dumps(observation), "")

        provider = CAPACITY.EnvironmentEvidenceProvider(
            '["environment-helper"]',
            "a" * 40,
            runner_machine_id_sha256="f" * 64,
            runner=runner,
            now=lambda: now,
        )
        with self.assertRaisesRegex(RuntimeError, "executing gate machine"):
            provider.observe()

        with tempfile.TemporaryDirectory() as directory:
            machine_id = pathlib.Path(directory) / "machine-id"
            machine_id.write_text("0123456789abcdef0123456789abcdef\n", encoding="ascii")
            self.assertEqual(
                CAPACITY.local_linux_machine_id_sha256(machine_id),
                hashlib.sha256(b"0123456789abcdef0123456789abcdef").hexdigest(),
            )

    def test_controlled_restart_rebases_only_the_control_plane_process_once(self) -> None:
        now = 1_700_000_000.0
        pre = environment_observation(observed_at=now)
        post = environment_observation(observed_at=now)
        post["runtime_evidence"]["control_plane"]["container"][
            "started_at"
        ] = "2026-08-03T00:00:01Z"
        post["runtime_evidence"]["restart_identity"][
            "started_at"
        ] = "2026-08-03T00:00:01Z"
        payloads = [pre, post, json.loads(json.dumps(post))]

        def runner(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
            return subprocess.CompletedProcess(argv, 0, json.dumps(payloads.pop(0)), "")

        provider = CAPACITY.EnvironmentEvidenceProvider(
            '["environment-helper"]', "a" * 40, runner=runner, now=lambda: now
        )
        observed_pre = provider.observe(establish_stable=False)
        provider.observe(restart_previous=observed_pre)
        provider.observe()

        unchanged = environment_observation(observed_at=now)
        payloads = [unchanged, json.loads(json.dumps(unchanged))]
        provider = CAPACITY.EnvironmentEvidenceProvider(
            '["environment-helper"]', "a" * 40, runner=runner, now=lambda: now
        )
        observed_pre = provider.observe(establish_stable=False)
        with self.assertRaisesRegex(RuntimeError, "did not change"):
            provider.observe(restart_previous=observed_pre)

        for invalid_started_at, message in (
            ("2026-08-02T23:59:59Z", "did not change"),
            ("2026-08-03 00:00:01", "RFC3339Nano"),
        ):
            pre = environment_observation(observed_at=now)
            post = environment_observation(observed_at=now)
            post["runtime_evidence"]["control_plane"]["container"][
                "started_at"
            ] = invalid_started_at
            post["runtime_evidence"]["restart_identity"][
                "started_at"
            ] = invalid_started_at
            payloads = [pre, post]
            provider = CAPACITY.EnvironmentEvidenceProvider(
                '["environment-helper"]', "a" * 40, runner=runner, now=lambda: now
            )
            observed_pre = provider.observe(establish_stable=False)
            with self.assertRaisesRegex(RuntimeError, message):
                provider.observe(restart_previous=observed_pre)

    def test_server_build_and_atomic_evidence_are_bound_to_candidate(self) -> None:
        commit = "a" * 40
        build = CAPACITY.validate_server_build(
            {
                "build": {
                    "version": "1.0.0",
                    "commit_sha": commit,
                    "profile": "production",
                    "target": "x86_64-unknown-linux-gnu",
                }
            },
            commit,
            "production",
        )
        self.assertEqual(build["commit_sha"], commit)
        with self.assertRaisesRegex(RuntimeError, "does not match"):
            CAPACITY.validate_server_build(
                {"build": {**build, "commit_sha": "b" * 40}},
                commit,
                "production",
            )

        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "capacity.json"
            report = CAPACITY.GateReport(
                profile="smoke", started_at="now", expected={}
            )
            writer = CAPACITY.EvidenceWriter(report, path)
            writer.event("test_event", safe=True)
            writer.prometheus_snapshot(
                1,
                "smoke",
                1_700_000_000.0,
                7_200.0,
                {"metric": 1.0},
                {"pool_connections": 1.0, "pool_idle_connections": 1.0},
            )
            writer.environment_snapshot(
                "qualification", None, environment_observation()
            )
            writer.finalize()
            on_disk = json.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(on_disk["schema_version"], 2)
            self.assertEqual(on_disk["logs"]["index"][0]["records"], 1)
            self.assertEqual(len(on_disk["logs"]["index"][0]["sha256"]), 64)
            self.assertEqual(on_disk["logs"]["index"][1]["records"], 1)
            self.assertEqual(len(on_disk["logs"]["index"][1]["sha256"]), 64)
            self.assertEqual(on_disk["logs"]["index"][2]["records"], 1)
            self.assertEqual(len(on_disk["logs"]["index"][2]["sha256"]), 64)
            metrics_record = json.loads(
                path.with_suffix(".metrics.ndjson").read_text(encoding="utf-8")
            )
            self.assertEqual(metrics_record["sampled_at_epoch_seconds"], 1_700_000_000.0)
            self.assertEqual(metrics_record["sample_clock_seconds"], 7_200.0)
            self.assertEqual(
                metrics_record["storage"],
                {"pool_connections": 1.0, "pool_idle_connections": 1.0},
            )

    def test_required_prometheus_metric_never_defaults_missing_evidence_to_zero(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "omitted finite metric"):
            CAPACITY.required_metric_value({}, "missing_metric")
        with self.assertRaisesRegex(RuntimeError, "omitted finite metric"):
            CAPACITY.required_metric_value({"metric": float("nan")}, "metric")
        self.assertEqual(CAPACITY.required_metric_value({"metric": 0.0}, "metric"), 0.0)

    def test_soak_anomaly_baseline_can_only_be_established_at_the_boundary(self) -> None:
        commit = "a" * 40
        build = {
            "version": "1.0.0",
            "commit_sha": commit,
            "profile": "development",
            "target": "x86_64-pc-windows-msvc",
        }
        readiness = {
            "build": build,
            "storage": {"pool_connections": 1, "pool_idle_connections": 1},
        }
        base_metrics = {
            "ojos_orchestrator_process_resident_memory_bytes": 1_000_000.0,
            "ojos_orchestrator_process_threads": 10.0,
            "ojos_orchestrator_http_active_requests": 1.0,
            "ojos_orchestrator_job_metrics_collection_error": 0.0,
            "ojos_orchestrator_expired_job_leases": 0.0,
            "ojos_orchestrator_oldest_leased_job_heartbeat_age_seconds": 0.0,
            "ojos_orchestrator_expired_job_lease_transitions_total": 0.0,
            "ojos_orchestrator_operation_over_300_seconds_transitions_total": 0.0,
            "ojos_orchestrator_operation_invalid_updated_at_transitions_total": 0.0,
            "ojos_orchestrator_control_plane_anomaly_observation_errors_total": 0.0,
            "ojos_orchestrator_control_plane_process_starts_total": 1.0,
            "ojos_orchestrator_control_plane_anomaly_state_loaded": 1.0,
            "ojos_orchestrator_process_start_time_seconds": 1_700_000_000.0,
        }
        changed_metrics = dict(base_metrics)
        changed_metrics[
            "ojos_orchestrator_operation_over_300_seconds_transitions_total"
        ] = 1.0
        args = CAPACITY.argparse.Namespace(
            profile="smoke",
            source_commit=commit,
            permanent_running_seconds=300,
        )

        with tempfile.TemporaryDirectory() as directory:
            report = CAPACITY.GateReport(
                profile="smoke", started_at="now", expected={}
            )
            report.identity = {"server_build": build}
            writer = CAPACITY.EvidenceWriter(
                report, pathlib.Path(directory) / "boundary.json"
            )
            with mock.patch.object(
                CAPACITY, "ready_snapshot", return_value=(1.0, readiness)
            ), mock.patch.object(
                CAPACITY, "metrics", side_effect=[base_metrics, changed_metrics]
            ):
                CAPACITY.capture_sample(
                    object(),
                    report,
                    writer,
                    args,
                    "soak_boundary",
                    CAPACITY.time.monotonic() - 1,
                    checkpoint=False,
                )
                with self.assertRaisesRegex(RuntimeError, "anomaly counter changed"):
                    CAPACITY.capture_sample(
                        object(),
                        report,
                        writer,
                        args,
                        "soak",
                        CAPACITY.time.monotonic() - 1,
                        checkpoint=False,
                    )
            self.assertEqual(
                report.evidence["anomaly_counter_baseline"],
                report.samples[0]["anomalies"],
            )

            no_boundary = CAPACITY.GateReport(
                profile="smoke", started_at="now", expected={}
            )
            no_boundary.identity = {"server_build": build}
            no_boundary_writer = CAPACITY.EvidenceWriter(
                no_boundary, pathlib.Path(directory) / "no-boundary.json"
            )
            with mock.patch.object(
                CAPACITY, "ready_snapshot", return_value=(1.0, readiness)
            ), mock.patch.object(CAPACITY, "metrics", return_value=base_metrics):
                with self.assertRaisesRegex(RuntimeError, "anomaly counter changed"):
                    CAPACITY.capture_sample(
                        object(),
                        no_boundary,
                        no_boundary_writer,
                        args,
                        "soak",
                        CAPACITY.time.monotonic() - 1,
                        checkpoint=False,
                    )
            self.assertNotIn("anomaly_counter_baseline", no_boundary.evidence)

    def test_atomic_checkpoint_ignores_stale_tmp_and_preserves_previous_on_failure(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "capacity.json"
            path.write_text('{"checkpoint":1}\n', encoding="utf-8")
            stale = path.with_suffix(path.suffix + ".tmp")
            stale.write_text("interrupted-old-writer", encoding="utf-8")

            CAPACITY.atomic_write(path, '{"checkpoint":2}\n')
            self.assertEqual(path.read_text(encoding="utf-8"), '{"checkpoint":2}\n')
            self.assertEqual(stale.read_text(encoding="utf-8"), "interrupted-old-writer")

            with mock.patch.object(
                CAPACITY.os, "replace", side_effect=OSError("injected replace failure")
            ), self.assertRaisesRegex(OSError, "injected replace failure"):
                CAPACITY.atomic_write(path, '{"checkpoint":3}\n')
            self.assertEqual(path.read_text(encoding="utf-8"), '{"checkpoint":2}\n')
            self.assertEqual(
                list(path.parent.glob(f".{path.name}.checkpoint-*.tmp")), []
            )

    def test_periodic_checkpoint_continues_while_the_main_thread_is_blocked(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "capacity.json"
            report = CAPACITY.GateReport(
                profile="smoke", started_at="now", expected={}
            )
            writer = CAPACITY.EvidenceWriter(report, path)
            writer.checkpoint()
            writer.start_periodic_checkpoints(0.02)
            time_started = CAPACITY.time.monotonic()
            CAPACITY.time.sleep(0.075)
            writer.stop_periodic_checkpoints()
            elapsed = CAPACITY.time.monotonic() - time_started
            writer.finalize()

            on_disk = json.loads(path.read_text(encoding="utf-8"))
            history = on_disk["evidence"]["checkpoint_history"]
            timestamps = [entry["clock_seconds"] for entry in history]
            self.assertGreaterEqual(len(history), 4)
            self.assertEqual(
                on_disk["evidence"]["checkpoint_count"], len(history)
            )
            self.assertEqual(on_disk["evidence"]["checkpoint_interval_seconds"], 0.02)
            self.assertEqual(
                [entry["sequence"] for entry in history],
                list(range(1, len(history) + 1)),
            )
            self.assertGreater(elapsed, 0)
            self.assertLess(
                max(
                    right - left
                    for left, right in zip(timestamps, timestamps[1:])
                ),
                0.05,
            )

    def test_checkpoint_io_does_not_hold_the_report_mutation_lock(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            report = CAPACITY.GateReport(
                profile="smoke", started_at="now", expected={}
            )
            writer = CAPACITY.EvidenceWriter(
                report, pathlib.Path(directory) / "capacity.json"
            )
            entered = CAPACITY.threading.Event()
            release = CAPACITY.threading.Event()
            original = CAPACITY.atomic_write

            def slow_write(path: pathlib.Path, contents: str) -> None:
                entered.set()
                self.assertTrue(release.wait(2))
                original(path, contents)

            with mock.patch.object(CAPACITY, "atomic_write", side_effect=slow_write):
                thread = CAPACITY.threading.Thread(target=writer.checkpoint)
                thread.start()
                self.assertTrue(entered.wait(1))
                acquired = report._checkpoint_lock.acquire(timeout=0.2)
                self.assertTrue(acquired)
                if acquired:
                    report.observed["mutation_during_io"] = 1
                    report._checkpoint_lock.release()
                release.set()
                thread.join(2)
                self.assertFalse(thread.is_alive())

    def test_concurrent_mutations_and_checkpoints_preserve_latest_generation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            report = CAPACITY.GateReport(
                profile="smoke", started_at="now", expected={}
            )
            writer = CAPACITY.EvidenceWriter(
                report, pathlib.Path(directory) / "capacity.json"
            )

            def mutate() -> None:
                for _ in range(50):
                    with report._checkpoint_lock:
                        report.observed["counter"] = report.observed.get("counter", 0) + 1

            mutations = [CAPACITY.threading.Thread(target=mutate) for _ in range(4)]
            checkpoints = [
                CAPACITY.threading.Thread(target=writer.checkpoint) for _ in range(20)
            ]
            for thread in [*mutations, *checkpoints]:
                thread.start()
            for thread in [*mutations, *checkpoints]:
                thread.join(5)
                self.assertFalse(thread.is_alive())
            writer.checkpoint()

            on_disk = json.loads(writer.report_path.read_text(encoding="utf-8"))
            history = on_disk["evidence"]["checkpoint_history"]
            self.assertEqual(on_disk["observed"]["counter"], 200)
            self.assertEqual(on_disk["evidence"]["checkpoint_count"], len(history))
            self.assertEqual(
                [entry["sequence"] for entry in history],
                list(range(1, len(history) + 1)),
            )

    def test_initial_evidence_failure_stops_the_periodic_writer(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            argv = [
                "orchestrator-capacity-gate.py",
                "--profile",
                "smoke",
                "--base-url",
                "http://127.0.0.1:1",
                "--report",
                str(pathlib.Path(directory) / "capacity.json"),
            ]
            with mock.patch.object(sys, "argv", argv):
                args = CAPACITY.arguments()
            with mock.patch.object(
                CAPACITY.EvidenceWriter,
                "event",
                side_effect=OSError("injected event failure"),
            ), self.assertRaisesRegex(OSError, "injected event failure"):
                CAPACITY.run_gate(args)
            self.assertFalse(
                any(
                    thread.name == "orchestrator-capacity-checkpoint"
                    and thread.is_alive()
                    for thread in CAPACITY.threading.enumerate()
                )
            )

    def test_https_origin_normalization_is_shared_and_fail_closed(self) -> None:
        self.assertEqual(
            CAPACITY.normalize_https_origin("https://CAPACITY.example.test:443/"),
            "https://capacity.example.test",
        )
        self.assertEqual(
            CAPACITY.normalize_https_origin("https://[2001:0DB8::1]:8443"),
            "https://[2001:db8::1]:8443",
        )
        for value in (
            "https://user@capacity.example.test",
            "https://capacity.example.test/path",
            "https://capacity.example.test?query=1",
            "https://capacity.example.test#fragment",
            " https://capacity.example.test",
        ):
            with self.assertRaises(RuntimeError):
                CAPACITY.normalize_https_origin(value)

    def test_runner_service_probe_binds_current_cgroup_and_rejects_restart(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            cgroup = pathlib.Path(directory) / "cgroup"
            boot_id = pathlib.Path(directory) / "boot_id"
            boot_id.write_text(
                "11111111-2222-3333-4444-555555555555\n", encoding="utf-8"
            )
            unit = "actions.runner.owner-repo.soak-1.service"
            expected_control_group = f"/system.slice/{unit}"
            cgroup.write_text(f"0::{expected_control_group}\n", encoding="utf-8")
            proc_root = pathlib.Path(directory) / "proc"
            write_fake_listener(proc_root, expected_control_group)
            write_fake_process(
                proc_root,
                pid=5_554,
                parent_pid=9_876,
                executable="/opt/actions-runner/bin/Runner.Worker",
                start_ticks=600_000,
            )
            write_fake_process(
                proc_root,
                pid=5_555,
                parent_pid=5_554,
                executable="/usr/bin/python3",
                start_ticks=700_000,
            )
            (proc_root / "1234").mkdir()
            invocation = ["1" * 32]
            active_state = ["active"]
            control_group = [f"/system.slice/{unit}"]
            calls: list[tuple[list[str], dict[str, Any]]] = []

            def runner(
                argv: list[str], **kwargs: Any
            ) -> subprocess.CompletedProcess[str]:
                calls.append((argv, kwargs))
                properties = {
                    "Id": unit,
                    "LoadState": "loaded",
                    "ActiveState": active_state[0],
                    "SubState": "running",
                    "ActiveEnterTimestamp": "Sun 2026-08-02 00:00:00 UTC",
                    "ActiveEnterTimestampMonotonic": "1000000",
                    "ExecMainStartTimestamp": "Sun 2026-08-02 00:00:00 UTC",
                    "ExecMainStartTimestampMonotonic": "900000",
                    "InvocationID": invocation[0],
                    "MainPID": "4321",
                    "ControlGroup": control_group[0],
                }
                stdout = "\n".join(
                    f"{name}={properties[name]}"
                    for name in CAPACITY.RUNNER_SERVICE_PROPERTIES
                )
                return subprocess.CompletedProcess(argv, 0, stdout, "")

            monotonic = [7_201_000_000_000]
            boottime_extra = [100_000_000_000]
            probe = CAPACITY.RunnerServiceProbe(
                "soak-1",
                cgroup_path=cgroup,
                proc_root=proc_root,
                boot_id_path=boot_id,
                systemctl_path=pathlib.PurePosixPath("/usr/bin/systemctl"),
                runner=runner,
                now=lambda: 1_700_000_000.0,
                monotonic_ns=lambda: monotonic[0],
                boottime_ns=lambda: monotonic[0] + boottime_extra[0],
                clock_ticks_per_second=100,
                process_id=5_555,
            )
            baseline = probe.observe()
            self.assertEqual(baseline["unit"], unit)
            self.assertAlmostEqual(baseline["active_uptime_seconds"], 7_200.0)
            self.assertEqual(baseline["listener_pid"], 9_876)
            self.assertEqual(baseline["listener_start_ticks"], 100_000)
            self.assertEqual(baseline["listener_ancestor_depth"], 2)
            self.assertEqual(calls[0][0][0], "/usr/bin/systemctl")
            self.assertIs(calls[0][1]["shell"], False)

            monotonic[0] += 30_000_000_000
            invocation[0] = "2" * 32
            with self.assertRaisesRegex(RuntimeError, "continuity changed"):
                probe.observe(baseline)
            invocation[0] = "1" * 32
            active_state[0] = "inactive"
            with self.assertRaisesRegex(RuntimeError, "not continuously active"):
                probe.observe(baseline)
            active_state[0] = "active"
            control_group[0] = "/system.slice/actions.runner.wrong.service"
            with self.assertRaisesRegex(RuntimeError, "does not identify"):
                probe.observe(baseline)
            control_group[0] = f"/system.slice/{unit}"
            boot_id.write_text(
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n", encoding="utf-8"
            )
            with self.assertRaisesRegex(RuntimeError, "continuity changed"):
                probe.observe(baseline)
            boot_id.write_text(
                "11111111-2222-3333-4444-555555555555\n", encoding="utf-8"
            )
            write_fake_listener(
                proc_root,
                expected_control_group,
                start_ticks=100_001,
            )
            with self.assertRaisesRegex(RuntimeError, "listener_start_ticks"):
                probe.observe(baseline)
            write_fake_listener(
                proc_root,
                expected_control_group,
                start_ticks=100_000,
            )
            boottime_extra[0] += 120_000_000_000
            with self.assertRaisesRegex(RuntimeError, "suspended"):
                probe.observe(baseline)

    def test_runner_service_clock_brackets_tolerate_scheduler_preemption(self) -> None:
        start = 7_200_000_000_000
        monotonic_values = iter(
            (
                start,
                start + 10_000_000_000,
                start + 20_000_000_000,
                start + 30_000_000_000,
            )
        )
        boottime_values = iter(
            (start + 105_000_000_000, start + 125_000_000_000)
        )
        before = CAPACITY.bracket_clock_sample(
            lambda: next(monotonic_values), lambda: next(boottime_values)
        )
        after = CAPACITY.bracket_clock_sample(
            lambda: next(monotonic_values), lambda: next(boottime_values)
        )

        self.assertEqual(
            CAPACITY.intersect_clock_offsets(before, after, "during test"),
            (95_000_000, 105_000_000),
        )

    def test_runner_service_probe_rejects_a_different_or_missing_runner_unit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            cgroup = pathlib.Path(directory) / "cgroup"
            cgroup.write_text(
                "0::/system.slice/actions.runner.owner-repo.someone-else.service\n",
                encoding="utf-8",
            )
            probe = CAPACITY.RunnerServiceProbe(
                "soak-1", cgroup_path=cgroup, boottime_ns=lambda: 1
            )
            with self.assertRaisesRegex(RuntimeError, "does not match RUNNER_NAME"):
                probe.observe()
            cgroup.write_text("0::/user.slice/session.scope\n", encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "exactly one"):
                probe.observe()

    def test_runner_listener_must_be_an_ancestor_and_ignores_unreadable_unrelated_pid(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            proc_root = pathlib.Path(directory) / "proc"
            control_group = (
                "/system.slice/actions.runner.owner-repo.soak-1.service"
            )
            write_fake_process(
                proc_root,
                pid=5_555,
                parent_pid=5_554,
                executable="/usr/bin/python3",
                start_ticks=700_000,
            )
            write_fake_process(
                proc_root,
                pid=5_554,
                parent_pid=0,
                executable="/opt/actions-runner/bin/Runner.Worker",
                start_ticks=600_000,
            )
            write_fake_listener(proc_root, control_group)
            (proc_root / "1234").mkdir()
            probe = CAPACITY.RunnerServiceProbe(
                "soak-1",
                proc_root=proc_root,
                process_id=5_555,
                clock_ticks_per_second=100,
            )
            original_bounded = getattr(probe, "_bounded_bytes")

            def reject_unrelated_read(
                path: pathlib.Path, label: str, limit: int = 65_536
            ) -> bytes:
                if path.parent.name == "1234":
                    raise AssertionError("unrelated unreadable PID was inspected")
                return original_bounded(path, label, limit)

            with mock.patch.object(
                probe, "_bounded_bytes", side_effect=reject_unrelated_read
            ), self.assertRaisesRegex(RuntimeError, "Runner.Listener ancestor"):
                probe._listener_identity(  # pylint: disable=protected-access
                    control_group
                )

    def test_github_run_metadata_proves_the_dispatch_time_and_candidate(self) -> None:
        payload = {
            "id": 123,
            "workflow_id": 456,
            "run_attempt": 1,
            "event": "workflow_dispatch",
            "head_sha": "a" * 40,
            "head_branch": "main",
            "name": "Orchestrator capacity and soak gate",
            "path": ".github/workflows/orchestrator-capacity.yml",
            "created_at": "2026-08-03T01:02:03Z",
        }

        class Response:
            status = 200
            headers: dict[str, str] = {
                "Date": "Mon, 03 Aug 2026 01:12:03 GMT"
            }

            def __enter__(self) -> "Response":
                return self

            def __exit__(self, *args: Any) -> None:
                return None

            def read(self, size: int) -> bytes:
                self.requested_size = size
                return json.dumps(payload).encode("utf-8")

        requests: list[Any] = []

        def opener(request: Any, **kwargs: Any) -> Response:
            requests.append((request, kwargs))
            return Response()

        metadata = CAPACITY.github_run_metadata(
            "secret-actions-token",
            "owner/repo",
            "123",
            "1",
            "Orchestrator capacity and soak gate",
            "a" * 40,
            opener=opener,
            now=lambda: 1_785_719_525.0,
            monotonic_ns=lambda: 7_300_000_000_000,
            boottime_ns=lambda: 7_400_000_000_000,
        )
        self.assertTrue(metadata["api_verified"])
        self.assertEqual(metadata["workflow_id"], 456)
        self.assertEqual(
            metadata["path"], ".github/workflows/orchestrator-capacity.yml"
        )
        self.assertEqual(metadata["created_at_epoch_seconds"], 1_785_718_923.0)
        self.assertEqual(metadata["api_date_epoch_seconds"], 1_785_719_523.0)
        self.assertEqual(metadata["api_local_clock_skew_seconds"], 2.0)
        self.assertEqual(
            requests[0][0].full_url,
            "https://api.github.com/repos/owner/repo/actions/runs/123",
        )
        self.assertEqual(requests[0][1]["timeout"], 15)

        payload["path"] = ".github/workflows/not-the-capacity-gate.yml"
        with self.assertRaisesRegex(RuntimeError, "path"):
            CAPACITY.github_run_metadata(
                "secret-actions-token",
                "owner/repo",
                "123",
                "1",
                "Orchestrator capacity and soak gate",
                "a" * 40,
                opener=opener,
                now=lambda: 1_785_719_525.0,
                monotonic_ns=lambda: 7_300_000_000_000,
                boottime_ns=lambda: 7_400_000_000_000,
            )
        payload["path"] = ".github/workflows/orchestrator-capacity.yml"
        payload["workflow_id"] = 0
        with self.assertRaisesRegex(RuntimeError, "workflow_id"):
            CAPACITY.github_run_metadata(
                "secret-actions-token",
                "owner/repo",
                "123",
                "1",
                "Orchestrator capacity and soak gate",
                "a" * 40,
                opener=opener,
                now=lambda: 1_785_719_525.0,
                monotonic_ns=lambda: 7_300_000_000_000,
                boottime_ns=lambda: 7_400_000_000_000,
            )
        payload["workflow_id"] = 456

        payload["head_branch"] = "feature"
        with self.assertRaisesRegex(RuntimeError, "head_branch"):
            CAPACITY.github_run_metadata(
                "secret-actions-token",
                "owner/repo",
                "123",
                "1",
                "Orchestrator capacity and soak gate",
                "a" * 40,
                opener=opener,
                now=lambda: 1_785_719_525.0,
                monotonic_ns=lambda: 7_300_000_000_000,
                boottime_ns=lambda: 7_400_000_000_000,
            )

        payload["head_branch"] = "main"
        with self.assertRaisesRegex(RuntimeError, "does not permit workflow reruns"):
            CAPACITY.github_run_metadata(
                "secret-actions-token",
                "owner/repo",
                "123",
                "2",
                "Orchestrator capacity and soak gate",
                "a" * 40,
                opener=opener,
            )

        Response.headers = {}
        with self.assertRaisesRegex(RuntimeError, "Date header"):
            CAPACITY.github_run_metadata(
                "secret-actions-token",
                "owner/repo",
                "123",
                "1",
                "Orchestrator capacity and soak gate",
                "a" * 40,
                opener=opener,
                now=lambda: 1_785_719_525.0,
                monotonic_ns=lambda: 7_300_000_000_000,
                boottime_ns=lambda: 7_400_000_000_000,
            )
        Response.headers = {"Date": "Mon, 03 Aug 2026 01:12:03 GMT"}
        with self.assertRaisesRegex(RuntimeError, "more than 30 seconds"):
            CAPACITY.github_run_metadata(
                "secret-actions-token",
                "owner/repo",
                "123",
                "1",
                "Orchestrator capacity and soak gate",
                "a" * 40,
                opener=opener,
                now=lambda: 1_785_719_323.0,
                monotonic_ns=lambda: 7_300_000_000_000,
                boottime_ns=lambda: 7_400_000_000_000,
            )

    def test_production_workflow_grants_actions_read_and_passes_ephemeral_token(self) -> None:
        workflow = (
            pathlib.Path(__file__).parents[3]
            / ".github"
            / "workflows"
            / "orchestrator-capacity.yml"
        ).read_text(encoding="utf-8")
        self.assertIn("actions: read", workflow)
        self.assertIn("ORCHESTRATOR_GATE_GITHUB_TOKEN: ${{ github.token }}", workflow)
        self.assertNotIn(
            "secrets.ORCHESTRATOR_GATE_RESTART_ARGV_JSON", workflow
        )
        self.assertIn(
            '[[ -n "${ORCHESTRATOR_GATE_RESTART_ARGV_JSON:-}" ]]', workflow
        )
        self.assertIn("Verify the pinned production evidence toolchain", workflow)
        self.assertIn('[[ "$(command -v gh)" == /usr/local/bin/gh ]]', workflow)
        self.assertIn("^gh version 2\\.97\\.0", workflow)
        self.assertIn('[[ "$(command -v jq)" == /usr/local/bin/jq ]]', workflow)
        self.assertIn('[[ "$(jq --version)" == jq-1.7.1 ]]', workflow)

    def test_topology_resource_count_reads_the_applied_revision_not_the_draft(self) -> None:
        total, latencies = CAPACITY.topology_resource_count(
            TopologyClient(), [{"topology_id": "topology-1"}]
        )
        self.assertEqual(total, 5)
        self.assertEqual(latencies, [2.0, 3.0])

    def test_selects_real_deployments_evenly_across_the_observed_page_set(self) -> None:
        deployments = [
            {
                "node_id": f"node-{index % 100:03d}",
                "instance": {
                    "deployment_id": f"deployment-{index:04d}",
                    "container_id": f"container-{index:04d}",
                },
            }
            for index in range(2_000)
        ]

        targets = CAPACITY.select_operation_targets(deployments, 50)

        self.assertEqual(len(targets), 50)
        self.assertEqual(len({deployment for deployment, _, _ in targets}), 50)
        self.assertEqual(targets[0][0], "deployment-0000")
        self.assertEqual(targets[-1][0], "deployment-0099")
        self.assertEqual(len({node for _, node, _ in targets}), 50)
        observed = {
            (
                item["instance"]["deployment_id"],
                item["node_id"],
                item["instance"]["container_id"],
            )
            for item in deployments
        }
        self.assertTrue(set(targets).issubset(observed))

    def test_rejects_missing_real_deployment_node_pairs(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "distinct Nodes"):
            CAPACITY.select_operation_targets(
                [
                    {
                        "node_id": "node-1",
                        "instance": {
                            "deployment_id": "deployment-1",
                            "container_id": "container-1",
                        },
                    },
                    {
                        "node_id": "node-without-container",
                        "instance": {
                            "deployment_id": "deployment-without-container"
                        },
                    },
                ],
                2,
            )

    def test_operation_cycle_targets_real_deployment_and_accepts_terminal_cancel_race(self) -> None:
        client = OperationClient(409, "SUCCEEDED")

        samples, operation_id, event_latency = CAPACITY.operation_cycle(
            client,
            "capacity-run",
            7,
            "deployment-real-0042",
            "node-real-0042",
            "container-real-0042",
        )

        self.assertEqual(operation_id, "operation-1")
        self.assertGreater(event_latency, 0)
        assert client.plan is not None
        self.assertEqual(
            client.plan["fields"]["deployment_id"], "deployment-real-0042"
        )
        self.assertEqual(client.plan["fields"]["target_node_id"], "node-real-0042")
        self.assertEqual(
            client.plan["fields"]["payload"],
            {"container_id": "container-real-0042"},
        )
        cancel = next(sample for sample in samples if sample.name == "operation.cancel")
        self.assertTrue(cancel.ok)
        self.assertEqual(cancel.status, 409)
        self.assertIn("SUCCEEDED", cancel.detail)
        self.assertIn(
            ("GET", "/api/v1/operations/operation-1", None, ""), client.calls
        )
        self.assertEqual(len(client.event_requests), 2)
        baseline_id = operation_event_cursor(2)
        self.assertNotIn(
            "Last-Event-ID", client.event_requests[0]["request_headers"]
        )
        self.assertEqual(
            client.event_requests[1]["request_headers"]["Last-Event-ID"],
            baseline_id,
        )
        self.assertEqual(
            client.event_requests[1]["maximum_response_bytes"],
            CAPACITY.MAX_OPERATION_SSE_BYTES,
        )

    def test_operation_event_latency_rejects_only_historical_events(self) -> None:
        client = OperationClient(202, event_mode="old")
        samples, _, event_latency = CAPACITY.operation_cycle(
            client,
            "capacity-run",
            3,
            "deployment-real-0003",
            "node-real-0003",
            "container-real-0003",
        )
        self.assertEqual(event_latency, 0)
        event_sample = next(
            sample for sample in samples if sample.name == "operation.events"
        )
        self.assertFalse(event_sample.ok)
        self.assertIn("strictly advance", event_sample.detail)

    def test_operation_event_latency_counts_a_new_event_after_reconnect(self) -> None:
        client = OperationClient(202, event_mode="reconnect")
        samples, _, event_latency = CAPACITY.operation_cycle(
            client,
            "capacity-run",
            4,
            "deployment-real-0004",
            "node-real-0004",
            "container-real-0004",
        )
        self.assertGreater(event_latency, 0)
        self.assertEqual(len(client.event_requests), 3)
        baseline_id = operation_event_cursor(2)
        self.assertEqual(
            [
                request["request_headers"].get("Last-Event-ID")
                for request in client.event_requests
            ],
            [None, baseline_id, baseline_id],
        )
        self.assertTrue(
            next(sample for sample in samples if sample.name == "operation.events").ok
        )

    def test_non_apply_mutation_p95_is_gated_independently(self) -> None:
        report = CAPACITY.GateReport(
            profile="smoke", started_at="now", expected={}
        )
        measurements = {
            "read": [1.0],
            "event": [1.0],
            "mutation": [1.0] * 100 + [600.0],
            "mutation_plan": [1.0],
            "mutation_confirm": [600.0],
            "mutation_apply": [1.0],
            "mutation_cancel": [1.0],
        }
        CAPACITY.finalize_latency_measurements(
            report,
            measurements,
            read_threshold_ms=200,
            mutation_threshold_ms=500,
            event_threshold_ms=1_000,
        )
        self.assertLessEqual(report.measurements_ms["mutation_accept_p95"], 500)
        self.assertEqual(report.measurements_ms["mutation_confirm_p95"], 600)
        self.assertTrue(
            any("mutation_confirm_p95" in failure for failure in report.failures)
        )

    def test_operation_round_records_every_mutation_action_latency(self) -> None:
        samples = [
            CAPACITY.Sample("operation.plan", 201, 10.0, True, ""),
            CAPACITY.Sample("operation.confirm", 200, 20.0, True, ""),
            CAPACITY.Sample("operation.event_baseline", 200, 5.0, True, ""),
            CAPACITY.Sample("operation.apply", 202, 30.0, True, ""),
            CAPACITY.Sample("operation.events", 200, 4.0, True, ""),
            CAPACITY.Sample("operation.cancel", 202, 40.0, True, ""),
        ]
        report = CAPACITY.GateReport(
            profile="smoke", started_at="now", expected={}
        )
        measurements = {
            "read": [],
            "event": [],
            "mutation": [],
            "mutation_plan": [],
            "mutation_confirm": [],
            "mutation_apply": [],
            "mutation_cancel": [],
        }
        args = CAPACITY.argparse.Namespace(concurrent_operations=1)
        with mock.patch.object(
            CAPACITY,
            "operation_cycle",
            return_value=(samples, "operation-1", 4.0),
        ):
            self.assertTrue(
                CAPACITY.execute_operation_round(
                    object(),
                    report,
                    args,
                    [("deployment-1", "node-1", "container-1")],
                    "run-1",
                    1,
                    "soak",
                    measurements,
                )
            )
        self.assertEqual(measurements["mutation"], [10.0, 20.0, 30.0, 40.0])
        self.assertEqual(measurements["mutation_plan"], [10.0])
        self.assertEqual(measurements["mutation_confirm"], [20.0])
        self.assertEqual(measurements["mutation_apply"], [30.0])
        self.assertEqual(measurements["mutation_cancel"], [40.0])
        round_report = report.operation_rounds[0]
        self.assertEqual(round_report["unique_created_operations"], 1)
        self.assertEqual(round_report["target_nodes"], 1)
        self.assertEqual(round_report["target_deployments"], 1)
        self.assertEqual(round_report["target_containers"], 1)
        self.assertEqual(
            round_report["target_identities"],
            [
                {
                    "deployment_id": "deployment-1",
                    "node_id": "node-1",
                    "container_id": "container-1",
                }
            ],
        )
        self.assertEqual(round_report["operation_ids"], ["operation-1"])
        self.assertRegex(round_report["target_identities_sha256"], r"^[0-9a-f]{64}$")
        self.assertRegex(round_report["operation_ids_sha256"], r"^[0-9a-f]{64}$")

    def test_operation_round_rejects_duplicate_operation_ids(self) -> None:
        samples = [CAPACITY.Sample("operation.plan", 201, 1.0, True, "")]
        report = CAPACITY.GateReport(profile="smoke", started_at="now", expected={})
        measurements = {
            "read": [],
            "event": [],
            "mutation": [],
            "mutation_plan": [],
            "mutation_confirm": [],
            "mutation_apply": [],
            "mutation_cancel": [],
        }
        args = CAPACITY.argparse.Namespace(concurrent_operations=2, profile="smoke")
        with mock.patch.object(
            CAPACITY,
            "operation_cycle",
            return_value=(samples, "operation-duplicate", 1.0),
        ):
            self.assertFalse(
                CAPACITY.execute_operation_round(
                    object(),
                    report,
                    args,
                    [
                        ("deployment-1", "node-1", "container-1"),
                        ("deployment-2", "node-2", "container-2"),
                    ],
                    "run-1",
                    1,
                    "soak",
                    measurements,
                )
            )
        self.assertEqual(report.operation_rounds[0]["created_operations"], 2)
        self.assertEqual(report.operation_rounds[0]["unique_created_operations"], 1)

    def test_restart_probe_snapshot_binds_operation_request_and_event_cursor(self) -> None:
        operation = {
            "operation_id": "operation-restart",
            "action": "deployment.health",
            "status": "PLANNED",
            "revision": 1,
            "request": {"target_node_id": "node-1"},
            "updated_at_ms": 1_700_000_000_000,
        }

        class RestartClient:
            def json(self, method: str, path: str) -> tuple[int, Any, float, dict[str, str]]:
                self.last_json = (method, path)
                return 200, {"data": {"operation": operation}}, 1.0, {}

            def call(self, method: str, path: str, **kwargs: Any) -> tuple[int, bytes, float, dict[str, str]]:
                self.last_call = (method, path, kwargs)
                return (
                    200,
                    operation_sse("operation-restart", 1),
                    1.0,
                    {"content-type": "text/event-stream; charset=utf-8"},
                )

        snapshot = CAPACITY.operation_durability_snapshot(
            RestartClient(), "operation-restart"
        )
        self.assertEqual(snapshot["status"], "PLANNED")
        self.assertEqual(snapshot["action"], "deployment.health")
        self.assertEqual(snapshot["revision"], 1)
        self.assertEqual(snapshot["event_cursor"], operation_event_cursor(1))
        self.assertEqual(snapshot["request_sha256"], CAPACITY.canonical_json_sha256(operation["request"]))
        self.assertEqual(snapshot["operation_sha256"], CAPACITY.canonical_json_sha256(operation))

    def test_cancel_conflict_is_rejected_until_get_confirms_formal_terminal_state(self) -> None:
        for observed_status in ("RUNNING", "CANCELLING", "COMPLETED", ""):
            with self.subTest(observed_status=observed_status):
                client = OperationClient(409, observed_status)
                samples, _, _ = CAPACITY.operation_cycle(
                    client,
                    "capacity-run",
                    1,
                    "deployment-real-0001",
                    "node-real-0001",
                    "container-real-0001",
                )
                cancel = next(
                    sample for sample in samples if sample.name == "operation.cancel"
                )
                self.assertFalse(cancel.ok)

    def test_accepted_cancel_does_not_require_follow_up_get(self) -> None:
        client = OperationClient(202)

        samples, _, _ = CAPACITY.operation_cycle(
            client,
            "capacity-run",
            2,
            "deployment-real-0002",
            "node-real-0002",
            "container-real-0002",
        )

        cancel = next(sample for sample in samples if sample.name == "operation.cancel")
        self.assertTrue(cancel.ok)
        self.assertFalse(
            any(
                method == "GET" and path == "/api/v1/operations/operation-1"
                for method, path, _, _ in client.calls
            )
        )


if __name__ == "__main__":
    unittest.main()
