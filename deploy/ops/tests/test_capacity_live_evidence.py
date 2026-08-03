from __future__ import annotations

import importlib.util
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time
import unittest
import copy
from typing import Any


MODULE_PATH = (
    pathlib.Path(__file__).parents[2]
    / "capacity"
    / "orchestrator-capacity-live-evidence.py"
)
SPEC = importlib.util.spec_from_file_location("capacity_live_evidence", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
LIVE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = LIVE
SPEC.loader.exec_module(LIVE)

CANDIDATE = "a" * 40
IMAGE = "registry.example/capacity@sha256:" + "b" * 64
CONTROL_PLANE_IMAGE = "registry.example/control-plane@sha256:" + "c" * 64
AGENT_IMAGE = "registry.example/agent@sha256:" + "d" * 64
POSTGRES_IMAGE = "registry.example/postgres@sha256:" + "e" * 64
DOCKER_ENGINE_IMAGE = "registry.example/docker@sha256:" + "f" * 64


def config(root: pathlib.Path) -> dict[str, Any]:
    files = {}
    for name in (
        "inventory.yml",
        "extra-vars.json",
        "live.yml",
        "engine.py",
        "runtime.py",
        "environment.py",
        "nodes.json",
        "fixture.json",
        "ca.pem",
    ):
        path = root / name
        path.write_text(f"fixture-{name}\n", encoding="utf-8")
        files[name] = str(path)
    python = root / "python3"
    ansible = root / "ansible-playbook"
    python.write_text("python", encoding="utf-8")
    ansible.write_text("ansible", encoding="utf-8")
    token = root / "token-helper"
    restart = root / "restart-helper"
    token.write_text("token", encoding="utf-8")
    restart.write_text("restart", encoding="utf-8")
    os.chmod(token, 0o700)
    os.chmod(restart, 0o700)
    helper_manifest = root / "helper-manifest.json"
    helper_manifest.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "files": {
                    str(token): hashlib.sha256(token.read_bytes()).hexdigest(),
                    str(restart): hashlib.sha256(restart.read_bytes()).hexdigest(),
                },
            }
        ),
        encoding="utf-8",
    )
    provenance = root / "candidate-image-provenance.json"
    provenance.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "candidate_sha": CANDIDATE,
                "repository": "owner/repo",
                "source_workflow": ".github/workflows/orchestrator-candidate-images.yml",
                "source_workflow_run_id": "123",
                "source_workflow_run_attempt": 1,
                "github_oidc_issuer": "https://token.actions.githubusercontent.com",
                "control_plane": {
                    "reference": CONTROL_PLANE_IMAGE,
                    "digest": CONTROL_PLANE_IMAGE.rsplit("@", 1)[1],
                },
                "agent": {
                    "reference": AGENT_IMAGE,
                    "digest": AGENT_IMAGE.rsplit("@", 1)[1],
                },
                "capacity_fixture": {
                    "reference": IMAGE,
                    "digest": IMAGE.rsplit("@", 1)[1],
                },
            }
        ),
        encoding="utf-8",
    )
    runtime_expected = root / "runtime-expected.json"
    runtime_expected.write_text(
        json.dumps(
            {
                "schema_version": 2,
                "candidate_sha": CANDIDATE,
                "control_plane_origin": "https://capacity.example.test:8090",
                "control_plane": {"image": CONTROL_PLANE_IMAGE, "configuration": {}},
                "postgres": {"image": POSTGRES_IMAGE, "configuration": {}},
                "agent": {"image": AGENT_IMAGE},
                "engine": {"image": DOCKER_ENGINE_IMAGE},
            },
            sort_keys=True,
            separators=(",", ":"),
        ),
        encoding="utf-8",
    )
    applied_manifest = root / "applied-manifest.json"
    applied_manifest.write_text('{"schema_version":1,"files":{}}', encoding="utf-8")
    return {
        "schema_version": 1,
        "candidate_sha": CANDIDATE,
        "fixture_image": IMAGE,
        "control_plane_image": CONTROL_PLANE_IMAGE,
        "agent_image": AGENT_IMAGE,
        "postgres_image": POSTGRES_IMAGE,
        "docker_engine_image": DOCKER_ENGINE_IMAGE,
        "ansible_executable": str(ansible),
        "ansible_inventory": files["inventory.yml"],
        "ansible_extra_vars_file": files["extra-vars.json"],
        "ansible_playbook": files["live.yml"],
        "engine_evidence_script": files["engine.py"],
        "runtime_evidence_script": files["runtime.py"],
        "runtime_expected_manifest": str(runtime_expected),
        "environment_script": files["environment.py"],
        "python_executable": str(python),
        "nodes_file": files["nodes.json"],
        "fixture_file": files["fixture.json"],
        "base_url": "https://capacity.example.test:8090",
        "ca_file": files["ca.pem"],
        "token_argv_json": json.dumps([str(token)]),
        "restart_argv_json": json.dumps([str(restart)]),
        "image_provenance_record": str(provenance),
        "helper_manifest": str(helper_manifest),
        "applied_manifest": str(applied_manifest),
    }


def aggregate() -> dict[str, Any]:
    workers = []
    for worker in range(10):
        engines = []
        for engine in range(10):
            node = f"capacity-node-{worker:02d}-{engine:02d}"
            containers = [
                {
                    "deployment_id": f"deployment-{worker:02d}-{engine:02d}-{index:02d}",
                    "container_id": f"container-{worker:02d}-{engine:02d}-{index:02d}",
                }
                for index in range(20)
            ]
            engines.append({"node_id": node, "containers": containers})
        workers.append({"engines": engines})
    return {
        "schema_version": 1,
        "candidate_sha": CANDIDATE,
        "fixture_image": IMAGE,
        "worker_count": 10,
        "engine_count": 100,
        "container_count": 2_000,
        "workers": workers,
    }


def runtime_aggregate(value: dict[str, Any]) -> dict[str, Any]:
    manifest = json.loads(
        pathlib.Path(value["runtime_expected_manifest"]).read_text(encoding="utf-8")
    )
    manifest_sha = hashlib.sha256(
        json.dumps(
            manifest, ensure_ascii=False, sort_keys=True, separators=(",", ":")
        ).encode()
    ).hexdigest()
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
    control_plane = {
        "image": {
            "repo_digest": CONTROL_PLANE_IMAGE,
            "image_id": "sha256:" + "1" * 64,
        },
        "container": {
            "container_id": "2" * 64,
            "container_name": "orchestrator",
            "started_at": "2026-08-03T00:00:00Z",
        },
        "configuration": {
            "effective_sha256": "3" * 64,
            "provisioned_sha256": "3" * 64,
        },
        "database_tls_identity": {"peer_leaf_sha256": "4" * 64},
    }
    return {
        "schema_version": 2,
        "candidate_sha": CANDIDATE,
        "provision_manifest_sha256": manifest_sha,
        "host_count": 13,
        "host_identity_sha256": "5" * 64,
        "hosts": hosts,
        "control_plane": control_plane,
        "postgres": {
            "image": {"repo_digest": POSTGRES_IMAGE},
            "container": {"state": "RUNNING", "health": "HEALTHY"},
            "configuration": {
                "effective_sha256": "6" * 64,
                "provisioned_sha256": "6" * 64,
            },
            "server_leaf_sha256": "4" * 64,
        },
        "restart_identity": {
            "container_id": "2" * 64,
            "started_at": "2026-08-03T00:00:00Z",
            "image_id": "sha256:" + "1" * 64,
            "repo_digest": CONTROL_PLANE_IMAGE,
        },
        "agents": {
            "count": 100,
            "running": 100,
            "control_plane_origin": value["base_url"],
            "image": {"repo_digest": AGENT_IMAGE},
            "node_ids_sha256": "7" * 64,
            "container_ids_sha256": "8" * 64,
            "started_at_sha256": "9" * 64,
            "spiffe_ids_sha256": "a" * 64,
            "certificate_fingerprints_sha256": "b" * 64,
            "ledger_identities_sha256": "c" * 64,
            "independent_mtls_identities": 100,
            "independent_sqlite_ledgers": 100,
        },
        "engines": {
            "count": 100,
            "running": 100,
            "healthy": 100,
            "inner_daemon_count": 100,
            "container_count": 2_000,
            "image": {"repo_digest": DOCKER_ENGINE_IMAGE},
            "outer_container_ids_sha256": "d" * 64,
            "inner_daemon_ids_sha256": "e" * 64,
            "socket_volumes_sha256": "f" * 64,
            "data_volumes_sha256": "0" * 64,
        },
    }


class CapacityLiveEvidenceTests(unittest.TestCase):
    def test_observation_timeout_reports_the_enforced_limit(self):
        with self.assertRaisesRegex(
            LIVE.LiveEvidenceError,
            rf"exceeded {LIVE.MAX_OBSERVATION_SECONDS} seconds",
        ):
            LIVE.remaining(time.monotonic() - 1)

    def test_config_is_strict_absolute_and_digest_pinned(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            value = config(root)
            self.assertEqual(LIVE.validate_config(value)["candidate_sha"], CANDIDATE)

            invalid = dict(value)
            invalid["secret"] = "must not be accepted"
            with self.assertRaisesRegex(LIVE.LiveEvidenceError, "unexpected"):
                LIVE.validate_config(invalid)

            invalid = dict(value)
            invalid["fixture_image"] = "capacity:latest"
            with self.assertRaisesRegex(LIVE.LiveEvidenceError, "digest"):
                LIVE.validate_config(invalid)

            invalid = dict(value)
            invalid["nodes_file"] = "relative.json"
            with self.assertRaisesRegex(LIVE.LiveEvidenceError, "absolute"):
                LIVE.validate_config(invalid)

    def test_aggregate_identity_hashes_all_100_nodes_and_2000_resources(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            value = LIVE.aggregate_identity(
                aggregate(), config(pathlib.Path(directory))
            )
            self.assertEqual(set(value), {
                "node_ids_sha256",
                "deployment_ids_sha256",
                "container_ids_sha256",
            })
            self.assertTrue(all(len(digest) == 64 for digest in value.values()))

            incomplete = aggregate()
            incomplete["workers"][0]["engines"][0]["containers"].pop()
            with self.assertRaisesRegex(LIVE.LiveEvidenceError, "Deployment"):
                LIVE.aggregate_identity(incomplete, config(pathlib.Path(directory)))

    def test_child_process_boundary_is_argv_only_bounded_and_redacted(self) -> None:
        calls: list[tuple[list[str], dict[str, Any]]] = []

        def runner(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
            calls.append((argv, kwargs))
            return subprocess.CompletedProcess(argv, 0, "{}", "secret stderr")

        output = LIVE.run_redacted(
            ["/protected/observer", "--once"],
            time.monotonic() + 10,
            stdout=True,
            runner=runner,
        )
        self.assertEqual(output, "{}")
        self.assertIs(calls[0][1]["shell"], False)
        self.assertLessEqual(calls[0][1]["timeout"], 10)
        self.assertNotIn("secret stderr", output)

    def test_configuration_fingerprint_changes_without_exposing_input(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            value = config(root)
            config_path = root / "config.json"
            program_path = root / "live-evidence.py"
            config_path.write_text(json.dumps(value), encoding="utf-8")
            program_path.write_text("program", encoding="utf-8")
            first = LIVE.configuration_fingerprint(
                value, config_path, program_path, {}
            )
            (root / "extra-vars.json").write_text("changed secret", encoding="utf-8")
            second = LIVE.configuration_fingerprint(
                value, config_path, program_path, {}
            )
            self.assertNotEqual(first, second)
            self.assertRegex(first, r"^[0-9a-f]{64}$")

    def test_runtime_v2_requires_real_postgres_agent_and_engine_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            value = config(pathlib.Path(directory))
            runtime = runtime_aggregate(value)
            self.assertEqual(
                LIVE.validate_runtime_aggregate(runtime, value)["schema_version"], 2
            )
            for path, replacement, message in (
                (("provision_manifest_sha256",), "0" * 64, "identity"),
                (("postgres", "container", "health"), "UNHEALTHY", "configuration"),
                (("agents", "independent_mtls_identities"), 99, "configuration"),
                (("agents", "independent_sqlite_ledgers"), 99, "configuration"),
                (("engines", "inner_daemon_count"), 99, "configuration"),
            ):
                invalid = copy.deepcopy(runtime)
                target = invalid
                for key in path[:-1]:
                    target = target[key]
                target[path[-1]] = replacement
                with self.subTest(path=path), self.assertRaisesRegex(
                    LIVE.LiveEvidenceError, message
                ):
                    LIVE.validate_runtime_aggregate(invalid, value)

    def test_origin_normalization_is_exact_and_rejects_non_origins(self) -> None:
        expected = "https://capacity.example.test"
        for value in (
            "https://CAPACITY.example.test",
            "https://capacity.example.test:443/",
        ):
            self.assertEqual(LIVE.normalize_https_origin(value), expected)
        self.assertEqual(
            LIVE.normalize_https_origin("https://[2001:0DB8::1]:8443"),
            "https://[2001:db8::1]:8443",
        )
        for value in (
            "https://user@capacity.example.test",
            "https://capacity.example.test/path",
            "https://capacity.example.test?query=1",
            "https://capacity.example.test#fragment",
            " https://capacity.example.test",
        ):
            with self.assertRaises(LIVE.LiveEvidenceError):
                LIVE.normalize_https_origin(value)

    def test_helper_and_image_provenance_tampering_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            value = LIVE.validate_config(config(root))
            helper_files = LIVE.verify_helper_manifest(value)
            self.assertEqual(len(helper_files), 2)
            provenance = LIVE.verify_image_provenance(value)
            self.assertEqual(provenance["fixture_reference"], IMAGE)
            pathlib.Path(next(iter(helper_files))).write_text("changed", encoding="utf-8")
            with self.assertRaisesRegex(LIVE.LiveEvidenceError, "changed"):
                LIVE.verify_helper_manifest(value)


if __name__ == "__main__":
    unittest.main()
