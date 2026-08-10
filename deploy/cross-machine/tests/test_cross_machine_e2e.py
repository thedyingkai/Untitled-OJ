from __future__ import annotations

import copy
import hashlib
import http.client
import importlib.util
import inspect
import json
import socket
import sys
import tempfile
import threading
import time
import types
import unittest
from pathlib import Path
from unittest import mock


MODULE_PATH = Path(__file__).resolve().parents[1] / "cross_machine_e2e.py"
SPEC = importlib.util.spec_from_file_location("cross_machine_e2e", MODULE_PATH)
assert SPEC and SPEC.loader
gate = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = gate
SPEC.loader.exec_module(gate)

FULL_COMPONENTS_MODULE_PATH = MODULE_PATH.parent / "full_components.py"
FULL_COMPONENTS_SPEC = importlib.util.spec_from_file_location(
    "cross_machine_full_components", FULL_COMPONENTS_MODULE_PATH
)
assert FULL_COMPONENTS_SPEC and FULL_COMPONENTS_SPEC.loader
full_components = importlib.util.module_from_spec(FULL_COMPONENTS_SPEC)
sys.modules[FULL_COMPONENTS_SPEC.name] = full_components
FULL_COMPONENTS_SPEC.loader.exec_module(full_components)

FIXTURE_MODULE_PATH = MODULE_PATH.parent / "fixture" / "fixture.py"
FIXTURE_SPEC = importlib.util.spec_from_file_location(
    "cross_machine_fixture", FIXTURE_MODULE_PATH
)
assert FIXTURE_SPEC and FIXTURE_SPEC.loader
fixture_service = importlib.util.module_from_spec(FIXTURE_SPEC)
sys.modules[FIXTURE_SPEC.name] = fixture_service
FIXTURE_SPEC.loader.exec_module(fixture_service)


class RunnerTests(unittest.TestCase):
    def test_stdin_payload_is_delivered_without_entering_argv(self) -> None:
        secret = "stdin-only-management-bearer"
        result = gate.Runner().run(
            [
                sys.executable,
                "-c",
                "import sys; print(sys.stdin.buffer.read().decode('utf-8'))",
            ],
            input_data=secret,
        )
        self.assertEqual(result.stdout.strip(), secret)
        self.assertNotIn(secret, result.argv)

    def test_descendant_inheriting_output_does_not_hold_runner_open(self) -> None:
        child = "import time; time.sleep(2)"
        parent = (
            "import subprocess,sys;"
            f"subprocess.Popen([sys.executable,'-c',{child!r}]);"
            "print('parent-complete')"
        )
        started = time.monotonic()
        result = gate.Runner().run([sys.executable, "-c", parent], timeout=1)
        self.assertLess(time.monotonic() - started, 1)
        self.assertEqual(result.stdout.strip(), "parent-complete")

    def test_timeout_reports_bounded_captured_output(self) -> None:
        command = "import time; print('before-timeout', flush=True); time.sleep(5)"
        started = time.monotonic()
        with self.assertRaisesRegex(gate.GateError, "before-timeout"):
            gate.Runner().run([sys.executable, "-c", command], timeout=0.1)
        self.assertLess(time.monotonic() - started, 2)

    def test_nonzero_exit_reports_redirected_stderr(self) -> None:
        command = "import sys; print('engine-unavailable', file=sys.stderr); sys.exit(7)"
        with self.assertRaisesRegex(gate.GateError, "engine-unavailable"):
            gate.Runner().run([sys.executable, "-c", command])


class JsonClientStatusOnlyTests(unittest.TestCase):
    @staticmethod
    def _client(
        status: int, body: bytes
    ) -> tuple[full_components.JsonClient, mock.MagicMock]:
        client = full_components.JsonClient.__new__(full_components.JsonClient)
        client.origin = "https://fixture.example"
        client.default_headers = {}
        client.opener = mock.Mock()
        response = mock.MagicMock()
        response.status = status
        response.headers = {"Content-Type": "text/plain", "X-Fixture": "status"}
        response.read.return_value = body
        client.opener.open.return_value = response
        return client, response

    @staticmethod
    def _http_error(
        client: full_components.JsonClient, status: int, body: bytes
    ) -> mock.MagicMock:
        stream = mock.MagicMock()
        stream.read.return_value = body
        client.opener.open.side_effect = full_components.urllib.error.HTTPError(
            "https://fixture.example/removed-route",
            status,
            "fixture error",
            {"Content-Type": "text/plain", "X-Fixture": "status"},
            stream,
        )
        return stream

    def test_default_mode_still_rejects_a_non_json_response(self) -> None:
        client, response = self._client(200, b"plain-text-not-json")

        with self.assertRaisesRegex(
            full_components.FullGateError, "did not return JSON"
        ):
            client.request("GET", "/negative", expected=(200,))

        response.read.assert_called_once_with()

    def test_status_only_accepts_expected_404_without_reading_its_body(self) -> None:
        client, _ = self._client(404, b"unused")
        stream = self._http_error(client, 404, b"must-not-enter-evidence")

        document, headers, status = client.request(
            "GET", "/removed-route", expected=(404,), status_only=True
        )

        self.assertEqual(document, {})
        self.assertEqual(headers["x-fixture"], "status")
        self.assertEqual(status, 404)
        stream.read.assert_not_called()

    def test_status_only_rejects_unexpected_status_without_returning_body(self) -> None:
        client, _ = self._client(500, b"unused")
        stream = self._http_error(
            client, 500, b"must-not-enter-error-evidence"
        )

        with self.assertRaisesRegex(
            full_components.FullGateError,
            r"returned 500, expected \[404\]",
        ) as raised:
            client.request(
                "GET", "/removed-route", expected=(404,), status_only=True
            )

        stream.read.assert_not_called()
        self.assertNotIn("must-not-enter", str(raised.exception))


class EngineConfigurationTests(unittest.TestCase):
    def test_windows_uses_vfs_to_avoid_nested_overlay_deadlock(self) -> None:
        self.assertEqual(gate.default_dind_storage_driver("Windows"), "vfs")

    def test_linux_keeps_overlay2(self) -> None:
        self.assertEqual(gate.default_dind_storage_driver("Linux"), "overlay2")

    def test_workflow_uses_the_same_digest_pinned_dind_image(self) -> None:
        workflow = (
            MODULE_PATH.parents[2]
            / ".github"
            / "workflows"
            / "orchestrator-cross-machine-e2e.yml"
        ).read_text(encoding="utf-8")
        self.assertIn(
            f"OJOS_CROSS_MACHINE_DIND_IMAGE: {gate.DEFAULT_DIND_IMAGE}", workflow
        )
        self.assertIn("OJOS_CROSS_MACHINE_DIND_STORAGE_DRIVER: vfs", workflow)

    def test_dind_entrypoint_is_given_an_explicit_dockerd_command(self) -> None:
        source = inspect.getsource(gate.LiveGate._start_engines)
        self.assertIn('dind_image,\n                "dockerd",', source)
        self.assertIn('"--tls=false"', source)

    def test_wait_engine_requires_two_complete_stable_identity_probes(self) -> None:
        engine = mock.Mock()
        incomplete = {
            "ID": "",
            "Name": "engine-a",
            "Driver": "vfs",
            "ServerVersion": "29",
            "OSType": "linux",
            "DockerRootDir": "/var/lib/docker",
        }
        ready = {
            **incomplete,
            "ID": "11111111-1111-4111-8111-111111111111",
        }
        engine.command.side_effect = [
            gate.Completed(["docker", "info"], 0, json.dumps(incomplete), ""),
            gate.Completed(["docker", "info"], 0, json.dumps(ready), ""),
            gate.Completed(["docker", "info"], 0, json.dumps(ready), ""),
        ]

        with mock.patch.object(gate.time, "monotonic", side_effect=[0, 0, 0, 0]), mock.patch.object(
            gate.time, "sleep"
        ):
            observed = gate.wait_engine(engine, timeout=10)

        self.assertEqual(observed, ready)
        self.assertEqual(engine.command.call_count, 3)

    def test_wait_engine_resets_stability_after_transient_command_error(self) -> None:
        engine = mock.Mock()
        ready = {
            "ID": "11111111-1111-4111-8111-111111111111",
            "Name": "engine-a",
            "Driver": "vfs",
            "ServerVersion": "29",
            "OSType": "linux",
            "DockerRootDir": "/var/lib/docker",
        }
        engine.command.side_effect = [
            gate.Completed(["docker", "info"], 0, json.dumps(ready), ""),
            gate.GateError("daemon connection reset"),
            gate.Completed(["docker", "info"], 0, json.dumps(ready), ""),
            gate.Completed(["docker", "info"], 0, json.dumps(ready), ""),
        ]

        with mock.patch.object(gate.time, "monotonic", side_effect=[0] * 5), mock.patch.object(
            gate.time, "sleep"
        ):
            observed = gate.wait_engine(engine, timeout=10)

        self.assertEqual(observed, ready)
        self.assertEqual(engine.command.call_count, 4)

    def test_volume_create_timeout_is_tracked_for_exact_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            live = gate.LiveGate(Path(directory), Path(directory) / "evidence.json", False)

            def root_command(*args, **_kwargs):
                if args[:2] == ("volume", "inspect"):
                    return gate.Completed(["docker", *args], 1, "", "no such volume")
                if args[:2] == ("volume", "create"):
                    raise gate.GateError("create response was lost")
                return gate.Completed(["docker", *args], 0, "", "")

            live.root.command = mock.Mock(side_effect=root_command)
            with self.assertRaisesRegex(gate.GateError, "response was lost"):
                live._start_engines()

            self.assertEqual(live.dind_data_volumes_created, [live.a_data_volume])

    def test_preexisting_run_scoped_volume_is_never_adopted_for_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            live = gate.LiveGate(Path(directory), Path(directory) / "evidence.json", False)

            def root_command(*args, **_kwargs):
                if args[:2] == ("volume", "inspect"):
                    return gate.Completed(["docker", *args], 0, "[]", "")
                return gate.Completed(["docker", *args], 0, "", "")

            live.root.command = mock.Mock(side_effect=root_command)
            with self.assertRaisesRegex(gate.GateError, "already exists"):
                live._start_engines()

            self.assertEqual(live.dind_data_volumes_created, [])

    def test_required_engine_identity_rejects_an_incomplete_info_response(self) -> None:
        with self.assertRaisesRegex(gate.GateError, "ServerVersion"):
            gate.required_engine_identity(
                {
                    "ID": "11111111-1111-4111-8111-111111111111",
                    "Name": "engine-a",
                    "Driver": "vfs",
                }
            )

    def test_checkpoint_is_atomic_running_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory) / "evidence.json"
            live = gate.LiveGate(Path(directory), evidence, full_components=True)
            live.checkpoint("full.build-images")
            value = json.loads(evidence.read_text(encoding="utf-8"))
            self.assertEqual(value["status"], "RUNNING")
            self.assertEqual(value["phase"], "full.build-images")
            self.assertEqual(value["run_id"], live.run_id)
            self.assertFalse(evidence.with_name("evidence.json.tmp").exists())


class RuntimeProjectionConvergenceTests(unittest.TestCase):
    @staticmethod
    def _projection(observed_at_ms: int, *, attested: bool = True) -> dict:
        return {
            "node_id": "node-b",
            "last_observed_at_ms": observed_at_ms,
            "drift_reason": "" if attested else "HostConfig drift",
            "instance": {
                "deployment_id": "deployment-worker-b",
                "desired_state": "RUNNING",
                "observed_state": "RUNNING",
                "health": "HEALTHY",
                "runtime_attested": attested,
            },
        }

    @staticmethod
    def _scenario(*payloads: dict):
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.h = types.SimpleNamespace(evidence={})
        scenario._control_get = mock.Mock(
            side_effect=[{"data": {"deployment": payload}} for payload in payloads]
        )
        return scenario

    def test_requires_immediate_healthy_runtime_attestation(self) -> None:
        scenario = self._scenario(self._projection(100, attested=False))
        with self.assertRaisesRegex(full_components.FullGateError, "immediate lifecycle"):
            scenario._managed_runtime_convergence(
                "deployment-worker-b",
                "node-b",
                {"result": {"runtime_observed_at_ms": 100}},
                timeout=0,
            )

    def test_accepts_only_a_strictly_newer_agent_inventory(self) -> None:
        immediate = self._projection(100)
        inventory = self._projection(130)
        scenario = self._scenario(immediate, inventory)
        with mock.patch.object(full_components.time, "monotonic", return_value=0), mock.patch.object(
            full_components.time, "sleep", return_value=None
        ):
            evidence = scenario._managed_runtime_convergence(
                "deployment-worker-b",
                "node-b",
                {"result": {"runtime_observed_at_ms": 100}},
                timeout=1,
            )

        self.assertEqual(evidence["completion_watermark_ms"], 100)
        self.assertEqual(evidence["immediate_payload"], immediate)
        self.assertEqual(evidence["inventory_payload"], inventory)
        self.assertEqual(scenario._control_get.call_count, 2)

    def test_timeout_retains_the_last_complete_deployment_payload(self) -> None:
        latest = self._projection(100)
        latest["diagnostic_marker"] = "full-payload-retained"
        scenario = self._scenario(latest)
        with self.assertRaisesRegex(
            full_components.FullGateError, "full-payload-retained"
        ):
            scenario._managed_runtime_convergence(
                "deployment-worker-b",
                "node-b",
                {"result": {"runtime_observed_at_ms": 100}},
                timeout=0,
            )

        failure = scenario.h.evidence["runtime_inventory_convergence_failures"][
            "deployment-worker-b"
        ]
        self.assertEqual(failure["last_payload"], latest)

    def test_rejects_a_missing_lifecycle_watermark(self) -> None:
        scenario = self._scenario(self._projection(100))
        with self.assertRaisesRegex(
            full_components.FullGateError, "runtime_observed_at_ms"
        ):
            scenario._managed_runtime_convergence(
                "deployment-worker-b", "node-b", {"result": {}}, timeout=0
            )


class ProviderProjectionIntegrityConvergenceTests(unittest.TestCase):
    revision_id = "topology-a-b:r7:" + "7" * 64
    content_sha256 = "a" * 64

    @classmethod
    def _topology_response(cls, *, converged: bool) -> tuple[dict, dict, int]:
        status = {
            "desired_revision_id": cls.revision_id,
            "observed_revision_id": cls.revision_id if converged else None,
            "state": "IN_SYNC" if converged else "DEGRADED",
            "drift": []
            if converged
            else [{"detail": "must-not-leak-transitional-secret"}],
        }
        return (
            {
                "data": {
                    "draft": {
                        "revision_id": cls.revision_id,
                        "content_sha256": cls.content_sha256,
                        "ignored_secret": "must-not-leak-document-secret",
                    },
                    "heads": {
                        "draft_revision_id": cls.revision_id,
                        "applied_revision_id": cls.revision_id,
                        "applying_revision_id": None,
                        "applying_operation_id": None,
                    },
                    "status": status,
                }
            },
            {"etag": f'"{cls.revision_id}"', "x-secret": "not-evidence"},
            200,
        )

    @classmethod
    def _projection_snapshot(cls, provider: str) -> dict:
        return {
            "projection": {
                "provider": provider,
                "topology_id": "topology-a-b",
                "revision_id": cls.revision_id,
                "content_sha256": cls.content_sha256,
                "routes": [],
                "grants": [],
            }
        }

    @classmethod
    def _provider_status(cls, provider: str, *, digest: str | None = None) -> dict:
        return {
            "api_version": "v1",
            "provider": provider,
            "topology_id": "topology-a-b",
            "absent": False,
            "observed_revision_id": cls.revision_id,
            "observed_content_sha256": cls.content_sha256,
            "observed_projection_sha256": digest
            or full_components._effective_projection_sha256([], []),
        }

    @classmethod
    def _scenario(cls) -> full_components.FullComponentsScenario:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.topology_id = "topology-a-b"
        scenario.h = types.SimpleNamespace(
            run_id="projection-convergence-test", evidence={}
        )
        scenario.control_client = mock.Mock()
        scenario._gateway_topology_projection_snapshot = mock.Mock(
            return_value=cls._projection_snapshot("gateway")
        )
        scenario._auth_topology_projection_snapshot = mock.Mock(
            return_value=cls._projection_snapshot("auth")
        )
        scenario._provider_topology_status = mock.Mock(
            side_effect=lambda provider: cls._provider_status(provider)
        )
        return scenario

    def test_waits_for_topology_transition_then_captures_one_stable_revision(
        self,
    ) -> None:
        scenario = self._scenario()
        scenario.control_client.request.side_effect = [
            self._topology_response(converged=False),
            self._topology_response(converged=True),
            self._topology_response(converged=True),
        ]

        evidence = scenario._capture_provider_projection_integrity(
            "worker-recovery", timeout=1, poll_interval=0
        )

        self.assertEqual(evidence["attempts"], 2)
        self.assertGreaterEqual(evidence["converged_after_ms"], 0)
        self.assertEqual(evidence["applied_revision_id"], self.revision_id)
        self.assertEqual(
            evidence["last_transitional_diagnostic"]["stage"],
            "topology-before",
        )
        self.assertEqual(
            evidence["last_transitional_diagnostic"]["status"]["drift_count"], 1
        )
        self.assertEqual(scenario.control_client.request.call_count, 3)
        self.assertNotIn("secret", json.dumps(evidence))

    def test_retries_the_whole_round_when_provider_digest_is_transitional(
        self,
    ) -> None:
        scenario = self._scenario()
        converged = self._topology_response(converged=True)
        scenario.control_client.request.side_effect = [
            converged,
            converged,
            converged,
        ]
        scenario._provider_topology_status.side_effect = [
            self._provider_status("gateway", digest="f" * 64),
            self._provider_status("gateway"),
            self._provider_status("auth"),
        ]

        evidence = scenario._capture_provider_projection_integrity(
            "binding-reconfigure", timeout=1, poll_interval=0
        )

        self.assertEqual(evidence["attempts"], 2)
        self.assertEqual(
            evidence["last_transitional_diagnostic"]["stage"],
            "provider-gateway",
        )
        self.assertTrue(evidence["all_match"])
        self.assertEqual(set(evidence["providers"]), {"gateway", "auth"})
        self.assertEqual(scenario.control_client.request.call_count, 3)

    def test_timeout_persists_only_bounded_secret_free_transition_diagnostic(
        self,
    ) -> None:
        scenario = self._scenario()
        scenario.control_client.request.return_value = self._topology_response(
            converged=False
        )

        with self.assertRaisesRegex(
            full_components.FullGateError,
            "did not converge after 1 attempts",
        ) as raised:
            scenario._capture_provider_projection_integrity(
                "worker-recovery", timeout=0, poll_interval=0
            )

        failure = scenario.h.evidence["provider_projection_integrity_failures"][
            "worker-recovery"
        ]
        self.assertEqual(failure["attempts"], 1)
        self.assertEqual(
            failure["last_transitional_diagnostic"]["stage"], "topology-before"
        )
        self.assertEqual(
            failure["last_transitional_diagnostic"]["status"]["drift_count"], 1
        )
        serialized = json.dumps(failure) + str(raised.exception)
        self.assertNotIn("must-not-leak", serialized)
        self.assertNotIn("x-secret", serialized)


class OperationTimeoutDiagnosticTests(unittest.TestCase):
    @staticmethod
    def _scenario() -> full_components.FullComponentsScenario:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.h = types.SimpleNamespace(
            evidence={"phase": "full.problem-artifact-gc"}, checkpoint=mock.Mock()
        )
        scenario.a = mock.Mock()
        scenario.control_client = mock.Mock()
        return scenario

    def test_job_readback_is_parameterized_read_only_and_excludes_credentials(self) -> None:
        scenario = self._scenario()
        operation_id = "op-topology-deadbeef"
        scenario.a.command.return_value = types.SimpleNamespace(
            stdout=json.dumps(
                {
                    "operation_id": operation_id,
                    "transaction_read_only": True,
                    "row_count": 1,
                    "items": [
                        {
                            "job_id": "job-finalize",
                            "operation_id": operation_id,
                            "node_id": "control-plane",
                            "kind": "topology_apply",
                            "status": "SUCCEEDED",
                            "payload_status": "SUCCEEDED",
                            "payload_status_matches_column": True,
                            "attempt": 1,
                            "max_attempts": 1,
                            "lease_expires_at_ms": None,
                            "lease_credential_present": True,
                            "completion_fingerprint_present": True,
                            "result_present": True,
                            "result_phase": "FINALIZE_GROUP",
                            "result_code": None,
                            "result_state": None,
                            "error_present": False,
                        }
                    ],
                }
            ),
            stderr="",
            returncode=0,
        )

        snapshot = scenario._operation_job_rows(operation_id, 10_000)

        self.assertEqual(snapshot["row_count"], 1)
        self.assertEqual(snapshot["items"][0]["status"], "SUCCEEDED")
        self.assertFalse(snapshot["items"][0]["lease_expired_at_capture"])
        self.assertFalse(snapshot["lease_credentials_recorded"])
        argv = scenario.a.command.call_args.args
        self.assertIn(
            "PGOPTIONS=-cojos.evidence_operation_id=" + operation_id, argv
        )
        query = argv[argv.index("-c") + 1]
        self.assertIn("BEGIN READ ONLY", query)
        self.assertIn("ROLLBACK", query)
        self.assertNotIn("INSERT ", query)
        self.assertNotIn("UPDATE ", query)
        self.assertNotIn("DELETE ", query)
        self.assertNotIn("lease_token", json.dumps(snapshot))

    def test_operation_logs_follow_cursor_and_redact_credentials(self) -> None:
        scenario = self._scenario()
        scenario.control_client.request.side_effect = [
            (
                {
                    "data": {
                        "items": [
                            {
                                "message": "first Bearer abc.def.ghi",
                                "lease_token": "do-not-record",
                            }
                        ],
                        "next_cursor": "cursor-1",
                    }
                },
                {},
                200,
            ),
            (
                {
                    "data": {
                        "items": [{"message": "second"}],
                        "next_cursor": None,
                    }
                },
                {},
                200,
            ),
        ]

        logs = scenario._operation_log_records("op-topology-deadbeef")

        self.assertEqual(logs["page_count"], 2)
        self.assertEqual(logs["item_count"], 2)
        self.assertFalse(logs["truncated"])
        self.assertTrue(logs["items"][0]["lease_token_redacted"])
        serialized = json.dumps(logs)
        self.assertNotIn("do-not-record", serialized)
        self.assertNotIn("Bearer abc.def.ghi", serialized)
        second_path = scenario.control_client.request.call_args_list[1].args[1]
        self.assertIn("cursor=cursor-1", second_path)

    def test_orchestrator_log_window_keeps_early_and_late_context(self) -> None:
        scenario = self._scenario()
        early = "revision conflict op-topology-deadbeef Bearer leaked-token\n"
        middle = "x" * (full_components.OPERATION_TIMEOUT_ORCHESTRATOR_LOG_MAX_CHARS + 100)
        late = "\nlate job-finalize context"
        scenario.a.command.return_value = types.SimpleNamespace(
            stdout=early + middle + late,
            stderr="",
            returncode=0,
        )
        latest = {
            "created_at_ms": 10_000,
            "job_bindings": [{"job_id": "job-finalize"}],
        }

        logs = scenario._operation_orchestrator_log_window(
            "op-topology-deadbeef", latest
        )

        self.assertTrue(logs["window_truncated"])
        self.assertIn("revision conflict", logs["window"])
        self.assertIn("late job-finalize context", logs["window"])
        self.assertNotIn("Bearer leaked-token", logs["window"])
        self.assertIn("revision conflict", logs["correlated_lines"])
        self.assertIn("job-finalize", logs["correlated_lines"])
        argv = scenario.a.command.call_args.args
        self.assertEqual(argv[0], "logs")
        self.assertIn("--since", argv)
        self.assertIn("--timestamps", argv)
        self.assertEqual(
            argv[argv.index("--tail") + 1],
            str(full_components.OPERATION_TIMEOUT_ORCHESTRATOR_LOG_TAIL_LINES),
        )
        self.assertEqual(argv[-1], "orchestrator-a")

    def test_wait_timeout_persists_all_diagnostics_and_keeps_error_concise(self) -> None:
        scenario = self._scenario()
        latest = {
            "operation_id": "op-topology-deadbeef",
            "status": "RUNNING",
            "revision": 13,
            "planned_jobs": [{"large": "x" * 20_000}],
        }
        scenario._control_get = mock.Mock(
            return_value={"data": {"operation": latest}}
        )
        scenario._operation_job_rows = mock.Mock(return_value={"row_count": 6})
        scenario._operation_log_records = mock.Mock(return_value={"item_count": 0})
        scenario._operation_orchestrator_log_window = mock.Mock(
            return_value={"window": "revision conflict"}
        )

        with mock.patch.object(
            full_components.time, "monotonic", side_effect=[0.0, 0.0, 2.0]
        ), mock.patch.object(full_components.time, "sleep", return_value=None):
            with self.assertRaisesRegex(
                full_components.FullGateError,
                r"status='RUNNING' revision=13; see evidence\.operation_timeout_diagnostic",
            ) as raised:
                scenario._wait_operation("op-topology-deadbeef", 1)

        self.assertLess(len(str(raised.exception)), 300)
        diagnostic = scenario.h.evidence["operation_timeout_diagnostic"]
        self.assertEqual(diagnostic["job_rows"], {"row_count": 6})
        self.assertEqual(diagnostic["operation_logs"], {"item_count": 0})
        self.assertEqual(
            diagnostic["orchestrator_logs"], {"window": "revision conflict"}
        )
        scenario.h.checkpoint.assert_called_once_with("full.problem-artifact-gc")

    def test_diagnostic_failure_does_not_replace_operation_timeout(self) -> None:
        scenario = self._scenario()
        scenario._operation_job_rows = mock.Mock(side_effect=RuntimeError("postgres down"))
        scenario._operation_log_records = mock.Mock(return_value={"item_count": 2})
        scenario._operation_orchestrator_log_window = mock.Mock(
            return_value={"window": "worker error"}
        )

        diagnostic = scenario._capture_operation_timeout_diagnostics(
            "op-topology-deadbeef", {"status": "RUNNING"}, 300
        )

        self.assertNotIn("job_rows", diagnostic)
        self.assertEqual(diagnostic["operation_logs"]["item_count"], 2)
        self.assertEqual(diagnostic["errors"][0]["source"], "job_rows")
        self.assertIn("postgres down", diagnostic["errors"][0]["error"])


class FullComponentGeneratedConfigTests(unittest.TestCase):
    def test_gateway_and_storage_disable_buffering_timeout(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.h = types.SimpleNamespace(a_ip="10.88.0.10")

        gateway = scenario._gateway_config()
        storage = scenario._storage_config()

        for name, generated in (("gateway", gateway), ("storage", storage)):
            with self.subTest(name=name):
                self.assertIn("Timeout: 600000\n", generated)
                self.assertIn(
                    "Middlewares:\n  Timeout: false\n  Recover: false\n", generated
                )
        self.assertEqual(gateway.count("TimeoutMS: 30000"), 3)

    def test_judge_claim_timeout_is_route_scoped(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        generated = scenario._judge_config()

        self.assertIn("Timeout: 3000\n", generated)
        self.assertNotIn("Timeout: 35000\n", generated)


class FullComponentCommandArgumentTests(unittest.TestCase):
    def test_failed_worker_database_readback_is_parameterized_and_read_only(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.a = mock.Mock()
        scenario.a.command.return_value = types.SimpleNamespace(
            stdout=(
                '{"runtime_instance_count":0,"binding_count":0,'
                '"active_or_staged_binding_count":0}\n'
            )
        )
        deployment_id = "deployment-judge-worker-0123456789abcdef"

        result = scenario._failed_worker_database_counts(deployment_id)

        self.assertEqual(result["runtime_instance_count"], 0)
        self.assertTrue(result["control_plane_database_read_only_verification"])
        argv = scenario.a.command.call_args.args
        self.assertEqual(argv[:2], ("exec", "--env"))
        self.assertIn(
            "PGOPTIONS=-cojos.failed_deployment_id=" + deployment_id, argv
        )
        query = argv[argv.index("-c") + 1]
        self.assertIn("BEGIN READ ONLY", query)
        self.assertIn("current_setting('ojos.failed_deployment_id')", query)
        self.assertIn("ROLLBACK", query)
        self.assertNotIn(deployment_id, query)

    def test_failed_worker_database_readback_rejects_unsafe_identifier(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.a = mock.Mock()
        with self.assertRaisesRegex(full_components.FullGateError, "not safe"):
            scenario._failed_worker_database_counts("deployment-worker';DELETE")
        scenario.a.command.assert_not_called()

    def test_problem_gc_trigger_sql_is_a_nonempty_single_line_argument(self) -> None:
        sql = full_components._single_line_sql(
            """
            CREATE FUNCTION example() RETURNS trigger LANGUAGE plpgsql AS $body$
            BEGIN
              RAISE EXCEPTION 'rollback proof';
            END
            $body$;
            """
        )

        self.assertTrue(sql)
        self.assertNotIn("\n", sql)
        self.assertNotIn("\r", sql)
        self.assertIn("RAISE EXCEPTION 'rollback proof'", sql)

    def test_empty_inline_sql_is_rejected_before_command_execution(self) -> None:
        with self.assertRaisesRegex(full_components.FullGateError, "must not be empty"):
            full_components._single_line_sql("\r\n  \n")

    def test_control_plane_run_uses_the_tls_docker_healthcheck_environment(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.a = mock.Mock()
        scenario.a_network = "engine-a-network"
        scenario.secure_fixture_image = "fixture-image"
        scenario.images = {
            "catalog": "catalog-image",
            "orchestrator": "orchestrator-image",
        }
        scenario.postgres_password = "postgres-password"
        scenario.catalog_trust = {}
        scenario.catalog_sources = []
        scenario.internal_token = "internal-token"
        scenario.auth_internal_token = "auth-token"
        scenario.workload_issuer_token = "workload-token"
        scenario.commit = "1" * 40
        scenario.ca_cert = Path("unused-test-ca.pem")
        scenario.h = mock.Mock()
        scenario.h.a_ip = "192.0.2.10"
        scenario.h.evidence = {"engines": {"a": {"engine_id": "engine-a"}}}
        scenario._outer_port = mock.Mock(side_effect=(18090, 18443))
        scenario._wait_json = mock.Mock(
            side_effect=(
                {"data": {"build": {"commit_sha": scenario.commit, "profile": "production"}}},
                {},
            )
        )
        scenario._wait_control_plane_container_healthy = mock.Mock()

        with mock.patch.object(
            full_components, "JsonClient", side_effect=(mock.Mock(), mock.Mock())
        ):
            scenario._start_identity_catalog_and_orchestrator()

        orchestrator_argv = next(
            call.args
            for call in scenario.a.command.call_args_list
            if len(call.args) > 3
            and call.args[:3] == ("run", "-d", "--name")
            and call.args[3] == "orchestrator-a"
        )
        environment = {
            str(orchestrator_argv[index + 1]).split("=", 1)[0]: str(
                orchestrator_argv[index + 1]
            ).split("=", 1)[1]
            for index, argument in enumerate(orchestrator_argv[:-1])
            if argument == "--env"
        }
        self.assertEqual(
            environment["ORCHESTRATOR_HEALTHCHECK_URL"],
            full_components.CONTROL_PLANE_HEALTHCHECK_URL,
        )
        self.assertEqual(
            environment["ORCHESTRATOR_HEALTHCHECK_CA_CERT"],
            full_components.CONTROL_PLANE_HEALTHCHECK_CA_CERT,
        )
        scenario._wait_control_plane_container_healthy.assert_called_once_with(
            timeout=90
        )

    def test_control_plane_health_wait_records_only_sanitized_inspect_evidence(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.a = mock.Mock()
        scenario.h = mock.Mock()
        scenario.h.evidence = {"engines": {"a": {"engine_id": "engine-a"}}}
        scenario.a.command.return_value.stdout = json.dumps(
            [
                {
                    "Id": "a" * 64,
                    "State": {"Running": True, "Health": {"Status": "healthy"}},
                    "Config": {
                        "Env": [
                            "ORCHESTRATOR_TLS_CERT=/opt/ojos-pki/server.pem",
                            "ORCHESTRATOR_TLS_KEY=/opt/ojos-pki/server.key",
                            "ORCHESTRATOR_HEALTHCHECK_URL="
                            + full_components.CONTROL_PLANE_HEALTHCHECK_URL,
                            "ORCHESTRATOR_HEALTHCHECK_CA_CERT="
                            + full_components.CONTROL_PLANE_HEALTHCHECK_CA_CERT,
                            "ORCHESTRATOR_INTERNAL_TOKEN=must-not-be-recorded",
                        ]
                    },
                }
            ]
        )

        evidence = scenario._wait_control_plane_container_healthy(timeout=1)

        self.assertEqual(evidence, scenario.h.evidence["control_plane_runtime"])
        self.assertEqual(evidence["docker_health"], "HEALTHY")
        self.assertEqual(evidence["engine_id"], "engine-a")
        self.assertNotIn("must-not-be-recorded", json.dumps(evidence))


class TLSProxyFramingTests(unittest.TestCase):
    @staticmethod
    def _serve(handler):
        server = fixture_service.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        return server, thread

    @staticmethod
    def _stop(server, thread) -> None:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)

    def test_unknown_length_large_body_is_reencoded_as_complete_chunked_response(self) -> None:
        body = bytes(range(251)) * 700

        class ChunkedUpstream(fixture_service.QuietHandler):
            protocol_version = "HTTP/1.1"

            def do_GET(self) -> None:  # noqa: N802
                self.send_response(200)
                self.send_header("content-type", "application/octet-stream")
                self.send_header("transfer-encoding", "chunked")
                self.end_headers()
                for offset in range(0, len(body), 997):
                    chunk = body[offset : offset + 997]
                    self.wfile.write(f"{len(chunk):X}\r\n".encode("ascii"))
                    self.wfile.write(chunk)
                    self.wfile.write(b"\r\n")
                    self.wfile.flush()
                self.wfile.write(b"0\r\n\r\n")
                self.wfile.flush()

        upstream, upstream_thread = self._serve(ChunkedUpstream)

        class Proxy(fixture_service.TLSProxyHandler):
            capture_paths = ("/large",)
            capture_lock = threading.Lock()
            captures = []
            capture_sequence = 0

        Proxy.upstream = f"http://127.0.0.1:{upstream.server_address[1]}"
        proxy, proxy_thread = self._serve(Proxy)
        try:
            with socket.create_connection(proxy.server_address, timeout=2) as client:
                client.settimeout(2)
                client.sendall(
                    b"GET /large HTTP/1.1\r\n"
                    b"Host: fixture.test\r\n"
                    b"Connection: close\r\n\r\n"
                )
                wire = bytearray()
                while True:
                    part = client.recv(64 * 1024)
                    if not part:
                        break
                    wire.extend(part)

            response_headers, encoded_body = bytes(wire).split(b"\r\n\r\n", 1)
            lower_headers = response_headers.lower()
            self.assertIn(b"transfer-encoding: chunked", lower_headers)
            self.assertNotIn(b"content-length:", lower_headers)
            self.assertNotIn(b"connection: close", lower_headers)

            decoded_body = bytearray()
            chunk_sizes = []
            position = 0
            while True:
                line_end = encoded_body.index(b"\r\n", position)
                chunk_size = int(encoded_body[position:line_end], 16)
                position = line_end + 2
                if chunk_size == 0:
                    self.assertEqual(encoded_body[position:], b"\r\n")
                    break
                chunk_sizes.append(chunk_size)
                decoded_body.extend(encoded_body[position : position + chunk_size])
                position += chunk_size
                self.assertEqual(encoded_body[position : position + 2], b"\r\n")
                position += 2

            self.assertGreater(len(body), 2 * 1024)
            self.assertGreaterEqual(len(chunk_sizes), 2)
            self.assertEqual(bytes(decoded_body), body)
            self.assertTrue(encoded_body.endswith(b"0\r\n\r\n"))

            self.assertEqual(len(Proxy.captures), 1)
            capture = Proxy.captures[0]
            expected_sha = "sha256:" + hashlib.sha256(body).hexdigest()
            self.assertEqual(capture["response_size_bytes"], len(body))
            self.assertEqual(capture["response_sha256"], expected_sha)
            self.assertEqual(capture["body"]["non_json_sha256"], expected_sha)
            self.assertEqual(capture["body"]["size_bytes"], len(body))
        finally:
            self._stop(proxy, proxy_thread)
            self._stop(upstream, upstream_thread)

    def test_head_without_content_length_does_not_advertise_chunked_body(self) -> None:
        class HeadUpstream(fixture_service.QuietHandler):
            protocol_version = "HTTP/1.1"

            def do_HEAD(self) -> None:  # noqa: N802
                self.send_response(200)
                self.end_headers()

        upstream, upstream_thread = self._serve(HeadUpstream)

        class Proxy(fixture_service.TLSProxyHandler):
            captures = []

        Proxy.upstream = f"http://127.0.0.1:{upstream.server_address[1]}"
        proxy, proxy_thread = self._serve(Proxy)
        try:
            connection = http.client.HTTPConnection(
                "127.0.0.1", proxy.server_address[1], timeout=2
            )
            connection.request("HEAD", "/head")
            response = connection.getresponse()
            self.assertEqual(response.status, 200)
            self.assertIsNone(response.getheader("transfer-encoding"))
            self.assertEqual(response.read(), b"")
            connection.close()
        finally:
            self._stop(proxy, proxy_thread)
            self._stop(upstream, upstream_thread)


class GatewayBindingPathTests(unittest.TestCase):
    @staticmethod
    def _serve(handler):
        server = fixture_service.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        return server, thread

    @staticmethod
    def _stop(server, thread) -> None:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)

    def test_binding_base_maps_to_provider_path_without_duplication(self) -> None:
        release = json.loads(
            (
                FIXTURE_MODULE_PATH.parent
                / "contracts"
                / "echo-provider.release.json"
            ).read_text(encoding="utf-8")
        )
        provider_path = release["provides"]["apis"][0]["path"]
        self.assertEqual(provider_path, "/echo")

        provider, provider_thread = self._serve(fixture_service.ProviderHandler)

        class Gateway(fixture_service.GatewayHandler):
            workload_token = "fixture-binding-token"

        Gateway.routes = {
            "/internal/apis/fixture.contract.echo": {
                "kind": "proxy",
                "upstream": (
                    f"http://127.0.0.1:{provider.server_address[1]}{provider_path}"
                ),
                "consumer_service": "contract-echo-consumer",
            }
        }
        gateway, gateway_thread = self._serve(Gateway)
        try:
            context = {
                "gateway": {
                    "origin": f"http://127.0.0.1:{gateway.server_address[1]}"
                },
                "bindings": {
                    "echo": {"base_path": "/internal/apis/fixture.contract.echo"}
                },
            }
            request = fixture_service.urllib.request.Request(
                fixture_service.binding_url(context, "echo"),
                headers={"authorization": "Bearer fixture-binding-token"},
            )
            opener = fixture_service.urllib.request.build_opener(
                fixture_service.urllib.request.ProxyHandler({})
            )
            with opener.open(request, timeout=2) as response:
                payload = json.loads(response.read())

            self.assertEqual(payload["path"], "/echo")
            self.assertNotEqual(payload["path"], "/echo/echo")
        finally:
            self._stop(gateway, gateway_thread)
            self._stop(provider, provider_thread)


class BindingHeadProbeTests(unittest.TestCase):
    class _Response:
        def __init__(self, status: int, headers: dict[str, str]) -> None:
            self.status = status
            self.headers = headers

        def __enter__(self):
            return self

        def __exit__(self, *_args) -> None:
            return None

    @staticmethod
    def _context() -> dict:
        return {
            "schema_version": 1,
            "deployment": {
                "id": "deployment-problem",
                "service": "problem-service",
                "node": "node-a",
            },
            "gateway": {
                "origin": "https://gateway-a:8443",
                "ca_file": "/run/ojos/service/ca.pem",
            },
            "bindings": {
                "storage_head": {
                    "binding_id": "binding-storage-head",
                    "api_id": "storage.object.head",
                    "base_path": "/internal/apis/storage.object.head",
                    "timeout_ms": 300000,
                }
            },
            "credential_file": "/run/ojos/service/token",
            "generation": 3,
        }

    def _context_file(self, directory: str) -> Path:
        path = Path(directory) / "context.json"
        path.write_text(json.dumps(self._context()), encoding="utf-8")
        return path

    def test_fixture_head_uses_named_binding_and_returns_only_response_metadata(self) -> None:
        captured: dict[str, object] = {}

        def open_request(request, *, timeout):
            captured.update(request=request, timeout=timeout)
            return self._Response(
                200,
                {
                    "X-OJOS-Object-Sha256": "a" * 64,
                    "Content-Length": "321",
                    "X-OJOS-Storage-Result": "present",
                },
            )

        tls = object()
        opener = mock.Mock()
        opener.open.side_effect = open_request
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            fixture_service,
            "load_context",
            return_value=(self._context(), "deployment.jwt.value", tls),
        ), mock.patch.object(
            fixture_service.urllib.request, "build_opener", return_value=opener
        ) as build_opener:
            result = fixture_service.binding_head(
                self._context_file(directory),
                "storage_head",
                "/problems/package.zip",
                "deployment-problem",
            )

        request = captured["request"]
        self.assertEqual(request.get_method(), "HEAD")
        self.assertEqual(
            request.full_url,
            "https://gateway-a:8443/internal/apis/storage.object.head/problems/package.zip",
        )
        self.assertEqual(request.get_header("Authorization"), "Bearer deployment.jwt.value")
        self.assertEqual(captured["timeout"], fixture_service.BINDING_HEAD_TIMEOUT_SECONDS)
        proxy_handler, redirect_handler, https_handler = build_opener.call_args.args
        self.assertEqual(proxy_handler.proxies, {})
        self.assertIsInstance(redirect_handler, fixture_service.NoRedirectHandler)
        self.assertIs(https_handler._context, tls)
        self.assertEqual(
            result,
            {
                "status": 200,
                "sha256_header": "a" * 64,
                "size_bytes": 321,
                "storage_result_header": "present",
            },
        )
        self.assertNotIn("deployment.jwt.value", json.dumps(result))

    def test_fixture_head_does_not_follow_redirects_with_the_workload_token(self) -> None:
        redirected = fixture_service.NoRedirectHandler().redirect_request(
            fixture_service.urllib.request.Request(
                "https://gateway-a:8443/internal/apis/storage.object.head/problems/key",
                method="HEAD",
                headers={"authorization": "Bearer deployment.jwt.value"},
            ),
            None,
            302,
            "found",
            {"Location": "https://attacker.invalid/collect"},
            "https://attacker.invalid/collect",
        )
        self.assertIsNone(redirected)

        error = fixture_service.urllib.error.HTTPError(
            "https://gateway-a:8443/internal/apis/storage.object.head/problems/key",
            302,
            "found",
            {"Location": "https://attacker.invalid/collect"},
            fixture_service.io.BytesIO(),
        )
        opener = mock.Mock()
        opener.open.side_effect = error
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            fixture_service,
            "load_context",
            return_value=(self._context(), "deployment.jwt.value", object()),
        ), mock.patch.object(
            fixture_service.urllib.request, "build_opener", return_value=opener
        ):
            result = fixture_service.binding_head(
                self._context_file(directory),
                "storage_head",
                "/problems/key",
                "deployment-problem",
            )

        self.assertEqual(result["status"], 302)
        self.assertNotIn("deployment.jwt.value", json.dumps(result))
        opener.open.assert_called_once()

    def test_fixture_head_returns_bounded_404_metadata_without_body_or_token(self) -> None:
        error = fixture_service.urllib.error.HTTPError(
            "https://gateway-a:8443/internal/apis/storage.object.head/problems/missing",
            404,
            "not found",
            {},
            fixture_service.io.BytesIO(),
        )
        opener = mock.Mock()
        opener.open.side_effect = error
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            fixture_service,
            "load_context",
            return_value=(self._context(), "deployment.jwt.value", object()),
        ), mock.patch.object(
            fixture_service.urllib.request, "build_opener", return_value=opener
        ):
            result = fixture_service.binding_head(
                self._context_file(directory),
                "storage_head",
                "/problems/missing",
                "deployment-problem",
            )

        self.assertEqual(
            result,
            {
                "status": 404,
                "sha256_header": "",
                "size_bytes": -1,
                "storage_result_header": "",
            },
        )
        self.assertNotIn("deployment.jwt.value", json.dumps(result))

    def test_fixture_head_preserves_authoritative_provider_not_found_result(self) -> None:
        error = fixture_service.urllib.error.HTTPError(
            "https://gateway-a:8443/internal/apis/storage.object.head/problems/missing",
            404,
            "not found",
            {"X-OJOS-Storage-Result": "object-not-found"},
            fixture_service.io.BytesIO(),
        )
        opener = mock.Mock()
        opener.open.side_effect = error
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            fixture_service,
            "load_context",
            return_value=(self._context(), "deployment.jwt.value", object()),
        ), mock.patch.object(
            fixture_service.urllib.request, "build_opener", return_value=opener
        ):
            result = fixture_service.binding_head(
                self._context_file(directory),
                "storage_head",
                "/problems/missing",
                "deployment-problem",
            )

        self.assertEqual(result["status"], 404)
        self.assertEqual(result["storage_result_header"], "object-not-found")
        self.assertNotIn("deployment.jwt.value", json.dumps(result))

    def test_fixture_head_rejects_absolute_or_traversing_resource_paths(self) -> None:
        for relative_path in (
            "https://storage-a/api/storage/objects/problems/key",
            "/problems/../secrets",
            "/problems/%2e%2e/secrets",
            "/problems/%252e%252e/secrets",
            "//storage-a/problems/key",
        ):
            with self.subTest(relative_path=relative_path), self.assertRaisesRegex(
                ValueError, "relative path"
            ):
                fixture_service.binding_head(
                    Path("/run/ojos/service/context.json"),
                    "storage_head",
                    relative_path,
                    "deployment-problem",
                )

    def test_fixture_head_rejects_untrusted_context_identity_and_paths(self) -> None:
        mutations = (
            ("deployment", "id", "another-deployment"),
            ("gateway", "origin", "http://gateway-a:8080"),
            ("gateway", "ca_file", "/tmp/other-ca.pem"),
            (None, "credential_file", "/tmp/token"),
            ("bindings", "storage_head", {}),
        )
        for section, field, replacement in mutations:
            context = copy.deepcopy(self._context())
            if section == "bindings":
                context[section][field] = replacement
            elif section is None:
                context[field] = replacement
            else:
                context[section][field] = replacement
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "context.json"
                path.write_text(json.dumps(context), encoding="utf-8")
                with self.subTest(section=section, field=field), mock.patch.object(
                    fixture_service, "load_context"
                ) as load_context, self.assertRaisesRegex(
                    ValueError, "Service Context|ApiBinding"
                ):
                    fixture_service.binding_head(
                        path,
                        "storage_head",
                        "/problems/package.zip",
                        "deployment-problem",
                    )
                load_context.assert_not_called()

    def test_full_harness_mounts_the_inspected_context_read_only_for_head(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.a = mock.Mock()
        scenario.fixture_image = "plain-fixture"
        scenario.a.command.return_value = types.SimpleNamespace(
            stdout=json.dumps(
                {
                    "status": 200,
                    "sha256_header": "b" * 64,
                    "size_bytes": 42,
                    "storage_result_header": "present",
                }
            )
            + "\n"
        )

        deployment_id = "deployment-problem"
        component = fixture_service.hashlib.sha256(deployment_id.encode()).hexdigest()[:32]
        context_source = f"/var/lib/ojos-agent-a/runtime-contexts/{component}/service"
        result = scenario._binding_head_probe(
            context_source,
            "/problems/package.zip",
            deployment_id,
            "c" * 64,
        )

        self.assertEqual(result["status"], 200)
        argv = scenario.a.command.call_args.args
        self.assertIn(
            "type=bind,source=" + context_source + ",target=/run/ojos/service,readonly",
            argv,
        )
        self.assertIn("container:" + "c" * 64, argv)
        self.assertIn("plain-fixture", argv)
        self.assertIn("binding-head", argv)
        self.assertIn("storage_head", argv)
        self.assertNotIn("8085", " ".join(map(str, argv)))
        self.assertNotIn("deployment.jwt", " ".join(map(str, argv)))
        self.assertEqual(scenario.a.command.call_args.kwargs["timeout"], 30)

    def test_full_harness_resolves_only_a_read_only_service_context_bind(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.a = mock.Mock()
        deployment_id = "deployment-problem"
        component = fixture_service.hashlib.sha256(deployment_id.encode()).hexdigest()[:32]
        expected_source = f"/var/lib/ojos-agent-a/runtime-contexts/{component}/service"
        scenario.a.command.side_effect = (
            types.SimpleNamespace(stdout="c" * 64 + "\n"),
            types.SimpleNamespace(
                stdout=json.dumps(
                    [
                        {
                            "Mounts": [
                                {
                                    "Type": "bind",
                                    "Source": expected_source,
                                    "Destination": "/run/ojos/service",
                                    "RW": False,
                                }
                            ]
                        }
                    ]
                )
            ),
        )

        source, container_id = scenario._managed_service_context_source(
            "problem-service", deployment_id
        )

        self.assertEqual(source, expected_source)
        self.assertEqual(container_id, "c" * 64)

    def test_full_harness_rejects_an_arbitrary_host_context_source(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.a = mock.Mock()
        scenario.a.command.side_effect = (
            types.SimpleNamespace(stdout="c" * 64 + "\n"),
            types.SimpleNamespace(
                stdout=json.dumps(
                    [
                        {
                            "Mounts": [
                                {
                                    "Type": "bind",
                                    "Source": "/tmp/attacker-controlled",
                                    "Destination": "/run/ojos/service",
                                    "RW": False,
                                }
                            ]
                        }
                    ]
                )
            ),
        )

        with self.assertRaisesRegex(full_components.FullGateError, "Service Context"):
            scenario._managed_service_context_source(
                "problem-service", "deployment-problem"
            )


class LiveGateFailureIntegrityTests(unittest.TestCase):
    def test_diagnostic_error_collection_is_count_and_size_bounded(self) -> None:
        errors = []
        for index in range(100):
            gate.append_bounded_diagnostic_error(errors, f"operation-{index}", "x" * 10_000)

        self.assertEqual(len(errors), gate.MAX_DIAGNOSTIC_ERRORS)
        self.assertLessEqual(len(errors[0]["error"]), gate.MAX_DIAGNOSTIC_ERROR_CHARS)
        self.assertEqual(errors[-1]["operation"], "diagnostic-error-limit")
        self.assertEqual(errors[-1]["omitted"], 69)

    def test_diagnostic_and_cleanup_timeouts_preserve_original_failure(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence_path = Path(directory) / "evidence.json"
            live = gate.LiveGate(Path(directory), evidence_path, full_components=False)
            live.checkpoint = mock.Mock()
            live.ensure_live_prerequisites = mock.Mock(
                side_effect=gate.GateError("original scenario failure")
            )
            nested_engine = mock.Mock()
            nested_engine.command.side_effect = gate.GateError("diagnostic command timed out")
            live.a = nested_engine
            live.root.command = mock.Mock(side_effect=gate.GateError("cleanup command timed out"))

            with self.assertRaisesRegex(gate.GateError, "original scenario failure"):
                live.run()

            value = json.loads(evidence_path.read_text(encoding="utf-8"))
            self.assertEqual(value["status"], "FAILED")
            self.assertEqual(value["failure"], "original scenario failure")
            self.assertTrue(value["failure_log_errors"])
            self.assertIn("diagnostic command timed out", value["failure_log_errors"][0]["error"])
            self.assertEqual(len(value["cleanup_errors"]), 3)
            self.assertTrue(
                all("cleanup command timed out" in item["error"] for item in value["cleanup_errors"])
            )
            self.assertFalse(evidence_path.with_name("evidence.json.tmp").exists())

    def test_failure_log_timeout_does_not_prevent_collecting_other_logs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            live = gate.LiveGate(Path(directory), Path(directory) / "evidence.json", False)
            nested_engine = mock.Mock()
            nested_engine.command.side_effect = [
                gate.Completed(["docker", "ps"], 0, "timed-out\nhealthy\n", ""),
                gate.GateError("logs timed out"),
                gate.Completed(["docker", "logs", "healthy"], 0, "useful log", ""),
            ]
            live.a = nested_engine

            live._collect_failure_logs()

            self.assertEqual(live.evidence["failure_logs"]["engine_a/healthy"], "useful log")
            self.assertEqual(
                live.evidence["failure_log_errors"][0]["operation"], "engine_a/timed-out"
            )
            self.assertIn("logs timed out", live.evidence["failure_log_errors"][0]["error"])

    def test_failure_log_attempts_remain_bounded_when_every_log_times_out(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            live = gate.LiveGate(Path(directory), Path(directory) / "evidence.json", False)
            nested_engine = mock.Mock()

            def command(*args, **_kwargs):
                if args[0] == "ps":
                    names = "\n".join(f"container-{index}" for index in range(100))
                    return gate.Completed(["docker", "ps"], 0, names, "")
                raise gate.GateError("logs timed out")

            nested_engine.command.side_effect = command
            live.a = nested_engine

            live._collect_failure_logs()

            self.assertEqual(nested_engine.command.call_count, gate.MAX_FAILURE_LOG_ENTRIES + 1)
            self.assertEqual(len(live.evidence["failure_log_errors"]), gate.MAX_DIAGNOSTIC_ERRORS)
            self.assertTrue(live.evidence["failure_logs_truncated"])

    def test_successful_scenario_with_cleanup_timeout_is_failed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence_path = Path(directory) / "evidence.json"
            live = gate.LiveGate(Path(directory), evidence_path, full_components=False)
            live.checkpoint = mock.Mock()
            live.ensure_live_prerequisites = mock.Mock(return_value={})
            live._start_engines = mock.Mock()
            live._run_scenario = mock.Mock()
            live.root.command = mock.Mock(side_effect=gate.GateError("cleanup timed out"))

            with mock.patch.object(gate, "validate_repository", return_value={}), mock.patch.object(
                gate, "verify_evidence"
            ):
                with self.assertRaisesRegex(gate.GateError, "cleanup failed"):
                    live.run()

            value = json.loads(evidence_path.read_text(encoding="utf-8"))
            self.assertEqual(value["status"], "FAILED")
            self.assertEqual(value["failure"], "cleanup failed after the live scenario succeeded")
            self.assertEqual(len(value["cleanup_errors"]), 3)
            self.assertFalse(evidence_path.with_name("evidence.json.tmp").exists())

    def test_successful_scenario_with_nonzero_cleanup_is_failed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence_path = Path(directory) / "evidence.json"
            live = gate.LiveGate(Path(directory), evidence_path, full_components=False)
            live.checkpoint = mock.Mock()
            live.ensure_live_prerequisites = mock.Mock(return_value={})
            live._start_engines = mock.Mock()
            live._run_scenario = mock.Mock()
            live.root.command = mock.Mock(
                return_value=gate.Completed(["docker", "rm"], 1, "", "daemon unavailable")
            )

            with mock.patch.object(gate, "validate_repository", return_value={}), mock.patch.object(
                gate, "verify_evidence"
            ):
                with self.assertRaisesRegex(gate.GateError, "cleanup failed"):
                    live.run()

            value = json.loads(evidence_path.read_text(encoding="utf-8"))
            self.assertEqual(value["status"], "FAILED")
            self.assertEqual(len(value["cleanup_errors"]), 3)
            self.assertTrue(all("daemon unavailable" in item["error"] for item in value["cleanup_errors"]))

    def test_cleanup_treats_already_absent_resources_as_success(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            live = gate.LiveGate(Path(directory), Path(directory) / "evidence.json", False)
            live.root.command = mock.Mock(
                side_effect=[
                    gate.Completed(["docker", "rm"], 1, "", "No such container: a"),
                    gate.Completed(["docker", "rm"], 1, "", "No such container: b"),
                    gate.Completed(
                        ["docker", "network", "rm"],
                        1,
                        "",
                        f"network {live.outer_network} not found",
                    ),
                ]
            )

            self.assertEqual(live.cleanup(), [])

    def test_cleanup_reconciles_timed_out_container_that_is_already_absent(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            live = gate.LiveGate(Path(directory), Path(directory) / "evidence.json", False)
            live.b_name = "outside-run-scope"
            live.outer_network = "outside-run-scope"
            live.root.command = mock.Mock(
                side_effect=[
                    gate.GateError("container removal timed out"),
                    gate.Completed(
                        ["docker", "container", "inspect"],
                        1,
                        "",
                        f"Error: No such container: {live.a_name}",
                    ),
                ]
            )

            self.assertEqual(live.cleanup(), [])
            self.assertEqual(
                live.root.command.call_args_list,
                [
                    mock.call(
                        "rm",
                        "--force",
                        "--volumes",
                        live.a_name,
                        timeout=gate.DIND_CONTAINER_REMOVE_TIMEOUT_SECONDS,
                        check=False,
                    ),
                    mock.call(
                        "container",
                        "inspect",
                        live.a_name,
                        timeout=gate.CLEANUP_RECONCILE_INSPECT_TIMEOUT_SECONDS,
                        check=False,
                    ),
                ],
            )

    def test_cleanup_reports_timed_out_container_that_still_exists(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            live = gate.LiveGate(Path(directory), Path(directory) / "evidence.json", False)
            live.b_name = "outside-run-scope"
            live.outer_network = "outside-run-scope"
            present = gate.Completed(
                ["docker", "container", "inspect"], 0, live.a_name, ""
            )
            live.root.command = mock.Mock(
                side_effect=[
                    gate.GateError("container removal timed out"),
                    present,
                    gate.GateError("container removal retry timed out"),
                    present,
                ]
            )

            errors = live.cleanup()

            self.assertEqual(len(errors), 1)
            self.assertEqual(errors[0]["operation"], f"remove-container/{live.a_name}")
            self.assertIn("container removal timed out", errors[0]["error"])
            self.assertIn("container removal retry timed out", errors[0]["error"])
            self.assertIn("still exists", errors[0]["error"])
            self.assertEqual(
                live.root.command.call_args_list[2],
                mock.call(
                    "rm",
                    "--force",
                    "--volumes",
                    live.a_name,
                    timeout=gate.DIND_CONTAINER_REMOVE_RETRY_TIMEOUT_SECONDS,
                    check=False,
                ),
            )

    def test_cleanup_reports_timeout_when_exact_inspection_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            live = gate.LiveGate(Path(directory), Path(directory) / "evidence.json", False)
            live.b_name = "outside-run-scope"
            live.outer_network = "outside-run-scope"
            live.root.command = mock.Mock(
                side_effect=[
                    gate.GateError("container removal timed out"),
                    gate.GateError("container inspection timed out"),
                ]
            )

            errors = live.cleanup()

            self.assertEqual(len(errors), 1)
            self.assertEqual(errors[0]["operation"], f"remove-container/{live.a_name}")
            self.assertIn("container removal timed out", errors[0]["error"])
            self.assertIn("container inspection timed out", errors[0]["error"])

    def test_cleanup_removes_only_run_scoped_dind_containers_and_data_volumes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            live = gate.LiveGate(Path(directory), Path(directory) / "evidence.json", False)
            live.dind_data_volumes_created = [
                live.a_data_volume,
                live.b_data_volume,
            ]

            def root_command(*args, **_kwargs):
                output = ""
                if args[:2] == ("volume", "inspect") and "--format" in args:
                    output = json.dumps(
                        {"ojos.cross-machine.run": live.run_id}
                    )
                return gate.Completed(["docker", *args], 0, output, "")

            live.root.command = mock.Mock(side_effect=root_command)

            self.assertEqual(live.cleanup(), [])

            self.assertEqual(
                live.root.command.call_args_list,
                [
                    mock.call(
                        "rm",
                        "--force",
                        "--volumes",
                        live.a_name,
                        timeout=gate.DIND_CONTAINER_REMOVE_TIMEOUT_SECONDS,
                        check=False,
                    ),
                    mock.call(
                        "rm",
                        "--force",
                        "--volumes",
                        live.b_name,
                        timeout=gate.DIND_CONTAINER_REMOVE_TIMEOUT_SECONDS,
                        check=False,
                    ),
                    mock.call(
                        "volume",
                        "inspect",
                        "--format",
                        "{{json .Labels}}",
                        live.a_data_volume,
                        timeout=gate.CLEANUP_RECONCILE_INSPECT_TIMEOUT_SECONDS,
                        check=False,
                    ),
                    mock.call(
                        "volume",
                        "rm",
                        live.a_data_volume,
                        timeout=gate.DIND_VOLUME_REMOVE_TIMEOUT_SECONDS,
                        check=False,
                    ),
                    mock.call(
                        "volume",
                        "inspect",
                        "--format",
                        "{{json .Labels}}",
                        live.b_data_volume,
                        timeout=gate.CLEANUP_RECONCILE_INSPECT_TIMEOUT_SECONDS,
                        check=False,
                    ),
                    mock.call(
                        "volume",
                        "rm",
                        live.b_data_volume,
                        timeout=gate.DIND_VOLUME_REMOVE_TIMEOUT_SECONDS,
                        check=False,
                    ),
                    mock.call(
                        "network",
                        "rm",
                        live.outer_network,
                        timeout=30,
                        check=False,
                    ),
                ],
            )

    def test_cleanup_refuses_to_delete_a_volume_owned_by_another_run(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            live = gate.LiveGate(Path(directory), Path(directory) / "evidence.json", False)
            live.a_name = "outside-run-scope"
            live.b_name = "outside-run-scope"
            live.outer_network = "outside-run-scope"
            live.dind_data_volumes_created = [live.a_data_volume]
            live.root.command = mock.Mock(
                return_value=gate.Completed(
                    ["docker"],
                    0,
                    json.dumps({"ojos.cross-machine.run": "another-run"}),
                    "",
                )
            )

            errors = live.cleanup()

            self.assertEqual(len(errors), 1)
            self.assertEqual(
                errors[0]["operation"],
                f"verify-volume-owner/{live.a_data_volume}",
            )
            self.assertFalse(
                any(call.args[:2] == ("volume", "rm") for call in live.root.command.call_args_list)
            )

    def test_cleanup_reconciles_a_lost_volume_remove_response(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            live = gate.LiveGate(Path(directory), Path(directory) / "evidence.json", False)
            live.a_name = "outside-run-scope"
            live.b_name = "outside-run-scope"
            live.outer_network = "outside-run-scope"
            live.dind_data_volumes_created = [live.a_data_volume]
            live.root.command = mock.Mock(
                side_effect=[
                    gate.Completed(
                        ["docker"],
                        0,
                        json.dumps({"ojos.cross-machine.run": live.run_id}),
                        "",
                    ),
                    gate.GateError("volume remove response was lost"),
                    gate.Completed(["docker"], 1, "", "no such volume"),
                ]
            )

            self.assertEqual(live.cleanup(), [])
            self.assertEqual(live.root.command.call_count, 3)

    def test_partial_dind_start_failure_removes_both_run_scoped_data_volumes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            evidence_path = Path(directory) / "evidence.json"
            live = gate.LiveGate(Path(directory), evidence_path, full_components=False)
            live.checkpoint = mock.Mock()
            live.ensure_live_prerequisites = mock.Mock(return_value={})
            run_count = 0
            created_volumes: set[str] = set()

            def root_command(*args, **_kwargs):
                nonlocal run_count
                if args[:2] == ("volume", "inspect"):
                    volume = str(args[-1])
                    if volume not in created_volumes:
                        return gate.Completed(
                            ["docker", *args], 1, "", "no such volume"
                        )
                    if "--format" in args:
                        return gate.Completed(
                            ["docker", *args],
                            0,
                            json.dumps(
                                {"ojos.cross-machine.run": live.run_id}
                            ),
                            "",
                        )
                    return gate.Completed(
                        ["docker", *args],
                        0,
                        json.dumps(
                            [
                                {
                                    "Name": volume,
                                    "Labels": {
                                        "ojos.cross-machine.run": live.run_id
                                    },
                                }
                            ]
                        ),
                        "",
                    )
                if args[:2] == ("volume", "create"):
                    created_volumes.add(str(args[-1]))
                    return gate.Completed(
                        ["docker", *args], 0, str(args[-1]) + "\n", ""
                    )
                if args[:2] == ("run", "-d"):
                    run_count += 1
                    if run_count == 2:
                        raise gate.GateError("Engine B failed to start")
                return gate.Completed(["docker", *args], 0, "", "")

            live.root.command = mock.Mock(side_effect=root_command)

            with mock.patch.object(gate, "validate_repository", return_value={}):
                with self.assertRaisesRegex(gate.GateError, "Engine B failed to start"):
                    live.run()

            remove_calls = [
                call for call in live.root.command.call_args_list if call.args[:1] == ("rm",)
            ]
            self.assertEqual(
                remove_calls,
                [
                    mock.call(
                        "rm",
                        "--force",
                        "--volumes",
                        live.a_name,
                        timeout=gate.DIND_CONTAINER_REMOVE_TIMEOUT_SECONDS,
                        check=False,
                    ),
                    mock.call(
                        "rm",
                        "--force",
                        "--volumes",
                        live.b_name,
                        timeout=gate.DIND_CONTAINER_REMOVE_TIMEOUT_SECONDS,
                        check=False,
                    ),
                ],
            )
            volume_remove_calls = [
                call
                for call in live.root.command.call_args_list
                if call.args[:2] == ("volume", "rm")
            ]
            self.assertEqual(
                volume_remove_calls,
                [
                    mock.call(
                        "volume",
                        "rm",
                        live.a_data_volume,
                        timeout=gate.DIND_VOLUME_REMOVE_TIMEOUT_SECONDS,
                        check=False,
                    ),
                    mock.call(
                        "volume",
                        "rm",
                        live.b_data_volume,
                        timeout=gate.DIND_VOLUME_REMOVE_TIMEOUT_SECONDS,
                        check=False,
                    ),
                ],
            )


class ContractTests(unittest.TestCase):
    def test_effective_projection_digest_matches_go_rust_golden(self) -> None:
        route = {
            "binding_id": "binding-1",
            "requirement_name": "storage_get",
            "consumer_deployment_id": "worker-b",
            "consumer_service_id": "judge-worker",
            "consumer_node_id": "node-b",
            "credential_generation": 3,
            "api_id": "storage.object.get",
            "provider_deployment_id": "storage-a",
            "provider_service_id": "storage",
            "provider_node_id": "node-a",
            "provider_endpoint": "10.0.0.1:8080:storage",
            "upstream_base": "https://10.0.0.1:8080",
            "provider_path": "/objects",
            "virtual_path": "/internal/apis/storage.object.get",
            "auth_mode": "workload",
            "provider_auth_mode": "workload",
            "permission": "storage.object.read",
            "methods": ["GET"],
            "timeout_ms": 300000,
        }
        grant = {
            "binding_id": "binding-1",
            "requirement_name": "storage_get",
            "consumer_deployment_id": "worker-b",
            "consumer_service_id": "judge-worker",
            "consumer_node_id": "node-b",
            "credential_generation": 3,
            "api_id": "storage.object.get",
            "permission": "storage.object.read",
        }
        expected = "afcaf1f6a8b8be8ae64fa9f7e14d645e3a66657fdeac42cfe8db349b2ba0efbd"
        self.assertEqual(gate.effective_projection_sha256([route], [grant]), expected)
        self.assertEqual(
            full_components._effective_projection_sha256([route], [grant]), expected
        )
        self.assertEqual(
            gate.effective_projection_sha256([], []),
            "fa9d28278a0d02b19bfebeae5afd5aa6dde1c685d8396acc8defe8832848865c",
        )

    def test_provider_status_reads_management_bearer_from_stdin_not_argv(self) -> None:
        scenario = object.__new__(full_components.FullComponentsScenario)
        scenario.topology_id = "cross-machine-a-b"
        scenario.internal_token = "gateway-management-secret"
        scenario.auth_internal_token = "auth-management-secret"
        scenario.a = mock.Mock()
        scenario.a.command.return_value = types.SimpleNamespace(
            stdout=json.dumps(
                {
                    "api_version": "v1",
                    "provider": "gateway",
                    "topology_id": scenario.topology_id,
                    "observed_revision_id": "revision-1",
                    "observed_content_sha256": "a" * 64,
                    "observed_projection_sha256": "b" * 64,
                    "absent": False,
                    "endpoints": [],
                    "links": [],
                }
            )
        )

        status = scenario._provider_topology_status("gateway")

        self.assertEqual(status["provider"], "gateway")
        call = scenario.a.command.call_args
        self.assertNotIn(scenario.internal_token, call.args)
        self.assertIn(scenario.internal_token, call.kwargs["input_data"])
        self.assertEqual(call.args[:6], ("exec", "-i", "orchestrator-a", "curl", "--config", "-"))

    def test_judge_sandbox_security_options_accept_only_docker_privileged_normalization(
        self,
    ) -> None:
        for valid in (
            ["apparmor=unconfined"],
            ["apparmor=unconfined", "label=disable"],
            ["label=disable", "apparmor=unconfined"],
        ):
            self.assertTrue(
                full_components._judge_sandbox_security_options_are_exact(valid)
            )
        for invalid in (
            [],
            ["label=disable"],
            ["apparmor:unconfined"],
            ["apparmor=unconfined", "seccomp=unconfined"],
            ["apparmor=unconfined", "apparmor=unconfined"],
            ["apparmor=unconfined", "label=disable", "label=disable"],
            "apparmor=unconfined",
        ):
            self.assertFalse(
                full_components._judge_sandbox_security_options_are_exact(invalid)
            )

    def test_judge_sandbox_host_mounts_accept_engine_omitted_false_only(self) -> None:
        mounts = [
            {
                "Type": "bind",
                "Source": "/opt/ojos/work",
                "Target": "/var/lib/ojos-worker/work",
                "BindOptions": {"Propagation": "rprivate"},
            },
            {
                "Type": "volume",
                "Source": "ojos-judge-cache-abc",
                "Target": "/var/lib/ojos-worker/cache",
                "VolumeOptions": {"NoCopy": True},
            },
            {
                "Type": "bind",
                "Source": "/sys/fs/cgroup",
                "Target": "/sys/fs/cgroup",
                "BindOptions": {"Propagation": "rprivate"},
            },
            {
                "Type": "tmpfs",
                "Target": "/tmp",
                "TmpfsOptions": {"SizeBytes": 268435456, "Mode": 0o1777},
            },
            {
                "Type": "bind",
                "Source": "/opt/ojos/context",
                "Target": "/run/ojos/service",
                "ReadOnly": True,
                "BindOptions": {"Propagation": "rprivate"},
            },
        ]

        self.assertTrue(full_components._judge_sandbox_host_mounts_are_exact(mounts))

        bind_defaults = copy.deepcopy(mounts)
        for mount in bind_defaults:
            if mount["Type"] == "bind":
                mount["BindOptions"]["ReadOnlyNonRecursive"] = False
        self.assertTrue(
            full_components._judge_sandbox_host_mounts_are_exact(bind_defaults)
        )

        invalid_bind_default = copy.deepcopy(mounts)
        invalid_bind_default[0]["BindOptions"]["ReadOnlyNonRecursive"] = True
        self.assertFalse(
            full_components._judge_sandbox_host_mounts_are_exact(invalid_bind_default)
        )

        null_bind_default = copy.deepcopy(mounts)
        null_bind_default[0]["BindOptions"]["ReadOnlyNonRecursive"] = None
        self.assertFalse(
            full_components._judge_sandbox_host_mounts_are_exact(null_bind_default)
        )

        explicit_false = copy.deepcopy(mounts)
        for mount in explicit_false[:-1]:
            mount["ReadOnly"] = False
        self.assertTrue(
            full_components._judge_sandbox_host_mounts_are_exact(explicit_false)
        )

        for target in (
            "/var/lib/ojos-worker/work",
            "/var/lib/ojos-worker/cache",
            "/sys/fs/cgroup",
            "/tmp",
        ):
            invalid = copy.deepcopy(mounts)
            next(item for item in invalid if item["Target"] == target)["ReadOnly"] = True
            with self.subTest(target=target):
                self.assertFalse(
                    full_components._judge_sandbox_host_mounts_are_exact(invalid)
                )

        for context_read_only in (None, False):
            invalid = copy.deepcopy(mounts)
            context = next(
                item
                for item in invalid
                if item["Target"] == "/run/ojos/service"
            )
            if context_read_only is None:
                context.pop("ReadOnly")
            else:
                context["ReadOnly"] = context_read_only
            with self.subTest(context_read_only=context_read_only):
                self.assertFalse(
                    full_components._judge_sandbox_host_mounts_are_exact(invalid)
                )

    def setUp(self) -> None:
        fixture = Path(__file__).resolve().parents[1] / "fixture/contracts"
        self.consumer = gate.load_contract(fixture / "echo-consumer.release.json")
        self.provider = gate.load_contract(fixture / "echo-provider.release.json")
        self.link = json.loads((fixture / "echo-link.json").read_text(encoding="utf-8"))

    def test_runtime_hosts_enroll_with_the_public_standalone_node_role(self) -> None:
        labels = {"purpose": "judge", "providers": {"storage": True}}

        request = full_components._standalone_node_enrollment(
            "node-b", "192.0.2.20", labels
        )
        labels["providers"]["storage"] = False

        self.assertEqual(
            request,
            {
                "node_id": "node-b",
                "host_ip": "192.0.2.20",
                "role": "standalone",
                "labels": {"purpose": "judge", "providers": {"storage": True}},
                "ttl_seconds": 600,
            },
        )

    def test_manifest_and_link_produce_a_deterministic_generic_binding(self) -> None:
        first = gate.resolve_binding(self.consumer, [self.provider], self.link)
        second = gate.resolve_binding(
            copy.deepcopy(self.consumer), [copy.deepcopy(self.provider)], copy.deepcopy(self.link)
        )
        self.assertEqual(gate.canonical_json(first), gate.canonical_json(second))
        self.assertEqual(first["requirement"], "echo")
        self.assertEqual(first["base_path"], "/internal/apis/fixture.contract.echo")
        self.assertEqual(first["provider_path"], "/echo")

    def test_echo_fixture_is_optional_but_permission_authority_is_required(self) -> None:
        requirements = {
            item["name"]: item
            for item in self.consumer["requires"]["apis"]
        }
        self.assertTrue(requirements["echo"]["optional"])
        self.assertFalse(requirements["permission_check"]["optional"])

    def test_missing_provider_fails_closed(self) -> None:
        with self.assertRaisesRegex(gate.GateError, "no compatible provider"):
            gate.resolve_binding(self.consumer, [], self.link)

    def test_ambiguous_provider_fails_closed(self) -> None:
        duplicate = copy.deepcopy(self.provider)
        duplicate["service"]["id"] = "another-provider"
        with self.assertRaisesRegex(gate.GateError, "ambiguous provider"):
            gate.resolve_binding(self.consumer, [self.provider, duplicate], self.link)

    def test_topology_cannot_silently_change_requirement(self) -> None:
        link = copy.deepcopy(self.link)
        link["api_bindings"][0]["api_id"] = "different.api"
        with self.assertRaisesRegex(gate.GateError, "does not match"):
            gate.resolve_binding(self.consumer, [self.provider], link)

    def test_storage_head_fault_provider_is_healthy_but_404_is_unproven(self) -> None:
        server = fixture_service.ThreadingHTTPServer(
            ("127.0.0.1", 0),
            fixture_service.StorageHeadProvenanceMissHandler,
        )
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            connection = http.client.HTTPConnection(
                "127.0.0.1", server.server_address[1], timeout=2
            )
            connection.request("GET", "/health")
            health = connection.getresponse()
            health.read()
            self.assertEqual(health.status, 200)

            connection.request("HEAD", "/api/storage/objects/problems/object")
            missing = connection.getresponse()
            missing.read()
            self.assertEqual(missing.status, 404)
            self.assertIsNone(missing.getheader("X-OJOS-Storage-Result"))
            connection.close()
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)

    def test_full_harness_queries_the_problem_subject_outbox_key(self) -> None:
        source = (MODULE_PATH.parent / "full_components.py").read_text(encoding="utf-8")
        self.assertIn("aggregate_id='problem/' || p.id::text", source)
        self.assertNotIn("aggregate_id=p.id::text", source)

    def test_actual_flow_uses_declared_default_group_and_checks_producer_validation(self) -> None:
        source = inspect.getsource(full_components.FullComponentsScenario._run_actual_flow)
        self.assertIn('"group": 0', source)
        self.assertNotIn('"group": 1', source)
        self.assertIn('/package/validate', source)
        self.assertIn('package_validation.get("valid") is not True', source)

    def test_actual_flow_canonicalizes_database_digests_at_evidence_boundary(self) -> None:
        raw_digest = "a" * 64
        problem, projection, submission = (
            full_components._normalize_actual_flow_digest_evidence(
                {
                    "problem": {"package_sha256": raw_digest},
                    "projection": {"package_sha256": "sha256:" + raw_digest},
                },
                raw_digest,
            )
        )

        expected = "sha256:" + raw_digest
        self.assertEqual(problem["package_sha256"], expected)
        self.assertEqual(projection["package_sha256"], expected)
        self.assertEqual(submission, expected)
        source = inspect.getsource(full_components.FullComponentsScenario._run_actual_flow)
        self.assertIn("_normalize_actual_flow_digest_evidence(", source)

    def test_actual_flow_digest_evidence_rejects_malformed_values(self) -> None:
        valid = "b" * 64
        for malformed in (None, "", "B" * 64, "sha256:" + "z" * 64):
            with self.subTest(malformed=malformed):
                with self.assertRaisesRegex(
                    full_components.FullGateError, "SHA-256 digest"
                ):
                    full_components._normalize_actual_flow_digest_evidence(
                        {
                            "problem": {"package_sha256": malformed},
                            "projection": {"package_sha256": valid},
                        },
                        valid,
                    )

    def test_actual_flow_fixture_usernames_are_bounded_and_unique(self) -> None:
        build = full_components.FullComponentsScenario._actual_flow_username
        first = build("2bacd2526d", "first")
        recovered = build("2bacd2526d", "recovered")
        long_suffix = build("run-id-that-is-deliberately-long", "x" * 256)

        self.assertNotEqual(first, recovered)
        self.assertTrue(first.startswith("cm_first_"))
        self.assertTrue(recovered.startswith("cm_recovere_"))
        for username in (first, recovered, long_suffix):
            self.assertGreaterEqual(len(username), 3)
            self.assertLessEqual(len(username), 32)
            self.assertRegex(username, r"^[a-z0-9_]+$")

    def test_full_harness_uses_natural_conflict_and_operator_api_for_artifact_gc(self) -> None:
        source = inspect.getsource(
            full_components.FullComponentsScenario._prove_problem_artifact_gc_failure_recovery
        )
        self.assertIn('"method": "duplicate-problem-no-http-conflict"', source)
        self.assertIn('"problem_no": problem_no', source)
        self.assertIn('_problem_artifact_gc_action(', source)
        self.assertIn('"storage-head-provenance-miss-provider"', source)
        self.assertIn("require_same_service=False", source)
        self.assertIn('"storage_head",\n            "ACTIVE"', source)
        self.assertNotIn("_without_topology_requirement(", source)
        self.assertIn('"package_intent_count"', source)
        self.assertIn('"content_intent_count"', source)
        for forbidden in (
            "psql",
            "_query_json",
            "problem_artifact_upload_intents",
            "CREATE TRIGGER",
            "UPDATE ",
            "INSERT INTO ",
            "DELETE FROM ",
        ):
            with self.subTest(forbidden=forbidden):
                self.assertNotIn(forbidden, source)

    def test_full_harness_uses_required_status_and_literal_colon_operator_routes(self) -> None:
        list_source = inspect.getsource(
            full_components.FullComponentsScenario._problem_artifact_gc_intents
        )
        action_source = inspect.getsource(
            full_components.FullComponentsScenario._problem_artifact_gc_action
        )
        self.assertIn('query = {"status": normalized_status, "limit": "200"}', list_source)
        self.assertIn('path = f"/api/problem/admin/artifact-gc/intents:{action}"', action_source)
        self.assertIn("expected=(202,)", action_source)
        self.assertNotIn("/intents/reconcile", action_source)
        self.assertNotIn("/intents/retry", action_source)

    def test_artifact_gc_intent_read_tolerates_cross_filter_state_race(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        records = {
            "PENDING": [{"artifact_uri": "storage://problems/object", "status": "PENDING"}],
            "DELETING": [],
            "NEEDS_ATTENTION": [
                {"artifact_uri": "storage://problems/object", "status": "NEEDS_ATTENTION"}
            ],
        }
        scenario._problem_artifact_gc_intents = mock.Mock(
            side_effect=lambda status: records[status]
        )

        observed = scenario._problem_artifact_gc_intent("storage://problems/object")

        self.assertEqual(observed["status"], "NEEDS_ATTENTION")

    def test_artifact_gc_intent_list_always_sends_required_status_query(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario._ensure_admin_token = mock.Mock(return_value="admin-token")
        scenario.gateway_client = mock.Mock()
        scenario.gateway_client.request.return_value = (
            {
                "intents": [
                    {
                        "artifact_uri": "storage://problems/object",
                        "artifact_sha256": "a" * 64,
                        "artifact_size_bytes": 7,
                        "status": "NEEDS_ATTENTION",
                        "failure_count": 1,
                        "last_failure": {
                            "message": "provider route missing",
                            "stage": "inspect",
                            "kind": "PROVIDER_HTTP",
                            "http_status": 404,
                            "provider_result": "HTTP_404",
                            "deterministic": True,
                        },
                        "upload_completed_at": "2030-01-01T00:00:00Z",
                        "needs_attention_at": "2030-01-01T00:00:01Z",
                        "updated_at": "2030-01-01T00:00:01Z",
                    }
                ],
                "next_cursor": "",
            },
            {},
            200,
        )

        result = scenario._problem_artifact_gc_intents("needs_attention")

        self.assertEqual(len(result), 1)
        request = scenario.gateway_client.request.call_args
        self.assertEqual(request.args[0], "GET")
        self.assertEqual(
            request.args[1],
            "/api/problem/admin/artifact-gc/intents?status=NEEDS_ATTENTION&limit=200",
        )
        self.assertEqual(request.kwargs["expected"], (200,))

    def test_artifact_gc_intent_read_rejects_duplicate_within_one_filter(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        duplicate = {"artifact_uri": "storage://problems/object", "status": "PENDING"}
        scenario._problem_artifact_gc_intents = mock.Mock(
            side_effect=lambda status: [duplicate, duplicate] if status == "PENDING" else []
        )

        with self.assertRaisesRegex(full_components.FullGateError, "duplicated intent"):
            scenario._problem_artifact_gc_intent("storage://problems/object")

    def test_artifact_gc_operator_action_records_202_literal_colon_replay(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.h = types.SimpleNamespace(run_id="run-1")
        scenario._ensure_admin_token = mock.Mock(return_value="admin-token")
        body = {
            "artifact_uri": "storage://problems/object",
            "artifact_sha256": "a" * 64,
            "artifact_size_bytes": 7,
            "reason": "operator proof",
        }
        common = {
            "action_id": 41,
            "request_id": "artifact-gc-action-41",
            "artifact_uri": body["artifact_uri"],
            "queued": True,
            "from_status": "PENDING",
            "to_status": "PENDING",
            "reason_recorded": True,
        }
        scenario.gateway_client = mock.Mock()
        scenario.gateway_client.request.side_effect = [
            ({**common, "idempotent_replay": False}, {}, 202),
            ({**common, "idempotent_replay": True}, {}, 202),
        ]

        evidence = scenario._problem_artifact_gc_action("reconcile", body, "proof")

        self.assertEqual(
            evidence["endpoint"],
            "/api/problem/admin/artifact-gc/intents:reconcile",
        )
        self.assertEqual(evidence["first_http_status"], 202)
        self.assertEqual(evidence["replay_http_status"], 202)
        self.assertTrue(evidence["duplicate_action_id_matched"])
        self.assertTrue(evidence["duplicate_request_id_matched"])
        calls = scenario.gateway_client.request.call_args_list
        self.assertEqual(calls[0].args[:2], ("POST", evidence["endpoint"]))
        self.assertEqual(calls[1].args[:2], ("POST", evidence["endpoint"]))
        self.assertEqual(
            calls[0].kwargs["headers"]["idempotency-key"],
            calls[1].kwargs["headers"]["idempotency-key"],
        )
        self.assertEqual(calls[0].kwargs["expected"], (202,))

    def test_artifact_gc_operator_action_rejects_mismatched_replay_identity(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.h = types.SimpleNamespace(run_id="run-1")
        scenario._ensure_admin_token = mock.Mock(return_value="admin-token")
        body = {
            "artifact_uri": "storage://problems/object",
            "expected_failure_count": 1,
            "reason": "operator retry proof",
        }
        common = {
            "action_id": 41,
            "request_id": "artifact-gc-action-41",
            "artifact_uri": body["artifact_uri"],
            "queued": True,
            "from_status": "NEEDS_ATTENTION",
            "to_status": "PENDING",
            "reason_recorded": True,
        }
        scenario.gateway_client = mock.Mock()
        scenario.gateway_client.request.side_effect = [
            ({**common, "idempotent_replay": False}, {}, 202),
            (
                {
                    **common,
                    "request_id": "different-request",
                    "idempotent_replay": True,
                },
                {},
                202,
            ),
        ]

        with self.assertRaisesRegex(full_components.FullGateError, "idempotency"):
            scenario._problem_artifact_gc_action("retry", body, "proof")

    def test_current_topology_etag_uses_the_actual_strong_response_header(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.topology_id = "topology-a-b"
        scenario.control_client = mock.Mock()
        scenario.control_client.request.return_value = (
            {
                "data": {
                    "draft": {"revision_id": "revision-restored"},
                    "heads": {"applied_revision_id": "revision-restored"},
                }
            },
            {"etag": '"revision-restored"'},
            200,
        )

        self.assertEqual(scenario._current_topology_etag(), '"revision-restored"')

    def test_current_topology_etag_rejects_a_stale_or_missing_header(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.topology_id = "topology-a-b"
        scenario.control_client = mock.Mock()
        scenario.control_client.request.return_value = (
            {
                "data": {
                    "draft": {"revision_id": "revision-restored"},
                    "heads": {"applied_revision_id": "revision-restored"},
                }
            },
            {"etag": '"revision-revoked"'},
            200,
        )

        with self.assertRaisesRegex(full_components.FullGateError, "strong revision ETag"):
            scenario._current_topology_etag()

    def test_topology_revision_creation_rejects_a_missing_etag(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.topology_id = "topology-a-b"
        scenario.control_client = mock.Mock()
        scenario.provider_deployments = {
            "contract-echo-provider": "deployment-echo",
            "auth-service": "deployment-auth",
            "storage-head-provenance-miss-provider": "deployment-head-miss",
        }
        scenario._control_mutation = mock.Mock(
            return_value=(
                {"data": {"revision": {"revision_id": "revision-initial"}}},
                {},
                201,
            )
        )
        scenario._wait_operation = mock.Mock()

        with self.assertRaisesRegex(full_components.FullGateError, "strong revision ETag"):
            scenario._create_and_apply_provider_topology()
        scenario._wait_operation.assert_not_called()

        scenario._control_mutation.reset_mock()
        scenario._control_mutation.return_value = (
            {"data": {"revision": {"revision_id": "revision-next"}}},
            {},
            201,
        )
        with self.assertRaisesRegex(full_components.FullGateError, "strong ETag"):
            scenario._apply_topology_spec(
                {"topology_id": scenario.topology_id},
                "revision-initial",
                "missing-etag",
            )
        scenario._wait_operation.assert_not_called()

    def test_formal_topology_rollback_proves_new_applied_revision(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.topology_id = "topology-a-b"
        target_content = "1" * 64
        parent_content = "2" * 64
        target_id = f"topology-a-b:r1:{target_content}"
        parent_id = f"topology-a-b:r2:{parent_content}"
        created_id = f"topology-a-b:r3:{target_content}"
        target_spec = {"api_version": "v1", "topology_id": "topology-a-b"}
        target = {
            "revision_id": target_id,
            "revision_number": 1,
            "content_sha256": target_content,
            "spec": target_spec,
        }
        parent = {
            "revision_id": parent_id,
            "revision_number": 2,
            "content_sha256": parent_content,
            "spec": {**target_spec, "links": []},
        }
        created = {
            "revision_id": created_id,
            "revision_number": 3,
            "parent_revision_id": parent_id,
            "rollback_of_revision_id": target_id,
            "content_sha256": target_content,
            "spec": copy.deepcopy(target_spec),
        }
        converged = {
            "data": {
                "heads": {
                    "draft_revision_id": created_id,
                    "applied_revision_id": created_id,
                    "applying_revision_id": None,
                },
                "status": {
                    "desired_revision_id": created_id,
                    "observed_revision_id": created_id,
                    "state": "IN_SYNC",
                    "drift": [],
                    "last_operation_id": "operation-rollback",
                },
            }
        }
        scenario.control_client = mock.Mock()
        scenario.control_client.request.side_effect = [
            ({"data": {"revision": target}}, {"etag": f'"{target_id}"'}, 200),
            ({"data": {"revision": parent}}, {"etag": f'"{parent_id}"'}, 200),
            ({"data": {"revision": created}}, {"etag": f'"{created_id}"'}, 200),
            (converged, {"etag": f'"{created_id}"'}, 200),
        ]
        scenario._control_mutation = mock.Mock(
            return_value=(
                {
                    "data": {
                        "operation_id": "operation-rollback",
                        "topology_id": scenario.topology_id,
                        "revision_id": created_id,
                    }
                },
                {},
                202,
            )
        )
        scenario._wait_operation = mock.Mock(
            return_value={
                "operation_id": "operation-rollback",
                "action": "topology.rollback",
                "status": "SUCCEEDED",
            }
        )

        operation, revision_id, proof = scenario._rollback_topology_revision(
            target_revision_id=target_id,
            parent_revision_id=parent_id,
            key="formal-rollback",
        )

        self.assertEqual(operation["action"], "topology.rollback")
        self.assertEqual(revision_id, created_id)
        self.assertEqual(proof["created_rollback_of_revision_id"], target_id)
        self.assertEqual(proof["applied_revision_id"], created_id)
        scenario._control_mutation.assert_called_once_with(
            f"/api/v1/topologies/{scenario.topology_id}:rollback",
            {"revision_id": target_id},
            "formal-rollback",
            expected=(202,),
            headers={"if-match": f'"{parent_id}"'},
        )

    def test_formal_topology_rollback_rejects_missing_revision_etag(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.topology_id = "topology-a-b"
        content = "1" * 64
        target_id = f"topology-a-b:r1:{content}"
        scenario.control_client = mock.Mock()
        scenario.control_client.request.return_value = (
            {
                "data": {
                    "revision": {
                        "revision_id": target_id,
                        "revision_number": 1,
                        "content_sha256": content,
                        "spec": {
                            "api_version": "v1",
                            "topology_id": scenario.topology_id,
                        },
                    }
                }
            },
            {},
            200,
        )
        scenario._control_mutation = mock.Mock()

        with self.assertRaisesRegex(
            full_components.FullGateError, "revision/ETag mismatch"
        ):
            scenario._rollback_topology_revision(
                target_revision_id=target_id,
                parent_revision_id=f"topology-a-b:r2:{'2' * 64}",
                key="formal-rollback-missing-etag",
            )
        scenario._control_mutation.assert_not_called()

    def test_worker_failure_compensation_rolls_back_before_normal_store_retry(
        self,
    ) -> None:
        run_source = inspect.getsource(full_components.FullComponentsScenario.run)
        compensation_call = run_source.index(
            "self._prove_worker_install_failure_compensation(topology_etag)"
        )
        install_call = run_source.index("self._install_worker(topology_etag)")
        self.assertLess(compensation_call, install_call)
        self.assertIn("compensation_evidence, topology_etag", run_source)

        proof_source = inspect.getsource(
            full_components.FullComponentsScenario._prove_worker_install_failure_compensation
        )
        rollback_call = proof_source.index("self._rollback_topology_revision(")
        recovery_return = proof_source.index(
            "return evidence, f'\"{recovery_revision}\"'"
        )
        self.assertLess(rollback_call, recovery_return)
        self.assertNotIn("return evidence, after_etag", proof_source)

    def test_projection_rollback_identity_excludes_only_credential_generation(
        self,
    ) -> None:
        before = {
            "provider": "gateway",
            "topology_id": "topology-a-b",
            "revision_id": "revision-before",
            "content_sha256": "a" * 64,
            "operation_id": "operation-before",
            "updated_at": "before",
            "spec": {"topology_id": "topology-a-b"},
            "routes": [
                {
                    "binding_id": "binding-1",
                    "consumer_deployment_id": "deployment-problem-a",
                    "credential_generation": 3,
                    "api_id": "storage.object.put",
                }
            ],
            "grants": [
                {
                    "binding_id": "binding-1",
                    "consumer_deployment_id": "deployment-problem-a",
                    "credential_generation": 3,
                    "api_id": "storage.object.put",
                }
            ],
        }
        recovered = copy.deepcopy(before)
        recovered.update(
            revision_id="revision-recovered",
            operation_id="operation-recovered",
            updated_at="recovered",
        )
        recovered["routes"][0]["credential_generation"] = 4
        recovered["grants"][0]["credential_generation"] = 4

        self.assertEqual(
            full_components.FullComponentsScenario._projection_stable_fields(before),
            full_components.FullComponentsScenario._projection_stable_fields(
                recovered
            ),
        )
        self.assertEqual(
            full_components.FullComponentsScenario._projection_consumer_generations(
                before
            ),
            {"deployment-problem-a": 3},
        )
        self.assertEqual(
            full_components.FullComponentsScenario._projection_consumer_generations(
                recovered
            ),
            {"deployment-problem-a": 4},
        )

    def test_full_harness_captures_the_complete_gateway_workload_transcript(self) -> None:
        source = (MODULE_PATH.parent / "full_components.py").read_text(encoding="utf-8")
        self.assertIn('"/internal/apis/judge.worker.control/*"', source)
        self.assertIn('"/internal/apis/storage.object.get/*"', source)
        self.assertIn('"workload_request_transcript"', source)
        self.assertNotIn('CAPTURE_PATHS_JSON=["/judge/worker/tasks/claim"]', source)

    def test_specialization_scan_excludes_contract_tests_not_runtime_sources(self) -> None:
        source = MODULE_PATH.read_text(encoding="utf-8")
        self.assertIn('is_runtime_source = "tests" not in relative_parts', source)

    def test_full_harness_external_store_request_keeps_required_node_member(self) -> None:
        source = (MODULE_PATH.parent / "full_components.py").read_text(encoding="utf-8")
        self.assertIn('"target_node_id": ""', source)

    def test_full_control_plane_uses_tokio_postgres_tls_mode_with_explicit_ca(self) -> None:
        database_url = full_components._orchestrator_postgres_database_url(
            "fixture-password"
        )

        self.assertEqual(
            database_url,
            "postgresql://postgres:fixture-password@postgres-a:5432/"
            "ojos_orchestrator?sslmode=require",
        )
        self.assertNotIn("sslmode=verify-full", database_url)
        source = FULL_COMPONENTS_MODULE_PATH.read_text(encoding="utf-8")
        self.assertIn(
            '"ORCHESTRATOR_POSTGRES_CA_CERT": "/opt/ojos-pki/ca.pem"', source
        )
        self.assertIn(
            '"ORCHESTRATOR_OIDC_CA_CERT": "/opt/ojos-pki/ca.pem"', source
        )
        self.assertIn(
            "db_url = _orchestrator_postgres_database_url(self.postgres_password)",
            source,
        )

    def test_derived_orchestrator_image_materializes_shared_ca_at_runtime_path(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.ca_cert = mock.Mock()
        scenario.ca_cert.read_bytes.return_value = b"fixture-ca"
        scenario.ca_key = mock.Mock()
        scenario.ca_key.read_bytes.return_value = b"fixture-ca-key"
        scenario.server_cert = mock.Mock()
        scenario.server_cert.read_bytes.return_value = b"fixture-server-cert"
        scenario.server_key = mock.Mock()
        scenario.server_key.read_bytes.return_value = b"fixture-server-key"
        scenario.images = {"orchestrator": "orchestrator-base:fixture"}
        scenario._derive_image = mock.Mock(return_value="orchestrator-derived:fixture")

        derived = scenario._derive_orchestrator_image()

        self.assertEqual(derived, "orchestrator-derived:fixture")
        name, base, files, docker_lines = scenario._derive_image.call_args.args
        self.assertEqual(name, "orchestrator-production")
        self.assertEqual(base, "orchestrator-base:fixture")
        self.assertEqual(files["ca.pem"], b"fixture-ca")
        self.assertIn(
            "COPY ca.pem server.pem server.key node-ca.pem node-ca.key /opt/ojos-pki/",
            docker_lines,
        )
        source = FULL_COMPONENTS_MODULE_PATH.read_text(encoding="utf-8")
        self.assertIn(
            '"ORCHESTRATOR_POSTGRES_CA_CERT": "/opt/ojos-pki/ca.pem"', source
        )
        self.assertIn(
            '"ORCHESTRATOR_CATALOG_CA_FILE": "/opt/ojos-pki/ca.pem"', source
        )
        self.assertIn(
            '"ORCHESTRATOR_GATEWAY_WORKLOAD_CA_FILE": "/opt/ojos-pki/ca.pem"',
            source,
        )

    def test_agent_host_seed_creates_nested_engine_bind_source_first(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.a = mock.Mock()
        scenario.b = mock.Mock()
        scenario.root = mock.Mock()
        scenario.h = types.SimpleNamespace(a_name="engine-a-outer", b_name="engine-b-outer")
        scenario.secure_fixture_image = "fixture:latest"
        calls = []
        scenario.root.command.side_effect = lambda *args, **kwargs: calls.append(
            ("root", args, kwargs)
        )
        scenario.a.command.side_effect = lambda *args, **kwargs: calls.append(
            ("engine", args, kwargs)
        )

        scenario._seed_agent_host(
            scenario.a, "/var/lib/ojos-agent-a", {"policy.json": b"{}\n"}
        )

        self.assertEqual(calls[0][0], "root")
        self.assertEqual(
            calls[0][1],
            ("exec", "engine-a-outer", "mkdir", "-p", "/var/lib/ojos-agent-a"),
        )
        self.assertEqual(calls[1][0], "engine")
        self.assertIn(
            "type=bind,source=/var/lib/ojos-agent-a,target=/host", calls[1][1]
        )

    def test_full_image_bundle_uses_explicit_argv_and_one_load_per_engine(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            scenario = full_components.FullComponentsScenario.__new__(
                full_components.FullComponentsScenario
            )
            scenario.tmp = Path(directory)
            scenario.root = mock.Mock()
            scenario.a = mock.Mock()
            scenario.b = mock.Mock()
            scenario.image_bundle_save_timeout = 37.0
            scenario.image_bundle_load_timeout = 73.0
            scenario.h = types.SimpleNamespace(evidence={}, checkpoint=mock.Mock())

            def save_archive(*args, **_kwargs):
                if args[:3] == ("image", "save", "--output"):
                    Path(args[3]).write_bytes(b"docker-image-bundle")
                return types.SimpleNamespace(stdout="", stderr="", returncode=0)

            scenario.root.command.side_effect = save_archive
            scenario._distribute_image_bundles(
                ["shared:1", "service-a:1", "shared:1"],
                ["shared:1", "agent-b:1"],
            )

            a_archive = Path(directory) / "image-bundles" / "engine-a.tar"
            b_archive = Path(directory) / "image-bundles" / "engine-b.tar"
            a_partial = Path(directory) / "image-bundles" / "engine-a.tar.partial"
            b_partial = Path(directory) / "image-bundles" / "engine-b.tar.partial"
            self.assertEqual(
                scenario.root.command.call_args_list,
                [
                    mock.call(
                        "image",
                        "save",
                        "--output",
                        a_partial,
                        "shared:1",
                        "service-a:1",
                        timeout=37.0,
                    ),
                    mock.call(
                        "image",
                        "save",
                        "--output",
                        b_partial,
                        "shared:1",
                        "agent-b:1",
                        timeout=37.0,
                    ),
                ],
            )
            scenario.a.command.assert_has_calls(
                [
                    mock.call("image", "load", "--input", a_archive, timeout=73.0),
                    mock.call(
                        "image", "inspect", "shared:1", "service-a:1", timeout=300
                    ),
                ]
            )
            scenario.b.command.assert_has_calls(
                [
                    mock.call("image", "load", "--input", b_archive, timeout=73.0),
                    mock.call(
                        "image", "inspect", "shared:1", "agent-b:1", timeout=300
                    ),
                ]
            )
            self.assertEqual(
                sum(
                    call.args[:2] == ("image", "load")
                    for call in scenario.a.command.call_args_list
                ),
                1,
            )
            self.assertEqual(
                sum(
                    call.args[:2] == ("image", "load")
                    for call in scenario.b.command.call_args_list
                ),
                1,
            )
            self.assertFalse(a_archive.exists())
            self.assertFalse(b_archive.exists())
            self.assertFalse(a_partial.exists())
            self.assertFalse(b_partial.exists())
            self.assertEqual(
                scenario.h.evidence["image_distribution"]["engine_load_invocations"],
                {"a": 1, "b": 1},
            )
            scenario.h.checkpoint.assert_called_once_with("full.images-distributed")

    @staticmethod
    def _docker_build_retry_scenario():
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.root = mock.Mock()
        scenario.h = types.SimpleNamespace(evidence={}, checkpoint=mock.Mock())
        return scenario

    def test_host_docker_build_retries_transient_layer_download_then_records_success(
        self,
    ) -> None:
        scenario = self._docker_build_retry_scenario()
        transient = full_components.FullGateError(
            "failed to copy: httpReadSeeker: failed open: OpenSSL SSL_connect: "
            "SSL_ERROR_SYSCALL in connection to registry-1.docker.io:443"
        )
        completed = types.SimpleNamespace(stdout="built", stderr="", returncode=0)
        scenario.root.command.side_effect = [transient, completed]

        with mock.patch.object(full_components.time, "sleep") as sleep:
            result = scenario._host_docker_build(
                "build", "--tag", "candidate:1", Path("context"),
                label="orchestrator", timeout=3600,
            )

        self.assertIs(result, completed)
        self.assertEqual(scenario.root.command.call_count, 2)
        self.assertEqual(
            scenario.root.command.call_args_list,
            [
                mock.call(
                    "build", "--tag", "candidate:1", Path("context"), timeout=3600
                ),
                mock.call(
                    "build", "--tag", "candidate:1", Path("context"), timeout=3600
                ),
            ],
        )
        sleep.assert_called_once_with(2.0)
        self.assertEqual(
            scenario.h.checkpoint.call_args_list,
            [
                mock.call("full.docker-build-retry"),
                mock.call("full.docker-build-retry-resolved"),
            ],
        )
        events = scenario.h.evidence["docker_build_retry_events"]
        self.assertEqual([item["outcome"] for item in events], ["RETRYING", "SUCCEEDED"])
        self.assertEqual(events[0]["failure_kind"], "tls-syscall")
        self.assertRegex(events[0]["error_fingerprint"], r"^sha256:[0-9a-f]{64}$")
        self.assertNotIn("SSL_ERROR_SYSCALL", json.dumps(events))

    def test_host_docker_build_does_not_retry_deterministic_failure(self) -> None:
        scenario = self._docker_build_retry_scenario()
        deterministic = full_components.FullGateError(
            "Dockerfile parse error on line 17: unknown instruction: COPYX"
        )
        scenario.root.command.side_effect = deterministic

        with mock.patch.object(full_components.time, "sleep") as sleep:
            with self.assertRaisesRegex(full_components.FullGateError, "COPYX"):
                scenario._host_docker_build(
                    "build", "--tag", "candidate:1", Path("context"),
                    label="orchestrator", timeout=3600,
                )

        scenario.root.command.assert_called_once()
        sleep.assert_not_called()
        scenario.h.checkpoint.assert_not_called()
        self.assertNotIn("docker_build_retry_events", scenario.h.evidence)

    def test_root_image_pull_retries_transient_layer_short_read(self) -> None:
        scenario = self._docker_build_retry_scenario()
        missing = types.SimpleNamespace(stdout="", stderr="", returncode=1)
        transient = full_components.FullGateError(
            "failed to pull and unpack image redis:8.8.0: failed to copy: "
            "failed to read expected number of bytes: unexpected EOF"
        )
        completed = types.SimpleNamespace(stdout="pulled", stderr="", returncode=0)
        scenario.root.command.side_effect = [missing, transient, completed]

        with mock.patch.object(full_components.time, "sleep") as sleep:
            scenario._ensure_root_image("redis:8.8.0")

        self.assertEqual(
            scenario.root.command.call_args_list,
            [
                mock.call("image", "inspect", "redis:8.8.0", check=False),
                mock.call("pull", "redis:8.8.0", timeout=600),
                mock.call("pull", "redis:8.8.0", timeout=600),
            ],
        )
        sleep.assert_called_once_with(2.0)
        self.assertEqual(
            scenario.h.checkpoint.call_args_list,
            [
                mock.call("full.docker-pull-retry"),
                mock.call("full.docker-pull-retry-resolved"),
            ],
        )
        events = scenario.h.evidence["docker_pull_retry_events"]
        self.assertEqual([item["outcome"] for item in events], ["RETRYING", "SUCCEEDED"])
        self.assertEqual(events[0]["failure_kind"], "registry-layer-short-read")
        self.assertRegex(events[0]["error_fingerprint"], r"^sha256:[0-9a-f]{64}$")
        self.assertNotIn("unexpected EOF", json.dumps(events))

    def test_root_image_pull_does_not_retry_deterministic_failure(self) -> None:
        scenario = self._docker_build_retry_scenario()
        missing = types.SimpleNamespace(stdout="", stderr="", returncode=1)
        deterministic = full_components.FullGateError(
            "manifest for redis:definitely-missing not found: manifest unknown"
        )
        scenario.root.command.side_effect = [missing, deterministic]

        with mock.patch.object(full_components.time, "sleep") as sleep:
            with self.assertRaisesRegex(full_components.FullGateError, "manifest unknown"):
                scenario._ensure_root_image("redis:definitely-missing")

        self.assertEqual(
            scenario.root.command.call_args_list,
            [
                mock.call("image", "inspect", "redis:definitely-missing", check=False),
                mock.call("pull", "redis:definitely-missing", timeout=600),
            ],
        )
        sleep.assert_not_called()
        scenario.h.checkpoint.assert_not_called()
        self.assertNotIn("docker_pull_retry_events", scenario.h.evidence)

    def test_host_docker_build_exhausts_bounded_retries_with_atomic_evidence(self) -> None:
        scenario = self._docker_build_retry_scenario()
        transient = full_components.FullGateError(
            "failed to resolve source metadata for docker.io/library/node:22: "
            "failed to copy: short read"
        )
        scenario.root.command.side_effect = [
            transient for _ in range(full_components.DOCKER_BUILD_MAX_ATTEMPTS)
        ]

        with mock.patch.object(full_components.time, "sleep") as sleep:
            with self.assertRaisesRegex(full_components.FullGateError, "short read"):
                scenario._host_docker_build(
                    "build", "--tag", "candidate:1", Path("context"),
                    label="orchestrator", timeout=3600,
                )

        self.assertEqual(
            scenario.root.command.call_count, full_components.DOCKER_BUILD_MAX_ATTEMPTS
        )
        self.assertEqual(
            [call.args[0] for call in sleep.call_args_list],
            list(full_components.DOCKER_BUILD_RETRY_DELAYS_SECONDS),
        )
        self.assertEqual(
            scenario.h.checkpoint.call_count, full_components.DOCKER_BUILD_MAX_ATTEMPTS
        )
        events = scenario.h.evidence["docker_build_retry_events"]
        self.assertEqual(len(events), full_components.DOCKER_BUILD_MAX_ATTEMPTS)
        self.assertEqual(events[-1]["outcome"], "EXHAUSTED")
        self.assertIsNone(events[-1]["retry_after_seconds"])

    def test_transient_classifier_requires_network_or_layer_context_for_eof(self) -> None:
        self.assertEqual(
            full_components._transient_docker_build_failure_kind(
                "failed to copy layer sha256:abc: unexpected EOF"
            ),
            "registry-layer-short-read",
        )
        self.assertIsNone(
            full_components._transient_docker_build_failure_kind(
                "compiler error: unexpected EOF while parsing src/main.rs"
            )
        )
        self.assertIsNone(
            full_components._transient_docker_build_failure_kind(
                "error: failed to parse manifest at Cargo.toml: unexpected EOF"
            )
        )
        self.assertIsNone(
            full_components._transient_docker_build_failure_kind(
                "failed to resolve registry.invalid: no such host"
            )
        )
        self.assertIsNone(
            full_components._transient_docker_build_failure_kind(
                "build step failed to fetch local fixture: unexpected EOF"
            )
        )

    def test_initial_image_sets_keep_b_on_registry_pull_boundary(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.fixture_image = "fixture:run"
        scenario.secure_fixture_image = "secure-fixture:run"
        keys = (
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
        scenario.images = {key: f"{key}:run" for key in keys}

        a_images, b_images = scenario._initial_image_bundle_sets()

        self.assertEqual(
            a_images,
            ["fixture:run", "secure-fixture:run", *(f"{key}:run" for key in keys)],
        )
        self.assertEqual(b_images, ["secure-fixture:run", "agent:run"])
        self.assertNotIn("worker:run", b_images)
        self.assertNotIn("echo:run", b_images)

        del scenario.images["registry"]
        with self.assertRaisesRegex(full_components.FullGateError, "registry"):
            scenario._initial_image_bundle_sets()

    def test_image_bundle_timeout_configuration_is_bounded(self) -> None:
        with mock.patch.dict(
            "os.environ",
            {
                full_components.IMAGE_BUNDLE_SAVE_TIMEOUT_ENV: "123.5",
                full_components.IMAGE_BUNDLE_LOAD_TIMEOUT_ENV: "456",
            },
            clear=False,
        ):
            self.assertEqual(
                full_components._configured_timeout_seconds(
                    full_components.IMAGE_BUNDLE_SAVE_TIMEOUT_ENV, 1
                ),
                123.5,
            )
            self.assertEqual(
                full_components._configured_timeout_seconds(
                    full_components.IMAGE_BUNDLE_LOAD_TIMEOUT_ENV, 1
                ),
                456.0,
            )
        for invalid in ("0", "nan", "21601", "not-a-number"):
            with self.subTest(invalid=invalid), mock.patch.dict(
                "os.environ",
                {full_components.IMAGE_BUNDLE_LOAD_TIMEOUT_ENV: invalid},
                clear=False,
            ):
                with self.assertRaises(full_components.FullGateError):
                    full_components._configured_timeout_seconds(
                        full_components.IMAGE_BUNDLE_LOAD_TIMEOUT_ENV, 1
                    )

    def test_image_distribution_evidence_is_not_published_after_partial_load(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            scenario = full_components.FullComponentsScenario.__new__(
                full_components.FullComponentsScenario
            )
            scenario.tmp = Path(directory)
            scenario.root = mock.Mock()
            scenario.a = mock.Mock()
            scenario.b = mock.Mock()
            scenario.image_bundle_save_timeout = 37.0
            scenario.image_bundle_load_timeout = 73.0
            scenario.h = types.SimpleNamespace(evidence={}, checkpoint=mock.Mock())

            def save_archive(*args, **_kwargs):
                if args[:3] == ("image", "save", "--output"):
                    Path(args[3]).write_bytes(b"docker-image-bundle")
                return types.SimpleNamespace(stdout="", stderr="", returncode=0)

            scenario.root.command.side_effect = save_archive
            scenario.b.command.side_effect = full_components.FullGateError("load failed")

            with self.assertRaisesRegex(full_components.FullGateError, "load failed"):
                scenario._distribute_image_bundles(["service-a:1"], ["agent-b:1"])

            self.assertNotIn("image_distribution", scenario.h.evidence)
            scenario.h.checkpoint.assert_not_called()
            self.assertFalse(
                (Path(directory) / "image-bundles" / "engine-b.tar").exists()
            )

    def test_image_bundle_save_failure_removes_partial_archive(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            scenario = full_components.FullComponentsScenario.__new__(
                full_components.FullComponentsScenario
            )
            scenario.tmp = Path(directory)
            scenario.root = mock.Mock()
            scenario.image_bundle_save_timeout = 37.0
            scenario.image_bundle_load_timeout = 73.0
            engine = mock.Mock()

            def failed_save(*args, **_kwargs):
                Path(args[3]).write_bytes(b"partial-docker-image-bundle")
                raise full_components.FullGateError("save timed out")

            scenario.root.command.side_effect = failed_save
            with self.assertRaisesRegex(full_components.FullGateError, "save timed out"):
                scenario._transfer_image_bundle("engine-a", ["service-a:1"], engine)

            engine.command.assert_not_called()
            bundle_root = Path(directory) / "image-bundles"
            self.assertFalse((bundle_root / "engine-a.tar.partial").exists())
            self.assertFalse((bundle_root / "engine-a.tar").exists())

    def test_full_catalog_image_is_built_inside_engine_a_without_an_extra_load(self) -> None:
        source = FULL_COMPONENTS_MODULE_PATH.read_text(encoding="utf-8")
        self.assertIn(
            'self.a.command("build", "--tag", catalog_image, catalog_context, timeout=900)',
            source,
        )
        self.assertNotIn("self.h._transfer_image(catalog_image", source)

    def test_preprovisioned_dependency_is_a_v2_image_release_for_external_install(self) -> None:
        contract = full_components._preprovisioned_dependency_contract(
            service_id="postgresql",
            version="17.0.0",
            service_type="database",
            protocol="postgres",
            port=5432,
            health_path="",
        )

        self.assertEqual(contract["schema_version"], 2)
        self.assertEqual(contract["service_name"], "postgresql")
        self.assertEqual(contract["runtime"]["kind"], "image")
        self.assertEqual(contract["dependencies"], [])
        self.assertEqual(contract["runtime_contract"]["id"], "standard-container-v1")

    def test_full_catalog_aggregates_and_reuses_real_external_dependencies(self) -> None:
        source = FULL_COMPONENTS_MODULE_PATH.read_text(encoding="utf-8")
        self.assertIn('"--catalog-id", "cross-machine-services"', source)
        self.assertIn('"--additional-release-manifest"', source)
        self.assertIn('"postgresql",\n                "postgresql",\n                "17.0.0"', source)
        self.assertIn('"redis",\n                "redis",\n                "8.8.0"', source)
        self.assertIn('"minio",\n                "minio",\n                "2025.9.7"', source)
        self.assertIn("managed {service_id} reinstalled healthy External dependencies", source)
        self.assertIn('running_image_id != catalog_image_id', source)

    @staticmethod
    def _write_catalog_audit_fixture(root: Path) -> None:
        (root / "metadata").mkdir(parents=True)
        image = "engine-a:5000/ojos/fixture@sha256:" + "a" * 64
        postgres_image = "engine-a:5000/ojos/postgres@sha256:" + "b" * 64
        fixture_metadata = json.dumps(
            {
                "service_name": "fixture-service",
                "version": "1.2.3",
                "dependencies": ["postgresql"],
                "runtime": {"image": image},
            }
        ).encode("utf-8")
        postgres_metadata = json.dumps(
            {
                "service_name": "postgresql",
                "version": "17.0.0",
                "dependencies": [],
                "runtime": {"image": postgres_image},
            }
        ).encode("utf-8")
        fixture_path = root / "metadata" / "fixture-service-1.2.3.release.json"
        postgres_path = root / "metadata" / "postgresql-17.0.0.release.json"
        fixture_path.write_bytes(fixture_metadata)
        postgres_path.write_bytes(postgres_metadata)
        (root / "catalog.json").write_text(
            json.dumps(
                {
                    "id": "fixture-catalog",
                    "modules": [
                        {
                            "id": "fixture-service",
                            "releases": [
                                {
                                    "version": "1.2.3",
                                    "platforms": [{"os": "linux", "arch": "x86_64"}],
                                    "dependencies": [
                                        {
                                            "module_id": "postgresql",
                                            "requirement": "=17.0.0",
                                            "channel": "stable",
                                        }
                                    ],
                                    "metadata": {
                                        "url": "https://catalog.example/metadata/fixture-service-1.2.3.release.json",
                                        "sha256": full_components._sha256(fixture_metadata),
                                    },
                                    "oci_image": image,
                                }
                            ],
                        },
                        {
                            "id": "postgresql",
                            "releases": [
                                {
                                    "version": "17.0.0",
                                    "platforms": [{"os": "linux", "arch": "x86_64"}],
                                    "dependencies": [],
                                    "metadata": {
                                        "url": "https://catalog.example/metadata/postgresql-17.0.0.release.json",
                                        "sha256": full_components._sha256(postgres_metadata),
                                    },
                                    "oci_image": postgres_image,
                                }
                            ],
                        },
                    ],
                }
            ),
            encoding="utf-8",
        )

    def test_generated_catalog_audit_accepts_aligned_contract(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self._write_catalog_audit_fixture(root)

            audit = full_components._audit_generated_catalog(
                root,
                expected_catalog_id="fixture-catalog",
                expected_os="linux",
                expected_arch="x86_64",
                expected_contracts={
                    ("fixture-service", "1.2.3"): {
                        "dependencies": ["postgresql"],
                        "oci_image": "engine-a:5000/ojos/fixture@sha256:" + "a" * 64,
                    },
                    ("postgresql", "17.0.0"): {
                        "dependencies": [],
                        "oci_image": "engine-a:5000/ojos/postgres@sha256:" + "b" * 64,
                    },
                },
            )

            by_service = {item["service_id"]: item for item in audit}
            self.assertEqual(by_service["fixture-service"]["version"], "1.2.3")
            self.assertEqual(by_service["fixture-service"]["dependencies"], ["postgresql"])
            self.assertEqual(by_service["postgresql"]["dependencies"], [])

    def test_generated_catalog_audit_rejects_dependency_version_and_platform_divergence(self) -> None:
        cases = (
            (
                "dependency sets differ",
                lambda catalog, metadata: metadata.update({"dependencies": ["redis"]}),
            ),
            (
                "module/version differs",
                lambda catalog, metadata: metadata.update({"version": "1.2.4"}),
            ),
            (
                "dependency version differs",
                lambda catalog, metadata: catalog["modules"][0]["releases"][0][
                    "dependencies"
                ][0].update({"requirement": "=18.0.0"}),
            ),
            (
                "platform differs",
                lambda catalog, metadata: catalog["modules"][0]["releases"][0].update(
                    {"platforms": [{"os": "linux", "arch": "aarch64"}]}
                ),
            ),
        )
        for expected_error, mutate in cases:
            with self.subTest(expected_error=expected_error), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self._write_catalog_audit_fixture(root)
                catalog_path = root / "catalog.json"
                metadata_path = root / "metadata" / "fixture-service-1.2.3.release.json"
                catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
                metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
                mutate(catalog, metadata)
                metadata_bytes = json.dumps(metadata).encode("utf-8")
                metadata_path.write_bytes(metadata_bytes)
                catalog["modules"][0]["releases"][0]["metadata"]["sha256"] = (
                    full_components._sha256(metadata_bytes)
                )
                catalog_path.write_text(json.dumps(catalog), encoding="utf-8")

                with self.assertRaisesRegex(full_components.FullGateError, expected_error):
                    full_components._audit_generated_catalog(
                        root,
                        expected_catalog_id="fixture-catalog",
                        expected_os="linux",
                        expected_arch="x86_64",
                    )

    def test_bootstrap_health_probe_uses_inner_ip_not_inner_dns_from_dind_host(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.a = mock.Mock()
        scenario.root = mock.Mock()
        scenario.h = types.SimpleNamespace(a_name="engine-a-outer")
        scenario.a.command.return_value = types.SimpleNamespace(
            stdout="172.30.0.12\n", stderr="", returncode=0
        )
        scenario.root.command.return_value = types.SimpleNamespace(
            stdout="ok", stderr="", returncode=0
        )

        scenario._wait_a_http("auth-a", 8081, "/health", timeout=1)

        scenario.a.command.assert_called_once_with(
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            "auth-a",
            timeout=30,
        )
        scenario.root.command.assert_called_once_with(
            "exec",
            "engine-a-outer",
            "wget",
            "-qO-",
            "http://172.30.0.12:8081/health",
            timeout=5,
            check=False,
        )

    def test_minio_health_probe_does_not_use_inner_dns_from_dind_host(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.root = mock.Mock()
        scenario.h = types.SimpleNamespace(a_name="engine-a-outer")
        scenario.A_IPS = {"minio": "172.30.0.15"}
        scenario.root.command.return_value = types.SimpleNamespace(
            stdout="ok", stderr="", returncode=0
        )

        scenario._wait_minio()

        scenario.root.command.assert_called_once_with(
            "exec",
            "engine-a-outer",
            "wget",
            "-qO-",
            "http://172.30.0.15:9000/minio/health/ready",
            timeout=5,
            check=False,
        )

    def test_full_harness_uses_a_signed_storage_canary_for_semantic_rebind(self) -> None:
        source = FULL_COMPONENTS_MODULE_PATH.read_text(encoding="utf-8")
        self.assertIn('"storage-service-canary"', source)
        self.assertIn('"version: 0.1.1"', source)
        self.assertIn('"catalog_source_id": "storage-service-canary"', source)
        self.assertIn('endpoint = f"{self.h.a_ip}:8086:storage-service"', source)
        self.assertIn('requirement_name="storage_get"', source)
        self.assertIn('"semantic_provider_rebind": True', source)
        self.assertNotIn("provider-preserving context generation rotation", source)
        self.assertNotIn('self.managed_a_runtimes["storage-service-canary"]', source)

    @staticmethod
    def _storage_rebind_spec() -> dict:
        return {
            "api_version": "v1",
            "topology_id": "main",
            "root_endpoint": "auth:8081:auth-service",
            "authority": {
                "root_endpoint": "auth:8081:auth-service",
                "exposure_policy": "internal",
            },
            "endpoints": [
                {
                    "endpoint": "worker:9101:judge-worker",
                    "service_id": "judge-worker",
                    "protocol": "http",
                    "config": {"deployment_id": "worker-b"},
                },
                {
                    "endpoint": "storage:8085:storage-service",
                    "service_id": "storage-service",
                    "protocol": "http",
                    "config": {"deployment_id": "storage-old"},
                },
                {
                    "endpoint": "storage:8086:storage-service",
                    "service_id": "storage-service",
                    "protocol": "http",
                    "config": {"deployment_id": "storage-canary"},
                },
            ],
            "links": [
                {
                    "source_endpoint": "worker:9101:judge-worker",
                    "target_endpoint": "storage:8085:storage-service",
                    "protocol": "http",
                    "auth_mode": "workload",
                    "scope": "api-binding",
                    "enabled": True,
                    "api_bindings": [
                        {
                            "requirement": "storage_get",
                            "api_id": "storage.object.get",
                            "provider_deployment_id": "storage-old",
                        },
                        {
                            "requirement": "storage_head",
                            "api_id": "storage.object.head",
                            "provider_deployment_id": "storage-old",
                        },
                    ],
                }
            ],
        }

    def test_semantic_rebind_moves_only_the_selected_requirement(self) -> None:
        original = self._storage_rebind_spec()
        rebound, evidence = full_components._rebind_topology_requirement(
            original,
            consumer_deployment_id="worker-b",
            requirement_name="storage_get",
            old_provider_deployment_id="storage-old",
            new_provider_deployment_id="storage-canary",
        )

        self.assertEqual(
            original["links"][0]["api_bindings"][0]["provider_deployment_id"],
            "storage-old",
        )
        by_requirement = {
            selection["requirement"]: (link["target_endpoint"], selection)
            for link in rebound["links"]
            for selection in link.get("api_bindings", [])
        }
        self.assertEqual(
            by_requirement["storage_get"][0], "storage:8086:storage-service"
        )
        self.assertEqual(
            by_requirement["storage_get"][1]["provider_deployment_id"],
            "storage-canary",
        )
        self.assertEqual(
            by_requirement["storage_head"][0], "storage:8085:storage-service"
        )
        self.assertEqual(evidence["old_provider_deployment_id"], "storage-old")
        self.assertEqual(evidence["new_provider_deployment_id"], "storage-canary")

    def test_fault_rebind_keeps_required_selection_while_provider_service_changes(self) -> None:
        original = self._storage_rebind_spec()
        fault_endpoint = original["endpoints"][2]
        fault_endpoint["endpoint"] = "head-miss:8080:head-miss-provider"
        fault_endpoint["service_id"] = "head-miss-provider"

        with self.assertRaisesRegex(full_components.FullGateError, "same service"):
            full_components._rebind_topology_requirement(
                original,
                consumer_deployment_id="worker-b",
                requirement_name="storage_head",
                old_provider_deployment_id="storage-old",
                new_provider_deployment_id="storage-canary",
            )

        rebound, _ = full_components._rebind_topology_requirement(
            original,
            consumer_deployment_id="worker-b",
            requirement_name="storage_head",
            old_provider_deployment_id="storage-old",
            new_provider_deployment_id="storage-canary",
            require_same_service=False,
        )
        matches = [
            (link["target_endpoint"], selection)
            for link in rebound["links"]
            for selection in link.get("api_bindings", [])
            if selection.get("requirement") == "storage_head"
        ]
        self.assertEqual(len(matches), 1)
        self.assertEqual(matches[0][0], "head-miss:8080:head-miss-provider")
        self.assertEqual(
            matches[0][1]["provider_deployment_id"], "storage-canary"
        )

    def test_semantic_rebind_fails_closed_on_ambiguous_or_forged_selection(self) -> None:
        ambiguous = self._storage_rebind_spec()
        ambiguous["links"].append(copy.deepcopy(ambiguous["links"][0]))
        with self.assertRaisesRegex(full_components.FullGateError, "expected one storage_get"):
            full_components._rebind_topology_requirement(
                ambiguous,
                consumer_deployment_id="worker-b",
                requirement_name="storage_get",
                old_provider_deployment_id="storage-old",
                new_provider_deployment_id="storage-canary",
            )

        forged = self._storage_rebind_spec()
        forged["links"][0]["api_bindings"][0]["provider_deployment_id"] = "other"
        with self.assertRaisesRegex(full_components.FullGateError, "expected old provider"):
            full_components._rebind_topology_requirement(
                forged,
                consumer_deployment_id="worker-b",
                requirement_name="storage_get",
                old_provider_deployment_id="storage-old",
                new_provider_deployment_id="storage-canary",
            )

    def test_full_harness_uses_auth_bootstrap_and_login_not_a_forged_jwt(self) -> None:
        source = (MODULE_PATH.parent / "full_components.py").read_text(encoding="utf-8")
        self.assertIn('"/api/auth/bootstrap/admin"', source)
        self.assertIn('"/api/auth/login"', source)
        self.assertIn('"jwt_source": "auth-service-login-endpoint"', source)
        self.assertIn('"jwt_self_signed_by_harness": False', source)
        self.assertIn('"manual_database_role_seed": False', source)
        self.assertNotRegex(source, r"(?m)^\s*def\s+_user_jwt\s*\(")
        self.assertNotRegex(source, r"\bhmac\.new\s*\(")

    def test_full_harness_has_no_local_user_jwt_signing_path(self) -> None:
        source = FULL_COMPONENTS_MODULE_PATH.read_text(encoding="utf-8")
        self.assertNotIn("def _user_jwt", source)
        self.assertNotIn("hmac.new", source)
        self.assertNotIn('"HS256"', source)

    def test_auth_bootstrap_secret_is_not_embedded_in_failure_argv(self) -> None:
        source = FULL_COMPONENTS_MODULE_PATH.read_text(encoding="utf-8")
        self.assertIn('"--env-file", str(bootstrap_env)', source)
        self.assertIn("bootstrap_env.unlink(missing_ok=True)", source)
        self.assertNotIn(
            '"--env", "AUTH_ADMIN_BOOTSTRAP_SECRET=" + self.auth_bootstrap_secret',
            source,
        )

    def test_full_result_evidence_waits_for_async_redis_outbox_relay(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.a = mock.Mock()
        scenario.a.command.side_effect = [
            types.SimpleNamespace(stdout="[]", stderr="", returncode=0),
            types.SimpleNamespace(
                stdout=json.dumps(
                    [
                        [
                            "1786000000000-0",
                            [
                                "task_id",
                                "task-7",
                                "submission_id",
                                "41",
                                "status",
                                "ACCEPTED",
                                "worker_id",
                                "worker-b",
                            ],
                        ]
                    ]
                ),
                stderr="",
                returncode=0,
            ),
        ]

        result = scenario._redis_result(
            "task-7", 41, timeout=1, poll_interval=0
        )

        self.assertEqual(result["result_id"], "1786000000000-0")
        self.assertEqual(result["status"], "ACCEPTED")
        self.assertEqual(scenario.a.command.call_count, 2)


class AuthBootstrapEvidenceTests(unittest.TestCase):
    def test_bootstrap_login_rejections_and_database_state_are_evidenced_without_secrets(self) -> None:
        scenario = full_components.FullComponentsScenario.__new__(
            full_components.FullComponentsScenario
        )
        scenario.admin_token = ""
        scenario.admin_username = "initial-admin"
        scenario.admin_password = "admin-password-never-evidence"
        scenario.auth_bootstrap_secret = "bootstrap-secret-never-evidence-123456789"
        scenario.h = types.SimpleNamespace(evidence={})
        gateway = mock.Mock()
        gateway.request.side_effect = [
            (
                {
                    "code": 0,
                    "data": {"user_id": 41, "username": scenario.admin_username},
                },
                {},
                201,
            ),
            ({"code": 40931, "msg": "unavailable"}, {}, 409),
            ({"code": 40331, "msg": "invalid credential"}, {}, 403),
            (
                {
                    "code": 0,
                    "data": {
                        "token": "login-jwt-never-evidence",
                        "user_id": 41,
                        "username": scenario.admin_username,
                        "roles": ["user", "super_admin"],
                        "permissions": ["system.admin"],
                    },
                },
                {},
                200,
            ),
            (
                {
                    "code": 0,
                    "data": {
                        "user_id": 41,
                        "username": scenario.admin_username,
                        "roles": ["super_admin", "user"],
                        "permissions": ["system.admin"],
                    },
                },
                {},
                200,
            ),
        ]
        scenario.gateway_client = gateway
        database_proof = {
            "marker_completed": True,
            "marker_user_id": "41",
            "super_admin_assigned": True,
            "bootstrap_audit_count": 1,
        }
        scenario._query_json = mock.Mock(return_value=database_proof)

        token = scenario._ensure_admin_token()

        self.assertEqual(token, "login-jwt-never-evidence")
        self.assertEqual(scenario.admin_token, token)
        paths = [call.args[1] for call in gateway.request.call_args_list]
        self.assertEqual(
            paths,
            [
                "/api/auth/bootstrap/admin",
                "/api/auth/bootstrap/admin",
                "/api/auth/bootstrap/admin",
                "/api/auth/login",
                "/api/auth/me",
            ],
        )
        first_headers = gateway.request.call_args_list[0].kwargs["headers"]
        replay_headers = gateway.request.call_args_list[1].kwargs["headers"]
        denied_headers = gateway.request.call_args_list[2].kwargs["headers"]
        self.assertEqual(
            first_headers["x-ojos-bootstrap-secret"], scenario.auth_bootstrap_secret
        )
        self.assertEqual(
            replay_headers["x-ojos-bootstrap-secret"], scenario.auth_bootstrap_secret
        )
        self.assertNotEqual(
            denied_headers["x-ojos-bootstrap-secret"], scenario.auth_bootstrap_secret
        )
        profile_headers = gateway.request.call_args_list[4].kwargs["headers"]
        self.assertEqual(profile_headers["authorization"], "Bearer " + token)
        scenario._query_json.assert_called_once()
        self.assertEqual(scenario._query_json.call_args.args[0], "ojos_auth")
        self.assertIn("auth_bootstrap_state", scenario._query_json.call_args.args[1])
        self.assertIn("auth.bootstrap.initial_admin", scenario._query_json.call_args.args[1])

        evidence = scenario.h.evidence["auth_admin_bootstrap"]
        self.assertEqual(evidence["created_status"], 201)
        self.assertEqual(evidence["created_code"], 0)
        self.assertEqual(evidence["login_status"], 200)
        self.assertEqual(evidence["profile_status"], 200)
        self.assertEqual(evidence["replay_status"], 409)
        self.assertEqual(evidence["replay_code"], 40931)
        self.assertEqual(evidence["wrong_secret_status"], 403)
        self.assertEqual(evidence["wrong_secret_code"], 40331)
        self.assertEqual(evidence["database_proof"], database_proof)
        self.assertEqual(evidence["jwt_source"], "auth-service-login-endpoint")
        self.assertFalse(evidence["jwt_self_signed_by_harness"])
        self.assertFalse(evidence["manual_database_role_seed"])
        self.assertFalse(evidence["secret_or_token_recorded"])
        serialized = json.dumps(evidence, sort_keys=True)
        for forbidden in (
            scenario.auth_bootstrap_secret,
            scenario.admin_password,
            token,
        ):
            self.assertNotIn(forbidden, serialized)
        gate._verify_auth_admin_bootstrap(scenario.h.evidence)
        gate._verify_no_secret_material(scenario.h.evidence)


class ManagedContextTests(unittest.TestCase):
    def context(self) -> dict:
        return {
            "schema_version": 1,
            "deployment": {"id": "worker-b", "service": "judge-worker", "node": "node-b"},
            "gateway": {"origin": "https://gateway-a:8443", "ca_file": "/run/ojos/service/ca.pem"},
            "bindings": {
                "storage_get": {
                    "binding_id": "binding-storage",
                    "api_id": "storage.object.get",
                    "base_path": "/internal/apis/storage.object.get",
                    "timeout_ms": 300000,
                }
            },
            "credential_file": "/run/ojos/service/token",
            "generation": 1,
        }

    def test_context_carries_references_but_no_credentials(self) -> None:
        gate.validate_service_context(self.context())

    def test_context_rejects_embedded_management_token(self) -> None:
        value = self.context()
        value["gateway_admin_token"] = "forbidden"
        with self.assertRaisesRegex(gate.GateError, "management"):
            gate.validate_service_context(value)

    def test_context_rejects_http_gateway(self) -> None:
        value = self.context()
        value["gateway"]["origin"] = "http://gateway-a:8080"
        with self.assertRaisesRegex(gate.GateError, "HTTPS"):
            gate.validate_service_context(value)

    def test_resource_ref_rejects_absolute_url_and_dot_segment(self) -> None:
        good = {
            "url": "",
            "binding": "storage_get",
            "api_id": "storage.object.get",
            "relative_path": "/problems/objects/sha256:test",
            "sha256": "sha256:" + "a" * 64,
            "size_bytes": 1,
        }
        gate.validate_resource_ref(good)
        absolute = copy.deepcopy(good)
        absolute["url"] = "https://minio/private"
        with self.assertRaisesRegex(gate.GateError, "URL"):
            gate.validate_resource_ref(absolute)
        traversal = copy.deepcopy(good)
        traversal["relative_path"] = "/problems/../private"
        with self.assertRaisesRegex(gate.GateError, "dot segments"):
            gate.validate_resource_ref(traversal)


def valid_evidence() -> dict:
    run_id = "abcde12345"
    dind_digest = gate.DEFAULT_DIND_IMAGE.rsplit("@", 1)[1]
    image_config_id = "sha256:" + "c" * 64
    image_repo_digests = ["docker.io/library/docker@" + dind_digest]
    engine_a_id = "11111111-1111-4111-8111-111111111111"
    engine_b_id = "22222222-2222-4222-8222-222222222222"
    resource = {
        "url": "",
        "binding": "storage_get",
        "api_id": "storage.object.get",
        "relative_path": "/objects/item",
        "sha256": "sha256:" + "a" * 64,
        "size_bytes": 1,
    }
    transitions = [
        "problem.transaction_committed_with_outbox",
        "problem.snapshot_published",
        "judge.inbox_recorded",
        "judge.problem_projection_applied",
        "judge.submission_froze_problem_revision",
        "judge.task_queued",
        "worker.long_poll_claimed",
        "worker.result_reported",
        "judge.submission_completed",
    ]
    denied_names = set(gate.PRIVATE_PORTS) | set(gate.MANAGEMENT_PORTS)
    return {
        "schema_version": 1,
        "status": "PASSED",
        "run_id": run_id,
        "cleanup_completed": True,
        "cleanup_errors": [],
        "engines": {
            "a": {
                "engine_id": engine_a_id,
                "local_engine_id": engine_a_id,
                "engine_name": "engine-a",
                "storage_driver": "vfs",
                "server_version": "29.0.0",
                "os_type": "linux",
                "docker_root_dir": "/var/lib/docker",
                "outer_container_id": "a" * 64,
                "outer_ip": "172.20.0.2",
                "host_endpoint": "tcp://127.0.0.1:32001",
                "host_endpoint_matches_local_socket": True,
                "data_volume": gate.safe_name(run_id, "engine-a-data"),
                "marker_volume": gate.safe_name(run_id, "only-a"),
                "image_config_id": image_config_id,
                "image_repo_digests": image_repo_digests.copy(),
            },
            "b": {
                "engine_id": engine_b_id,
                "local_engine_id": engine_b_id,
                "engine_name": "engine-b",
                "storage_driver": "vfs",
                "server_version": "29.0.0",
                "os_type": "linux",
                "docker_root_dir": "/var/lib/docker",
                "outer_container_id": "b" * 64,
                "outer_ip": "172.20.0.3",
                "host_endpoint": "tcp://127.0.0.1:32002",
                "host_endpoint_matches_local_socket": True,
                "data_volume": gate.safe_name(run_id, "engine-b-data"),
                "marker_volume": gate.safe_name(run_id, "only-b"),
                "image_config_id": image_config_id,
                "image_repo_digests": image_repo_digests.copy(),
            },
            "routing_proof": "host TCP endpoint ID matches outer unix socket",
            "storage_roots_distinct": True,
            "isolation_proof": "mutually-invisible marker volumes",
            "dind_image": gate.DEFAULT_DIND_IMAGE,
        },
        "engine_probe": {
            "a": {
                "host_endpoint": "tcp://127.0.0.1:32001",
                "host_engine_id": engine_a_id,
                "local_engine_id": engine_a_id,
                "local_identity": {
                    "engine_id": engine_a_id,
                    "engine_name": "engine-a",
                    "driver": "vfs",
                    "server_version": "29.0.0",
                    "os_type": "linux",
                    "docker_root_dir": "/var/lib/docker",
                },
                "outer_container_id": "a" * 64,
                "data_volume": gate.safe_name(run_id, "engine-a-data"),
                "engine_name": "engine-a",
                "driver": "vfs",
                "server_version": "29.0.0",
                "os_type": "linux",
                "docker_root_dir": "/var/lib/docker",
                "image_config_id": image_config_id,
                "image_repo_digests": image_repo_digests.copy(),
            },
            "b": {
                "host_endpoint": "tcp://127.0.0.1:32002",
                "host_engine_id": engine_b_id,
                "local_engine_id": engine_b_id,
                "local_identity": {
                    "engine_id": engine_b_id,
                    "engine_name": "engine-b",
                    "driver": "vfs",
                    "server_version": "29.0.0",
                    "os_type": "linux",
                    "docker_root_dir": "/var/lib/docker",
                },
                "outer_container_id": "b" * 64,
                "data_volume": gate.safe_name(run_id, "engine-b-data"),
                "engine_name": "engine-b",
                "driver": "vfs",
                "server_version": "29.0.0",
                "os_type": "linux",
                "docker_root_dir": "/var/lib/docker",
                "image_config_id": image_config_id,
                "image_repo_digests": image_repo_digests.copy(),
            },
            "dind_image": gate.DEFAULT_DIND_IMAGE,
        },
        "network_boundary": {
            "gateway_ready": True,
            "denied": [{"name": name, "denied": True} for name in sorted(denied_names)],
            "agent_connectivity": [
                {"name": name, "status": 200} for name in sorted(gate.MANAGEMENT_PORTS)
            ],
        },
        "managed_context": {
            "validated": True,
            "mount_read_only": True,
            "credential_embedded": False,
            "management_token_present": False,
        },
        "component_flow": {
            "gateway_evidence": {
                "transitions": transitions,
                "task_state": "succeeded",
                "result": {"status": "ACCEPTED"},
                "claim_prefer": "wait=25",
                "claim_wait_ms": 300,
                "identity_headers_removed": True,
                "workload_requests": 7,
                "task": {"source": resource, "problem_package": copy.deepcopy(resource)},
            }
        },
        "third_party_fixture": {
            "specialized_product_code": False,
            "consumer_evidence": {
                "response": {
                    "value": "cross-engine-binding-ok",
                    "provider": "contract-echo-provider",
                    "caller": "contract-echo-consumer",
                    "path": "/echo",
                }
            },
        },
    }


def valid_full_evidence() -> dict:
    value = valid_evidence()

    def runtime_projection(
        deployment_id: str, node_id: str, watermark: int
    ) -> dict:
        def payload(observed_at_ms: int) -> dict:
            return {
                "node_id": node_id,
                "last_observed_at_ms": observed_at_ms,
                "drift_reason": "",
                "instance": {
                    "deployment_id": deployment_id,
                    "desired_state": "RUNNING",
                    "observed_state": "RUNNING",
                    "health": "HEALTHY",
                    "runtime_attested": True,
                },
            }

        return {
            "completion_watermark_ms": watermark,
            "immediate_payload": payload(watermark),
            "inventory_payload": payload(watermark + 30_000),
        }

    def projection_integrity(
        phase: str,
        revision_id: str,
        content_sha256: str,
        captured_at_unix_ms: int,
        credential_generation: int,
    ) -> dict:
        route = {
            "binding_id": "binding-storage-get",
            "requirement_name": "storage_get",
            "consumer_deployment_id": "deployment-worker-b",
            "consumer_service_id": "judge-worker",
            "consumer_node_id": "node-b",
            "credential_generation": credential_generation,
            "api_id": "storage.object.get",
            "provider_deployment_id": "deployment-storage-a",
            "provider_service_id": "storage-service",
            "provider_node_id": "node-a",
            "provider_endpoint": "172.20.0.2:8085:storage-service",
            "upstream_base": "http://172.20.0.2:8085",
            "provider_path": "/api/storage",
            "virtual_path": "/internal/apis/storage.object.get",
            "auth_mode": "workload",
            "provider_auth_mode": "workload",
            "permission": "storage.object.read",
            "methods": ["GET", "HEAD"],
            "timeout_ms": 300000,
        }
        grant = {
            "binding_id": route["binding_id"],
            "requirement_name": route["requirement_name"],
            "consumer_deployment_id": route["consumer_deployment_id"],
            "consumer_service_id": route["consumer_service_id"],
            "consumer_node_id": route["consumer_node_id"],
            "credential_generation": credential_generation,
            "api_id": route["api_id"],
            "permission": route["permission"],
        }
        projection = {"routes": [route], "grants": [grant]}
        digest = gate.effective_projection_sha256(
            projection["routes"], projection["grants"]
        )
        providers = {}
        for provider in ("gateway", "auth"):
            providers[provider] = {
                "source": "provider-present-status-and-durable-projection",
                "api_version": "v1",
                "provider": provider,
                "topology_id": "cross-machine-a-b",
                "absent": False,
                "observed_revision_id": revision_id,
                "observed_content_sha256": content_sha256,
                "observed_projection_sha256": digest,
                "recomputed_projection_sha256": digest,
                "projection": copy.deepcopy(projection),
                "route_count": 1,
                "grant_count": 1,
                "matches_expected": True,
            }
        return {
            "phase": phase,
            "captured_at_unix_ms": captured_at_unix_ms,
            "topology_id": "cross-machine-a-b",
            "applied_revision_id": revision_id,
            "applied_content_sha256": content_sha256,
            "topology_etag": f'"{revision_id}"',
            "topology_status_state": "IN_SYNC",
            "topology_status_drift": [],
            "expected_projection_sha256": digest,
            "providers": providers,
            "all_match": True,
        }

    value["network_boundary"]["worker_bridge_denied"] = copy.deepcopy(
        value["network_boundary"]["denied"]
    )
    value.update(
        {
            "gate": "cross-machine-service-contract-v2",
            "mode": "full-components",
            "run_id": value["run_id"],
            "started_at_unix": 1_900_000_000,
            "completed_at_unix": 1_900_000_900,
            "build_identity": {
                "version": "1.0.0-rc.1",
                "commit_sha": "1" * 40,
                "profile": "production",
                "target": "x86_64-unknown-linux-gnu",
            },
            "control_plane_runtime": {
                "evidence_source": "docker-inspect",
                "container_id": "c" * 64,
                "engine_id": value["engines"]["a"]["engine_id"],
                "running": True,
                "docker_health": "HEALTHY",
                "healthcheck_url": gate.CONTROL_PLANE_HEALTHCHECK_URL,
                "healthcheck_ca_cert": gate.CONTROL_PLANE_HEALTHCHECK_CA_CERT,
                "tls_enabled": True,
            },
            "auth_admin_bootstrap": {
                "created_status": 201,
                "created_code": 0,
                "created_user_id": "1",
                "login_status": 200,
                "login_code": 0,
                "login_user_matches_bootstrap": True,
                "login_has_super_admin": True,
                "login_has_system_admin": True,
                "profile_status": 200,
                "profile_code": 0,
                "profile_authenticated_same_user": True,
                "replay_status": 409,
                "replay_code": 40931,
                "wrong_secret_status": 403,
                "wrong_secret_code": 40331,
                "database_proof": {
                    "marker_completed": True,
                    "marker_user_id": "1",
                    "super_admin_assigned": True,
                    "bootstrap_audit_count": 1,
                },
                "jwt_source": "auth-service-login-endpoint",
                "jwt_self_signed_by_harness": False,
                "manual_database_role_seed": False,
                "database_transactional": True,
                "secret_or_token_recorded": False,
            },
        }
    )
    task_resource = copy.deepcopy(
        value["component_flow"]["gateway_evidence"]["task"]["source"]
    )
    task_resource.pop("url", None)
    package_resource = copy.deepcopy(task_resource)
    package_resource["relative_path"] = "/objects/package"
    package_resource["sha256"] = "sha256:" + "d" * 64
    value["deployment_via_store_agent"] = True
    value["worker_implementation"] = "repository Rust judge-worker image (Agent-created)"
    value["component_flow"].update(
        {
            "same_chain": True,
            "source": "actual-services",
            "problem_created_via_http_api": True,
            "submission_created_via_http_api": True,
            "manual_judge_problem_insert": False,
            "problem": {
                "problem_id": "problem-7",
                "aggregate_version": 3,
                "package_revision": 2,
                "outbox_event_id": "event-problem-7-v3",
                "event_type": "io.ojos.problem.snapshot.v1",
                "package_sha256": package_resource["sha256"],
            },
            "judge_projection": {
                "problem_id": "problem-7",
                "aggregate_version": 3,
                "event_id": "event-problem-7-v3",
                "package_sha256": package_resource["sha256"],
            },
            "submission": {
                "submission_id": "submission-9",
                "problem_id": "problem-7",
                "package_sha256": package_resource["sha256"],
            },
            "task": {
                "task_id": "task-11",
                "submission_id": "submission-9",
                "problem_id": "problem-7",
                "source": task_resource,
                "problem_package": package_resource,
                "wire_capture": "gateway TLS ingress claim response",
            },
            "result": {
                "result_id": "result-12",
                "task_id": "task-11",
                "submission_id": "submission-9",
                "status": "ACCEPTED",
            },
            "actual_components": [
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
            ],
        }
    )
    etag = '"revision-worker-compensation-recovery"'
    bindings = [
        {"requirement": "judge_control", "provider_deployment_id": "judge-api-a"},
        {"requirement": "storage_get", "provider_deployment_id": "storage-a"},
    ]
    value["store_agent_evidence"] = {
        "agent": {
            "enrolled": True,
            "mtls": True,
            "node_id": "node-b",
            "instance_id": "agent-b-instance-1",
            "certificate_serial": "01ab",
            "runtime_health_sample": "final-agent-report",
            "runtime_health": {
                "node_id": "node-b",
                "status": "READY",
                "ready": True,
                "accepting_jobs": True,
                "agent_reachable": True,
                "last_observed_at": "unix-ms:1900000800000",
                "observation_age_ms": 1_000,
                "freshness_threshold_ms": 60_000,
                "unhealthy_deployments": 0,
            },
        },
        "store_validate": {
            "accepted": True,
            "request_id": "request-validate-worker",
            "topology_etag": etag,
            "bindings": copy.deepcopy(bindings),
            "request_fields": [
                "bindings",
                "catalog_source_id",
                "channel",
                "service_id",
                "target_node_id",
                "topology_etag",
                "topology_id",
                "version",
            ],
        },
        "store_install": {
            "accepted": True,
            "request_id": "request-install-worker",
            "topology_etag": etag,
            "bindings": copy.deepcopy(bindings),
            "request_fields": [
                "bindings",
                "catalog_source_id",
                "channel",
                "mode",
                "service_id",
                "start",
                "target_node_id",
                "topology_etag",
                "topology_id",
                "version",
            ],
            "request_endpoint_present": False,
            "response_published_endpoint": None,
            "topology_logical_endpoint": "172.20.0.3:9101:judge-worker",
            "automatic_logical_endpoint": True,
            "operation_id": "operation-install-worker",
            "deployment_id": "deployment-worker-b",
        },
        "operation": {
            "operation_id": "operation-install-worker",
            "status": "SUCCEEDED",
            "job_id": "job-install-worker",
        },
        "agent_job": {
            "job_id": "job-install-worker",
            "attempt_id": "attempt-install-worker-1",
            "lease_id": "lease-install-worker-1",
            "lease_owner_instance_id": "agent-b-instance-1",
            "status": "SUCCEEDED",
            "completed_by_agent": True,
        },
        "deployment": {
            "deployment_id": "deployment-worker-b",
            "node_id": "node-b",
            "desired_state": "RUNNING",
            "observed_state": "RUNNING",
            "health": "HEALTHY",
            "runtime_profile": "judge-sandbox-v1",
            "runtime_attested": True,
            "drift_reason": "",
            "last_observed_at_ms": 1_900_000_030_000,
            "runtime_projection": runtime_projection(
                "deployment-worker-b", "node-b", 1_900_000_000_000
            ),
        },
        "bindings": [
            {
                "requirement_name": item["requirement"],
                "binding_id": "binding-" + item["requirement"].replace("_", "-"),
                "provider_deployment_id": item["provider_deployment_id"],
                "desired_state": "ACTIVE",
                "observed_state": "ACTIVE",
                "health": "HEALTHY",
            }
            for item in bindings
        ],
        "service_context": {
            "generation": 1,
            "deployment_id": "deployment-worker-b",
            "node_id": "node-b",
            "binding_ids": ["binding-judge-control", "binding-storage-get"],
            "mount_read_only": True,
            "credential_embedded": False,
            "management_token_present": False,
        },
        "runtime": {
            "created_by_agent": True,
            "context_mount_read_only": True,
            "health_gate": "HEALTHY",
            "runtime_profile": "judge-sandbox-v1",
            "host_config_digest": "sha256:" + "b" * 64,
            "image_repo_digest": "sha256:" + "c" * 64,
            "container_id": "0123456789abcdef",
            "engine_id": value["engines"]["b"]["engine_id"],
        },
    }
    recovered_flow = copy.deepcopy(value["component_flow"])
    recovered_flow["workload_transcript_correlated"] = True
    recovered_flow["problem"].update(
        {
            "problem_id": "problem-recovered",
            "outbox_event_id": "event-problem-recovered-v1",
        }
    )
    recovered_flow["judge_projection"].update(
        {
            "problem_id": "problem-recovered",
            "event_id": "event-problem-recovered-v1",
        }
    )
    recovered_flow["submission"].update(
        {"submission_id": "submission-recovered", "problem_id": "problem-recovered"}
    )
    recovered_flow["task"].update(
        {
            "task_id": "task-recovered",
            "submission_id": "submission-recovered",
            "problem_id": "problem-recovered",
        }
    )
    recovered_flow["result"].update(
        {
            "result_id": "result-recovered",
            "task_id": "task-recovered",
            "submission_id": "submission-recovered",
        }
    )
    worker_container_id = value["store_agent_evidence"]["runtime"]["container_id"]
    gateway_container_id = "a" * 64
    judge_container_id = "b" * 64
    value["worker_recovery"] = {
        "worker_deployment_id": "deployment-worker-b",
        "worker_container_id_before": worker_container_id,
        "worker_container_id_after": worker_container_id,
        "capture_baseline_sequence": 40,
        "disruption_started_at_unix_ms": 1_900_000_100_000,
        "restore_started_at_unix_ms": 1_900_000_130_000,
        "disrupted_services": [
            {"name": "gateway", "container_id": gateway_container_id},
            {"name": "judge-api", "container_id": judge_container_id},
        ],
        "restored_services": [
            {
                "name": "gateway",
                "container_id": gateway_container_id,
                "running": True,
            },
            {
                "name": "judge-api",
                "container_id": judge_container_id,
                "running": True,
                "health": "HEALTHY",
            },
        ],
        "health_timeline": [
            {
                "phase": "before-disruption",
                "observed_at_unix_ms": 1_900_000_099_000,
                "container_id": worker_container_id,
                "running": True,
                "status": "HEALTHY",
            },
            {
                "phase": "services-unavailable",
                "observed_at_unix_ms": 1_900_000_120_000,
                "container_id": worker_container_id,
                "running": True,
                "status": "UNHEALTHY",
            },
            {
                "phase": "services-restored",
                "observed_at_unix_ms": 1_900_000_140_000,
                "container_id": worker_container_id,
                "running": True,
                "status": "HEALTHY",
            },
        ],
        "reregistration": {
            "sequence": 41,
            "captured_at_unix_ms": 1_900_000_135_000,
            "method": "POST",
            "path": "/internal/apis/judge.worker.control/register",
            "status": 200,
            "worker_id": "deployment-worker-b",
        },
        "recovered_flow": recovered_flow,
        "provider_projection_integrity": projection_integrity(
            "worker-recovery",
            "revision-worker-installed",
            "a" * 64,
            1_900_000_150_000,
            1,
        ),
    }
    value["a_business_stack_mode"] = "production-service-contract-v2"
    value["a_agent_evidence"] = {
        "identity_source": "agent-enroll-output",
        "enrolled": True,
        "mtls": True,
        "node_id": "node-a",
        "instance_id": "agent-a-instance-1",
        "certificate_serial": "02cd",
        "management_credentials_present": False,
        "management_environment_inspected": True,
        "forbidden_management_environment": [],
        "container_id": "a" * 64,
        "engine_id": value["engines"]["a"]["engine_id"],
        "runtime_health_sample": "final-agent-report",
        "runtime_health": {
            "node_id": "node-a",
            "status": "READY",
            "ready": True,
            "accepting_jobs": True,
            "agent_reachable": True,
            "last_observed_at": "unix-ms:1900000800000",
            "observation_age_ms": 1_000,
            "freshness_threshold_ms": 35_000,
            "unhealthy_deployments": 0,
        },
    }
    value["managed_a_network"] = {
        "evidence_source": "live-socket-and-psql-probes",
        "source_network": "engine-a-default-bridge",
        "engine_id": value["engines"]["a"]["engine_id"],
        "postgres_plaintext_rejected": True,
        "targets": [
            {"name": name, "connected": True, "elapsed_ms": 1}
            for name in (
                "postgresql-tls",
                "redis-events",
                "minio-s3",
                "gateway-workload",
                "control-plane",
                "oci-registry",
            )
        ],
        "postgres_tls": {
            "verify_full_succeeded": True,
            "server_ssl_enabled": True,
            "plaintext_rejected": True,
            "ca_sha256": "sha256:" + "2" * 64,
            "verified_server_name": "172.20.0.2",
        },
    }

    storage_deployment_id = "deployment-storage-a"
    auth_deployment_id = "deployment-auth-a"

    def managed_a_deployment(
        service_id: str,
        deployment_id: str,
        container_digit: str,
        requirements: list[str],
        publishes: list[str],
        subscribes: list[str],
    ) -> dict:
        bindings = []
        for requirement in requirements:
            provider = (
                auth_deployment_id
                if requirement == "permission_check"
                else storage_deployment_id
            )
            bindings.append(
                {
                    "requirement_name": requirement,
                    "binding_id": f"binding-{service_id}-{requirement}",
                    "provider_deployment_id": provider,
                    "desired_state": "ACTIVE",
                    "observed_state": "ACTIVE",
                    "health": "HEALTHY",
                }
            )
        context_required = bool(requirements)
        event_required = bool(publishes or subscribes)
        job_id = "job-install-" + service_id
        return {
            "service_id": service_id,
            "deployment_id": deployment_id,
            "node_id": "node-a",
            "created_by_agent": True,
            "container_id": container_digit * 64,
            "image_repo_digest": "sha256:" + container_digit * 64,
            "host_config_digest": "sha256:" + container_digit * 64,
            "engine_id": value["engines"]["a"]["engine_id"],
            "desired_state": "RUNNING",
            "observed_state": "RUNNING",
            "health": "HEALTHY",
            "runtime_attested": True,
            "drift_reason": "",
            "last_observed_at_ms": 1_900_000_030_000,
            "runtime_projection": runtime_projection(
                deployment_id, "node-a", 1_900_000_000_000
            ),
            "operation_id": "operation-install-" + service_id,
            "operation_status": "SUCCEEDED",
            "agent_job": {
                "job_id": job_id,
                "attempt_id": job_id + ":attempt:1",
                "lease_id": "sha256:" + container_digit * 64,
                "lease_owner_instance_id": "agent-a-instance-1",
                "status": "SUCCEEDED",
                "completed_by_agent": True,
            },
            "bindings": bindings,
            "binding_requirements": sorted(requirements),
            "service_context": (
                {
                    "required": True,
                    "present": True,
                    "generation": 1,
                    "mount_read_only": True,
                    "credential_embedded": False,
                    "management_token_present": False,
                    "binding_ids": [item["binding_id"] for item in bindings],
                }
                if context_required
                else {
                    "required": False,
                    "present": False,
                    "binding_ids": [],
                    "generation": None,
                    "mount_read_only": False,
                    "credential_embedded": False,
                    "management_token_present": False,
                }
            ),
            "event_context": (
                {
                    "required": True,
                    "present": True,
                    "generation": 1,
                    "connection_id": "a-events",
                    "stream": "ojos:events:v1",
                    "publish_types": publishes,
                    "subscriptions": [
                        {"event_type": event_type, "consumer_group": "judge-api"}
                        for event_type in subscribes
                    ],
                    "connection_secret_recorded": False,
                }
                if event_required
                else {
                    "required": False,
                    "present": False,
                    "generation": None,
                    "connection_id": None,
                    "stream": None,
                    "publish_types": [],
                    "subscriptions": [],
                    "connection_secret_recorded": False,
                }
            ),
            "legacy_environment_present": False,
        }

    value["managed_a_deployments"] = {
        "storage-service": managed_a_deployment(
            "storage-service", storage_deployment_id, "3", [], [], []
        ),
        "problem-service": managed_a_deployment(
            "problem-service",
            "deployment-problem-a",
            "4",
            ["permission_check", "storage_put", "storage_head", "storage_delete"],
            ["io.ojos.problem.snapshot.v1", "io.ojos.problem.deleted.v1"],
            [],
        ),
        "judge-api": managed_a_deployment(
            "judge-api",
            "deployment-judge-a",
            "5",
            ["permission_check", "storage_get", "storage_put", "storage_head"],
            [],
            ["io.ojos.problem.snapshot.v1", "io.ojos.problem.deleted.v1"],
        ),
    }

    failed_worker_id = "deployment-worker-b"
    failed_worker_component = hashlib.sha256(
        failed_worker_id.encode("utf-8")
    ).hexdigest()[:32]
    compensation_gateway_id = "9" * 64
    consumer_rollbacks = []
    for service_id in ("problem-service", "judge-api"):
        deployment = value["managed_a_deployments"][service_id]
        binding_names = list(deployment["binding_requirements"])
        binding_ids = [
            item["binding_id"] for item in deployment["bindings"]
        ]
        api_ids = {
            "permission_check": "auth.user.permission.check",
            "storage_put": "storage.object.put",
            "storage_get": "storage.object.get",
            "storage_head": "storage.object.head",
            "storage_delete": "storage.object.delete",
        }
        binding_routes = [
            {
                "requirement_name": item["requirement_name"],
                "binding_id": item["binding_id"],
                "api_id": api_ids[item["requirement_name"]],
                "base_path": "/internal/apis/" + api_ids[item["requirement_name"]],
                "timeout_ms": 35_000,
            }
            for item in deployment["bindings"]
        ]
        binding_routes.sort(key=lambda route: route["requirement_name"])
        context_sha = "sha256:" + ("6" if service_id == "problem-service" else "7") * 64
        context_sha_recovered = (
            "sha256:" + ("a" if service_id == "problem-service" else "b") * 64
        )
        credential_sha_before = (
            "sha256:" + ("8" if service_id == "problem-service" else "9") * 64
        )
        credential_sha_after = (
            "sha256:" + ("c" if service_id == "problem-service" else "d") * 64
        )
        credential_sha_recovered = (
            "sha256:" + ("e" if service_id == "problem-service" else "f") * 64
        )
        generation = 3
        claims_before = {
            "deployment_id": deployment["deployment_id"],
            "service_id": service_id,
            "node_id": "node-a",
            "credential_generation": generation,
            "issuer": "ojos-auth/workload",
            "audience": ["ojos-gateway"],
            "expires_at_unix": 2_000_000_000,
            "jti_sha256": "sha256:" + "1" * 64,
        }
        claims_after = {
            **claims_before,
            "expires_at_unix": 2_000_000_300,
            "jti_sha256": "sha256:" + "2" * 64,
        }
        claims_recovered = {
            **claims_after,
            "credential_generation": generation + 1,
            "expires_at_unix": 2_000_000_600,
            "jti_sha256": "sha256:" + "3" * 64,
        }
        consumer_rollbacks.append(
            {
                "service_id": service_id,
                "deployment_id": deployment["deployment_id"],
                "node_id": "node-a",
                "container_id_before": deployment["container_id"],
                "container_id_after": deployment["container_id"],
                "context_generation_before": generation,
                "context_generation_after": generation,
                "context_generation_recovered": generation + 1,
                "binding_names_before": binding_names,
                "binding_names_after": copy.deepcopy(binding_names),
                "binding_names_recovered": copy.deepcopy(binding_names),
                "binding_ids_before": binding_ids,
                "binding_ids_after": copy.deepcopy(binding_ids),
                "binding_ids_recovered": copy.deepcopy(binding_ids),
                "binding_routes_before": binding_routes,
                "binding_routes_after": copy.deepcopy(binding_routes),
                "binding_routes_recovered": copy.deepcopy(binding_routes),
                "context_sha256_before": context_sha,
                "context_sha256_after": context_sha,
                "context_sha256_recovered": context_sha_recovered,
                "credential_claims_before": claims_before,
                "credential_claims_after": claims_after,
                "credential_claims_recovered": claims_recovered,
                "workload_credential_file_sha256_before": credential_sha_before,
                "workload_credential_file_sha256_after": credential_sha_after,
                "workload_credential_file_sha256_recovered": (
                    credential_sha_recovered
                ),
                "context_content_unchanged": True,
                "credential_claim_identity_unchanged": True,
                "credential_expiry_non_decreasing": True,
                "credential_refresh_during_fault_window": True,
                "rollback_generation_increment": 1,
                "context_and_credential_generation_aligned": True,
                "context_content_rotated": True,
                "credential_file_rotated": True,
                "route_identity_preserved": True,
            }
        )
    value["worker_install_failure_compensation"] = {
        "fault": {
            "kind": "stop-container",
            "component": "gateway-tls-a",
            "container_id": compensation_gateway_id,
            "started_at_unix_ms": 1_900_000_010_000,
            "running_before_fault": True,
            "running_at_install_start": False,
            "running_at_install_completion": False,
        },
        "failed_deployment": {
            "deployment_id": failed_worker_id,
            "node_id": "node-b",
            "logical_endpoint": "172.20.0.3:9101:judge-worker",
            "cache_volume_name": "ojos-judge-cache-" + failed_worker_component,
            "context_directory": (
                "/var/lib/ojos-agent/runtime-contexts/" + failed_worker_component
            ),
        },
        "operation": {
            "operation_id": "operation-install-worker-faulted",
            "status": "FAILED",
            "needs_attention": False,
            "attention_job_ids_count": 0,
            "resource_cleanup_derived_from_operation_result": False,
        },
        "agent_attempt": {
            "job_id": "job-install-worker-faulted",
            "node_id": "node-b",
            "lease_owner_instance_id": "agent-b-instance-1",
            "attempt": 3,
            "status": "FAILED",
            "result_action": "install",
            "result_compensated": True,
            "removed_container_id": "4" * 64,
            "failure_health_gate": "timeout",
            "failure_probe_count": 7,
            "last_health_observation": {
                "probe": 7,
                "observed_state": "RUNNING",
                "health": "UNHEALTHY",
                "probe_reason": "container healthcheck reported unhealthy",
            },
            "post_start_health_gate_failure": True,
        },
        "container_readback": {
            "source": "docker-ps-by-deployment-label",
            "deployment_id": failed_worker_id,
            "expected_name": "ojos-" + failed_worker_id,
            "matches": [],
            "exact_name_inspect_exit_code": 1,
            "exact_name_absent": True,
            "absent": True,
        },
        "volume_readback": {
            "source": "docker-volume-ls-by-deployment-label",
            "deployment_id": failed_worker_id,
            "expected_name": "ojos-judge-cache-" + failed_worker_component,
            "matches": [],
            "exact_name_inspect_exit_code": 1,
            "exact_name_absent": True,
            "absent": True,
        },
        "context_readback": {
            "source": "node-b-agent-host-filesystem",
            "deployment_id": failed_worker_id,
            "path": "/var/lib/ojos-agent/runtime-contexts/" + failed_worker_component,
            "exists": False,
            "context_or_credential_file_present": False,
        },
        "runtime_readback": {
            "source": "GET /api/v1/deployments/{deploymentId}",
            "http_status": 404,
            "problem_code": "DEPLOYMENT_NOT_FOUND",
            "fake_running_projection_present": False,
        },
        "binding_readback": {
            "source": "GET /api/v1/deployments/{deploymentId}/bindings",
            "http_status": 404,
            "problem_code": "DEPLOYMENT_NOT_FOUND",
            "staged_or_active_present": False,
        },
        "control_plane_database_readback": {
            "runtime_instance_count": 0,
            "binding_count": 0,
            "active_or_staged_binding_count": 0,
            "query_mode": "postgres-fixed-parameterized-read-only-transaction",
            "control_plane_database_read_only_verification": True,
            "business_database_write_used": False,
            "row_payload_recorded": False,
        },
        "topology_readback": {
            "source": "GET /api/v1/topologies/{topologyId}",
            "topology_id": "cross-machine-a-b",
            "selected_revision_id": "revision-1",
            "baseline_status_desired_revision_id": "revision-1",
            "baseline_status_observed_revision_id": "revision-1",
            "baseline_status_state": "IN_SYNC",
            "baseline_status_drift": [],
            "operation_proposed_revision_id": "revision-worker-failed-draft",
            "draft_revision_id_after": "revision-worker-failed-draft",
            "draft_etag_after": '"revision-worker-failed-draft"',
            "applied_revision_id_after": "revision-1",
            "observed_revision_id_after": "revision-1",
            "desired_revision_id_after": "revision-1",
            "status_state_after": "IN_SYNC",
            "status_last_operation_id": "operation-install-worker-faulted",
            "status_drift": [],
            "status_snapshot_kind": "stable-reconciled-applied",
            "applying_revision_present": False,
            "selected_revision_readback_etag": '"revision-1"',
            "selected_revision_number": 1,
            "selected_content_sha256": "a" * 64,
            "selected_spec_sha256_before": "sha256:" + "a" * 64,
            "selected_spec_sha256_readback": "sha256:" + "a" * 64,
            "failed_draft_readback_etag": '"revision-worker-failed-draft"',
            "failed_draft_revision_number": 2,
            "failed_draft_parent_revision_id": "revision-1",
            "failed_draft_rollback_of_revision_id": None,
            "failed_draft_content_sha256": "b" * 64,
            "failed_draft_spec_sha256": "sha256:" + "b" * 64,
            "failed_draft_endpoint_count": 1,
            "failed_draft_link_count": 2,
            "failed_draft_requirements": ["judge_control", "storage_get"],
            "failed_draft_provider_deployment_ids": ["judge-api-a", "storage-a"],
            "failed_draft_retained": True,
            "applied_runtime_preserved": True,
            "recovery_revision_id": "revision-worker-compensation-recovery",
            "next_retry_etag": etag,
        },
        "gateway_active_projection_readback": {
            "source": "redis-get-gateway-topology-projection",
            "key": "ojos:gateway:topology-projection:v1:cross-machine-a-b",
            "index_key": "ojos:gateway:topology-projections:v1",
            "index_member_before": 1,
            "index_member_after": 1,
            "provider": "gateway",
            "topology_id": "cross-machine-a-b",
            "active_revision_id": "revision-1",
            "active_content_sha256": "a" * 64,
            "active_spec_sha256": "sha256:" + "a" * 64,
            "active_route_count": 8,
            "active_grant_count": 8,
            "business_sha256_before": "sha256:" + "c" * 64,
            "business_sha256_after": "sha256:" + "c" * 64,
            "routes_sha256_before": "sha256:" + "d" * 64,
            "routes_sha256_after": "sha256:" + "d" * 64,
            "grants_sha256_before": "sha256:" + "e" * 64,
            "grants_sha256_after": "sha256:" + "e" * 64,
            "operation_id_before": "operation-before-worker-fault",
            "operation_id_after": "operation-before-worker-fault",
            "updated_at_before": "2030-01-01T00:00:00Z",
            "updated_at_after": "2030-01-01T00:00:00Z",
            "failed_deployment_route_count": 0,
            "failed_deployment_grant_count": 0,
            "failed_deployment_endpoint_count": 0,
            "previous_projection_preserved": True,
            "business_database_write_used": False,
        },
        "auth_active_projection_readback": {
            "source": "postgres-auth-projection-read-only-transaction",
            "provider": "auth",
            "topology_id": "cross-machine-a-b",
            "active_revision_id": "revision-1",
            "active_content_sha256": "a" * 64,
            "active_spec_sha256": "sha256:" + "a" * 64,
            "active_route_count": 8,
            "active_grant_count": 8,
            "business_sha256_before": "sha256:" + "f" * 64,
            "business_sha256_after": "sha256:" + "f" * 64,
            "routes_sha256_before": "sha256:" + "d" * 64,
            "routes_sha256_after": "sha256:" + "d" * 64,
            "grants_sha256_before": "sha256:" + "e" * 64,
            "grants_sha256_after": "sha256:" + "e" * 64,
            "materialized_grant_count_before": 8,
            "materialized_grant_count_after": 8,
            "materialized_grants_sha256_before": "sha256:" + "7" * 64,
            "materialized_grants_sha256_after": "sha256:" + "7" * 64,
            "failed_deployment_route_count": 0,
            "failed_deployment_grant_count": 0,
            "failed_deployment_materialized_grant_count": 0,
            "previous_projection_preserved": True,
            "business_database_write_used": False,
        },
        "durable_binding_set_readback": {
            "source": "postgres-control-plane-bindings-read-only-transaction",
            "selected_revision_id": "revision-1",
            "binding_count_before": 8,
            "binding_count_after": 8,
            "active_count_before": 8,
            "active_count_after": 8,
            "non_active_count_after": 0,
            "wrong_revision_count_after": 0,
            "rows_sha256_before": "sha256:" + "8" * 64,
            "rows_sha256_after": "sha256:" + "8" * 64,
            "failed_deployment_binding_count": 0,
            "exactly_preserved": True,
            "business_database_write_used": False,
        },
        "recovery_rollback": {
            "api_path": "/api/v1/topologies/cross-machine-a-b:rollback",
            "topology_id": "cross-machine-a-b",
            "request_revision_id": "revision-1",
            "request_if_match": '"revision-worker-failed-draft"',
            "target_revision_id": "revision-1",
            "target_revision_number": 1,
            "target_content_sha256": "a" * 64,
            "target_spec_sha256": "sha256:" + "a" * 64,
            "parent_revision_id": "revision-worker-failed-draft",
            "parent_revision_number": 2,
            "parent_content_sha256": "b" * 64,
            "created_revision_id": "revision-worker-compensation-recovery",
            "created_revision_number": 3,
            "created_parent_revision_id": "revision-worker-failed-draft",
            "created_rollback_of_revision_id": "revision-1",
            "created_content_sha256": "a" * 64,
            "created_spec_sha256": "sha256:" + "a" * 64,
            "created_revision_etag": etag,
            "operation_id": "operation-worker-compensation-rollback",
            "operation_action": "topology.rollback",
            "operation_status": "SUCCEEDED",
            "draft_revision_id": "revision-worker-compensation-recovery",
            "applied_revision_id": "revision-worker-compensation-recovery",
            "applying_revision_id": None,
            "status_desired_revision_id": "revision-worker-compensation-recovery",
            "status_observed_revision_id": "revision-worker-compensation-recovery",
            "status_state": "IN_SYNC",
            "status_drift": [],
            "status_last_operation_id": "operation-worker-compensation-rollback",
            "gateway_projection_revision_id": "revision-worker-compensation-recovery",
            "gateway_projection_content_sha256": "a" * 64,
            "gateway_projection_spec_sha256": "sha256:" + "a" * 64,
            "gateway_stable_routes_sha256_before": "sha256:" + "d" * 64,
            "gateway_stable_routes_sha256_recovered": "sha256:" + "d" * 64,
            "gateway_stable_grants_sha256_before": "sha256:" + "e" * 64,
            "gateway_stable_grants_sha256_recovered": "sha256:" + "e" * 64,
            "gateway_consumer_generations_before": {
                "deployment-problem-a": 3,
                "deployment-judge-a": 3,
            },
            "gateway_consumer_generations_recovered": {
                "deployment-problem-a": 4,
                "deployment-judge-a": 4,
            },
            "gateway_index_member": 1,
            "auth_projection_revision_id": "revision-worker-compensation-recovery",
            "auth_projection_content_sha256": "a" * 64,
            "auth_projection_spec_sha256": "sha256:" + "a" * 64,
            "auth_stable_routes_sha256_before": "sha256:" + "d" * 64,
            "auth_stable_routes_sha256_recovered": "sha256:" + "d" * 64,
            "auth_stable_grants_sha256_before": "sha256:" + "e" * 64,
            "auth_stable_grants_sha256_recovered": "sha256:" + "e" * 64,
            "auth_consumer_generations_before": {
                "deployment-problem-a": 3,
                "deployment-judge-a": 3,
            },
            "auth_consumer_generations_recovered": {
                "deployment-problem-a": 4,
                "deployment-judge-a": 4,
            },
            "auth_materialized_stable_grants_sha256_before": (
                "sha256:" + "7" * 64
            ),
            "auth_materialized_stable_grants_sha256_recovered": (
                "sha256:" + "7" * 64
            ),
            "auth_materialized_consumer_generations_before": {
                "deployment-problem-a": 3,
                "deployment-judge-a": 3,
            },
            "auth_materialized_consumer_generations_recovered": {
                "deployment-problem-a": 4,
                "deployment-judge-a": 4,
            },
            "auth_failed_deployment_grant_count": 0,
            "durable_binding_count": 8,
            "durable_binding_active_count": 8,
            "durable_binding_non_active_count": 0,
            "durable_binding_wrong_revision_count": 0,
            "durable_binding_business_sha256_before": "sha256:" + "9" * 64,
            "durable_binding_business_sha256_recovered": "sha256:" + "9" * 64,
            "durable_consumer_generations_before": {
                "deployment-problem-a": 3,
                "deployment-judge-a": 3,
            },
            "durable_consumer_generations_recovered": {
                "deployment-problem-a": 4,
                "deployment-judge-a": 4,
            },
            "affected_consumer_deployment_ids": [
                "deployment-judge-a",
                "deployment-problem-a",
            ],
            "each_consumer_generation_increment": 1,
            "all_generation_sources_aligned": True,
            "next_retry_etag": etag,
            "business_state_preserved": True,
        },
        "consumer_context_rollback": consumer_rollbacks,
        "gateway_recovery": {
            "component": "gateway-tls-a",
            "container_id_before": compensation_gateway_id,
            "container_id_after": compensation_gateway_id,
            "same_container": True,
            "running": True,
            "public_health_status": 200,
            "node_b_ready": True,
            "node_b_unhealthy_deployments": 0,
        },
        "credential_material_recorded": False,
    }

    def gc_intent(digit: str, size: int, kind: str) -> dict:
        digest = digit * 64
        if kind == "package":
            relative_path = "/problems/package-sha256-" + digest + ".zip"
        else:
            relative_path = "/problems/problem-42-objects-sha256-" + digest
        return {
            "artifact_uri": "storage:/" + relative_path,
            "sha256": digest,
            "size_bytes": size,
            "kind": kind,
            "initial_status": "PENDING",
            "upload_completed_at": "2030-01-01T00:00:59Z",
            "relative_path": relative_path,
            "head_before": {
                "status": 200,
                "sha256_header": digest,
                "size_bytes": size,
                "storage_result_header": "present",
            },
            "recovery_action": "retry" if kind == "content" else "reconcile",
            "recovery_action_id": (
                "action-operator-retry" if kind == "content" else "action-package-reconcile"
            ),
            "head_after": {
                "status": 404,
                "sha256_header": "",
                "size_bytes": -1,
                "storage_result_header": "object-not-found",
            },
        }

    gc_intents = [gc_intent("8", 0, "content"), gc_intent("9", 123, "package")]
    gc_head_paths = sorted(
        "/internal/apis/storage.object.head" + intent["relative_path"]
        for intent in gc_intents
    )
    gc_delete_paths = sorted(
        "/internal/apis/storage.object.delete" + intent["relative_path"]
        for intent in gc_intents
    )
    gc_target = gc_intents[0]
    route_identity = {
        "requirement_name": "storage_head",
        "api_id": "storage.object.head",
        "api_version": "1.0.0",
        "consumer_deployment_id": "deployment-problem-a",
        "consumer_endpoint": "172.28.0.15:8083:problem-service",
        "required_binding_preserved": True,
    }
    operator_action = {
        "endpoint": "/api/problem/admin/artifact-gc/intents:retry",
        "first_http_status": 202,
        "replay_http_status": 202,
        "action_id": "action-operator-retry",
        "request_id": "request-operator-retry",
        "artifact_uri": gc_target["artifact_uri"],
        "queued": True,
        "first_request_replay": False,
        "duplicate_request_replay": True,
        "duplicate_action_id_matched": True,
        "duplicate_request_id_matched": True,
        "idempotency_key_used": True,
        "idempotency_key_recorded": False,
        "reason_recorded": True,
        "from_status": "NEEDS_ATTENTION",
        "to_status": "PENDING",
    }
    value["problem_artifact_gc"] = {
        "setup": {
            "method": "duplicate-problem-no-http-conflict",
            "request_marker": "artifact-gc-recovery-" + value["run_id"],
            "problem_no": "GC" + value["run_id"],
            "seed_problem_id": "41",
            "seed_status": 200,
            "failure_status": 500,
            "baseline_pending_count": 0,
            "new_intent_count": len(gc_intents),
            "package_intent_count": 1,
            "content_intent_count": 1,
            "business_database_write_used": False,
            "intent_rows_fabricated": False,
            "storage_objects_fabricated": False,
        },
        "intent_count": len(gc_intents),
        "intents": gc_intents,
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
            "target_uri": gc_target["artifact_uri"],
            "target_sha256": gc_target["sha256"],
            "target_size_bytes": gc_target["size_bytes"],
            "state_chain": ["PENDING", "NEEDS_ATTENTION", "PENDING", "ABSENT"],
            "binding_context_proof": {
                "expected_required_bindings": [
                    "permission_check",
                    "storage_delete",
                    "storage_head",
                    "storage_put",
                ],
                "initial": {
                    "source": (
                        "agent-materialized-service-context+deployment-binding-api"
                    ),
                    "required_binding_names": [
                        "permission_check",
                        "storage_delete",
                        "storage_head",
                        "storage_put",
                    ],
                    "required_bindings_complete": True,
                    "storage_head_binding_id": "binding-problem-storage-head",
                    "storage_head_api_id": "storage.object.head",
                    "storage_head_provider_deployment_id": "deployment-storage-a",
                    "binding_desired_state": "ACTIVE",
                    "binding_observed_state": "ACTIVE",
                    "context_generation": 4,
                },
                "fault_provider": {
                    "source": (
                        "agent-materialized-service-context+deployment-binding-api"
                    ),
                    "required_binding_names": [
                        "permission_check",
                        "storage_delete",
                        "storage_head",
                        "storage_put",
                    ],
                    "required_bindings_complete": True,
                    "storage_head_binding_id": "binding-problem-storage-head",
                    "storage_head_api_id": "storage.object.head",
                    "storage_head_provider_deployment_id": (
                        "deployment-storage-head-miss-a"
                    ),
                    "binding_desired_state": "ACTIVE",
                    "binding_observed_state": "ACTIVE",
                    "context_generation": 5,
                },
                "restored": {
                    "source": (
                        "agent-materialized-service-context+deployment-binding-api"
                    ),
                    "required_binding_names": [
                        "permission_check",
                        "storage_delete",
                        "storage_head",
                        "storage_put",
                    ],
                    "required_bindings_complete": True,
                    "storage_head_binding_id": "binding-problem-storage-head",
                    "storage_head_api_id": "storage.object.head",
                    "storage_head_provider_deployment_id": "deployment-storage-a",
                    "binding_desired_state": "ACTIVE",
                    "binding_observed_state": "ACTIVE",
                    "context_generation": 6,
                },
            },
            "route_fault_injection": {
                **route_identity,
                "old_provider_deployment_id": "deployment-storage-a",
                "old_provider_endpoint": "172.28.0.13:8085:storage-service",
                "new_provider_deployment_id": "deployment-storage-head-miss-a",
                "new_provider_endpoint": (
                    "172.28.0.24:8080:storage-head-provenance-miss-provider"
                ),
                "revision_id": "revision-gc-fault-provider",
                "operation_id": "operation-gc-fault-provider",
                "operation_status": "SUCCEEDED",
                "context_generation_before": 4,
                "context_generation_after": 5,
                "binding_desired_state": "ACTIVE",
                "binding_observed_state": "ACTIVE",
            },
            "fault_provider": {
                "service_id": "storage-head-provenance-miss-provider",
                "deployment_id": "deployment-storage-head-miss-a",
                "endpoint": (
                    "172.28.0.24:8080:storage-head-provenance-miss-provider"
                ),
                "api_id": "storage.object.head",
                "api_version": "1.0.0",
                "management_mode": "EXTERNAL",
                "observed_state": "RUNNING",
                "health": "HEALTHY",
                "head_path": (
                    "/api/storage/objects" + gc_target["relative_path"]
                ),
                "head_request_observed": True,
                "head_probe": {
                    "status": 404,
                    "sha256_header": "",
                    "size_bytes": -1,
                    "storage_result_header": "",
                },
                "storage_result_header_present": False,
            },
            "targeted_reconcile": {
                **operator_action,
                "endpoint": "/api/problem/admin/artifact-gc/intents:reconcile",
                "action_id": "action-provider-unproven-404-reconcile",
                "request_id": "request-provider-unproven-404-reconcile",
                "from_status": "PENDING",
                "operator_reason": "prove provider unproven-404 quarantine",
                "exact_identity_submitted": True,
            },
            "needs_attention": {
                "status": "NEEDS_ATTENTION",
                "failure_count": 1,
                "last_failure": {
                    "message": "bound Storage HEAD returned HTTP 404",
                    "stage": "inspect",
                    "kind": "PROVIDER_HTTP",
                    "http_status": 404,
                    "provider_result": "HTTP_404",
                    "deterministic": True,
                },
                "upload_completed_at": gc_target["upload_completed_at"],
                "manual_reconcile_requested_at": "",
                "manual_reconcile_marker_consumed": True,
                "needs_attention_at": "2030-01-01T00:01:01Z",
                "ledger_preserved": True,
                "claim_credential_exposed": False,
            },
            "route_restore": {
                **route_identity,
                "old_provider_deployment_id": "deployment-storage-head-miss-a",
                "old_provider_endpoint": (
                    "172.28.0.24:8080:storage-head-provenance-miss-provider"
                ),
                "new_provider_deployment_id": "deployment-storage-a",
                "new_provider_endpoint": "172.28.0.13:8085:storage-service",
                "revision_id": "revision-gc-restore",
                "operation_id": "operation-gc-restore",
                "operation_status": "SUCCEEDED",
                "context_generation_before": 5,
                "context_generation_after": 6,
                "binding_desired_state": "ACTIVE",
                "binding_observed_state": "ACTIVE",
            },
            "object_before_operator_retry": {
                "status": 200,
                "sha256_header": gc_target["sha256"],
                "size_bytes": gc_target["size_bytes"],
                "storage_result_header": "present",
            },
            "operator_retry": {
                **operator_action,
                "expected_failure_count": 1,
                "operator_reason": "retry after binding restoration",
            },
            "ledger_absent_after_retry": True,
            "object_absent_after_retry": True,
        },
        "latest_topology_etag": '"revision-gc-restore"',
        "ledger_removed": True,
        "ledger_rows_remaining": 0,
        "all_objects_removed": True,
        "gateway_storage_head_paths": gc_head_paths,
        "gateway_storage_head_observed": True,
        "gateway_storage_delete_paths": gc_delete_paths,
        "gateway_storage_delete_observed": True,
        "judge_database_connection_used": False,
        "direct_storage_management_credential_used": False,
        "runtime_health_fabricated": False,
    }

    rollback_topology_id = "cross-machine-a-b"
    rollback_target_content = "1" * 64
    rollback_parent_content = "2" * 64
    rollback_target_revision = (
        f"{rollback_topology_id}:r10:{rollback_target_content}"
    )
    rollback_parent_revision = (
        f"{rollback_topology_id}:r11:{rollback_parent_content}"
    )
    rollback_created_revision = (
        f"{rollback_topology_id}:r12:{rollback_target_content}"
    )
    credential_lifecycle = {
        "consumer_deployment_id": "deployment-echo-consumer-b",
        "container_id_before": "echo-consumer-container",
        "container_id_after": "echo-consumer-container",
        "generation_before": 1,
        "generation_revoked": 2,
        "generation_restored": 3,
        "rollback_target_revision_id": rollback_target_revision,
        "revoke_revision_id": rollback_parent_revision,
        "revoke_operation_id": "operation-revoke-echo",
        "revoke_operation_status": "SUCCEEDED",
        "restore_revision_id": rollback_created_revision,
        "restore_operation_id": "operation-restore-echo",
        "restore_operation_status": "SUCCEEDED",
        "old_token_existing_route_status": 401,
        "current_token_removed_route_status": 404,
        "current_token_retained_route_status": 200,
        "revoked_token_after_restore_status": 401,
        "restored_token_route_status": 200,
        "revoked_binding_desired_state": "REVOKED",
        "echo_requirement_optional": True,
        "permission_requirement_optional": False,
        "retained_permission_binding_desired_state": "ACTIVE",
        "retained_permission_binding_observed_state": "ACTIVE",
        "revoked_context_binding_names": ["permission_check"],
        "revoked_context_permission_binding_id": "binding-permission-check",
        "durable_permission_binding_id": "binding-permission-check",
        "consumer_observed_unbound_error": "binding echo is unavailable",
        "consumer_recovered": True,
        "recovered_success_count": 2,
        "tokens_recorded": False,
    }
    value["workload_credential_lifecycle"] = credential_lifecycle
    value["topology_rollback"] = {
        "api_path": f"/api/v1/topologies/{rollback_topology_id}:rollback",
        "topology_id": rollback_topology_id,
        "request_revision_id": rollback_target_revision,
        "request_if_match": f'"{rollback_parent_revision}"',
        "target_revision_id": rollback_target_revision,
        "target_revision_number": 10,
        "target_content_sha256": rollback_target_content,
        "target_spec_sha256": "sha256:" + "3" * 64,
        "parent_revision_id": rollback_parent_revision,
        "parent_revision_number": 11,
        "parent_content_sha256": rollback_parent_content,
        "created_revision_id": rollback_created_revision,
        "created_revision_number": 12,
        "created_parent_revision_id": rollback_parent_revision,
        "created_rollback_of_revision_id": rollback_target_revision,
        "created_content_sha256": rollback_target_content,
        "created_spec_sha256": "sha256:" + "3" * 64,
        "created_revision_etag": f'"{rollback_created_revision}"',
        "operation_id": "operation-restore-echo",
        "operation_action": "topology.rollback",
        "operation_status": "SUCCEEDED",
        "draft_revision_id": rollback_created_revision,
        "applied_revision_id": rollback_created_revision,
        "applying_revision_id": None,
        "status_desired_revision_id": rollback_created_revision,
        "status_observed_revision_id": rollback_created_revision,
        "status_state": "IN_SYNC",
        "status_drift": [],
        "status_last_operation_id": "operation-restore-echo",
        "restored_bindings": [
            {
                "requirement_name": "echo",
                "binding_id": "binding-echo",
                "provider_deployment_id": "deployment-echo-provider-a",
                "desired_state": "ACTIVE",
                "observed_state": "ACTIVE",
                "topology_revision_id": rollback_created_revision,
                "credential_generation": 3,
            },
            {
                "requirement_name": "permission_check",
                "binding_id": "binding-permission-check",
                "provider_deployment_id": "deployment-auth-a",
                "desired_state": "ACTIVE",
                "observed_state": "ACTIVE",
                "topology_revision_id": rollback_created_revision,
                "credential_generation": 3,
            },
        ],
    }

    flow_task = value["component_flow"]["task"]
    flow_submission = value["component_flow"]["submission"]
    value["component_flow"]["workload_transcript_correlated"] = True
    value["workload_request_transcript"] = {
        "capture_source": "gateway-tls-ingress",
        "task_id": flow_task["task_id"],
        "submission_id": flow_submission["submission_id"],
        "claim": {
            "method": "POST",
            "path": "/internal/apis/judge.worker.control/tasks/claim",
            "status": 200,
            "request_headers": {"prefer": "wait=25"},
            "request_size_bytes": 128,
            "request_sha256": "sha256:" + "6" * 64,
            "response_size_bytes": 512,
            "response_sha256": "sha256:" + "7" * 64,
        },
        "source_get": {
            "method": "GET",
            "path": "/internal/apis/storage.object.get"
            + flow_task["source"]["relative_path"],
            "status": 200,
            "request_headers": {},
            "request_size_bytes": 0,
            "request_sha256": "sha256:" + "8" * 64,
            "response_size_bytes": flow_task["source"]["size_bytes"],
            "response_sha256": flow_task["source"]["sha256"],
            "resource_ref": copy.deepcopy(flow_task["source"]),
        },
        "package_get": {
            "method": "GET",
            "path": "/internal/apis/storage.object.get"
            + flow_task["problem_package"]["relative_path"],
            "status": 200,
            "request_headers": {},
            "request_size_bytes": 0,
            "request_sha256": "sha256:" + "9" * 64,
            "response_size_bytes": flow_task["problem_package"]["size_bytes"],
            "response_sha256": flow_task["problem_package"]["sha256"],
            "resource_ref": copy.deepcopy(flow_task["problem_package"]),
        },
        "result_post": {
            "method": "POST",
            "path": "/internal/apis/judge.worker.control/tasks/"
            + flow_task["task_id"]
            + "/result",
            "status": 200,
            "request_headers": {"content-type": "application/json"},
            "request_size_bytes": 256,
            "request_sha256": "sha256:" + "a" * 64,
            "response_size_bytes": 64,
            "response_sha256": "sha256:" + "b" * 64,
            "task_id": flow_task["task_id"],
            "status_value": "ACCEPTED",
            "lease_version": 1,
        },
        "authorization_redacted": True,
        "identity_validated_by_gateway": True,
    }
    value["runtime_volume_isolation"] = {
        "verified": True,
        "inspection_source": "docker-inspect",
        "forbidden_shared_sources": [],
        "a_engine_id": value["engines"]["a"]["engine_id"],
        "b_engine_id": value["engines"]["b"]["engine_id"],
        "gateway_mount_sources": [],
        "judge_mount_sources": [],
        "worker_mount_sources": ["/var/lib/ojos-agent/runtime-contexts/example/service"],
    }
    canary_runtime = managed_a_deployment(
        "storage-service", "deployment-storage-canary-a", "6", [], [], []
    )
    canary_runtime["image_repo_digest"] = value["managed_a_deployments"][
        "storage-service"
    ]["image_repo_digest"]
    value["binding_reconfiguration"] = {
        "provider_preserving": True,
        "semantic_provider_rebind": True,
        "requirement_name": "storage_get",
        "api_id": "storage.object.get",
        "consumer_deployment_id": "deployment-worker-b",
        "consumer_endpoint": "172.20.0.3:9101:judge-worker",
        "old_provider_deployment_id": storage_deployment_id,
        "old_provider_endpoint": "172.20.0.2:8085:storage-service",
        "new_provider_deployment_id": "deployment-storage-canary-a",
        "new_provider_endpoint": "172.20.0.2:8086:storage-service",
        "canary_store": {
            "service_id": "storage-service",
            "catalog_source_id": "storage-service-canary",
            "version": "0.1.1",
            "endpoint": "172.20.0.2:8086:storage-service",
            "validate_request_id": "request-validate-storage-canary",
            "validation_valid": True,
            "validation_topology_changes": 1,
            "install_request_id": "request-install-storage-canary",
            "deployment_id": "deployment-storage-canary-a",
            "operation_id": "operation-install-storage-canary",
            "operation_status": "SUCCEEDED",
            "runtime": canary_runtime,
        },
        "operation_id": "operation-topology-context-2",
        "operation_status": "SUCCEEDED",
        "container_id_before": "0123456789abcdef",
        "container_id_after": "0123456789abcdef",
        "generation_before": 1,
        "generation_after": 2,
        "credential_generation_after": 2,
        "context_generation_after": 2,
        "post_update_request_succeeded": True,
        "post_update_submission_status": "ACCEPTED",
        "topology_revision_id": "revision-storage-canary-rebind",
        "provider_projection_integrity": projection_integrity(
            "binding-reconfigure",
            "revision-storage-canary-rebind",
            "b" * 64,
            1_900_000_160_000,
            2,
        ),
    }
    value["third_party_fixture"].update(
        {
            "manifest_only": True,
            "provider": {
                "service_id": "contract-echo-provider",
                "deployment_id": "deployment-echo-provider-a",
                "engine": "A",
                "installed_via_store": True,
                "management_mode": "EXTERNAL",
            },
            "consumer": {
                "service_id": "contract-echo-consumer",
                "deployment_id": "deployment-echo-consumer-b",
                "engine": "B",
                "installed_via_store_agent": True,
                "container_id": "echo-consumer-container",
            },
            "binding_plan": {
                "requirement_name": "echo",
                "api_id": "fixture.contract.echo",
                "provider_deployment_id": "deployment-echo-provider-a",
                "state": "ACTIVE",
                "optional": True,
            },
            "permission_binding_plan": {
                "requirement_name": "permission_check",
                "api_id": "auth.user.permission.check",
                "provider_deployment_id": "deployment-auth-a",
                "state": "ACTIVE",
                "optional": False,
            },
            "permission_provider_deployment_id": "deployment-auth-a",
            "workload_permission_check": {"data": {"allowed": True}},
            "operation_id": "operation-install-echo-consumer",
            "operation_status": "SUCCEEDED",
            "runtime_projection": runtime_projection(
                "deployment-echo-consumer-b", "node-b", 1_900_000_060_000
            ),
            "binding_lifecycle": copy.deepcopy(credential_lifecycle),
        }
    )
    value["final_provider_projection_integrity"] = projection_integrity(
        "final-applied",
        rollback_created_revision,
        rollback_target_content,
        1_900_000_170_000,
        3,
    )
    return value


class EvidenceTests(unittest.TestCase):
    def test_complete_contract_live_evidence_passes(self) -> None:
        gate.verify_evidence(valid_evidence())

    def test_one_engine_cannot_be_reported_as_two(self) -> None:
        value = valid_evidence()
        value["engines"]["b"]["engine_id"] = value["engines"]["a"]["engine_id"]
        with self.assertRaisesRegex(gate.GateError, "distinct"):
            gate.verify_evidence(value)

    def test_engine_probe_must_match_every_final_identity_field(self) -> None:
        value = valid_evidence()
        value["engine_probe"]["b"]["outer_container_id"] = "e" * 64
        with self.assertRaisesRegex(gate.GateError, "probe does not match"):
            gate.verify_evidence(value)

        value = valid_evidence()
        value["engine_probe"]["a"]["local_engine_id"] = value["engines"]["b"][
            "engine_id"
        ]
        with self.assertRaisesRegex(gate.GateError, "probe does not match"):
            gate.verify_evidence(value)

    def test_engine_evidence_requires_run_scoped_names_and_canonical_ids(self) -> None:
        value = valid_evidence()
        value["engines"]["b"]["outer_container_id"] = "outer-b"
        with self.assertRaisesRegex(gate.GateError, "not canonical"):
            gate.verify_evidence(value)

        value = valid_evidence()
        value["engines"]["b"]["data_volume"] = "unscoped-data"
        with self.assertRaisesRegex(gate.GateError, "data roots"):
            gate.verify_evidence(value)

    def test_engine_image_repo_digest_must_match_requested_pin(self) -> None:
        value = valid_evidence()
        wrong = ["docker.io/library/docker@sha256:" + "e" * 64]
        value["engines"]["b"]["image_repo_digests"] = wrong
        value["engine_probe"]["b"]["image_repo_digests"] = wrong
        with self.assertRaisesRegex(gate.GateError, "does not match the pin"):
            gate.verify_evidence(value)

    def test_missing_endpoint_or_data_root_proof_is_not_a_pass(self) -> None:
        value = valid_evidence()
        value["engines"]["b"]["host_endpoint_matches_local_socket"] = False
        with self.assertRaisesRegex(gate.GateError, "endpoint routing"):
            gate.verify_evidence(value)

        value = valid_evidence()
        value["engines"]["b"]["data_volume"] = "engine-a-data"
        with self.assertRaisesRegex(gate.GateError, "data roots"):
            gate.verify_evidence(value)

        value = valid_evidence()
        value["engines"]["dind_image"] = "docker:29-dind"
        with self.assertRaisesRegex(gate.GateError, "digest-pinned"):
            gate.verify_evidence(value)

    def test_missing_denial_is_not_a_pass(self) -> None:
        value = valid_evidence()
        value["network_boundary"]["denied"].pop()
        with self.assertRaisesRegex(gate.GateError, "deny every"):
            gate.verify_evidence(value)

    def test_full_evidence_requires_denials_from_worker_default_bridge(self) -> None:
        value = valid_full_evidence()
        value["network_boundary"]["worker_bridge_denied"].pop()
        with self.assertRaisesRegex(gate.GateError, "Worker default bridge"):
            gate.verify_evidence(value, require_full=True)

    def test_failed_or_skipped_document_is_not_a_pass(self) -> None:
        for status in ("FAILED", "SKIPPED", "RUNNING"):
            value = valid_evidence()
            value["status"] = status
            with self.assertRaisesRegex(gate.GateError, "not a passed"):
                gate.verify_evidence(value)

    def test_passed_document_without_successful_cleanup_is_rejected(self) -> None:
        for cleanup_completed, cleanup_errors in (
            (False, []),
            (True, [{"operation": "remove-container", "error": "still running"}]),
        ):
            value = valid_evidence()
            value["cleanup_completed"] = cleanup_completed
            value["cleanup_errors"] = cleanup_errors
            with self.subTest(
                cleanup_completed=cleanup_completed, cleanup_errors=cleanup_errors
            ), self.assertRaisesRegex(gate.GateError, "cleanup"):
                gate.verify_evidence(value)

    def test_require_full_rejects_contract_fixture_only_evidence(self) -> None:
        with self.assertRaisesRegex(gate.GateError, "identity|Store and an enrolled Agent"):
            gate.verify_evidence(valid_evidence(), require_full=True)

    def test_require_full_rejects_old_direct_docker_worker_and_component_probes(self) -> None:
        value = valid_evidence()
        value["worker_implementation"] = "repository Rust judge-worker image"
        value["component_probes"] = {
            "status": "PASSED",
            "tests": [{"test": "problem"}, {"test": "judge"}],
        }
        with self.assertRaisesRegex(gate.GateError, "identity|Store and an enrolled Agent"):
            gate.verify_evidence(value, require_full=True)

    def test_complete_store_agent_full_evidence_passes(self) -> None:
        gate.verify_evidence(valid_full_evidence(), require_full=True)

    def test_require_full_rejects_forged_worker_install_compensation(self) -> None:
        mutations = (
            (
                "needs-attention",
                lambda proof: proof["operation"].update(
                    attention_job_ids_count=1
                ),
                "FAILED Agent attempt",
            ),
            (
                "agent-failed-before-health-gate",
                lambda proof: proof["agent_attempt"].update(
                    failure_health_gate=None,
                    post_start_health_gate_failure=False,
                ),
                "FAILED Agent attempt",
            ),
            (
                "deterministic-container-still-exists",
                lambda proof: proof["container_readback"].update(
                    exact_name_inspect_exit_code=0,
                    exact_name_absent=False,
                ),
                "left a container",
            ),
            (
                "labeled-volume-still-exists",
                lambda proof: proof["volume_readback"].update(
                    matches=[proof["volume_readback"]["expected_name"]]
                ),
                "cache volume",
            ),
            (
                "runtime-row-remains",
                lambda proof: proof["control_plane_database_readback"].update(
                    runtime_instance_count=1
                ),
                "zero control-plane",
            ),
            (
                "staged-binding-remains",
                lambda proof: proof["control_plane_database_readback"].update(
                    binding_count=1,
                    active_or_staged_binding_count=1,
                ),
                "zero control-plane",
            ),
            (
                "topology-etag-changed",
                lambda proof: proof["topology_readback"].update(
                    draft_etag_after='"revision-leaked"'
                ),
                "failed immutable draft",
            ),
            (
                "failed-draft-hidden",
                lambda proof: proof["topology_readback"].update(
                    failed_draft_endpoint_count=0
                ),
                "failed immutable draft",
            ),
            (
                "gateway-active-route-leaked",
                lambda proof: proof[
                    "gateway_active_projection_readback"
                ].update(failed_deployment_route_count=1),
                "Gateway active projection",
            ),
            (
                "gateway-index-membership-lost",
                lambda proof: proof[
                    "gateway_active_projection_readback"
                ].update(index_member_after=0),
                "Gateway active projection",
            ),
            (
                "gateway-existing-routes-lost",
                lambda proof: proof[
                    "gateway_active_projection_readback"
                ].update(routes_sha256_after="sha256:" + "0" * 64),
                "Gateway active projection",
            ),
            (
                "auth-materialized-grant-changed",
                lambda proof: proof[
                    "auth_active_projection_readback"
                ].update(materialized_grants_sha256_after="sha256:" + "0" * 64),
                "Auth active projection",
            ),
            (
                "durable-binding-set-changed",
                lambda proof: proof[
                    "durable_binding_set_readback"
                ].update(rows_sha256_after="sha256:" + "0" * 64),
                "durable applied Binding set",
            ),
            (
                "failed-draft-lineage-changed",
                lambda proof: proof["topology_readback"].update(
                    failed_draft_parent_revision_id="revision-other"
                ),
                "failed immutable draft",
            ),
            (
                "rollback-not-applied",
                lambda proof: proof["recovery_rollback"].update(
                    applied_revision_id="revision-1"
                ),
                "compensation rollback",
            ),
            (
                "rollback-gateway-routes-changed",
                lambda proof: proof["recovery_rollback"].update(
                    gateway_stable_routes_sha256_recovered="sha256:" + "0" * 64
                ),
                "compensation rollback",
            ),
            (
                "rollback-generation-not-incremented",
                lambda proof: proof["recovery_rollback"][
                    "gateway_consumer_generations_recovered"
                ].update({"deployment-problem-a": 3}),
                "compensation rollback",
            ),
            (
                "rollback-generation-source-split",
                lambda proof: proof["recovery_rollback"][
                    "auth_consumer_generations_recovered"
                ].update({"deployment-problem-a": 5}),
                "compensation rollback",
            ),
            (
                "consumer-context-not-rotated",
                lambda proof: proof["consumer_context_rollback"][0].update(
                    context_sha256_recovered=proof["consumer_context_rollback"][0][
                        "context_sha256_before"
                    ]
                ),
                "atomically rotate",
            ),
            (
                "consumer-token-generation-split",
                lambda proof: proof["consumer_context_rollback"][0][
                    "credential_claims_recovered"
                ].update(credential_generation=9),
                "atomically rotate",
            ),
            (
                "consumer-token-expiry-regressed",
                lambda proof: proof["consumer_context_rollback"][0][
                    "credential_claims_after"
                ].update(expires_at_unix=1),
                "atomically rotate",
            ),
        )
        for name, mutate, message in mutations:
            value = valid_full_evidence()
            mutate(value["worker_install_failure_compensation"])
            with self.subTest(name=name), self.assertRaisesRegex(
                gate.GateError, message
            ):
                gate.verify_evidence(value, require_full=True)

    def test_full_evidence_requires_final_agent_health_samples(self) -> None:
        value = valid_full_evidence()
        value["store_agent_evidence"]["agent"]["runtime_health_sample"] = (
            "enrollment-snapshot"
        )
        with self.assertRaisesRegex(gate.GateError, "final runtime report"):
            gate.verify_evidence(value, require_full=True)

        value = valid_full_evidence()
        value["a_agent_evidence"]["runtime_health"]["last_observed_at"] = None
        with self.assertRaisesRegex(gate.GateError, "fresh, enrolled"):
            gate.verify_evidence(value, require_full=True)

    def test_full_evidence_requires_strict_inventory_takeover(self) -> None:
        value = valid_full_evidence()
        projection = value["store_agent_evidence"]["deployment"][
            "runtime_projection"
        ]
        projection["inventory_payload"]["last_observed_at_ms"] = projection[
            "completion_watermark_ms"
        ]
        with self.assertRaisesRegex(gate.GateError, "did not supersede"):
            gate.verify_evidence(value, require_full=True)

        value = valid_full_evidence()
        managed = value["managed_a_deployments"]["problem-service"]
        managed["runtime_projection"]["inventory_payload"]["instance"][
            "runtime_attested"
        ] = False
        with self.assertRaisesRegex(gate.GateError, "runtime-attested"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_worker_recovery_without_unhealthy_transition(self) -> None:
        value = valid_full_evidence()
        value["worker_recovery"]["health_timeline"][1]["status"] = "HEALTHY"
        with self.assertRaisesRegex(gate.GateError, "HEALTHY -> UNHEALTHY -> HEALTHY"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_worker_recovery_with_replaced_container(self) -> None:
        value = valid_full_evidence()
        value["worker_recovery"]["worker_container_id_after"] = "replacement-worker"
        with self.assertRaisesRegex(gate.GateError, "Store-created container"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_worker_recovery_without_new_reregistration(self) -> None:
        value = valid_full_evidence()
        value["worker_recovery"]["reregistration"]["sequence"] = value[
            "worker_recovery"
        ]["capture_baseline_sequence"]
        with self.assertRaisesRegex(gate.GateError, "re-registration"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_worker_recovery_that_reuses_old_task(self) -> None:
        value = valid_full_evidence()
        value["worker_recovery"]["recovered_flow"]["task"]["task_id"] = value[
            "component_flow"
        ]["task"]["task_id"]
        value["worker_recovery"]["recovered_flow"]["result"]["task_id"] = value[
            "component_flow"
        ]["task"]["task_id"]
        with self.assertRaisesRegex(gate.GateError, "reused the pre-disruption task"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_missing_or_forged_projection_digest_checkpoints(
        self,
    ) -> None:
        mutations = (
            (
                "missing-recovery",
                lambda value: value["worker_recovery"].pop(
                    "provider_projection_integrity"
                ),
            ),
            (
                "forged-provider-status",
                lambda value: value["worker_recovery"][
                    "provider_projection_integrity"
                ]["providers"]["gateway"].update(
                    observed_projection_sha256="0" * 64
                ),
            ),
            (
                "mutated-durable-route",
                lambda value: value["binding_reconfiguration"][
                    "provider_projection_integrity"
                ]["providers"]["auth"]["projection"]["routes"][0].update(
                    timeout_ms=1
                ),
            ),
            (
                "missing-reconfigure",
                lambda value: value["binding_reconfiguration"].pop(
                    "provider_projection_integrity"
                ),
            ),
            (
                "stale-final-revision",
                lambda value: value["final_provider_projection_integrity"].update(
                    applied_revision_id="stale-final-revision"
                ),
            ),
        )
        for name, mutate in mutations:
            value = valid_full_evidence()
            mutate(value)
            with self.subTest(name=name), self.assertRaisesRegex(
                gate.GateError, "projection|Status digest"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_artifact_gc_without_gateway_conditional_delete(self) -> None:
        value = valid_full_evidence()
        value["problem_artifact_gc"]["gateway_storage_delete_observed"] = False
        with self.assertRaisesRegex(gate.GateError, "artifact GC"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_artifact_gc_without_strict_bound_head_probe(self) -> None:
        mutations = (
            ("deployment_jwt_used", False),
            ("service_context_mount_read_only", False),
            ("binding", "direct-storage"),
        )
        for field, replacement in mutations:
            value = valid_full_evidence()
            value["problem_artifact_gc"]["storage_head_probe"][field] = replacement
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "artifact GC"
            ):
                gate.verify_evidence(value, require_full=True)

        value = valid_full_evidence()
        value["problem_artifact_gc"]["intents"][0]["head_before"][
            "token"
        ] = "must-not-be-accepted"
        with self.assertRaisesRegex(gate.GateError, "artifact GC|secret/token"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_partial_artifact_gc_set_evidence(self) -> None:
        value = valid_full_evidence()
        value["problem_artifact_gc"]["intents"][1]["head_after"]["status"] = 200
        with self.assertRaisesRegex(gate.GateError, "artifact GC"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_artifact_gc_database_or_fabricated_setup(self) -> None:
        for field in (
            "business_database_write_used",
            "intent_rows_fabricated",
            "storage_objects_fabricated",
        ):
            value = valid_full_evidence()
            value["problem_artifact_gc"]["setup"][field] = True
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "artifact GC"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_artifact_gc_incomplete_state_chain(self) -> None:
        for chain in (
            ["PENDING", "NEEDS_ATTENTION", "ABSENT"],
            ["PENDING", "PENDING", "ABSENT"],
            ["PENDING", "NEEDS_ATTENTION", "PENDING"],
        ):
            value = valid_full_evidence()
            value["problem_artifact_gc"]["failure_recovery"]["state_chain"] = chain
            with self.subTest(chain=chain), self.assertRaisesRegex(
                gate.GateError, "artifact GC"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_artifact_gc_unstructured_route_failure(self) -> None:
        mutations = (
            ("stage", "delete"),
            ("kind", "TRANSIENT"),
            ("http_status", 500),
            ("provider_result", "HTTP_500"),
            ("deterministic", False),
            ("message", ""),
        )
        for field, replacement in mutations:
            value = valid_full_evidence()
            failure = value["problem_artifact_gc"]["failure_recovery"][
                "needs_attention"
            ]["last_failure"]
            failure[field] = replacement
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "artifact GC"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_artifact_gc_unconsumed_or_unuploaded_intent(self) -> None:
        for invalid_timestamp in ("", "2030-01-01", "not-a-timestamp"):
            value = valid_full_evidence()
            value["problem_artifact_gc"]["intents"][0][
                "upload_completed_at"
            ] = invalid_timestamp
            with self.subTest(timestamp=invalid_timestamp), self.assertRaisesRegex(
                gate.GateError, "artifact GC"
            ):
                gate.verify_evidence(value, require_full=True)

        value = valid_full_evidence()
        attention = value["problem_artifact_gc"]["failure_recovery"][
            "needs_attention"
        ]
        attention["manual_reconcile_requested_at"] = "2030-01-01T00:01:00Z"
        with self.assertRaisesRegex(gate.GateError, "artifact GC"):
            gate.verify_evidence(value, require_full=True)

        value = valid_full_evidence()
        value["problem_artifact_gc"]["failure_recovery"]["needs_attention"][
            "manual_reconcile_marker_consumed"
        ] = False
        with self.assertRaisesRegex(gate.GateError, "artifact GC"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_artifact_gc_non_202_or_non_colon_action(self) -> None:
        cases = (
            ("targeted_reconcile", "first_http_status", 200),
            ("targeted_reconcile", "first_http_status", 202.0),
            ("operator_retry", "replay_http_status", 200),
            (
                "targeted_reconcile",
                "endpoint",
                "/api/problem/admin/artifact-gc/intents/reconcile",
            ),
            (
                "operator_retry",
                "endpoint",
                "/api/problem/admin/artifact-gc/intents/retry",
            ),
        )
        for action_name, field, replacement in cases:
            value = valid_full_evidence()
            action = value["problem_artifact_gc"]["failure_recovery"][action_name]
            action[field] = replacement
            with self.subTest(action=action_name, field=field), self.assertRaisesRegex(
                gate.GateError, "artifact GC"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_artifact_gc_non_idempotent_operator_action(self) -> None:
        mutations = (
            ("duplicate_request_replay", False),
            ("duplicate_action_id_matched", False),
            ("duplicate_request_id_matched", False),
            ("idempotency_key_used", False),
            ("reason_recorded", False),
            ("operator_reason", ""),
            ("from_status", "PENDING"),
        )
        for field, replacement in mutations:
            value = valid_full_evidence()
            retry = value["problem_artifact_gc"]["failure_recovery"][
                "operator_retry"
            ]
            retry[field] = replacement
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "artifact GC"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_artifact_gc_lost_ledger_or_object(self) -> None:
        value = valid_full_evidence()
        value["problem_artifact_gc"]["failure_recovery"]["needs_attention"][
            "ledger_preserved"
        ] = False
        with self.assertRaisesRegex(gate.GateError, "artifact GC"):
            gate.verify_evidence(value, require_full=True)

        for field, replacement in (
            ("status", 404),
            ("storage_result_header", "object-not-found"),
        ):
            value = valid_full_evidence()
            probe = value["problem_artifact_gc"]["failure_recovery"][
                "object_before_operator_retry"
            ]
            probe[field] = replacement
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "artifact GC"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_artifact_gc_stale_topology_restore(self) -> None:
        mutations = (
            ("same_revision", None),
            ("same_operation", None),
            ("non_increasing_generation", None),
            ("stale_etag", None),
        )
        for mutation, _ in mutations:
            value = valid_full_evidence()
            recovery = value["problem_artifact_gc"]["failure_recovery"]
            if mutation == "same_revision":
                recovery["route_restore"]["revision_id"] = recovery[
                    "route_fault_injection"
                ]["revision_id"]
            elif mutation == "same_operation":
                recovery["route_restore"]["operation_id"] = recovery[
                    "route_fault_injection"
                ]["operation_id"]
            elif mutation == "non_increasing_generation":
                recovery["route_restore"]["context_generation_after"] = recovery[
                    "route_restore"
                ]["context_generation_before"]
            else:
                value["problem_artifact_gc"]["latest_topology_etag"] = (
                    '"revision-gc-fault-provider"'
                )
            with self.subTest(mutation=mutation), self.assertRaisesRegex(
                gate.GateError, "artifact GC"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_artifact_gc_required_binding_revoke(self) -> None:
        for field, replacement in (
            ("required_binding_preserved", False),
            ("binding_desired_state", "REVOKED"),
            ("binding_observed_state", "REVOKED"),
        ):
            value = valid_full_evidence()
            value["problem_artifact_gc"]["failure_recovery"][
                "route_fault_injection"
            ][field] = replacement
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "artifact GC"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_unproven_404_not_observed_at_fault_provider(self) -> None:
        for field, replacement in (
            ("head_request_observed", False),
            ("storage_result_header_present", True),
            ("health", "UNHEALTHY"),
        ):
            value = valid_full_evidence()
            value["problem_artifact_gc"]["failure_recovery"]["fault_provider"][
                field
            ] = replacement
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "artifact GC"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_artifact_gc_incomplete_actual_service_context(self) -> None:
        value = valid_full_evidence()
        proof = value["problem_artifact_gc"]["failure_recovery"][
            "binding_context_proof"
        ]
        proof["fault_provider"]["required_binding_names"].remove("storage_put")
        proof["fault_provider"]["required_bindings_complete"] = False
        with self.assertRaisesRegex(gate.GateError, "artifact GC"):
            gate.verify_evidence(value, require_full=True)

        value = valid_full_evidence()
        value["problem_artifact_gc"]["failure_recovery"][
            "binding_context_proof"
        ]["restored"]["storage_head_provider_deployment_id"] = "wrong-provider"
        with self.assertRaisesRegex(gate.GateError, "artifact GC"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_artifact_gc_detached_retry_action(self) -> None:
        value = valid_full_evidence()
        value["problem_artifact_gc"]["intents"][0]["recovery_action_id"] = (
            "different-action"
        )
        with self.assertRaisesRegex(gate.GateError, "artifact GC"):
            gate.verify_evidence(value, require_full=True)

        value = valid_full_evidence()
        value["problem_artifact_gc"]["gateway_storage_delete_paths"].pop()
        with self.assertRaisesRegex(gate.GateError, "artifact GC"):
            gate.verify_evidence(value, require_full=True)

        value = valid_full_evidence()
        value["problem_artifact_gc"]["intent_count"] -= 1
        with self.assertRaisesRegex(gate.GateError, "artifact GC"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_missing_generic_service_fixture(self) -> None:
        value = valid_full_evidence()
        value.pop("third_party_fixture")
        with self.assertRaisesRegex(gate.GateError, "third-party"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_generic_fixture_without_workload_permission_binding(self) -> None:
        value = valid_full_evidence()
        value["third_party_fixture"].pop("permission_binding_plan")
        with self.assertRaisesRegex(gate.GateError, "generic manifest-only"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_wrong_generic_requirement_optionality(self) -> None:
        value = valid_full_evidence()
        value["third_party_fixture"]["binding_plan"]["optional"] = False
        with self.assertRaisesRegex(gate.GateError, "generic manifest-only"):
            gate.verify_evidence(value, require_full=True)

        value = valid_full_evidence()
        value["third_party_fixture"]["permission_binding_plan"]["optional"] = True
        with self.assertRaisesRegex(gate.GateError, "generic manifest-only"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_revocation_that_drops_required_permission(self) -> None:
        value = valid_full_evidence()
        value["workload_credential_lifecycle"][
            "retained_permission_binding_observed_state"
        ] = "REVOKED"
        value["third_party_fixture"]["binding_lifecycle"] = copy.deepcopy(
            value["workload_credential_lifecycle"]
        )
        with self.assertRaisesRegex(gate.GateError, "credential lifecycle"):
            gate.verify_evidence(value, require_full=True)

    def test_rejects_generic_fixture_without_trusted_caller_projection(self) -> None:
        value = valid_full_evidence()
        value["third_party_fixture"]["consumer_evidence"]["response"]["caller"] = ""
        with self.assertRaisesRegex(gate.GateError, "manifest-generated binding"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_hand_written_context_claim(self) -> None:
        value = valid_full_evidence()
        value["store_agent_evidence"]["runtime"]["created_by_agent"] = False
        with self.assertRaisesRegex(gate.GateError, "runtime evidence"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_non_api_problem_seed(self) -> None:
        value = valid_full_evidence()
        value["component_flow"]["problem_created_via_http_api"] = False
        with self.assertRaisesRegex(gate.GateError, "problem through the HTTP API"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_even_empty_legacy_url_member(self) -> None:
        value = valid_full_evidence()
        value["component_flow"]["task"]["source"]["url"] = ""
        with self.assertRaisesRegex(gate.GateError, "retired url field"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_restarted_worker_during_binding_update(self) -> None:
        value = valid_full_evidence()
        value["binding_reconfiguration"]["container_id_after"] = "replacement-container"
        with self.assertRaisesRegex(gate.GateError, "reconfiguration"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_cosmetic_or_forged_storage_rebind(self) -> None:
        value = valid_full_evidence()
        value["binding_reconfiguration"]["semantic_provider_rebind"] = False
        with self.assertRaisesRegex(gate.GateError, "reconfiguration"):
            gate.verify_evidence(value, require_full=True)

        value = valid_full_evidence()
        value["binding_reconfiguration"]["canary_store"]["deployment_id"] = (
            value["binding_reconfiguration"]["old_provider_deployment_id"]
        )
        with self.assertRaisesRegex(gate.GateError, "Store canary"):
            gate.verify_evidence(value, require_full=True)

        value = valid_full_evidence()
        value["binding_reconfiguration"]["canary_store"]["runtime"][
            "image_repo_digest"
        ] = "sha256:" + "f" * 64
        with self.assertRaisesRegex(gate.GateError, "Store canary"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_shared_problem_volume(self) -> None:
        value = valid_full_evidence()
        value["runtime_volume_isolation"]["forbidden_shared_sources"] = ["shared-problems"]
        with self.assertRaisesRegex(gate.GateError, "shared Problem/Submission"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_hand_written_worker_endpoint(self) -> None:
        value = valid_full_evidence()
        value["store_agent_evidence"]["store_install"]["request_fields"].append("endpoint")
        value["store_agent_evidence"]["store_install"]["request_endpoint_present"] = True
        with self.assertRaisesRegex(gate.GateError, "Store install request"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_development_or_unidentified_build(self) -> None:
        for field, replacement in (
            ("profile", "development"),
            ("commit_sha", "deadbeef"),
            ("target", ""),
        ):
            value = valid_full_evidence()
            value["build_identity"][field] = replacement
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "production build identity"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_missing_control_plane_runtime(self) -> None:
        value = valid_full_evidence()
        value.pop("control_plane_runtime")

        with self.assertRaisesRegex(gate.GateError, "Docker-healthy TLS control-plane"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_unhealthy_or_misconfigured_control_plane(self) -> None:
        mutations = (
            ("docker_health", "UNHEALTHY"),
            (
                "healthcheck_url",
                "http://127.0.0.1:8090/api/v1/healthz/ready",
            ),
            ("healthcheck_ca_cert", ""),
            ("engine_id", "engine-b"),
            ("tls_enabled", False),
        )
        for field, replacement in mutations:
            value = valid_full_evidence()
            value["control_plane_runtime"][field] = replacement
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "Docker-healthy TLS control-plane"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_incomplete_auth_admin_bootstrap(self) -> None:
        mutations = (
            ("created_status", 200),
            ("created_code", 50001),
            ("created_user_id", "0"),
            ("login_status", 401),
            ("login_code", 40101),
            ("login_user_matches_bootstrap", False),
            ("login_has_super_admin", False),
            ("login_has_system_admin", False),
            ("profile_status", 401),
            ("profile_code", 40101),
            ("profile_authenticated_same_user", False),
            ("replay_status", 201),
            ("replay_code", 0),
            ("wrong_secret_status", 201),
            ("wrong_secret_code", 0),
            ("jwt_self_signed_by_harness", True),
            ("manual_database_role_seed", True),
            ("database_transactional", False),
            ("secret_or_token_recorded", True),
        )
        for field, replacement in mutations:
            value = valid_full_evidence()
            value["auth_admin_bootstrap"][field] = replacement
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "Auth admin bootstrap"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_forged_admin_jwt_claim(self) -> None:
        value = valid_full_evidence()
        value["auth_admin_bootstrap"]["jwt_source"] = "harness-forged-hs256"
        with self.assertRaisesRegex(gate.GateError, "real login JWT"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_unproven_auth_bootstrap_database_state(self) -> None:
        for field, replacement in (
            ("marker_completed", False),
            ("marker_user_id", "2"),
            ("super_admin_assigned", False),
            ("bootstrap_audit_count", 0),
        ):
            value = valid_full_evidence()
            value["auth_admin_bootstrap"]["database_proof"][field] = replacement
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "Auth admin bootstrap"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_secret_or_token_material_anywhere(self) -> None:
        cases = (
            ("login_token", "sensitive-token-value"),
            (
                "debug_value",
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhZG1pbiJ9.c2lnbmF0dXJl",
            ),
            ("debug_header", "Bearer sensitive-token-value"),
            ("bootstrap_secret", "sensitive-bootstrap-value"),
            ("private_key", "-----BEGIN PRIVATE KEY-----\nsecret\n"),
        )
        for field, material in cases:
            value = valid_full_evidence()
            value["auth_admin_bootstrap"][field] = material
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "secret/token material"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_untrusted_or_stale_node_a_agent(self) -> None:
        mutations = (
            ("mtls", False),
            ("management_environment_inspected", False),
            ("engine_id", "engine-b"),
        )
        for field, replacement in mutations:
            value = valid_full_evidence()
            value["a_agent_evidence"][field] = replacement
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "node-a Agent"
            ):
                gate.verify_evidence(value, require_full=True)
        value = valid_full_evidence()
        health = value["a_agent_evidence"]["runtime_health"]
        health["observation_age_ms"] = health["freshness_threshold_ms"] + 1
        with self.assertRaisesRegex(gate.GateError, "node-a Agent"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_missing_tls_or_managed_a_network_target(self) -> None:
        value = valid_full_evidence()
        value["managed_a_network"]["postgres_tls"]["verify_full_succeeded"] = False
        with self.assertRaisesRegex(gate.GateError, "PostgreSQL TLS"):
            gate.verify_evidence(value, require_full=True)
        value = valid_full_evidence()
        value["managed_a_network"]["targets"][0]["connected"] = False
        with self.assertRaisesRegex(gate.GateError, "connectivity"):
            gate.verify_evidence(value, require_full=True)
        value = valid_full_evidence()
        value["managed_a_network"]["postgres_tls"]["ca_sha256"] = "sha256:unknown"
        with self.assertRaisesRegex(gate.GateError, "PostgreSQL TLS"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_empty_or_fake_managed_a_stack(self) -> None:
        value = valid_full_evidence()
        value["managed_a_deployments"] = {
            "storage-service": {"health": "HEALTHY"},
            "problem-service": {"health": "HEALTHY"},
            "judge-api": {"health": "HEALTHY"},
        }
        with self.assertRaisesRegex(gate.GateError, "digest-pinned Agent Job"):
            gate.verify_evidence(value, require_full=True)
        value = valid_full_evidence()
        value["managed_a_deployments"].pop("judge-api")
        with self.assertRaisesRegex(gate.GateError, "exactly Storage"):
            gate.verify_evidence(value, require_full=True)
        value = valid_full_evidence()
        value["managed_a_deployments"]["storage-service-canary"] = copy.deepcopy(
            value["managed_a_deployments"]["storage-service"]
        )
        with self.assertRaisesRegex(gate.GateError, "exactly Storage"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_unpinned_or_unleased_managed_a_runtime(self) -> None:
        value = valid_full_evidence()
        value["managed_a_deployments"]["problem-service"]["image_repo_digest"] = (
            "registry/problem:latest"
        )
        with self.assertRaisesRegex(gate.GateError, "digest-pinned Agent Job"):
            gate.verify_evidence(value, require_full=True)
        value = valid_full_evidence()
        value["managed_a_deployments"]["judge-api"]["agent_job"][
            "lease_owner_instance_id"
        ] = "other-agent"
        with self.assertRaisesRegex(gate.GateError, "digest-pinned Agent Job"):
            gate.verify_evidence(value, require_full=True)
        value = valid_full_evidence()
        value["managed_a_deployments"]["storage-service"]["operation_status"] = (
            "SUCCEEDED"
        )
        value["managed_a_deployments"]["storage-service"]["agent_job"][
            "completed_by_agent"
        ] = False
        with self.assertRaisesRegex(gate.GateError, "digest-pinned Agent Job"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_inactive_binding_or_fake_context(self) -> None:
        value = valid_full_evidence()
        value["managed_a_deployments"]["problem-service"]["bindings"][0][
            "observed_state"
        ] = "PENDING"
        with self.assertRaisesRegex(gate.GateError, "ApiBindings"):
            gate.verify_evidence(value, require_full=True)
        value = valid_full_evidence()
        value["managed_a_deployments"]["judge-api"]["service_context"][
            "mount_read_only"
        ] = False
        with self.assertRaisesRegex(gate.GateError, "ServiceContext"):
            gate.verify_evidence(value, require_full=True)
        value = valid_full_evidence()
        value["managed_a_deployments"]["problem-service"]["service_context"][
            "management_token_present"
        ] = True
        with self.assertRaisesRegex(gate.GateError, "ServiceContext"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_missing_or_wrong_event_context(self) -> None:
        value = valid_full_evidence()
        value["managed_a_deployments"]["problem-service"]["event_context"][
            "present"
        ] = False
        with self.assertRaisesRegex(gate.GateError, "EventContext"):
            gate.verify_evidence(value, require_full=True)
        value = valid_full_evidence()
        subscriptions = value["managed_a_deployments"]["judge-api"]["event_context"][
            "subscriptions"
        ]
        subscriptions[0]["consumer_group"] = "synthetic-consumer"
        with self.assertRaisesRegex(gate.GateError, "EventContext"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_fake_workload_credential_lifecycle(self) -> None:
        for field, replacement in (
            ("old_token_existing_route_status", 200),
            ("current_token_removed_route_status", 200),
            ("revoked_token_after_restore_status", 200),
            ("tokens_recorded", True),
        ):
            value = valid_full_evidence()
            lifecycle = value["workload_credential_lifecycle"]
            lifecycle[field] = replacement
            value["third_party_fixture"]["binding_lifecycle"] = copy.deepcopy(
                lifecycle
            )
            with self.subTest(field=field), self.assertRaisesRegex(
                gate.GateError, "credential lifecycle"
            ):
                gate.verify_evidence(value, require_full=True)
        value = valid_full_evidence()
        value["workload_credential_lifecycle"]["generation_restored"] = 2
        value["third_party_fixture"]["binding_lifecycle"] = copy.deepcopy(
            value["workload_credential_lifecycle"]
        )
        with self.assertRaisesRegex(gate.GateError, "credential lifecycle"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_detached_lifecycle_claim(self) -> None:
        value = valid_full_evidence()
        value["third_party_fixture"]["binding_lifecycle"][
            "restored_token_route_status"
        ] = 503
        with self.assertRaisesRegex(gate.GateError, "credential lifecycle"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_missing_or_forged_topology_rollback(self) -> None:
        mutations = (
            (
                "missing",
                lambda value: value.pop("topology_rollback"),
            ),
            (
                "wrong-action",
                lambda value: value["topology_rollback"].update(
                    operation_action="topology.apply"
                ),
            ),
            (
                "reused-revision",
                lambda value: value["topology_rollback"].update(
                    created_revision_id=value["topology_rollback"][
                        "target_revision_id"
                    ]
                ),
            ),
            (
                "drift",
                lambda value: value["topology_rollback"].update(
                    status_drift=[{"kind": "CHANGED"}]
                ),
            ),
            (
                "stale-binding",
                lambda value: value["topology_rollback"]["restored_bindings"][
                    0
                ].update(topology_revision_id="stale-revision"),
            ),
        )
        for name, mutate in mutations:
            value = valid_full_evidence()
            mutate(value)
            with self.subTest(name=name), self.assertRaisesRegex(
                gate.GateError, "[Rr]ollback"
            ):
                gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_incomplete_gateway_transcript(self) -> None:
        value = valid_full_evidence()
        value["workload_request_transcript"]["claim"]["request_headers"][
            "prefer"
        ] = "wait=1"
        with self.assertRaisesRegex(gate.GateError, "Prefer=wait=25"):
            gate.verify_evidence(value, require_full=True)
        value = valid_full_evidence()
        value["workload_request_transcript"]["source_get"]["response_sha256"] = (
            "sha256:" + "f" * 64
        )
        with self.assertRaisesRegex(gate.GateError, "source_get"):
            gate.verify_evidence(value, require_full=True)
        value = valid_full_evidence()
        value["workload_request_transcript"]["result_post"]["request_sha256"] = ""
        with self.assertRaisesRegex(gate.GateError, "result POST"):
            gate.verify_evidence(value, require_full=True)

    def test_require_full_rejects_authorization_value_in_transcript(self) -> None:
        value = valid_full_evidence()
        value["workload_request_transcript"]["package_get"]["request_headers"][
            "authorization"
        ] = "Bearer leaked"
        with self.assertRaisesRegex(gate.GateError, "package_get"):
            gate.verify_evidence(value, require_full=True)

    def test_atomic_evidence_round_trip(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "evidence.json"
            gate.atomic_json(path, valid_evidence())
            gate.verify_evidence(json.loads(path.read_text(encoding="utf-8")))
            self.assertFalse(path.with_name("evidence.json.tmp").exists())


if __name__ == "__main__":
    unittest.main()
