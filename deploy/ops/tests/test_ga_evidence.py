from __future__ import annotations

import hashlib
import importlib.util
import json
import pathlib
import tempfile
import unittest
import datetime
import copy


MODULE_PATH = pathlib.Path(__file__).parents[1] / "validate-orchestrator-ga-evidence.py"
SPEC = importlib.util.spec_from_file_location("validate_ga_evidence", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
VALIDATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VALIDATOR)


COMMIT = "a" * 40
FIXTURE_IMAGE = "registry.example/capacity@sha256:" + "1" * 64
CONTROL_PLANE_IMAGE = "registry.example/control-plane@sha256:" + "2" * 64
AGENT_IMAGE = "registry.example/agent@sha256:" + "3" * 64
POSTGRES_IMAGE = "registry.example/postgres@sha256:" + "4" * 64
DOCKER_ENGINE_IMAGE = "registry.example/docker@sha256:" + "5" * 64
OBSERVER_IDENTITY = {
    "program_sha256": "1" * 64,
    "config_sha256": "2" * 64,
    "applied_manifest_sha256": "3" * 64,
    "helper_manifest_sha256": "4" * 64,
    "helper_files_sha256": "5" * 64,
    "ansible_playbook_sha256": "6" * 64,
}
OBSERVER_IDENTITY_SHA256 = hashlib.sha256(
    json.dumps(OBSERVER_IDENTITY, sort_keys=True, separators=(",", ":")).encode()
).hexdigest()
WORKFLOW_CREATED_AT = "2023-11-14T19:26:40Z"
WORKFLOW_CREATED_EPOCH = 1_699_990_000.0
RUNNER_ACTIVE_ENTER_EPOCH = WORKFLOW_CREATED_EPOCH - 7_200
RUNNER_ACTIVE_ENTER_MONOTONIC_USEC = 1_000_000
RUNNER_EXEC_START_MONOTONIC_USEC = 900_000
RUNNER_CLOCK_OFFSET_USEC = 100_000_000
LISTENER_START_TICKS = 10_100
LISTENER_CLOCK_TICKS_PER_SECOND = 100
LISTENER_START_BOOTTIME_USEC = 101_000_000
API_DATE = "Tue, 14 Nov 2023 20:50:10 GMT"
API_DATE_EPOCH = 1_699_995_010.0
REPORT_STARTED_EPOCH = 1_699_995_000.0
QUALIFICATION_EPOCH = 1_699_998_900.0
PRE_RESTART_EPOCH = 1_699_999_000.0
POST_RESTART_EPOCH = 1_699_999_100.0
WARMUP_STARTED_EPOCH = 1_699_999_110.0
BOUNDARY_ENVIRONMENT_EPOCH = WARMUP_STARTED_EPOCH + 610
BOUNDARY_SAMPLE_EPOCH = BOUNDARY_ENVIRONMENT_EPOCH + 5
SOAK_STARTED_EPOCH = BOUNDARY_SAMPLE_EPOCH + 10
SOAK_COMPLETED_EPOCH = SOAK_STARTED_EPOCH + 86_400
REPORT_COMPLETED_EPOCH = SOAK_COMPLETED_EPOCH + 10


def rfc3339(epoch: float) -> str:
    return (
        datetime.datetime.fromtimestamp(epoch, datetime.timezone.utc)
        .isoformat(timespec="seconds")
        .replace("+00:00", "Z")
    )


def runtime_hosts() -> list[dict[str, str]]:
    return [
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


def runtime_host_identity_sha256() -> str:
    digest = hashlib.sha256()
    for host in sorted(runtime_hosts(), key=lambda item: item["role"]):
        digest.update(
            f"{host['role']}\0{host['machine_id_sha256']}\0{host['boot_id']}".encode()
        )
        digest.update(b"\n")
    return digest.hexdigest()


def runner_service_observation(observed_at: float) -> dict:
    observed_monotonic = RUNNER_ACTIVE_ENTER_MONOTONIC_USEC + round(
        (observed_at - RUNNER_ACTIVE_ENTER_EPOCH) * 1_000_000
    )
    active_uptime = (
        observed_monotonic - RUNNER_ACTIVE_ENTER_MONOTONIC_USEC
    ) / 1_000_000
    process_uptime = (
        observed_monotonic - RUNNER_EXEC_START_MONOTONIC_USEC
    ) / 1_000_000
    observed_boottime = observed_monotonic + RUNNER_CLOCK_OFFSET_USEC
    unit = "actions.runner.owner-repo.soak-1.service"
    control_group = f"/system.slice/{unit}"
    return {
        "schema_version": 1,
        "unit": unit,
        "boot_id": "11111111-2222-3333-4444-555555555555",
        "control_group": control_group,
        "process_control_group": control_group,
        "load_state": "loaded",
        "active_state": "active",
        "sub_state": "running",
        "active_enter_timestamp": "Tue 2023-11-14 17:26:40 UTC",
        "active_enter_monotonic_usec": RUNNER_ACTIVE_ENTER_MONOTONIC_USEC,
        "exec_main_start_timestamp": "Tue 2023-11-14 17:26:39 UTC",
        "exec_main_start_monotonic_usec": RUNNER_EXEC_START_MONOTONIC_USEC,
        "invocation_id": "1" * 32,
        "main_pid": 4321,
        "listener_pid": 9_876,
        "listener_start_ticks": LISTENER_START_TICKS,
        "listener_clock_ticks_per_second": LISTENER_CLOCK_TICKS_PER_SECOND,
        "listener_control_group": control_group,
        "listener_executable": "/opt/actions-runner/bin/Runner.Listener",
        "listener_start_boottime_usec": LISTENER_START_BOOTTIME_USEC,
        "listener_ancestor_depth": 2,
        "observer_pid": 5_555,
        "observer_start_ticks": 700_000,
        "observed_at_epoch_seconds": observed_at,
        "clock_before_monotonic_lower_usec": observed_monotonic,
        "clock_before_monotonic_upper_usec": observed_monotonic,
        "clock_before_boottime_usec": observed_boottime,
        "clock_after_monotonic_lower_usec": observed_monotonic,
        "clock_after_monotonic_upper_usec": observed_monotonic,
        "clock_after_boottime_usec": observed_boottime,
        "clock_offset_lower_usec": RUNNER_CLOCK_OFFSET_USEC,
        "clock_offset_upper_usec": RUNNER_CLOCK_OFFSET_USEC,
        "observed_monotonic_before_usec": observed_monotonic,
        "observed_monotonic_after_usec": observed_monotonic,
        "observed_monotonic_usec": observed_monotonic,
        "observed_boottime_before_usec": observed_boottime,
        "observed_boottime_after_usec": observed_boottime,
        "observed_boottime_usec": observed_boottime,
        "active_enter_epoch_seconds": observed_at - active_uptime,
        "service_start_monotonic_usec": RUNNER_ACTIVE_ENTER_MONOTONIC_USEC,
        "service_start_epoch_seconds": observed_at - active_uptime,
        "active_uptime_seconds": active_uptime,
        "main_process_uptime_seconds": process_uptime,
        "active_age_lower_bound_seconds": active_uptime,
        "listener_age_lower_bound_seconds": (
            observed_boottime - LISTENER_START_BOOTTIME_USEC
        )
        / 1_000_000,
    }


def runner_boottime_seconds(observed_at: float) -> float:
    return runner_service_observation(observed_at)["observed_boottime_usec"] / 1_000_000


def environment_check(
    sequence: int,
    phase: str,
    completed_at: float,
    operation_round_index: int | None,
) -> dict:
    post_restart = phase != "pre_restart"
    return {
        "sequence": sequence,
        "phase": phase,
        "operation_round_index": operation_round_index,
        "post_warmup_baseline": phase == "soak_boundary",
        "started_at_epoch_seconds": completed_at - 10,
        "completed_at_epoch_seconds": completed_at,
        "configuration_fingerprint_sha256": "e" * 64,
        "observer_identity_sha256": OBSERVER_IDENTITY_SHA256,
        "provenance_record_sha256": "9" * 64,
        "image_workflow_run_id": "456",
        "control_plane_image": CONTROL_PLANE_IMAGE,
        "agent_image": AGENT_IMAGE,
        "provenance_fixture_image": FIXTURE_IMAGE,
        "control_plane_origin_sha256": hashlib.sha256(
            b"https://capacity.example.test:8090"
        ).hexdigest(),
        "restart_argv_sha256": "b" * 64,
        "topology_id": "topology-capacity",
        "topology_revision_id": "revision-capacity",
        "topology_identity_sha256": "c" * 64,
        "runtime_provision_manifest_sha256": "d" * 64,
        "runtime_host_identity_sha256": runtime_host_identity_sha256(),
        "runner_machine_id_sha256": runtime_hosts()[2]["machine_id_sha256"],
        "control_plane_image_id": "sha256:" + "e" * 64,
        "control_plane_container_id": ("f" if post_restart else "e") * 64,
        "control_plane_started_at": rfc3339(
            POST_RESTART_EPOCH - 25 if post_restart else REPORT_STARTED_EPOCH - 60
        ),
        "control_plane_configuration_sha256": "8" * 64,
        "postgres_image": POSTGRES_IMAGE,
        "postgres_image_id": "sha256:" + "9" * 64,
        "postgres_container_id": "a" * 64,
        "postgres_started_at": rfc3339(REPORT_STARTED_EPOCH - 3_600),
        "postgres_configuration_sha256": "b" * 64,
        "postgres_server_leaf_sha256": "c" * 64,
        "agent_image_id": "sha256:" + "f" * 64,
        "agent_node_ids_sha256": "0" * 64,
        "agent_container_ids_sha256": "1" * 64,
        "agent_started_at_sha256": "2" * 64,
        "agent_spiffe_ids_sha256": "3" * 64,
        "agent_certificate_fingerprints_sha256": "4" * 64,
        "agent_ledger_identities_sha256": "5" * 64,
        "agent_independent_mtls_identities": 100,
        "agent_independent_sqlite_ledgers": 100,
        "docker_engine_image": DOCKER_ENGINE_IMAGE,
        "docker_engine_image_id": "sha256:" + "6" * 64,
        "engine_outer_container_ids_sha256": "7" * 64,
        "engine_inner_daemon_ids_sha256": "8" * 64,
        "engine_socket_volumes_sha256": "9" * 64,
        "engine_data_volumes_sha256": "a" * 64,
        "fixture_image": FIXTURE_IMAGE,
        "aggregate_sha256": "2" * 64,
        "node_ids_sha256": "3" * 64,
        "deployment_ids_sha256": "4" * 64,
        "container_ids_sha256": "5" * 64,
        "endpoint_ids_sha256": "6" * 64,
        "link_ids_sha256": "7" * 64,
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


def environment_sidecar_record(check: dict) -> dict:
    completed = check["completed_at_epoch_seconds"]
    hosts = runtime_hosts()
    observer_identity = OBSERVER_IDENTITY
    return {
        "sequence": check["sequence"],
        "phase": check["phase"],
        "operation_round_index": check["operation_round_index"],
        "recorded_at_epoch_seconds": completed,
        "observation": {
            "schema_version": 1,
            "candidate_sha": COMMIT,
            "started_at_epoch_seconds": check["started_at_epoch_seconds"],
            "completed_at_epoch_seconds": completed,
            "configuration_fingerprint_sha256": check[
                "configuration_fingerprint_sha256"
            ],
            "observer_identity": observer_identity,
            "provenance_identity": {
                "record_sha256": check["provenance_record_sha256"],
                "repository": "owner/repo",
                "source_workflow": ".github/workflows/orchestrator-candidate-images.yml",
                "source_workflow_run_id": check["image_workflow_run_id"],
                "source_workflow_run_attempt": 1,
                "github_oidc_issuer": "https://token.actions.githubusercontent.com",
                "control_plane_reference": CONTROL_PLANE_IMAGE,
                "control_plane_digest": CONTROL_PLANE_IMAGE.rsplit("@", 1)[1],
                "agent_reference": AGENT_IMAGE,
                "agent_digest": AGENT_IMAGE.rsplit("@", 1)[1],
                "fixture_reference": FIXTURE_IMAGE,
                "fixture_digest": FIXTURE_IMAGE.rsplit("@", 1)[1],
            },
            "deployment_identity": {
                "control_plane_origin_sha256": check[
                    "control_plane_origin_sha256"
                ],
                "restart_argv_sha256": check["restart_argv_sha256"],
                "topology_id": check["topology_id"],
                "topology_revision_id": check["topology_revision_id"],
                "topology_identity_sha256": check["topology_identity_sha256"],
            },
            "engine_evidence": {
                "fixture_image": check["fixture_image"],
                "worker_count": check["workers"],
                "engine_count": check["engines"],
                "container_count": check["containers"],
                "running_containers": check["running_containers"],
                "healthy_containers": check["healthy_containers"],
                "oldest_worker_observed_at_epoch_seconds": completed - 9,
                "newest_worker_observed_at_epoch_seconds": completed - 1,
                "worker_collection_spread_seconds": 8,
                "aggregate_sha256": check["aggregate_sha256"],
                "node_ids_sha256": check["node_ids_sha256"],
                "deployment_ids_sha256": check["deployment_ids_sha256"],
                "container_ids_sha256": check["container_ids_sha256"],
            },
            "network_evidence": {
                "checked_at_epoch_seconds": completed - 1,
                "endpoint_checks_total": check["endpoint_checks_total"],
                "endpoint_checks_healthy": check["endpoint_checks_healthy"],
                "endpoint_checks_failed": check["endpoint_checks_failed"],
                "link_probes_total": check["link_probes_total"],
                "link_probes_healthy": check["link_probes_healthy"],
                "link_probes_failed": check["link_probes_failed"],
                "drift": check["drift"],
                "endpoint_ids_sha256": check["endpoint_ids_sha256"],
                "link_ids_sha256": check["link_ids_sha256"],
            },
            "runtime_evidence": {
                "schema_version": 2,
                "candidate_sha": COMMIT,
                "provision_manifest_sha256": check[
                    "runtime_provision_manifest_sha256"
                ],
                "host_count": 13,
                "host_identity_sha256": check["runtime_host_identity_sha256"],
                "hosts": hosts,
                "control_plane": {
                    "schema_version": 2,
                    "candidate_sha": COMMIT,
                    "provision_manifest_sha256": check[
                        "runtime_provision_manifest_sha256"
                    ],
                    "host": hosts[0],
                    "image": {
                        "reference": CONTROL_PLANE_IMAGE,
                        "repo_digest": CONTROL_PLANE_IMAGE,
                        "image_id": check["control_plane_image_id"],
                        "oci_revision": COMMIT,
                    },
                    "container": {
                        "container_id": check["control_plane_container_id"],
                        "container_name": "orchestrator",
                        "started_at": check["control_plane_started_at"],
                        "state": "RUNNING",
                    },
                    "configuration": {
                        "effective_sha256": check[
                            "control_plane_configuration_sha256"
                        ],
                        "provisioned_sha256": check[
                            "control_plane_configuration_sha256"
                        ],
                        "non_sensitive": {},
                    },
                    "database_tls_identity": {
                        "verified_hostname": "postgres.capacity.internal",
                        "port": 5432,
                        "peer_leaf_sha256": check[
                            "postgres_server_leaf_sha256"
                        ],
                        "root_certificates_sha256": ["d" * 64],
                        "tls_version": "TLSv1.3",
                    },
                },
                "postgres": {
                    "schema_version": 2,
                    "candidate_sha": COMMIT,
                    "provision_manifest_sha256": check[
                        "runtime_provision_manifest_sha256"
                    ],
                    "host": hosts[1],
                    "image": {
                        "reference": check["postgres_image"],
                        "repo_digest": check["postgres_image"],
                        "image_id": check["postgres_image_id"],
                        "oci_revision": None,
                    },
                    "container": {
                        "container_id": check["postgres_container_id"],
                        "container_name": "postgres",
                        "started_at": check["postgres_started_at"],
                        "state": "RUNNING",
                        "health": "HEALTHY",
                    },
                    "configuration": {
                        "effective_sha256": check[
                            "postgres_configuration_sha256"
                        ],
                        "provisioned_sha256": check[
                            "postgres_configuration_sha256"
                        ],
                        "non_sensitive": {},
                    },
                    "server_leaf_sha256": check["postgres_server_leaf_sha256"],
                    "root_certificates_sha256": ["d" * 64],
                    "settings": {
                        "ssl": "on",
                        "ssl_cert_file": "/run/secrets/server.crt",
                        "ssl_key_file": "/run/secrets/server.key",
                        "ssl_ca_file": "/run/secrets/root.crt",
                        "data_directory": "/var/lib/postgresql/data",
                        "port": "5432",
                        "postmaster_started_at": datetime.datetime.fromtimestamp(
                            REPORT_STARTED_EPOCH - 3_600,
                            datetime.timezone.utc,
                        ).isoformat(sep=" "),
                    },
                },
                "restart_identity": {
                    "container_id": check["control_plane_container_id"],
                    "container_name": "orchestrator",
                    "started_at": check["control_plane_started_at"],
                    "image_id": check["control_plane_image_id"],
                    "repo_digest": CONTROL_PLANE_IMAGE,
                },
                "agents": {
                    "count": 100,
                    "running": 100,
                    "control_plane_origin": "https://capacity.example.test:8090",
                    "image": {
                        "reference": AGENT_IMAGE,
                        "repo_digest": AGENT_IMAGE,
                        "image_ids": [check["agent_image_id"]],
                        "oci_revision": COMMIT,
                    },
                    "node_ids_sha256": check["agent_node_ids_sha256"],
                    "container_ids_sha256": check["agent_container_ids_sha256"],
                    "started_at_sha256": check["agent_started_at_sha256"],
                    "spiffe_ids_sha256": check["agent_spiffe_ids_sha256"],
                    "certificate_fingerprints_sha256": check[
                        "agent_certificate_fingerprints_sha256"
                    ],
                    "ledger_identities_sha256": check[
                        "agent_ledger_identities_sha256"
                    ],
                    "independent_mtls_identities": check[
                        "agent_independent_mtls_identities"
                    ],
                    "independent_sqlite_ledgers": check[
                        "agent_independent_sqlite_ledgers"
                    ],
                },
                "engines": {
                    "count": 100,
                    "running": 100,
                    "healthy": 100,
                    "inner_daemon_count": 100,
                    "container_count": 2_000,
                    "image": {
                        "reference": check["docker_engine_image"],
                        "repo_digest": check["docker_engine_image"],
                        "image_ids": [check["docker_engine_image_id"]],
                    },
                    "outer_container_ids_sha256": check[
                        "engine_outer_container_ids_sha256"
                    ],
                    "inner_daemon_ids_sha256": check[
                        "engine_inner_daemon_ids_sha256"
                    ],
                    "socket_volumes_sha256": check[
                        "engine_socket_volumes_sha256"
                    ],
                    "data_volumes_sha256": check[
                        "engine_data_volumes_sha256"
                    ],
                },
            },
        },
    }


def prometheus_sidecar_record(sample: dict) -> dict:
    process = sample["process"]
    jobs = sample["jobs"]
    anomalies = sample["anomalies"]
    metrics = {
        VALIDATOR.PROMETHEUS_SAMPLE_METRICS["rss_bytes"]: process["rss_bytes"],
        VALIDATOR.PROMETHEUS_SAMPLE_METRICS["threads"]: process["threads"],
        VALIDATOR.PROMETHEUS_SAMPLE_METRICS["active_requests"]: process[
            "active_requests"
        ],
        VALIDATOR.PROMETHEUS_SAMPLE_METRICS["collection_error"]: jobs[
            "collection_error"
        ],
        VALIDATOR.PROMETHEUS_SAMPLE_METRICS["expired_leases"]: jobs[
            "expired_leases"
        ],
        VALIDATOR.PROMETHEUS_SAMPLE_METRICS[
            "oldest_leased_heartbeat_age_seconds"
        ]: jobs["oldest_leased_heartbeat_age_seconds"],
    }
    metrics.update(
        {
            metric_name: anomalies[field]
            for field, metric_name in VALIDATOR.ANOMALY_SAMPLE_METRICS.items()
        }
    )
    return {
        "sequence": sample["sequence"],
        "phase": sample["phase"],
        "sampled_at_epoch_seconds": sample["sampled_at_epoch_seconds"],
        "sample_clock_seconds": sample["sample_clock_seconds"],
        "metrics": metrics,
        "storage": copy.deepcopy(sample["storage"]),
    }


def valid_report() -> dict:
    redacted = {
        "base_origin_sha256": hashlib.sha256(
            b"https://capacity.example.test:8090"
        ).hexdigest(),
        "ca_configured": True,
        "authentication": "refreshing_oidc_helper",
        "internal_token_configured": False,
        "environment_evidence": "protected_argv_helper",
        "nodes": 100,
        "deployments": 2_000,
        "topology_resources": 10_000,
        "concurrent_operations": 50,
        "soak_seconds": 86_400,
        "warmup_seconds": 600,
        "sample_seconds": 30,
        "operation_interval_seconds": 300,
        "control_plane_image": CONTROL_PLANE_IMAGE,
        "agent_image": AGENT_IMAGE,
        "fixture_image": FIXTURE_IMAGE,
        "image_workflow_run_id": "456",
        "image_provenance_record_sha256": "9" * 64,
        "environment_observer_program_sha256": OBSERVER_IDENTITY[
            "program_sha256"
        ],
    }
    fingerprint = hashlib.sha256(
        json.dumps(redacted, separators=(",", ":"), sort_keys=True).encode("utf-8")
    ).hexdigest()
    sample_step = 86_400 / (2_736 - 1)
    warmup_samples = [
        {
            "sequence": index + 1,
            "phase": "warmup",
            "valid": True,
            "sampled_at_epoch_seconds": WARMUP_STARTED_EPOCH + index * 30,
            "sample_clock_seconds": runner_boottime_seconds(
                WARMUP_STARTED_EPOCH + index * 30
            ),
            "phase_elapsed_seconds": index * 30,
            "metrics": {
                "snapshot_record": index + 1,
                "snapshot_kind": "prometheus_snapshots_ndjson",
            },
            "process": {
                "rss_bytes": 1_000_000,
                "threads": 20,
                "active_requests": 1,
            },
            "storage": {"pool_connections": 10, "pool_idle_connections": 8},
            "jobs": {
                "collection_error": 0,
                "expired_leases": 0,
                "oldest_leased_heartbeat_age_seconds": 0,
            },
            "anomalies": {
                "expired_job_lease_transitions_total": 0,
                "operation_over_300_seconds_transitions_total": 0,
                "operation_invalid_updated_at_transitions_total": 0,
                "observation_errors_total": 0,
                "process_starts_total": 1,
                "state_loaded": 1,
                "process_start_time_seconds": REPORT_STARTED_EPOCH - 60,
            },
            "runner_service": runner_service_observation(
                WARMUP_STARTED_EPOCH + index * 30
            ),
        }
        for index in range(20)
    ]
    soak_boundary_sample = {
        "sequence": len(warmup_samples) + 1,
        "phase": "soak_boundary",
        "valid": True,
        "sampled_at_epoch_seconds": BOUNDARY_SAMPLE_EPOCH,
        "sample_clock_seconds": runner_boottime_seconds(BOUNDARY_SAMPLE_EPOCH),
        "phase_elapsed_seconds": (
            BOUNDARY_SAMPLE_EPOCH - BOUNDARY_ENVIRONMENT_EPOCH + 10
        ),
        "metrics": {
            "snapshot_record": len(warmup_samples) + 1,
            "snapshot_kind": "prometheus_snapshots_ndjson",
        },
        "process": {
            "rss_bytes": 1_000_000,
            "threads": 20,
            "active_requests": 1,
        },
        "storage": {"pool_connections": 10, "pool_idle_connections": 8},
        "jobs": {
            "collection_error": 0,
            "expired_leases": 0,
            "oldest_leased_heartbeat_age_seconds": 0,
        },
        "anomalies": copy.deepcopy(warmup_samples[-1]["anomalies"]),
        "runner_service": runner_service_observation(BOUNDARY_SAMPLE_EPOCH),
    }
    soak_samples = [
        {
            "sequence": len(warmup_samples) + index + 2,
            "phase": "soak",
            "valid": True,
            "sampled_at_epoch_seconds": SOAK_STARTED_EPOCH + index * sample_step,
            "sample_clock_seconds": runner_boottime_seconds(
                SOAK_STARTED_EPOCH + index * sample_step
            ),
            "phase_elapsed_seconds": index * sample_step,
            "metrics": {
                "snapshot_record": len(warmup_samples) + index + 2,
                "snapshot_kind": "prometheus_snapshots_ndjson",
            },
            "process": {
                "rss_bytes": 1_000_000,
                "threads": 20,
                "active_requests": 1,
            },
            "storage": {"pool_connections": 10, "pool_idle_connections": 8},
            "jobs": {
                "collection_error": 0,
                "expired_leases": 0,
                "oldest_leased_heartbeat_age_seconds": 0,
            },
            "anomalies": {
                "expired_job_lease_transitions_total": 0,
                "operation_over_300_seconds_transitions_total": 0,
                "operation_invalid_updated_at_transitions_total": 0,
                "observation_errors_total": 0,
                "process_starts_total": 1,
                "state_loaded": 1,
                "process_start_time_seconds": REPORT_STARTED_EPOCH - 60,
            },
            "runner_service": runner_service_observation(
                SOAK_STARTED_EPOCH + index * sample_step
            ),
        }
        for index in range(2_736)
    ]
    soak_samples[-1]["process"].update(
        rss_bytes=1_090_000,
        threads=22,
        active_requests=3,
    )
    soak_samples[-1]["storage"].update(
        pool_connections=12,
        pool_idle_connections=9,
    )
    samples = [*warmup_samples, soak_boundary_sample, *soak_samples]
    inventory_template = {
        "nodes_total": 100,
        "nodes_ready": 100,
        "deployments_total": 2_000,
        "deployments_running": 2_000,
        "topologies_total": 1,
        "topologies_in_sync": 1,
        "topology_resources": 10_000,
        "topology_drift": 0,
        "permanent_operations": [],
        "ok": True,
    }
    inventory_checks = [
        {
            **inventory_template,
            "phase": "qualification",
            "sampled_at_epoch_seconds": QUALIFICATION_EPOCH,
        }
    ] + [
        {
            **inventory_template,
            "phase": "soak",
            "sampled_at_epoch_seconds": SOAK_STARTED_EPOCH + index * 300,
            "phase_elapsed_seconds": index * 300,
        }
        for index in range(288)
    ]
    operation_rounds = [
        {
            "round": index + 1,
            "phase": "soak",
            "started_at_epoch_seconds": SOAK_STARTED_EPOCH + index * 300,
            "phase_elapsed_seconds": index * 300,
            "requested_operations": 50,
            "created_operations": 50,
            "event_streams_observed": 50,
            "failed_requests": 0,
            "target_nodes": 50,
            "target_deployments": 50,
            "target_containers": 50,
            "unique_created_operations": 50,
            "ok": True,
        }
        for index in range(288)
    ]
    environment_checks = [
        environment_check(1, "pre_restart", PRE_RESTART_EPOCH, None),
        environment_check(2, "post_restart", POST_RESTART_EPOCH, None),
        environment_check(3, "soak_boundary", BOUNDARY_ENVIRONMENT_EPOCH, None),
        *[
            environment_check(
                index + 4,
                "operation_round",
                SOAK_STARTED_EPOCH + index * 300,
                index + 1,
            )
            for index in range(288)
        ],
        environment_check(292, "final", SOAK_COMPLETED_EPOCH, None),
    ]
    for index, round_ in enumerate(operation_rounds):
        environment = environment_checks[index + 3]
        targets = [
            {
                "deployment_id": f"deployment-{target:03d}",
                "node_id": f"node-{target:03d}",
                "container_id": f"container-{target:03d}",
            }
            for target in range(50)
        ]
        operation_ids = [
            f"operation-{index + 1:03d}-{target:03d}" for target in range(50)
        ]
        round_.update(
            target_identities=targets,
            operation_ids=operation_ids,
            target_identities_sha256=hashlib.sha256(
                json.dumps(
                    targets, sort_keys=True, separators=(",", ":")
                ).encode("utf-8")
            ).hexdigest(),
            operation_ids_sha256=hashlib.sha256(
                json.dumps(operation_ids, separators=(",", ":")).encode("utf-8")
            ).hexdigest(),
            environment_record=environment["sequence"],
            environment_engine_aggregate_sha256=environment["aggregate_sha256"],
            environment_node_ids_sha256=environment["node_ids_sha256"],
            environment_deployment_ids_sha256=environment[
                "deployment_ids_sha256"
            ],
            environment_container_ids_sha256=environment["container_ids_sha256"],
        )
    baseline_service = runner_service_observation(1_699_995_000.0)
    api_request_monotonic = baseline_service[
        "clock_after_monotonic_upper_usec"
    ] + 10_000_000
    api_request_boottime = api_request_monotonic + RUNNER_CLOCK_OFFSET_USEC
    service_age_at_api_request = (
        api_request_monotonic
        - baseline_service["service_start_monotonic_usec"]
    ) / 1_000_000
    listener_age_at_api_request = (
        api_request_boottime - baseline_service["listener_start_boottime_usec"]
    ) / 1_000_000
    baseline_service.update(
        active_at_api_request_lower_seconds=service_age_at_api_request,
        listener_age_at_api_request_lower_seconds=listener_age_at_api_request,
        active_before_dispatch_seconds=(
            service_age_at_api_request
            + WORKFLOW_CREATED_EPOCH
            - (API_DATE_EPOCH + 1)
        ),
        listener_active_before_dispatch_seconds=(
            listener_age_at_api_request
            + WORKFLOW_CREATED_EPOCH
            - (API_DATE_EPOCH + 1)
        ),
    )
    report_started_at = rfc3339(REPORT_STARTED_EPOCH)
    report_completed_at = rfc3339(REPORT_COMPLETED_EPOCH)
    report_started_epoch = REPORT_STARTED_EPOCH
    report_completed_epoch = REPORT_COMPLETED_EPOCH
    checkpoint_epochs = [
        report_started_epoch + index * 30
        for index in range(
            int((report_completed_epoch - report_started_epoch) // 30) + 1
        )
    ]
    if checkpoint_epochs[-1] < report_completed_epoch:
        checkpoint_epochs.append(report_completed_epoch)
    checkpoint_boottime_origin = (
        runner_service_observation(report_started_epoch)["observed_boottime_usec"]
        / 1_000_000
    )
    checkpoint_history = [
        {
            "sequence": index + 1,
            "epoch_seconds": epoch,
            "clock_seconds": checkpoint_boottime_origin
            + epoch
            - report_started_epoch,
        }
        for index, epoch in enumerate(checkpoint_epochs)
    ]
    stable_environment = environment_checks[1]
    restart_revision = 1
    restart_cursor = json.dumps(
        {"job_sequences": {}, "operation_revision": restart_revision},
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8").hex()
    restart_probe = {
        "operation_id": "operation-restart-proof",
        "status": "PLANNED",
        "action": "deployment.health",
        "revision": restart_revision,
        "request_sha256": "a" * 64,
        "operation_sha256": "b" * 64,
        "event_cursor": restart_cursor,
    }
    soak_gaps = [
        right["sample_clock_seconds"] - left["sample_clock_seconds"]
        for left, right in zip(soak_samples, soak_samples[1:])
    ]
    global_gaps = [
        right["sample_clock_seconds"] - left["sample_clock_seconds"]
        for left, right in zip(samples, samples[1:])
    ]
    return {
        "schema_version": 2,
        "profile": "production",
        "started_at": report_started_at,
        "failures": [],
        "expected": {
            "nodes": 100,
            "deployments": 2_000,
            "topology_resources": 10_000,
            "concurrent_operations": 50,
        },
        "observed": {
            "nodes": 100,
            "deployments": 2_000,
            "topology_resources": 10_000,
            "concurrent_operations": 50,
        },
        "thresholds_ms": {
            "read_p95": 200,
            "mutation_accept_p95": 500,
            "event_p95": 1_000,
            "recovery": 60_000,
        },
        "measurements_ms": {
            "read_p95": 150,
            "mutation_accept_p95": 300,
            "event_p95": 700,
            "recovery": 40_000,
        },
        "identity": {
            "source_commit": COMMIT,
            "oci_revision": COMMIT,
            "provenance_commit": COMMIT,
            "server_build": {
                "version": "1.0.0",
                "commit_sha": COMMIT,
                "profile": "production",
                "target": "x86_64-unknown-linux-gnu",
            },
            "workflow": {
                "repository": "owner/repo",
                "workflow": "Orchestrator capacity and soak gate",
                "path": ".github/workflows/orchestrator-capacity.yml",
                "workflow_id": 789,
                "run_id": "123",
                "run_attempt": "1",
                "job": "production-soak",
                "ref": "refs/heads/main",
                "sha": COMMIT,
                "api_verified": True,
                "event": "workflow_dispatch",
                "head_branch": "main",
                "created_at": WORKFLOW_CREATED_AT,
                "created_at_epoch_seconds": WORKFLOW_CREATED_EPOCH,
                "api_date": API_DATE,
                "api_date_epoch_seconds": API_DATE_EPOCH,
                "api_local_received_at_epoch_seconds": API_DATE_EPOCH + 2,
                "api_local_clock_skew_seconds": 2.0,
                "api_request_monotonic_lower_usec": api_request_monotonic,
                "api_request_monotonic_upper_usec": api_request_monotonic,
                "api_request_boottime_usec": api_request_boottime,
                "api_response_monotonic_lower_usec": (
                    api_request_monotonic + 1_000_000
                ),
                "api_response_monotonic_upper_usec": (
                    api_request_monotonic + 1_000_000
                ),
                "api_response_boottime_usec": api_request_boottime + 1_000_000,
                "api_clock_offset_lower_usec": RUNNER_CLOCK_OFFSET_USEC,
                "api_clock_offset_upper_usec": RUNNER_CLOCK_OFFSET_USEC,
            },
            "runner": {
                "name": "soak-1",
                "os": "Linux",
                "arch": "X64",
                "environment": "self-hosted",
                "labels": ["self-hosted", "linux", "x64", "orchestrator-soak"],
                "expected_service_unit": (
                    "actions.runner.owner-repo.soak-1.service"
                ),
                "service": baseline_service,
            },
            "image_provenance": {
                "control_plane_image": CONTROL_PLANE_IMAGE,
                "agent_image": AGENT_IMAGE,
                "fixture_image": FIXTURE_IMAGE,
                "source_workflow_run_id": "456",
                "record_sha256": "9" * 64,
                "source_workflow": ".github/workflows/orchestrator-candidate-images.yml",
                "source_workflow_run_attempt": 1,
            },
        },
        "configuration": {
            "redacted": redacted,
            "fingerprint_sha256": fingerprint,
        },
        "process": {
            "baseline_rss_bytes": 1_000_000,
            "max_rss_bytes": 1_090_000,
            "baseline_threads": 20,
            "max_threads": 22,
            "baseline_pool_connections": 10,
            "max_pool_connections": 12,
            "baseline_active_requests": 1,
            "max_active_requests": 3,
            "max_pool_idle_connections": 9,
        },
        "evidence": {
            "source_commit": COMMIT,
            "soak_seconds_requested": 86_400,
            "soak_elapsed_seconds": 86_401,
            "warmup_seconds": 600,
            "warmup_elapsed_seconds": 610.1,
            "warmup_samples": len(warmup_samples),
            "sample_seconds": 30,
            "operation_interval_seconds": 300,
            "permanent_running_seconds": 300,
            "anomaly_counter_baseline": copy.deepcopy(
                soak_boundary_sample["anomalies"]
            ),
            "soak_boundary": {
                "sample_sequence": soak_boundary_sample["sequence"],
                "prometheus_snapshot_record": soak_boundary_sample["metrics"][
                    "snapshot_record"
                ],
                "sampled_at_epoch_seconds": soak_boundary_sample[
                    "sampled_at_epoch_seconds"
                ],
                "sample_clock_seconds": soak_boundary_sample[
                    "sample_clock_seconds"
                ],
                "environment_record": environment_checks[2]["sequence"],
                "environment_completed_at_epoch_seconds": environment_checks[2][
                    "completed_at_epoch_seconds"
                ],
                "environment_aggregate_sha256": environment_checks[2][
                    "aggregate_sha256"
                ],
                "anomalies": copy.deepcopy(soak_boundary_sample["anomalies"]),
            },
            "sampling_clock": "CLOCK_BOOTTIME",
            "soak_samples": len(soak_samples),
            "valid_soak_samples": len(soak_samples),
            "max_observed_sample_gap_seconds": max(soak_gaps),
            "max_observed_global_sample_gap_seconds": max(global_gaps),
            "soak_operation_rounds": 288,
            "restart_triggered": True,
            "restart_unavailable_observed": True,
            "restart_probe_recovered": True,
            "restart_probe_operation_id": "operation-restart-proof",
            "restart_probe_pre": copy.deepcopy(restart_probe),
            "restart_probe_post": copy.deepcopy(restart_probe),
            "token_refresh_count": 25,
            "runner_service_final": runner_service_observation(
                samples[-1]["sampled_at_epoch_seconds"] + 1
            ),
            "runner_service_observations": len(samples) + 2,
            "environment_observations": len(environment_checks),
            "environment_first_record": 1,
            "environment_last_record": len(environment_checks),
            "environment_final_record": len(environment_checks),
            "environment_configuration_fingerprint_sha256": "e" * 64,
            "environment_max_observation_gap_seconds": 300,
            "environment_identity": {
                key: stable_environment[key]
                for key in VALIDATOR.ENVIRONMENT_IDENTITY_FIELDS[1:]
            },
            "restart_pre_control_plane": {
                "container_id": environment_checks[0]["control_plane_container_id"],
                "started_at": environment_checks[0]["control_plane_started_at"],
            },
            "restart_post_control_plane": {
                "container_id": environment_checks[1]["control_plane_container_id"],
                "started_at": environment_checks[1]["control_plane_started_at"],
            },
            "checkpoint_interval_seconds": 30,
            "checkpoint_clock": "CLOCK_BOOTTIME",
            "checkpoint_history": checkpoint_history,
            "checkpoint_count": len(checkpoint_history),
            "completed_at": report_completed_at,
            "checkpointed_at": report_completed_at,
        },
        "samples": samples,
        "inventory_checks": inventory_checks,
        "operation_rounds": operation_rounds,
        "environment_checks": environment_checks,
        "logs": {
            "index": [
                {
                    "kind": "capacity_events_ndjson",
                    "path": "capacity.events.ndjson",
                    "sha256": "c" * 64,
                    "bytes": 100_000,
                    "records": 3_500,
                },
                {
                    "kind": "prometheus_snapshots_ndjson",
                    "path": "capacity.metrics.ndjson",
                    "sha256": "d" * 64,
                    "bytes": 200_000,
                    "records": len(samples),
                },
                {
                    "kind": "environment_observations_ndjson",
                    "path": "capacity.environment.ndjson",
                    "sha256": "e" * 64,
                    "bytes": 100_000,
                    "records": len(environment_checks),
                },
            ]
        },
    }


class GaEvidenceTests(unittest.TestCase):
    def test_accepts_same_commit_full_production_evidence(self) -> None:
        self.assertEqual(VALIDATOR.validate(valid_report(), COMMIT), [])

    def test_rejects_wrong_commit_and_short_soak(self) -> None:
        report = valid_report()
        report["identity"]["server_build"]["commit_sha"] = "b" * 40
        report["evidence"]["soak_seconds_requested"] = 3_600
        report["evidence"]["soak_elapsed_seconds"] = 3_601
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("server readiness build commit" in failure for failure in failures))
        self.assertTrue(any("shorter than 24 hours" in failure for failure in failures))

    def test_rejects_wrong_capacity_workflow_path_or_id(self) -> None:
        report = valid_report()
        report["identity"]["workflow"]["path"] = ".github/workflows/other.yml"
        report["identity"]["workflow"]["workflow_id"] = 0
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("workflow path" in failure for failure in failures))
        self.assertTrue(any("workflow_id" in failure for failure in failures))

    def test_recomputes_warmup_soak_round_and_process_summaries(self) -> None:
        cases = (
            (
                lambda report: report["evidence"].__setitem__("warmup_samples", 21),
                "warmup_samples",
            ),
            (
                lambda report: report["evidence"].__setitem__("soak_samples", 2_735),
                "soak_samples",
            ),
            (
                lambda report: report["evidence"].__setitem__(
                    "valid_soak_samples", 2_735
                ),
                "valid_soak_samples",
            ),
            (
                lambda report: report["evidence"].__setitem__(
                    "max_observed_sample_gap_seconds", 30
                ),
                "max_observed_sample_gap_seconds",
            ),
            (
                lambda report: report["evidence"].__setitem__(
                    "max_observed_global_sample_gap_seconds", 30
                ),
                "max_observed_global_sample_gap_seconds",
            ),
            (
                lambda report: report["evidence"].__setitem__(
                    "soak_operation_rounds", 289
                ),
                "soak_operation_rounds",
            ),
            (
                lambda report: report["process"].__setitem__(
                    "max_rss_bytes", 1_080_000
                ),
                "process.max_rss_bytes",
            ),
        )
        for mutate, expected_message in cases:
            report = valid_report()
            mutate(report)
            with self.subTest(expected_message=expected_message):
                failures = VALIDATOR.validate(report, COMMIT)
                self.assertTrue(
                    any(expected_message in failure for failure in failures), failures
                )

    def test_requires_exact_warmup_and_recomputed_redacted_configuration(self) -> None:
        report = valid_report()
        report["evidence"]["warmup_seconds"] = 601
        report["configuration"]["redacted"]["warmup_seconds"] = 601
        report["evidence"]["warmup_elapsed_seconds"] = 599
        report["configuration"]["redacted"]["nodes"] = 101
        canonical = json.dumps(
            report["configuration"]["redacted"],
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
        report["configuration"]["fingerprint_sha256"] = hashlib.sha256(
            canonical
        ).hexdigest()
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("exactly 600" in failure for failure in failures))
        self.assertTrue(any("shorter than 600" in failure for failure in failures))
        self.assertTrue(
            any("redacted.nodes does not match" in failure for failure in failures)
        )

    def test_rejects_changed_restart_probe_snapshot_or_cursor(self) -> None:
        report = valid_report()
        report["evidence"]["restart_probe_post"]["operation_sha256"] = "c" * 64
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("changed across" in failure for failure in failures))

        report = valid_report()
        report["evidence"]["restart_probe_pre"]["event_cursor"] = "not-a-cursor"
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("event cursor" in failure for failure in failures))

    def test_rejects_unproven_operation_targets_and_environment_linkage(self) -> None:
        report = valid_report()
        report["operation_rounds"][0]["operation_ids"][1] = report[
            "operation_rounds"
        ][0]["operation_ids"][0]
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("unique sorted" in failure for failure in failures))

        report = valid_report()
        report["operation_rounds"][0]["target_identities_sha256"] = "0" * 64
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("target identity digest" in failure for failure in failures))

        report = valid_report()
        report["operation_rounds"][0][
            "environment_container_ids_sha256"
        ] = "0" * 64
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("does not match its environment" in failure for failure in failures))

    def test_rejects_anomaly_counter_growth_after_the_soak_baseline(self) -> None:
        for counter, increased_value in (
            ("expired_job_lease_transitions_total", 1),
            ("process_starts_total", 2),
        ):
            report = valid_report()
            report["samples"][21]["anomalies"][counter] = increased_value
            failures = VALIDATOR.validate(report, COMMIT)
            with self.subTest(counter=counter):
                self.assertTrue(
                    any("changed after baseline" in failure for failure in failures),
                    failures,
                )

    def test_requires_exact_pre_operation_soak_boundary_binding(self) -> None:
        missing = valid_report()
        missing["samples"].pop(20)
        for sequence, sample in enumerate(missing["samples"], start=1):
            sample["sequence"] = sequence
            sample["metrics"]["snapshot_record"] = sequence
        missing["logs"]["index"][1]["records"] = len(missing["samples"])
        failures = VALIDATOR.validate(missing, COMMIT)
        self.assertTrue(any("exactly one soak boundary" in failure for failure in failures))

        wrong_environment = valid_report()
        wrong_environment["evidence"]["soak_boundary"]["environment_record"] = 4
        failures = VALIDATOR.validate(wrong_environment, COMMIT)
        self.assertTrue(any("exact baseline" in failure for failure in failures))

        late_boundary = valid_report()
        late_boundary["operation_rounds"][0][
            "started_at_epoch_seconds"
        ] = late_boundary["samples"][20]["sampled_at_epoch_seconds"] - 0.001
        failures = VALIDATOR.validate(late_boundary, COMMIT)
        self.assertTrue(any("after the first soak Operation" in failure for failure in failures))

    def test_prometheus_sidecar_is_bound_to_every_sample_field(self) -> None:
        report = valid_report()
        snapshots = [prometheus_sidecar_record(sample) for sample in report["samples"]]
        failures: list[str] = []
        summary = VALIDATOR.validate_samples(report, failures)
        self.assertEqual(failures, [])
        VALIDATOR.validate_prometheus_sidecar(snapshots, report, summary, failures)
        self.assertEqual(failures, [])

        mutations = (
            ("sequence", lambda record: record.__setitem__("sequence", 999)),
            (
                "timestamp",
                lambda record: record.__setitem__(
                    "sampled_at_epoch_seconds",
                    record["sampled_at_epoch_seconds"] + 0.001,
                ),
            ),
            (
                "rss",
                lambda record: record["metrics"].__setitem__(
                    VALIDATOR.PROMETHEUS_SAMPLE_METRICS["rss_bytes"], 999
                ),
            ),
            (
                "pool",
                lambda record: record["storage"].__setitem__(
                    "pool_connections", 11
                ),
            ),
            (
                "anomaly",
                lambda record: record["metrics"].__setitem__(
                    VALIDATOR.ANOMALY_SAMPLE_METRICS[
                        "expired_job_lease_transitions_total"
                    ],
                    1,
                ),
            ),
        )
        for name, mutate in mutations:
            invalid = copy.deepcopy(snapshots[20])
            mutate(invalid)
            snapshots[20] = invalid
            candidate_failures: list[str] = []
            VALIDATOR.validate_prometheus_sidecar(
                snapshots, report, summary, candidate_failures
            )
            with self.subTest(name=name):
                self.assertTrue(candidate_failures)
            snapshots[20] = prometheus_sidecar_record(report["samples"][20])

    def test_rejects_missing_reordered_or_gapped_checkpoint_history(self) -> None:
        missing = valid_report()
        missing["evidence"].pop("checkpoint_history")
        failures = VALIDATOR.validate(missing, COMMIT)
        self.assertTrue(any("checkpoint history" in failure for failure in failures))

        reordered = valid_report()
        reordered["evidence"]["checkpoint_history"][10]["sequence"] = 99
        failures = VALIDATOR.validate(reordered, COMMIT)
        self.assertTrue(any("checkpoint sequence" in failure for failure in failures))

        gapped = valid_report()
        for checkpoint in gapped["evidence"]["checkpoint_history"][11:]:
            checkpoint["clock_seconds"] += 10
        failures = VALIDATOR.validate(gapped, COMMIT)
        self.assertTrue(any("gap over 35" in failure for failure in failures))

    def test_rejects_cross_year_or_stale_timeline_evidence(self) -> None:
        cross_year = valid_report()
        cross_year["started_at"] = "2026-08-01T00:00:00Z"
        cross_year["evidence"]["completed_at"] = "2026-08-02T00:00:00Z"
        cross_year["evidence"]["checkpointed_at"] = "2026-08-02T00:00:00Z"
        failures = VALIDATOR.validate(cross_year, COMMIT)
        self.assertTrue(
            any(
                "workflow dispatch postdates report start" in failure
                or "outside the report lifetime" in failure
                for failure in failures
            ),
            failures,
        )

        stale_environment = valid_report()
        stale_environment["environment_checks"][2][
            "started_at_epoch_seconds"
        ] = REPORT_STARTED_EPOCH - 120
        stale_environment["environment_checks"][2][
            "completed_at_epoch_seconds"
        ] = REPORT_STARTED_EPOCH - 60
        failures = VALIDATOR.validate(stale_environment, COMMIT)
        self.assertTrue(
            any("environment_checks[2] falls outside" in failure for failure in failures),
            failures,
        )

    def test_rejects_warmup_boundary_global_gap_and_phase_clock_drift(self) -> None:
        boundary_gap = valid_report()
        warmup = [
            sample for sample in boundary_gap["samples"] if sample["phase"] == "warmup"
        ]
        boundary = next(
            sample
            for sample in boundary_gap["samples"]
            if sample["phase"] == "soak_boundary"
        )
        # Keep every wall/phase/service binding within its permitted 30-second
        # correlation tolerance while making the actual runner BOOTTIME gap
        # 99 seconds. A wall-clock-derived gap would see only 70 seconds and
        # incorrectly accept this report.
        for sample in warmup:
            sample["sampled_at_epoch_seconds"] -= 25
            sample["sample_clock_seconds"] -= 54
            sample["runner_service"] = runner_service_observation(
                sample["sampled_at_epoch_seconds"]
            )
        wall_gap = (
            boundary["sampled_at_epoch_seconds"]
            - warmup[-1]["sampled_at_epoch_seconds"]
        )
        boottime_gap = (
            boundary["sample_clock_seconds"] - warmup[-1]["sample_clock_seconds"]
        )
        self.assertEqual(wall_gap, 70)
        self.assertEqual(boottime_gap, 99)
        boundary_gap["evidence"]["warmup_elapsed_seconds"] = 635
        boundary_gap["evidence"][
            "max_observed_global_sample_gap_seconds"
        ] = boottime_gap
        failures = VALIDATOR.validate(boundary_gap, COMMIT)
        self.assertEqual(
            failures,
            [
                "evidence summary contains a global sample gap over 90 seconds",
                "global warmup/boundary/soak sample gap 99.00s exceeds 90s",
            ],
        )

        phase_drift = valid_report()
        for sample in phase_drift["samples"][100:]:
            if sample["phase"] == "soak":
                sample["phase_elapsed_seconds"] += 45
        failures = VALIDATOR.validate(phase_drift, COMMIT)
        self.assertTrue(
            any("epoch/phase_elapsed timeline" in failure for failure in failures),
            failures,
        )

    def test_runtime_v2_sidecar_is_deeply_bound_to_the_report(self) -> None:
        check = valid_report()["environment_checks"][1]
        record = environment_sidecar_record(check)
        VALIDATOR.validate_environment_sidecar_record(
            record, check, COMMIT, "runtime record"
        )
        for path, replacement, message in (
            (("provision_manifest_sha256",), "0" * 64, "runtime identity"),
            (("postgres", "container", "health"), "UNHEALTHY", "PostgreSQL"),
            (("agents", "independent_mtls_identities"), 99, "Agent"),
            (("agents", "ledger_identities_sha256"), "0" * 64, "Agent"),
            (("engines", "inner_daemon_count"), 99, "Docker Engine"),
        ):
            invalid = copy.deepcopy(record)
            target = invalid["observation"]["runtime_evidence"]
            for key in path[:-1]:
                target = target[key]
            target[path[-1]] = replacement
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, message):
                VALIDATOR.validate_environment_sidecar_record(
                    invalid, check, COMMIT, "runtime record"
                )
        wrong_runner = copy.deepcopy(check)
        wrong_runner["runner_machine_id_sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "runner host"):
            VALIDATOR.validate_environment_sidecar_record(
                record, wrong_runner, COMMIT, "runtime record"
            )

    def test_rejects_weakened_scale_latency_and_rss(self) -> None:
        report = valid_report()
        report["observed"]["nodes"] = 99
        report["thresholds_ms"]["read_p95"] = 201
        report["process"]["max_rss_bytes"] = 1_100_001
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("observed.nodes" in failure for failure in failures))
        self.assertTrue(any("thresholds_ms.read_p95" in failure for failure in failures))
        self.assertTrue(any("RSS evidence" in failure for failure in failures))

    def test_rejects_evidence_that_did_not_observe_a_real_restart(self) -> None:
        report = valid_report()
        report["evidence"]["restart_triggered"] = False
        report["evidence"]["restart_unavailable_observed"] = False
        report["evidence"]["restart_probe_recovered"] = False
        report["evidence"]["restart_probe_operation_id"] = ""
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("real control-plane restart" in failure for failure in failures))
        self.assertTrue(any("never became unavailable" in failure for failure in failures))
        self.assertTrue(any("not recovered" in failure for failure in failures))
        self.assertTrue(any("no restart probe" in failure for failure in failures))

    def test_rejects_sample_gap_unhealthy_node_and_static_token_configuration(self) -> None:
        report = valid_report()
        report["samples"][100]["phase_elapsed_seconds"] += 100
        report["inventory_checks"][20]["nodes_ready"] = 99
        report["inventory_checks"][20]["ok"] = False
        report["configuration"]["redacted"]["authentication"] = "static"
        canonical = json.dumps(
            report["configuration"]["redacted"], separators=(",", ":"), sort_keys=True
        ).encode("utf-8")
        report["configuration"]["fingerprint_sha256"] = hashlib.sha256(canonical).hexdigest()
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("strictly increasing" in failure or "sample gap" in failure for failure in failures))
        self.assertTrue(any("all Nodes ready" in failure for failure in failures))
        self.assertTrue(any("refreshing OIDC helper" in failure for failure in failures))

    def test_rejects_tampered_configuration_fingerprint_and_pool_growth(self) -> None:
        report = valid_report()
        report["configuration"]["fingerprint_sha256"] = "0" * 64
        report["process"]["max_pool_connections"] = 13
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("fingerprint" in failure for failure in failures))
        self.assertTrue(any("connection-pool" in failure for failure in failures))

    def test_rejects_runner_service_without_one_hour_before_dispatch(self) -> None:
        report = valid_report()
        created_at = RUNNER_ACTIVE_ENTER_EPOCH + 3_599.9
        report["identity"]["workflow"]["created_at_epoch_seconds"] = created_at
        report["identity"]["workflow"]["created_at"] = (
            VALIDATOR.datetime.datetime.fromtimestamp(
                created_at, tz=VALIDATOR.datetime.timezone.utc
            ).isoformat(timespec="milliseconds").replace("+00:00", "Z")
        )
        report["identity"]["runner"]["service"][
            "active_before_dispatch_seconds"
        ] = (
            report["identity"]["runner"]["service"][
                "active_at_api_request_lower_seconds"
            ]
            + created_at
            - (API_DATE_EPOCH + 1)
        )
        report["identity"]["runner"]["service"][
            "listener_active_before_dispatch_seconds"
        ] = (
            report["identity"]["runner"]["service"][
                "listener_age_at_api_request_lower_seconds"
            ]
            + created_at
            - (API_DATE_EPOCH + 1)
        )
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("less than one hour" in failure for failure in failures))

    def test_rejects_runner_service_restart_or_missing_final_observation(self) -> None:
        report = valid_report()
        report["samples"][100]["runner_service"]["invocation_id"] = "2" * 32
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("breaks runner service continuity" in failure for failure in failures))

        report = valid_report()
        report["samples"][100]["runner_service"]["listener_pid"] = 9_877
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("listener_pid" in failure for failure in failures))

        report = valid_report()
        listener = report["samples"][100]["runner_service"]
        listener["listener_start_ticks"] += 100
        listener["listener_start_boottime_usec"] += 1_000_000
        listener["listener_age_lower_bound_seconds"] -= 1
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("listener_start_ticks" in failure for failure in failures))

        report = valid_report()
        del report["evidence"]["runner_service_final"]
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("runner_service_final" in failure for failure in failures))

    def test_rejects_runner_boot_cgroup_or_suspend_discontinuity(self) -> None:
        report = valid_report()
        report["samples"][100]["runner_service"]["boot_id"] = (
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        )
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("boot_id" in failure for failure in failures))

        report = valid_report()
        report["samples"][100]["runner_service"][
            "process_control_group"
        ] = "/system.slice/unrelated.service"
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("outside systemd ControlGroup" in failure for failure in failures))

        report = valid_report()
        service = report["samples"][100]["runner_service"]
        for field in (
            "clock_before_boottime_usec",
            "clock_after_boottime_usec",
            "observed_boottime_before_usec",
            "observed_boottime_after_usec",
            "observed_boottime_usec",
        ):
            service[field] += 120_000_000
        service["clock_offset_lower_usec"] += 120_000_000
        service["clock_offset_upper_usec"] += 120_000_000
        service["listener_age_lower_bound_seconds"] += 120
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("suspend between" in failure for failure in failures))

    def test_rejects_runner_unit_control_group_and_identity_mismatches(self) -> None:
        report = valid_report()
        for observation in [
            report["identity"]["runner"]["service"],
            *(sample["runner_service"] for sample in report["samples"]),
            report["evidence"]["runner_service_final"],
        ]:
            observation["unit"] = "actions.runner.owner-repo.different.service"
            observation["control_group"] = "/system.slice/unrelated.service"
            observation["process_control_group"] = "/system.slice/unrelated.service"
            observation["listener_control_group"] = "/system.slice/unrelated.service"
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("does not match identity.runner.name" in failure for failure in failures))

        report = valid_report()
        baseline = report["identity"]["runner"]["service"]
        baseline["control_group"] = "/system.slice/unrelated.service"
        baseline["process_control_group"] = "/system.slice/unrelated.service"
        baseline["listener_control_group"] = "/system.slice/unrelated.service"
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("does not identify its unit" in failure for failure in failures))

    def test_rejects_rerun_wall_clock_skew_and_fixed_identity_fields(self) -> None:
        report = valid_report()
        report["identity"]["workflow"]["run_attempt"] = "2"
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("rerun" in failure or "first attempt" in failure for failure in failures))

        report = valid_report()
        report["identity"]["workflow"][
            "api_local_received_at_epoch_seconds"
        ] = API_DATE_EPOCH - 200
        report["identity"]["workflow"]["api_local_clock_skew_seconds"] = -200.0
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("over 30 seconds" in failure for failure in failures))

        report = valid_report()
        report["identity"]["workflow"]["workflow"] = "Different workflow"
        report["identity"]["workflow"]["job"] = "different-job"
        report["identity"]["workflow"]["run_id"] = "not-decimal"
        report["identity"]["runner"]["os"] = "Windows"
        report["identity"]["runner"]["arch"] = "ARM64"
        report["identity"]["runner"]["environment"] = "github-hosted"
        failures = VALIDATOR.validate(report, COMMIT)
        self.assertTrue(any("production capacity gate" in failure for failure in failures))
        self.assertTrue(any("production-soak" in failure for failure in failures))
        self.assertTrue(any("positive decimal" in failure for failure in failures))
        self.assertTrue(any("runner OS" in failure for failure in failures))
        self.assertTrue(any("runner architecture" in failure for failure in failures))
        self.assertTrue(any("runner environment" in failure for failure in failures))

    def test_clock_intervals_accept_non_atomic_scheduler_delay(self) -> None:
        report = valid_report()
        service = report["samples"][100]["runner_service"]
        original_monotonic = service["clock_after_monotonic_upper_usec"]
        service["clock_after_monotonic_lower_usec"] = original_monotonic
        service["clock_after_monotonic_upper_usec"] = (
            original_monotonic + 10_000_000
        )
        service["clock_after_boottime_usec"] = (
            original_monotonic + 10_000_000 + RUNNER_CLOCK_OFFSET_USEC
        )
        service["observed_monotonic_after_usec"] += 10_000_000
        service["observed_monotonic_usec"] += 10_000_000
        service["observed_boottime_after_usec"] += 10_000_000
        service["observed_boottime_usec"] += 10_000_000
        service["active_uptime_seconds"] += 10
        service["main_process_uptime_seconds"] += 10
        service["active_enter_epoch_seconds"] -= 10
        service["service_start_epoch_seconds"] -= 10

        self.assertEqual(VALIDATOR.validate(report, COMMIT), [])

    def test_validates_the_indexed_log_file_digest_size_and_record_count(self) -> None:
        report = valid_report()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            event_log = root / "capacity.events.ndjson"
            event_raw = b"{}\n" * 3_500
            event_log.write_bytes(event_raw)
            report["logs"]["index"][0].update(
                sha256=hashlib.sha256(event_raw).hexdigest(),
                bytes=len(event_raw),
                records=3_500,
            )
            metrics_log = root / "capacity.metrics.ndjson"
            metrics_raw = b"".join(
                (
                    json.dumps(
                        prometheus_sidecar_record(sample),
                        separators=(",", ":"),
                    )
                    + "\n"
                ).encode("utf-8")
                for sample in report["samples"]
            )
            metrics_log.write_bytes(metrics_raw)
            report["logs"]["index"][1].update(
                sha256=hashlib.sha256(metrics_raw).hexdigest(),
                bytes=len(metrics_raw),
                records=len(report["samples"]),
            )
            environment_log = root / "capacity.environment.ndjson"
            environment_raw = b"".join(
                (
                    json.dumps(
                        environment_sidecar_record(check),
                        separators=(",", ":"),
                    )
                    + "\n"
                ).encode("utf-8")
                for check in report["environment_checks"]
            )
            environment_log.write_bytes(environment_raw)
            report["logs"]["index"][2].update(
                sha256=hashlib.sha256(environment_raw).hexdigest(),
                bytes=len(environment_raw),
                records=len(report["environment_checks"]),
            )
            self.assertEqual(VALIDATOR.validate(report, COMMIT, root), [])
            event_log.write_bytes(event_raw + b"{}\n")
            failures = VALIDATOR.validate(report, COMMIT, root)
            self.assertTrue(any("byte count" in failure for failure in failures))
            self.assertTrue(any("digest" in failure for failure in failures))
            self.assertTrue(any("record count" in failure for failure in failures))


if __name__ == "__main__":
    unittest.main()
