from __future__ import annotations

import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from typing import Any
from unittest import mock


MODULE_PATH = (
    pathlib.Path(__file__).parents[2]
    / "capacity"
    / "orchestrator-capacity-runtime-evidence.py"
)
SPEC = importlib.util.spec_from_file_location("capacity_runtime_evidence", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
RUNTIME = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = RUNTIME
SPEC.loader.exec_module(RUNTIME)

CANDIDATE = "a" * 40
CP_IMAGE = "registry.example/control-plane@sha256:" + "b" * 64
PG_IMAGE = "registry.example/postgres@sha256:" + "c" * 64
AGENT_IMAGE = "registry.example/agent@sha256:" + "d" * 64
ENGINE_IMAGE = "registry.example/docker@sha256:" + "e" * 64
ORIGIN = "https://orchestrator-capacity.example.com:8090"
LEAF = "f" * 64
ROOT_CERT = "1" * 64


def write_env(path: pathlib.Path) -> None:
    path.write_text(
        "\n".join(
            (
                "ORCHESTRATOR_DATABASE_URL=postgresql://capacity:super-secret@db.example:5432/ojos_orchestrator?sslmode=verify-full&sslrootcert=/run/secrets/orchestrator-postgres-ca.crt",
                "ORCHESTRATOR_OIDC_ISSUER=https://login.example.com",
                "ORCHESTRATOR_OIDC_AUDIENCE=ojos-orchestrator",
                "ORCHESTRATOR_OIDC_CLIENT_ID=ojos-orchestrator-web",
                "ORCHESTRATOR_PUBLIC_BASE_URL=https://orchestrator-capacity.example.com:8090",
                "ORCHESTRATOR_OIDC_SCOPES=openid profile email",
                "ORCHESTRATOR_OIDC_ROLE_CLAIM=roles",
                "ORCHESTRATOR_OIDC_VIEWER_ROLE=viewer",
                "ORCHESTRATOR_OIDC_OPERATOR_ROLE=operator",
                "ORCHESTRATOR_OIDC_ADMIN_ROLE=admin",
                "ORCHESTRATOR_OIDC_JWKS_CACHE_SECONDS=300",
                "ORCHESTRATOR_OIDC_HTTP_TIMEOUT_SECONDS=5",
                "ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN=https://gateway-control.example.com",
                "ORCHESTRATOR_GATEWAY_ADMIN_TOKEN=gateway-admin-0123456789abcdef0123456789",
                "ORCHESTRATOR_AUTH_ADMIN_ORIGIN=https://auth-control.example.com",
                "ORCHESTRATOR_AUTH_ADMIN_TOKEN=auth-admin-0123456789abcdef012345678901",
                "ORCHESTRATOR_AUTH_WORKLOAD_ORIGIN=https://auth-workload.example.com",
                "ORCHESTRATOR_AUTH_WORKLOAD_TOKEN=auth-workload-0123456789abcdef012345678",
            )
        )
        + "\n",
        encoding="utf-8",
    )


def manifest(root: pathlib.Path) -> dict[str, Any]:
    env = root / "control-plane.env"
    write_env(env)
    return RUNTIME.generate_manifest(
        candidate_sha=CANDIDATE,
        control_plane_image=CP_IMAGE,
        postgres_image=PG_IMAGE,
        agent_image=AGENT_IMAGE,
        engine_image=ENGINE_IMAGE,
        control_plane_origin=ORIGIN,
        control_plane_listen_address="0.0.0.0",
        database_listen_address="192.0.2.10",
        postgres_database="ojos_orchestrator",
        postgres_user="capacity",
        control_plane_env_file=env,
    )


def host(role: str, ordinal: int) -> dict[str, str]:
    return {
        "role": role,
        "machine_id_sha256": f"{ordinal + 1:064x}",
        "boot_id": f"00000000-0000-4000-8000-{ordinal + 1:012x}",
    }


def image(
    reference: str, image_id_character: str, revision: str | None
) -> dict[str, Any]:
    return {
        "reference": reference,
        "repo_digest": reference,
        "image_id": "sha256:" + image_id_character * 64,
        "oci_revision": revision,
    }


def write_runtime_set(root: pathlib.Path, expected: dict[str, Any]) -> None:
    root.mkdir(parents=True, exist_ok=True)
    manifest_sha = RUNTIME.canonical_sha256(expected)
    cp_configuration = expected["control_plane"]["configuration"]
    (root / "control-plane.json").write_text(
        json.dumps(
            {
                "schema_version": 2,
                "candidate_sha": CANDIDATE,
                "provision_manifest_sha256": manifest_sha,
                "host": host("control-plane", 0),
                "image": image(CP_IMAGE, "2", CANDIDATE),
                "container": {
                    "container_id": "cp-container",
                    "container_name": "ojos-capacity-control-plane-orchestrator-1",
                    "started_at": "2026-08-03T00:00:00Z",
                    "state": "RUNNING",
                },
                "configuration": {
                    "effective_sha256": RUNTIME.canonical_sha256(cp_configuration),
                    "provisioned_sha256": RUNTIME.canonical_sha256(cp_configuration),
                    "non_sensitive": cp_configuration,
                },
                "database_tls_identity": {
                    "verified_hostname": "db.example",
                    "port": 5432,
                    "peer_leaf_sha256": LEAF,
                    "root_certificates_sha256": [ROOT_CERT],
                    "tls_version": "TLSv1.3",
                },
            }
        ),
        encoding="utf-8",
    )
    pg_configuration = {
        **expected["postgres"]["configuration"],
        "data_mount": {
            **expected["postgres"]["configuration"]["data_mount"],
            "source": "ojos-capacity-postgres_postgres-data",
        },
    }
    (root / "postgres.json").write_text(
        json.dumps(
            {
                "schema_version": 2,
                "candidate_sha": CANDIDATE,
                "provision_manifest_sha256": manifest_sha,
                "host": host("postgres", 1),
                "image": image(PG_IMAGE, "3", None),
                "container": {
                    "container_id": "postgres-container",
                    "container_name": "ojos-capacity-postgres-postgres-1",
                    "started_at": "2026-08-03T00:00:00Z",
                    "state": "RUNNING",
                    "health": "HEALTHY",
                },
                "configuration": {
                    "effective_sha256": RUNTIME.canonical_sha256(pg_configuration),
                    "provisioned_sha256": RUNTIME.canonical_sha256(pg_configuration),
                    "non_sensitive": pg_configuration,
                },
                "server_leaf_sha256": LEAF,
                "root_certificates_sha256": [ROOT_CERT],
                "settings": {
                    "ssl": "on",
                    "ssl_cert_file": "/run/secrets/server.crt",
                    "ssl_key_file": "/run/secrets/server.key",
                    "ssl_ca_file": "/run/secrets/root.crt",
                    "data_directory": "/var/lib/postgresql/data",
                    "port": "5432",
                    "postmaster_started_at": "2026-08-03 00:00:00+00",
                },
            }
        ),
        encoding="utf-8",
    )
    (root / "runner.json").write_text(
        json.dumps({"schema_version": 2, "host": host("runner", 2)}),
        encoding="utf-8",
    )
    for worker in range(10):
        agents: list[dict[str, Any]] = []
        engines: list[dict[str, Any]] = []
        for engine in range(10):
            node = f"capacity-node-{worker:02d}-{engine:02d}"
            global_ordinal = worker * 10 + engine + 1
            socket_volume = f"ojos-capacity-{worker:02d}_engine-{engine:02d}-socket"
            first_port = 20_000 + engine * 20
            agents.append(
                {
                    "node_id": node,
                    "instance": f"{node}-{CANDIDATE[:12]}",
                    "control_plane_origin": ORIGIN,
                    "container_id": f"agent-container-{worker:02d}-{engine:02d}",
                    "started_at": "2026-08-03T00:00:00Z",
                    "image_id": "sha256:" + "4" * 64,
                    "repo_digest": AGENT_IMAGE,
                    "oci_revision": CANDIDATE,
                    "state": "RUNNING",
                    "primary_user": "65532:65532",
                    "socket_supplemental_groups": ["10004"],
                    "effective_identity": {
                        "uid": 65_532,
                        "gid": 65_532,
                        "supplemental_groups": [10_004],
                        "docker_socket_gid": 10_004,
                        "docker_socket_mode": "0660",
                    },
                    "mount_identity": {
                        "socket_volume": socket_volume,
                        "ledger_source": f"/var/lib/ojos/capacity/agent-internal/{engine:02d}",
                        "export_source": f"/var/lib/ojos/capacity/workload-exports/{engine:02d}",
                        "ca_source": "/etc/ojos/capacity/control-plane-ca.pem",
                    },
                    "workload_export": {
                        "path": f"/var/lib/ojos/capacity/workload-exports/{engine:02d}",
                        "owner_uid": 65_532,
                        "mode": "0700",
                        "allowed_children": [],
                    },
                    "transport_ca_certificates_sha256": ["7" * 64],
                    "identity": {
                        "node_id": node,
                        "spiffe_id": f"spiffe://ojos.local/node/{node}",
                        "serial_hex": f"{global_ordinal:032x}",
                        "certificate_sha256": f"{global_ordinal:064x}",
                        "not_after_ms": 9_999_999_999_999,
                        "renew_after_ms": 9_999_000_000_000,
                        "node_ca_certificates_sha256": ["6" * 64],
                        "server_ca_certificates_sha256": ["7" * 64],
                        "private_key_present": True,
                        "private_key_mode": "0600",
                    },
                    "ledger": {
                        "path": f"/var/lib/ojos/capacity/agent-internal/{engine:02d}/execution-ledger.sqlite3",
                        "format": "sqlite3",
                        "device": worker + 1,
                        "inode": engine + 1,
                        "size_bytes": 4096,
                        "owner_uid": 65_532,
                    },
                    "state_root_owner_uid": 65_532,
                    "state_root_mode": "0750",
                }
            )
            engines.append(
                {
                    "engine_ordinal": engine,
                    "node_id": node,
                    "container_id": f"engine-container-{worker:02d}-{engine:02d}",
                    "started_at": "2026-08-03T00:00:00Z",
                    "state": "RUNNING",
                    "health": "HEALTHY",
                    "image_id": "sha256:" + "5" * 64,
                    "repo_digest": ENGINE_IMAGE,
                    "socket_volume": socket_volume,
                    "data_volume": f"ojos-capacity-{worker:02d}_engine-{engine:02d}-data",
                    "workload_export_mount": {
                        "type": "bind",
                        "source": f"/var/lib/ojos/capacity/workload-exports/{engine:02d}",
                        "destination": "/var/lib/ojos-workload-export",
                        "read_only": True,
                    },
                    "published_ports": [
                        {
                            "container_port": first_port + service,
                            "protocol": "tcp",
                            "host_ip": "0.0.0.0",
                            "host_port": first_port + service,
                        }
                        for service in range(20)
                    ],
                    "inner_daemon": {
                        "daemon_id": f"DAEMON-{worker:02d}-{engine:02d}-UNIQUE-ID",
                        "docker_root_dir": "/var/lib/docker",
                        "storage_driver": "overlay2",
                        "os_type": "linux",
                        "architecture": "x86_64",
                        "server_version": "28.0.0",
                        "containers": 20,
                        "containers_running": 20,
                    },
                }
            )
        (root / f"worker-{worker:02d}.json").write_text(
            json.dumps(
                {
                    "schema_version": 2,
                    "candidate_sha": CANDIDATE,
                    "provision_manifest_sha256": manifest_sha,
                    "worker_ordinal": worker,
                    "host": host(f"worker-{worker:02d}", worker + 3),
                    "agent_image": image(AGENT_IMAGE, "4", CANDIDATE),
                    "engine_image": image(ENGINE_IMAGE, "5", None),
                    "agent_count": 10,
                    "engine_count": 10,
                    "agents": agents,
                    "engines": engines,
                }
            ),
            encoding="utf-8",
        )


class RuntimeEvidenceTests(unittest.TestCase):
    def test_docker_invocations_are_argv_only_and_bounded(self) -> None:
        calls: list[tuple[list[str], dict[str, Any]]] = []

        def runner(argv: list[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
            calls.append((argv, kwargs))
            return subprocess.CompletedProcess(argv, 0, "ok", "")

        client = RUNTIME.DockerClient(runner=runner)
        self.assertEqual(client.run(("docker", "version")), "ok")
        self.assertEqual(calls[0][0], ["docker", "version"])
        self.assertIs(calls[0][1]["shell"], False)
        self.assertLessEqual(calls[0][1]["timeout"], 20)

    def test_manifest_is_canonical_and_does_not_emit_password(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            expected = manifest(pathlib.Path(directory))
            rendered = json.dumps(expected, sort_keys=True)
            self.assertNotIn("super-secret", rendered)
            database = expected["control_plane"]["configuration"]["environment"][
                "database"
            ]
            self.assertEqual(database["sslmode"], "verify-full")
            self.assertEqual(
                database["sslrootcert"],
                "/run/secrets/orchestrator-postgres-ca.crt",
            )
            self.assertEqual(expected["control_plane_origin"], ORIGIN)
            providers = expected["control_plane"]["configuration"]["environment"][
                "platform_providers"
            ]
            self.assertEqual(
                providers["gateway_admin_origin"],
                "https://gateway-control.example.com",
            )
            self.assertEqual(
                providers["credentials_present"],
                {
                    "gateway_admin": True,
                    "auth_admin": True,
                    "auth_workload": True,
                },
            )
            self.assertNotIn("gateway-admin-0123456789", rendered)
            self.assertEqual(len(RUNTIME.canonical_sha256(expected)), 64)

    def test_manifest_rejects_missing_or_insecure_platform_providers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            env = root / "control-plane.env"
            write_env(env)
            text = env.read_text(encoding="utf-8").replace(
                "ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN=https://gateway-control.example.com\n",
                "",
            )
            env.write_text(text, encoding="utf-8")
            with self.assertRaisesRegex(
                RUNTIME.RuntimeEvidenceError, "ORCHESTRATOR_GATEWAY_ADMIN_ORIGIN"
            ):
                RUNTIME.generate_manifest(
                    candidate_sha=CANDIDATE,
                    control_plane_image=CP_IMAGE,
                    postgres_image=PG_IMAGE,
                    agent_image=AGENT_IMAGE,
                    engine_image=ENGINE_IMAGE,
                    control_plane_origin=ORIGIN,
                    control_plane_listen_address="0.0.0.0",
                    database_listen_address="192.0.2.10",
                    postgres_database="ojos_orchestrator",
                    postgres_user="capacity",
                    control_plane_env_file=env,
                )

            write_env(env)
            text = env.read_text(encoding="utf-8").replace(
                "https://auth-workload.example.com", "http://auth-workload.example.com"
            )
            env.write_text(text, encoding="utf-8")
            with self.assertRaisesRegex(
                RUNTIME.RuntimeEvidenceError, "must be an HTTPS origin"
            ):
                RUNTIME.generate_manifest(
                    candidate_sha=CANDIDATE,
                    control_plane_image=CP_IMAGE,
                    postgres_image=PG_IMAGE,
                    agent_image=AGENT_IMAGE,
                    engine_image=ENGINE_IMAGE,
                    control_plane_origin=ORIGIN,
                    control_plane_listen_address="0.0.0.0",
                    database_listen_address="192.0.2.10",
                    postgres_database="ojos_orchestrator",
                    postgres_user="capacity",
                    control_plane_env_file=env,
                )

    def test_manifest_rejects_non_verify_full_database(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            env = root / "control-plane.env"
            write_env(env)
            text = env.read_text(encoding="utf-8").replace(
                "sslmode=verify-full", "sslmode=require"
            )
            env.write_text(text, encoding="utf-8")
            with self.assertRaisesRegex(
                RUNTIME.RuntimeEvidenceError, "sslmode=verify-full"
            ):
                RUNTIME.generate_manifest(
                    candidate_sha=CANDIDATE,
                    control_plane_image=CP_IMAGE,
                    postgres_image=PG_IMAGE,
                    agent_image=AGENT_IMAGE,
                    engine_image=ENGINE_IMAGE,
                    control_plane_origin=ORIGIN,
                    control_plane_listen_address="0.0.0.0",
                    database_listen_address="192.0.2.10",
                    postgres_database="ojos_orchestrator",
                    postgres_user="capacity",
                    control_plane_env_file=env,
                )

    def test_host_identity_is_hashed_and_role_bound(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            machine = root / "machine-id"
            boot = root / "boot-id"
            machine.write_text("1" * 32 + "\n", encoding="ascii")
            boot.write_text("11111111-2222-4333-8444-555555555555\n", encoding="ascii")
            observed = RUNTIME.host_identity(
                "runner", machine_id_path=machine, boot_id_path=boot
            )
            self.assertNotIn("1" * 32, observed.values())
            self.assertEqual(len(observed["machine_id_sha256"]), 64)
            self.assertEqual(observed["role"], "runner")

    def test_agent_state_binds_spiffe_certificate_private_key_and_sqlite(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            node = "capacity-node-00-00"
            generation = "01ab"
            generation_root = root / "identity" / "generations" / generation
            generation_root.mkdir(parents=True)
            (root / "identity" / "current.json").write_text(
                json.dumps({"schema_version": 1, "generation": generation}),
                encoding="utf-8",
            )
            (generation_root / "identity.json").write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "node_id": node,
                        "spiffe_id": f"spiffe://ojos.local/node/{node}",
                        "serial_hex": generation,
                        "not_after_ms": 10_000_000,
                        "renew_after_ms": 5_000_000,
                        "installed_at_ms": 1_000,
                    }
                ),
                encoding="utf-8",
            )
            for name in (
                "certificate.pem",
                "node-ca.pem",
                "server-ca.pem",
                "private-key.pem",
            ):
                (generation_root / name).write_bytes(b"x" * 128)
            (root / "execution-ledger.sqlite3").write_bytes(
                b"SQLite format 3\x00" + b"\x00" * 1008
            )

            with (
                mock.patch.object(
                    RUNTIME,
                    "require_owned_regular_file",
                    side_effect=lambda path, **_kwargs: type(
                        "OwnedAgentFileStat",
                        (),
                        {
                            **{
                                name: getattr(os.stat(path), name)
                                for name in (
                                    "st_mode",
                                    "st_size",
                                    "st_dev",
                                    "st_ino",
                                )
                            },
                            "st_uid": 65_532,
                        },
                    )(),
                ),
                mock.patch.object(
                    RUNTIME,
                    "pem_certificate_fingerprints",
                    return_value=["8" * 64],
                ),
                mock.patch.object(RUNTIME.time, "time", return_value=1.0),
                mock.patch.object(
                    RUNTIME.ssl,
                    "cert_time_to_seconds",
                    return_value=10_000,
                ),
                mock.patch.object(
                    RUNTIME.ssl._ssl,
                    "_test_decode_cert",
                    return_value={
                        "serialNumber": generation,
                        "notAfter": "ignored",
                        "subjectAltName": (
                            ("URI", f"spiffe://ojos.local/node/{node}"),
                        ),
                    },
                ),
            ):
                original_path_stat = pathlib.Path.stat

                def owned_path_stat(path: pathlib.Path, **kwargs: Any) -> Any:
                    information = original_path_stat(path, **kwargs)
                    if path != root:
                        return information
                    return type(
                        "OwnedStateRootStat",
                        (),
                        {
                            **{
                                name: getattr(information, name)
                                for name in (
                                    "st_size",
                                    "st_dev",
                                    "st_ino",
                                )
                            },
                            "st_mode": (information.st_mode & ~0o777) | 0o750,
                            "st_uid": 65_532,
                        },
                    )()

                with mock.patch.object(pathlib.Path, "stat", new=owned_path_stat):
                    observed = RUNTIME.collect_agent_state(root, node)
            self.assertEqual(
                observed["identity"]["spiffe_id"], f"spiffe://ojos.local/node/{node}"
            )
            self.assertEqual(observed["identity"]["private_key_mode"], "0600")
            self.assertEqual(observed["ledger"]["format"], "sqlite3")
            self.assertEqual(observed["state_root_owner_uid"], 65_532)
            self.assertEqual(observed["ledger"]["owner_uid"], 65_532)

    def test_workload_export_allows_only_context_and_resource_output_roots(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in ("runtime-contexts", "resource-outputs"):
                (root / name).mkdir()
            original_path_stat = pathlib.Path.stat

            def owned_directory_stat(path: pathlib.Path, **kwargs: Any) -> Any:
                information = original_path_stat(path, **kwargs)
                return type(
                    "OwnedExportDirectoryStat",
                    (),
                    {
                        "st_mode": (information.st_mode & ~0o777) | 0o700,
                        "st_uid": 65_532,
                    },
                )()

            with mock.patch.object(pathlib.Path, "stat", new=owned_directory_stat):
                observed = RUNTIME.collect_workload_export(root)
                self.assertEqual(
                    observed["allowed_children"],
                    ["resource-outputs", "runtime-contexts"],
                )
                (root / "identity").mkdir()
                with self.assertRaisesRegex(
                    RUNTIME.RuntimeEvidenceError, "Agent-internal"
                ):
                    RUNTIME.collect_workload_export(root)

    def test_agent_file_ownership_requires_workload_uid(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "agent-state"
            path.write_bytes(b"state")
            with self.assertRaisesRegex(
                RUNTIME.RuntimeEvidenceError, "ownership/mode"
            ):
                RUNTIME.require_owned_regular_file(path, expected_uid=65_532)

    def test_aggregate_proves_runtime_config_postgres_and_100_daemons(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            expected = manifest(root)
            runtime = root / "runtime"
            write_runtime_set(runtime, expected)
            evidence = RUNTIME.aggregate(runtime, expected)
            self.assertEqual(evidence["host_count"], 13)
            self.assertEqual(
                next(
                    item["machine_id_sha256"]
                    for item in evidence["hosts"]
                    if item["role"] == "runner"
                ),
                host("runner", 2)["machine_id_sha256"],
            )
            self.assertEqual(
                len({item["machine_id_sha256"] for item in evidence["hosts"]}),
                13,
            )
            self.assertEqual(evidence["agents"]["count"], 100)
            self.assertEqual(evidence["engines"]["inner_daemon_count"], 100)
            self.assertEqual(evidence["engines"]["container_count"], 2_000)
            self.assertEqual(evidence["postgres"]["container"]["health"], "HEALTHY")
            self.assertEqual(
                evidence["restart_identity"]["container_id"], "cp-container"
            )
            self.assertEqual(
                evidence["restart_identity"]["started_at"], "2026-08-03T00:00:00Z"
            )

    def test_aggregate_rejects_two_roles_on_same_physical_host(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            expected = manifest(root)
            runtime = root / "runtime"
            write_runtime_set(runtime, expected)
            runner = json.loads((runtime / "runner.json").read_text(encoding="utf-8"))
            postgres = json.loads(
                (runtime / "postgres.json").read_text(encoding="utf-8")
            )
            runner["host"]["machine_id_sha256"] = postgres["host"]["machine_id_sha256"]
            (runtime / "runner.json").write_text(json.dumps(runner), encoding="utf-8")
            with self.assertRaisesRegex(
                RUNTIME.RuntimeEvidenceError, "incomplete or duplicated"
            ):
                RUNTIME.aggregate(runtime, expected)

    def test_aggregate_rejects_duplicate_inner_docker_daemon_id(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            expected = manifest(root)
            runtime = root / "runtime"
            write_runtime_set(runtime, expected)
            worker = json.loads(
                (runtime / "worker-00.json").read_text(encoding="utf-8")
            )
            worker["engines"][1]["inner_daemon"]["daemon_id"] = worker["engines"][0][
                "inner_daemon"
            ]["daemon_id"]
            (runtime / "worker-00.json").write_text(
                json.dumps(worker), encoding="utf-8"
            )
            with self.assertRaisesRegex(
                RUNTIME.RuntimeEvidenceError, "incomplete or duplicated"
            ):
                RUNTIME.aggregate(runtime, expected)

    def test_aggregate_rejects_postgres_tls_identity_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            expected = manifest(root)
            runtime = root / "runtime"
            write_runtime_set(runtime, expected)
            postgres = json.loads(
                (runtime / "postgres.json").read_text(encoding="utf-8")
            )
            postgres["server_leaf_sha256"] = "9" * 64
            (runtime / "postgres.json").write_text(
                json.dumps(postgres), encoding="utf-8"
            )
            with self.assertRaisesRegex(
                RUNTIME.RuntimeEvidenceError, "PostgreSQL runtime/TLS"
            ):
                RUNTIME.aggregate(runtime, expected)

    def test_aggregate_rejects_effective_control_plane_config_drift(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            expected = manifest(root)
            runtime = root / "runtime"
            write_runtime_set(runtime, expected)
            control = json.loads(
                (runtime / "control-plane.json").read_text(encoding="utf-8")
            )
            control["configuration"]["effective_sha256"] = "8" * 64
            (runtime / "control-plane.json").write_text(
                json.dumps(control), encoding="utf-8"
            )
            with self.assertRaisesRegex(
                RUNTIME.RuntimeEvidenceError, "control-plane runtime"
            ):
                RUNTIME.aggregate(runtime, expected)

    def test_aggregate_rejects_agent_origin_ledger_or_engine_port_drift(self) -> None:
        mutations = (
            lambda worker: worker["agents"][0].__setitem__(
                "control_plane_origin", "https://other.example"
            ),
            lambda worker: worker["agents"][0]["mount_identity"].__setitem__(
                "ledger_source", "/var/lib/ojos/capacity/agent-internal/99"
            ),
            lambda worker: worker["engines"][0]["published_ports"][0].__setitem__(
                "host_port", 65535
            ),
            lambda worker: worker["agents"][0].__setitem__(
                "primary_user", "10004:10004"
            ),
            lambda worker: worker["agents"][0].__setitem__(
                "socket_supplemental_groups", []
            ),
            lambda worker: worker["engines"][0]["workload_export_mount"].__setitem__(
                "read_only", False
            ),
            lambda worker: worker["engines"][0]["workload_export_mount"].__setitem__(
                "source", "/var/lib/ojos/capacity/workload-exports/01"
            ),
        )
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                with tempfile.TemporaryDirectory() as directory:
                    root = pathlib.Path(directory)
                    expected = manifest(root)
                    runtime = root / "runtime"
                    write_runtime_set(runtime, expected)
                    path = runtime / "worker-00.json"
                    worker = json.loads(path.read_text(encoding="utf-8"))
                    mutate(worker)
                    path.write_text(json.dumps(worker), encoding="utf-8")
                    with self.assertRaisesRegex(
                        RUNTIME.RuntimeEvidenceError, "Node/Engine"
                    ):
                        RUNTIME.aggregate(runtime, expected)

    def test_manifest_schema_rejects_mutated_agent_command(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            expected = manifest(pathlib.Path(directory))
            expected["agent"]["command_prefix"][2] = "https://other.example"
            with self.assertRaisesRegex(
                RUNTIME.RuntimeEvidenceError, "Agent/Engine contract"
            ):
                RUNTIME.validate_manifest(expected)

    def test_manifest_schema_rejects_workload_identity_or_state_mount_drift(self) -> None:
        mutations = (
            lambda expected: expected["agent"].__setitem__(
                "primary_user", "10004:10004"
            ),
            lambda expected: expected["agent"].__setitem__(
                "socket_supplemental_groups", []
            ),
            lambda expected: expected["engine"].__setitem__(
                "workload_export_read_only", False
            ),
            lambda expected: expected["engine"].__setitem__(
                "workload_export_destination", "/wrong-export"
            ),
        )
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                with tempfile.TemporaryDirectory() as directory:
                    expected = manifest(pathlib.Path(directory))
                    mutate(expected)
                    with self.assertRaisesRegex(
                        RUNTIME.RuntimeEvidenceError, "Agent/Engine contract"
                    ):
                        RUNTIME.validate_manifest(expected)

    def test_image_revision_and_repo_digest_are_observed(self) -> None:
        valid = {
            "Id": "sha256:" + "2" * 64,
            "RepoDigests": [CP_IMAGE],
            "Config": {"Labels": {"org.opencontainers.image.revision": CANDIDATE}},
        }
        self.assertEqual(
            RUNTIME.validate_image(valid, CP_IMAGE, CANDIDATE)["repo_digest"], CP_IMAGE
        )
        valid["Config"]["Labels"]["org.opencontainers.image.revision"] = "7" * 40
        with self.assertRaisesRegex(
            RUNTIME.RuntimeEvidenceError, "revision does not match"
        ):
            RUNTIME.validate_image(valid, CP_IMAGE, CANDIDATE)

    def test_digest_pinned_third_party_image_does_not_require_revision_label(
        self,
    ) -> None:
        valid = {
            "Id": "sha256:" + "3" * 64,
            "RepoDigests": [PG_IMAGE],
            "Config": {"Labels": None},
        }
        observed = RUNTIME.validate_image(valid, PG_IMAGE)
        self.assertEqual(observed["repo_digest"], PG_IMAGE)
        self.assertIsNone(observed["oci_revision"])


if __name__ == "__main__":
    unittest.main()
