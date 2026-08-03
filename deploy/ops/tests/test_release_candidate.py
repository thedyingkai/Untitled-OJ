import hashlib
import importlib.util
import json
import pathlib
import sys
import tempfile
import types
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[3]
MODULE_PATH = ROOT / "deploy" / "release" / "orchestrator-candidate.py"
SPEC = importlib.util.spec_from_file_location("orchestrator_candidate", MODULE_PATH)
candidate = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = candidate
SPEC.loader.exec_module(candidate)
SHA = "a" * 40
REPOSITORY = "owner/repo"
WORKFLOW_REF = "refs/heads/main"
CANDIDATE_RUN_ID = "123"
CANDIDATE_RUN_ATTEMPT = 1
CAPACITY_RUN_ID = "456"
PUBLISHER = "CN=OJOS"


def digest_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


class ReleaseCandidateTests(unittest.TestCase):
    def make_payload(self, directory: pathlib.Path) -> None:
        version = "1.0.0"
        for platform in ("windows-x64", "linux-x86_64"):
            for name in candidate.provenance_subject_names(version, platform):
                path = directory / name
                if name.endswith(".spdx.json"):
                    path.write_text(
                        json.dumps(
                            {"spdxVersion": "SPDX-2.3", "name": name},
                            separators=(",", ":"),
                        ),
                        encoding="utf-8",
                    )
                else:
                    path.write_bytes(("primary:" + name).encode("utf-8"))

            base = f"ojos-orchestrator-{version}-ga-{platform}"
            subjects = [
                {
                    "name": name,
                    "digest": {"sha256": candidate.sha256(directory / name)},
                }
                for name in candidate.provenance_subject_names(version, platform)
            ]
            provenance = directory / f"{base}.provenance.json"
            provenance.write_text(
                json.dumps(
                    {
                        "_type": "https://in-toto.io/Statement/v1",
                        "subject": subjects,
                        "predicateType": "https://slsa.dev/provenance/v1",
                        "predicate": {
                            "buildDefinition": {
                                "buildType": "https://github.com/ojos/orchestrator/release-v1",
                                "externalParameters": {
                                    "version": version,
                                    "channel": "ga",
                                    "platform": platform,
                                },
                                "resolvedDependencies": [
                                    {
                                        "uri": f"git+https://github.com/{REPOSITORY}",
                                        "digest": {"gitCommit": SHA},
                                    }
                                ],
                            },
                            "runDetails": {
                                "builder": {
                                    "id": f"{REPOSITORY}/.github/workflows/release.yml@{WORKFLOW_REF}"
                                },
                                "metadata": {"invocationId": CANDIDATE_RUN_ID},
                            },
                        },
                    },
                    separators=(",", ":"),
                ),
                encoding="utf-8",
            )
            checksum_names = candidate.provenance_subject_names(version, platform) + [
                provenance.name
            ]
            (directory / f"{base}.SHA256SUMS").write_text(
                "".join(
                    f"{candidate.sha256(directory / name)}  {name}\n"
                    for name in checksum_names
                ),
                encoding="utf-8",
            )

        self.assertEqual(
            sorted(path.name for path in directory.iterdir()),
            sorted(candidate.primary_names(version)),
        )
        for name in candidate.primary_names(version):
            (directory / f"{name}.sigstore.json").write_text(
                json.dumps(
                    {"mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json"}
                ),
                encoding="utf-8",
            )

    def make_authenticode(self, path: pathlib.Path) -> None:
        executables = (
            "ojos-orchestrator-daemon.exe",
            "ojos-orchestrator-tui.exe",
            "ojos-orchestrator-agent.exe",
            "ojos-orchestrator-desktop.exe",
        )
        files = []
        hashes = {
            name: digest_bytes(("signed:" + name).encode("utf-8"))
            for name in (*executables, "WebView2Loader.dll")
        }
        for location in ("build", "portable", "msi"):
            packaged_executables = (
                executables
                if location in ("build", "portable")
                else ("ojos-orchestrator-desktop.exe",)
            )
            for name in packaged_executables:
                files.append(
                    {
                        "location": f"{location}/{name}",
                        "file_name": name,
                        "sha256": hashes[name],
                        "status": "Valid",
                        "publisher_subject": PUBLISHER,
                        "publisher_thumbprint": "1" * 40,
                        "timestamp_subject": "CN=Timestamp",
                        "timestamp_thumbprint": "2" * 40,
                        "timestamp_protocol": "RFC3161",
                        "timestamp_content_type_oid": "1.2.840.113549.1.9.16.1.4",
                        "timestamp_digest_oid": "2.16.840.1.101.3.4.2.1",
                        "timestamp_digest_algorithm": "SHA256",
                        "timestamp_message_imprint_length": 32,
                        "timestamp_message_imprint": "a" * 64,
                        "timestamp_token_signature_valid": True,
                        "timestamp_parent_signature_digest_verified": True,
                        "signtool_policy": "pa/all/v",
                        "signtool_output_sha256": "3" * 64,
                    }
                )
            files.append(
                {
                    "location": f"{location}/WebView2Loader.dll",
                    "file_name": "WebView2Loader.dll",
                    "sha256": hashes["WebView2Loader.dll"],
                    "status": "Valid",
                    "publisher_subject": "CN=Microsoft Corporation",
                    "publisher_thumbprint": "4" * 40,
                    "timestamp_subject": "CN=Microsoft Timestamp",
                    "timestamp_thumbprint": "5" * 40,
                    "timestamp_protocol": "RFC3161",
                    "timestamp_content_type_oid": "1.2.840.113549.1.9.16.1.4",
                    "timestamp_digest_oid": "2.16.840.1.101.3.4.2.1",
                    "timestamp_digest_algorithm": "SHA256",
                    "timestamp_message_imprint_length": 32,
                    "timestamp_message_imprint": "b" * 64,
                    "timestamp_token_signature_valid": True,
                    "timestamp_parent_signature_digest_verified": True,
                    "retained_vendor_signature": "Microsoft",
                    "signtool_policy": "pa/all/v",
                    "signtool_output_sha256": "6" * 64,
                }
            )
        files.append(
            {
                "location": "installer/ojos-orchestrator-1.0.0-windows-x64.msi",
                "file_name": "ojos-orchestrator-1.0.0-windows-x64.msi",
                "sha256": digest_bytes(b"signed-msi"),
                "status": "Valid",
                "publisher_subject": PUBLISHER,
                "publisher_thumbprint": "7" * 40,
                "timestamp_subject": "CN=Timestamp",
                "timestamp_thumbprint": "8" * 40,
                "timestamp_protocol": "RFC3161",
                "timestamp_content_type_oid": "1.2.840.113549.1.9.16.1.4",
                "timestamp_digest_oid": "2.16.840.1.101.3.4.2.1",
                "timestamp_digest_algorithm": "SHA256",
                "timestamp_message_imprint_length": 32,
                "timestamp_message_imprint": "c" * 64,
                "timestamp_token_signature_valid": True,
                "timestamp_parent_signature_digest_verified": True,
                "signtool_policy": "pa/all/v",
                "signtool_output_sha256": "9" * 64,
            }
        )
        path.write_text(
            json.dumps(
                {
                    "schema_version": 2,
                    "candidate_sha": SHA,
                    "expected_publisher_subject": PUBLISHER,
                    "timestamp_policy": {
                        "ojos_publisher": "RFC3161/SHA256",
                        "retained_microsoft": "verify-original-and-report-protocol",
                    },
                    "files": files,
                }
            ),
            encoding="utf-8",
        )

    def make_capacity(self, path: pathlib.Path) -> None:
        fixture_image = f"registry.example/fixture@sha256:{'1' * 64}"
        control_plane_image = f"registry.example/control-plane@sha256:{'2' * 64}"
        agent_image = f"registry.example/agent@sha256:{'3' * 64}"
        postgres_image = f"registry.example/postgres@sha256:{'4' * 64}"
        engine_image = f"registry.example/docker@sha256:{'5' * 64}"
        stable_identity = {
            "fixture_image": fixture_image,
            "node_ids_sha256": "2" * 64,
            "deployment_ids_sha256": "3" * 64,
            "container_ids_sha256": "4" * 64,
            "endpoint_ids_sha256": "5" * 64,
            "link_ids_sha256": "6" * 64,
            "observer_identity_sha256": "7" * 64,
            "provenance_record_sha256": "8" * 64,
            "image_workflow_run_id": "456",
            "control_plane_image": control_plane_image,
            "agent_image": agent_image,
            "provenance_fixture_image": fixture_image,
            "control_plane_origin_sha256": "9" * 64,
            "restart_argv_sha256": "a" * 64,
            "topology_id": "topology-capacity",
            "topology_revision_id": "revision-capacity",
            "topology_identity_sha256": "b" * 64,
            "runtime_provision_manifest_sha256": "c" * 64,
            "runtime_host_identity_sha256": "d" * 64,
            "runner_machine_id_sha256": "e" * 64,
            "control_plane_image_id": "sha256:" + "f" * 64,
            "control_plane_configuration_sha256": "0" * 64,
            "postgres_image": postgres_image,
            "postgres_image_id": "sha256:" + "1" * 64,
            "postgres_container_id": "2" * 64,
            "postgres_started_at": "2026-08-01T00:00:00Z",
            "postgres_configuration_sha256": "3" * 64,
            "postgres_server_leaf_sha256": "4" * 64,
            "agent_image_id": "sha256:" + "5" * 64,
            "agent_node_ids_sha256": "6" * 64,
            "agent_container_ids_sha256": "7" * 64,
            "agent_started_at_sha256": "8" * 64,
            "agent_spiffe_ids_sha256": "9" * 64,
            "agent_certificate_fingerprints_sha256": "a" * 64,
            "agent_ledger_identities_sha256": "b" * 64,
            "agent_independent_mtls_identities": 100,
            "agent_independent_sqlite_ledgers": 100,
            "docker_engine_image": engine_image,
            "docker_engine_image_id": "sha256:" + "c" * 64,
            "engine_outer_container_ids_sha256": "d" * 64,
            "engine_inner_daemon_ids_sha256": "e" * 64,
            "engine_socket_volumes_sha256": "f" * 64,
            "engine_data_volumes_sha256": "0" * 64,
        }
        checks = []
        for sequence, phase, round_index in (
            (1, "pre_restart", None),
            (2, "post_restart", None),
            (3, "soak_boundary", None),
            (4, "operation_round", 1),
            (5, "final", None),
        ):
            started = {1: 100.0, 2: 200.0, 3: 900.0, 4: 1_000.0, 5: 1_300.0}[
                sequence
            ]
            post_restart = sequence != 1
            checks.append(
                {
                    "sequence": sequence,
                    "phase": phase,
                    "operation_round_index": round_index,
                    "post_warmup_baseline": phase == "soak_boundary",
                    "started_at_epoch_seconds": started,
                    "completed_at_epoch_seconds": started + 1.0,
                    "configuration_fingerprint_sha256": "7" * 64,
                    **stable_identity,
                    "control_plane_container_id": "f" * 64,
                    "control_plane_started_at": (
                        "2026-08-03T00:00:01Z"
                        if post_restart
                        else "2026-08-03T00:00:00Z"
                    ),
                    "aggregate_sha256": "8" * 64,
                    "workers": 10,
                    "engines": 100,
                    "containers": 2_000,
                    "running_containers": 2_000,
                    "healthy_containers": 2_000,
                    "endpoint_checks_total": 2_000,
                    "endpoint_checks_healthy": 2_000,
                    "endpoint_checks_failed": 0,
                    "link_probes_total": 8_000,
                    "link_probes_healthy": 8_000,
                    "link_probes_failed": 0,
                    "drift": 0,
                    "ok": True,
                }
            )
        checkpoint_epochs = [float(value) for value in range(100, 1_301, 30)]
        checkpoint_epochs.append(1_301.0)
        checkpoint_history = [
            {
                "sequence": index + 1,
                "epoch_seconds": epoch,
                "clock_seconds": 1_000.0 + epoch - 100.0,
            }
            for index, epoch in enumerate(checkpoint_epochs)
        ]
        path.write_text(
            json.dumps(
                {
                    "schema_version": 2,
                    "profile": "production",
                    "started_at": "1970-01-01T00:01:40Z",
                    "failures": [],
                    "identity": {
                        "source_commit": SHA,
                        "oci_revision": SHA,
                        "provenance_commit": SHA,
                        "workflow": {
                            "repository": REPOSITORY,
                            "workflow": "Orchestrator capacity and soak gate",
                            "run_id": CAPACITY_RUN_ID,
                            "run_attempt": "1",
                            "job": "production-soak",
                            "ref": WORKFLOW_REF,
                            "sha": SHA,
                        },
                        "server_build": {
                            "version": "1.0.0",
                            "commit_sha": SHA,
                            "profile": "production",
                            "target": "x86_64-unknown-linux-gnu",
                        },
                        "image_provenance": {
                            "control_plane_image": control_plane_image,
                            "agent_image": agent_image,
                            "fixture_image": fixture_image,
                            "source_workflow_run_id": "456",
                            "record_sha256": "8" * 64,
                            "source_workflow": ".github/workflows/orchestrator-candidate-images.yml",
                            "source_workflow_run_attempt": 1,
                        },
                    },
                    "operation_rounds": [
                        {"round": 0, "phase": "qualification"},
                        {"round": 1, "phase": "soak"},
                    ],
                    "environment_checks": checks,
                    "logs": {
                        "index": [
                            {
                                "kind": "environment_observations_ndjson",
                                "path": "capacity.environment.ndjson",
                                "records": 5,
                                "bytes": 1_024,
                                "sha256": "9" * 64,
                            }
                        ]
                    },
                    "evidence": {
                        "source_commit": SHA,
                        "token_refresh_count": 2,
                        "environment_observations": 5,
                        "environment_first_record": 1,
                        "environment_last_record": 5,
                        "environment_final_record": 5,
                        "environment_configuration_fingerprint_sha256": "7" * 64,
                        "environment_max_observation_gap_seconds": 300.0,
                        "environment_identity": {
                            **stable_identity,
                            "control_plane_container_id": "f" * 64,
                            "control_plane_started_at": "2026-08-03T00:00:01Z",
                        },
                        "checkpoint_interval_seconds": 30,
                        "checkpoint_clock": "CLOCK_BOOTTIME",
                        "checkpoint_history": checkpoint_history,
                        "checkpoint_count": len(checkpoint_history),
                        "completed_at": "1970-01-01T00:21:41Z",
                        "checkpointed_at": "1970-01-01T00:21:41Z",
                    },
                }
            ),
            encoding="utf-8",
        )

    def create_candidate(self, root: pathlib.Path) -> tuple[pathlib.Path, pathlib.Path]:
        payload = root / "payload"
        payload.mkdir()
        self.make_payload(payload)
        auth = root / "auth.json"
        capacity_report = root / "capacity.json"
        manifest = root / "candidate-manifest.json"
        self.make_authenticode(auth)
        self.make_capacity(capacity_report)
        candidate.create(
            types.SimpleNamespace(
                version="1.0.0",
                candidate_sha=SHA,
                candidate_run_id=CANDIDATE_RUN_ID,
                candidate_run_attempt=CANDIDATE_RUN_ATTEMPT,
                capacity_run_id=CAPACITY_RUN_ID,
                repository=REPOSITORY,
                workflow_ref=WORKFLOW_REF,
                payload_dir=payload,
                authenticode_evidence=auth,
                capacity_evidence=capacity_report,
                output=manifest,
            )
        )
        return payload, manifest

    def verify_candidate(self, payload: pathlib.Path, manifest: pathlib.Path) -> None:
        candidate.verify(
            types.SimpleNamespace(
                payload_dir=payload,
                manifest=manifest,
                expected_sha=SHA,
                expected_run_id=CANDIDATE_RUN_ID,
                expected_run_attempt=CANDIDATE_RUN_ATTEMPT,
                expected_repository=REPOSITORY,
                expected_workflow_ref=WORKFLOW_REF,
            )
        )

    def test_create_and_verify_exact_22_file_payload(self):
        with tempfile.TemporaryDirectory() as temporary:
            payload, manifest = self.create_candidate(pathlib.Path(temporary))
            self.verify_candidate(payload, manifest)
            value = json.loads(manifest.read_text(encoding="utf-8"))
            self.assertEqual(value["status"], "SECURITY_ACCEPTANCE_PENDING")
            self.assertEqual(value["schema_version"], 2)
            self.assertEqual(value["candidate_workflow_run_attempt"], 1)
            self.assertIs(value["published"], False)
            self.assertEqual(value["payload"]["primary_count"], 11)
            self.assertEqual(value["payload"]["sigstore_bundle_count"], 11)
            self.assertEqual(value["payload"]["release_file_count"], 22)
            self.assertEqual(len(value["payload"]["assets"]), 11)
            self.assertNotIn(
                "candidate-manifest.json",
                {asset["name"] for asset in value["payload"]["assets"]},
            )
            self.assertNotIn("candidate-manifest.json", candidate.expected_payload("1.0.0"))

    def test_manifest_must_remain_security_pending_and_unpublished(self):
        for field, replacement in (
            ("status", "READY_TO_PUBLISH"),
            ("published", True),
        ):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as temporary:
                payload, manifest = self.create_candidate(pathlib.Path(temporary))
                value = json.loads(manifest.read_text(encoding="utf-8"))
                value[field] = replacement
                manifest.write_text(json.dumps(value), encoding="utf-8")
                with self.assertRaisesRegex(
                    candidate.CandidateError,
                    "unpublished security-pending candidate",
                ):
                    self.verify_candidate(payload, manifest)

    def test_extra_missing_directory_or_manifest_in_payload_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            payload = pathlib.Path(temporary)
            self.make_payload(payload)
            (payload / "candidate-manifest.json").write_text("{}", encoding="utf-8")
            with self.assertRaisesRegex(candidate.CandidateError, "exact 22-file"):
                candidate.require_exact_payload(payload, "1.0.0")
        with tempfile.TemporaryDirectory() as temporary:
            payload = pathlib.Path(temporary)
            self.make_payload(payload)
            (payload / "extra-directory").mkdir()
            with self.assertRaisesRegex(candidate.CandidateError, "exact 22-file"):
                candidate.require_exact_payload(payload, "1.0.0")
        with tempfile.TemporaryDirectory() as temporary:
            payload = pathlib.Path(temporary) / "payload"
            payload.mkdir()
            with self.assertRaisesRegex(candidate.CandidateError, "must not be part"):
                candidate.require_manifest_outside_payload(
                    payload, payload / "candidate-manifest.json"
                )

    def test_capacity_identity_must_bind_every_sha_and_run_field(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / "capacity.json"
            self.make_capacity(path)
            original = json.loads(path.read_text(encoding="utf-8"))
            mutations = (
                ("source", lambda value: value["identity"].__setitem__("source_commit", "b" * 40)),
                ("oci", lambda value: value["identity"].__setitem__("oci_revision", "b" * 40)),
                ("provenance", lambda value: value["identity"].__setitem__("provenance_commit", "b" * 40)),
                ("workflow", lambda value: value["identity"]["workflow"].__setitem__("sha", "b" * 40)),
                ("workflow attempt", lambda value: value["identity"]["workflow"].__setitem__("run_attempt", "2")),
                ("server", lambda value: value["identity"]["server_build"].__setitem__("commit_sha", "b" * 40)),
                ("summary", lambda value: value["evidence"].__setitem__("source_commit", "b" * 40)),
            )
            for label, mutate in mutations:
                with self.subTest(label=label):
                    value = json.loads(json.dumps(original))
                    mutate(value)
                    with self.assertRaises(candidate.CandidateError):
                        candidate.validate_capacity_evidence(
                            value,
                            SHA,
                            CAPACITY_RUN_ID,
                            REPOSITORY,
                            WORKFLOW_REF,
                        )

    def test_capacity_environment_evidence_covers_every_phase_and_sidecar(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / "capacity.json"
            self.make_capacity(path)
            original = json.loads(path.read_text(encoding="utf-8"))
            candidate.validate_capacity_evidence(
                original, SHA, CAPACITY_RUN_ID, REPOSITORY, WORKFLOW_REF
            )
            mutations = (
                ("missing round observation", lambda value: value["environment_checks"].pop(1)),
                (
                    "missing post-warmup baseline",
                    lambda value: value["environment_checks"][2].__setitem__(
                        "post_warmup_baseline", False
                    ),
                ),
                (
                    "wrong final phase",
                    lambda value: value["environment_checks"][-1].__setitem__(
                        "phase", "operation_round"
                    ),
                ),
                (
                    "wrong round index",
                    lambda value: value["environment_checks"][3].__setitem__(
                        "operation_round_index", 2
                    ),
                ),
                (
                    "unstable resource identity",
                    lambda value: value["environment_checks"][1].__setitem__(
                        "node_ids_sha256", "b" * 64
                    ),
                ),
                (
                    "wrong summary final",
                    lambda value: value["evidence"].__setitem__(
                        "environment_final_record", 2
                    ),
                ),
                (
                    "wrong sidecar count",
                    lambda value: value["logs"]["index"][0].__setitem__(
                        "records", 2
                    ),
                ),
            )
            for label, mutate in mutations:
                with self.subTest(label=label):
                    value = json.loads(json.dumps(original))
                    mutate(value)
                    with self.assertRaises(candidate.CandidateError):
                        candidate.validate_capacity_evidence(
                            value,
                            SHA,
                            CAPACITY_RUN_ID,
                            REPOSITORY,
                            WORKFLOW_REF,
                        )

    def test_capacity_candidate_rejects_deep_runtime_and_checkpoint_tampering(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / "capacity.json"
            self.make_capacity(path)
            original = json.loads(path.read_text(encoding="utf-8"))
            mutations = (
                (
                    "runner machine",
                    lambda value: value["environment_checks"][1].__setitem__(
                        "runner_machine_id_sha256", "0" * 64
                    ),
                ),
                (
                    "PostgreSQL configuration",
                    lambda value: value["environment_checks"][1].__setitem__(
                        "postgres_configuration_sha256", "0" * 64
                    ),
                ),
                (
                    "Engine daemon set",
                    lambda value: value["environment_checks"][1].__setitem__(
                        "engine_inner_daemon_ids_sha256", "0" * 64
                    ),
                ),
                (
                    "Agent mTLS count",
                    lambda value: value["environment_checks"][1].__setitem__(
                        "agent_independent_mtls_identities", 99
                    ),
                ),
                (
                    "checkpoint sequence",
                    lambda value: value["evidence"]["checkpoint_history"][1].__setitem__(
                        "sequence", 99
                    ),
                ),
                (
                    "checkpoint gap",
                    lambda value: [
                        checkpoint.__setitem__(
                            "clock_seconds", checkpoint["clock_seconds"] + 40
                        )
                        for checkpoint in value["evidence"]["checkpoint_history"][1:]
                    ],
                ),
                (
                    "token refresh",
                    lambda value: value["evidence"].__setitem__(
                        "token_refresh_count", 1
                    ),
                ),
                (
                    "image provenance subject",
                    lambda value: value["identity"]["image_provenance"].__setitem__(
                        "agent_image", f"registry.example/agent@sha256:{'0' * 64}"
                    ),
                ),
            )
            for label, mutate in mutations:
                with self.subTest(label=label):
                    value = json.loads(json.dumps(original))
                    mutate(value)
                    with self.assertRaises(candidate.CandidateError):
                        candidate.validate_capacity_evidence(
                            value,
                            SHA,
                            CAPACITY_RUN_ID,
                            REPOSITORY,
                            WORKFLOW_REF,
                        )

    def test_authenticode_location_timestamp_and_packaging_hashes_are_exact(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / "auth.json"
            self.make_authenticode(path)
            evidence = json.loads(path.read_text(encoding="utf-8"))
            candidate.validate_authenticode_evidence(evidence, SHA, PUBLISHER)
            publisher = evidence["files"][0]
            publisher.update(
                {
                    "timestamp_protocol": "AuthenticodeLegacy",
                    "timestamp_content_type_oid": None,
                    "timestamp_digest_oid": None,
                    "timestamp_digest_algorithm": "UNKNOWN",
                    "timestamp_message_imprint_length": 0,
                    "timestamp_message_imprint": None,
                    "timestamp_token_signature_valid": None,
                    "timestamp_parent_signature_digest_verified": None,
                }
            )
            with self.assertRaisesRegex(candidate.CandidateError, "publisher"):
                candidate.validate_authenticode_evidence(evidence, SHA, PUBLISHER)
            evidence = json.loads(path.read_text(encoding="utf-8"))
            microsoft = evidence["files"][4]
            microsoft.update(
                {
                    "timestamp_protocol": "AuthenticodeLegacy",
                    "timestamp_content_type_oid": None,
                    "timestamp_digest_oid": None,
                    "timestamp_digest_algorithm": "UNKNOWN",
                    "timestamp_message_imprint_length": 0,
                    "timestamp_message_imprint": None,
                    "timestamp_token_signature_valid": None,
                    "timestamp_parent_signature_digest_verified": None,
                }
            )
            candidate.validate_authenticode_evidence(evidence, SHA, PUBLISHER)
            evidence = json.loads(path.read_text(encoding="utf-8"))
            microsoft = evidence["files"][4]
            microsoft.update(
                {
                    "timestamp_subject": None,
                    "timestamp_thumbprint": None,
                    "timestamp_protocol": "None",
                    "timestamp_content_type_oid": None,
                    "timestamp_digest_oid": None,
                    "timestamp_digest_algorithm": "NONE",
                    "timestamp_message_imprint_length": 0,
                    "timestamp_message_imprint": None,
                    "timestamp_token_signature_valid": None,
                    "timestamp_parent_signature_digest_verified": None,
                }
            )
            candidate.validate_authenticode_evidence(evidence, SHA, PUBLISHER)
            evidence = json.loads(path.read_text(encoding="utf-8"))
            evidence["files"][4]["sha256"] = "f" * 64
            with self.assertRaisesRegex(candidate.CandidateError, "packaging changed"):
                candidate.validate_authenticode_evidence(evidence, SHA, PUBLISHER)

    def test_provenance_and_checksum_tampering_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            payload = pathlib.Path(temporary)
            self.make_payload(payload)
            provenance = payload / "ojos-orchestrator-1.0.0-ga-linux-x86_64.provenance.json"
            value = json.loads(provenance.read_text(encoding="utf-8"))
            value["predicate"]["buildDefinition"]["resolvedDependencies"][0]["digest"][
                "gitCommit"
            ] = "b" * 40
            provenance.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(candidate.CandidateError, "candidate commit"):
                candidate.validate_provenance(
                    payload,
                    "1.0.0",
                    "linux-x86_64",
                    SHA,
                    CANDIDATE_RUN_ID,
                    REPOSITORY,
                    WORKFLOW_REF,
                )

            self.make_payload_fresh(payload)
            checksum = payload / "ojos-orchestrator-1.0.0-ga-linux-x86_64.SHA256SUMS"
            checksum.write_text(checksum.read_text(encoding="utf-8") + "0" * 64 + "  extra\n", encoding="utf-8")
            with self.assertRaisesRegex(candidate.CandidateError, "exactly"):
                candidate.validate_checksum_manifest(payload, "1.0.0", "linux-x86_64")

    def make_payload_fresh(self, payload: pathlib.Path) -> None:
        for path in payload.iterdir():
            path.unlink()
        self.make_payload(payload)

    def test_manifest_identity_and_uppercase_sha_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            payload, manifest = self.create_candidate(pathlib.Path(temporary))
            value = json.loads(manifest.read_text(encoding="utf-8"))
            value["repository"] = "other/repo"
            manifest.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(candidate.CandidateError, "repository/workflow"):
                self.verify_candidate(payload, manifest)
        with self.assertRaisesRegex(candidate.CandidateError, "lowercase"):
            candidate.validate_sha("A" * 40)

    def test_candidate_and_manifest_reject_every_rerun_attempt(self):
        with self.assertRaisesRegex(candidate.CandidateError, "rerun artifacts"):
            candidate.validate_run_attempt(2, "candidate_run_attempt")
        with tempfile.TemporaryDirectory() as temporary:
            payload, manifest = self.create_candidate(pathlib.Path(temporary))
            value = json.loads(manifest.read_text(encoding="utf-8"))
            value["candidate_workflow_run_attempt"] = 2
            manifest.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(candidate.CandidateError, "first workflow attempt"):
                self.verify_candidate(payload, manifest)

    def test_promotion_acceptance_binds_run_attempt_artifact_and_manifest(self):
        artifact = {
            "id": 789,
            "name": "orchestrator-v1-signed-candidate",
            "expired": False,
            "digest": "sha256:" + "d" * 64,
            "workflow_run": {"id": int(CANDIDATE_RUN_ID), "head_sha": SHA},
        }
        arguments = {
            "candidate_sha": SHA,
            "candidate_run_id": CANDIDATE_RUN_ID,
            "candidate_run_attempt": 1,
            "accepted_sha": SHA,
            "accepted_run_id": CANDIDATE_RUN_ID,
            "accepted_manifest_sha256": "e" * 64,
            "accepted_artifact_id": "789",
            "accepted_artifact_digest": "sha256:" + "d" * 64,
            "actual_artifact_archive_sha256": "d" * 64,
            "actual_manifest_sha256": "e" * 64,
        }
        self.assertEqual(
            candidate.validate_promotion_acceptance(artifact, **arguments),
            ("789", "sha256:" + "d" * 64),
        )

        mutations = (
            ("same SHA, different accepted run", {"accepted_run_id": "124"}),
            ("rerun attempt", {"candidate_run_attempt": 2}),
            ("artifact id", {"accepted_artifact_id": "790"}),
            (
                "artifact digest",
                {"accepted_artifact_digest": "sha256:" + "f" * 64},
            ),
            ("manifest digest", {"actual_manifest_sha256": "f" * 64}),
            (
                "downloaded archive digest",
                {"actual_artifact_archive_sha256": "f" * 64},
            ),
        )
        for label, replacement in mutations:
            with self.subTest(label=label), self.assertRaises(candidate.CandidateError):
                candidate.validate_promotion_acceptance(
                    artifact, **(arguments | replacement)
                )

    def test_primary_names_are_exactly_five_windows_and_six_linux(self):
        names = candidate.primary_names("1.0.0")
        self.assertEqual(len(names), 11)
        self.assertEqual(sum("windows-x64" in name for name in names), 5)
        self.assertEqual(sum("linux-x86_64" in name for name in names), 6)

    def test_trust_script_strictly_binds_cosign_and_github_attestations(self):
        script = (
            ROOT / "deploy" / "release" / "verify-orchestrator-v1-trust.sh"
        ).read_text(encoding="utf-8")
        for required in (
            "--certificate-identity \"$workflow_identity\"",
            "--certificate-github-workflow-repository \"$repository\"",
            "--certificate-github-workflow-ref \"$workflow_ref\"",
            "--certificate-github-workflow-sha \"$candidate_sha\"",
            "--certificate-github-workflow-trigger workflow_dispatch",
            "--cert-identity \"$workflow_identity\"",
            "--signer-digest \"$candidate_sha\"",
            "--source-digest \"$candidate_sha\"",
            "--source-ref \"$workflow_ref\"",
            "--deny-self-hosted-runners",
            "--expected-run-attempt \"$candidate_run_attempt\"",
            '[[ "$candidate_run_attempt" == "1" ]]',
        ):
            self.assertIn(required, script)
        self.assertNotIn("eval ", script)
        self.assertNotIn("--signer-workflow", script)
        self.assertNotIn(
            'signer_workflow="https://github.com/',
            script,
        )

    def test_powershell_uses_argument_list_and_verifies_all_packaged_copies(self):
        script = (
            ROOT / "deploy" / "release" / "verify-windows-authenticode.ps1"
        ).read_text(encoding="utf-8")
        self.assertIn("[Diagnostics.ProcessStartInfo]::new()", script)
        self.assertIn("$startInfo.ArgumentList.Add($argument)", script)
        self.assertNotIn("Start-Process", script)
        self.assertIn('-Location "msi/$desktopName"', script)
        self.assertIn('-Location "msi/WebView2Loader.dll"', script)
        self.assertIn("$evidence.Count -ne 13", script)
        self.assertIn("Get-AuthenticodeTimestampInfo", script)
        self.assertIn("Assert-Rfc3161Sha256TimestampInfo", script)
        self.assertNotIn('timestamp_protocol = "RFC3161"', script)


if __name__ == "__main__":
    unittest.main()
