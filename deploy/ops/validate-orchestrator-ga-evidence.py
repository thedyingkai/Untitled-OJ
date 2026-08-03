#!/usr/bin/env python3
"""Fail-closed validator for same-commit Orchestrator GA capacity evidence v2."""

from __future__ import annotations

import argparse
import datetime
import email.utils
import hashlib
import json
import math
import re
import sys
from pathlib import Path
from typing import Any


REPORT_SCHEMA_VERSION = 2
COMMIT_SHA_PATTERN = re.compile(r"^[0-9a-f]{40}$")
SHA256_PATTERN = re.compile(r"^[0-9a-f]{64}$")
IMMUTABLE_OCI_PATTERN = re.compile(r"^[^\s@]+@sha256:[0-9a-f]{64}$")
MINIMUMS = {
    "nodes": 100,
    "deployments": 2_000,
    "topology_resources": 10_000,
    "concurrent_operations": 50,
}
MAX_THRESHOLDS_MS = {
    "read_p95": 200.0,
    "mutation_accept_p95": 500.0,
    "event_p95": 1_000.0,
    "recovery": 60_000.0,
}
MINIMUM_SOAK_SECONDS = 86_400
MINIMUM_WARMUP_SECONDS = 600
SAMPLE_SECONDS = 30
MINIMUM_VALID_SAMPLES = 2_736
MAX_SAMPLE_GAP_SECONDS = 90
OPERATION_INTERVAL_SECONDS = 300
MINIMUM_OPERATION_ROUNDS = 288
MAX_OPERATION_GAP_SECONDS = OPERATION_INTERVAL_SECONDS + MAX_SAMPLE_GAP_SECONDS
CHECKPOINT_INTERVAL_SECONDS = 30
MAX_CHECKPOINT_GAP_SECONDS = 35
TIMELINE_TOLERANCE_SECONDS = 30
CLOCK_OFFSET_TOLERANCE_USEC = 1_000_000
MAX_LOCAL_CLOCK_SKEW_SECONDS = 30
HTTP_DATE_RESOLUTION_SECONDS = 1.0
MAX_OPERATION_EVENT_CURSOR_BYTES = 16_384
REQUIRED_RUNNER_LABELS = {"self-hosted", "linux", "x64", "orchestrator-soak"}
CAPACITY_WORKFLOW_PATH = ".github/workflows/orchestrator-capacity.yml"
PROMETHEUS_SAMPLE_METRICS = {
    "rss_bytes": "ojos_orchestrator_process_resident_memory_bytes",
    "threads": "ojos_orchestrator_process_threads",
    "active_requests": "ojos_orchestrator_http_active_requests",
    "collection_error": "ojos_orchestrator_job_metrics_collection_error",
    "expired_leases": "ojos_orchestrator_expired_job_leases",
    "oldest_leased_heartbeat_age_seconds": (
        "ojos_orchestrator_oldest_leased_job_heartbeat_age_seconds"
    ),
}
ANOMALY_SAMPLE_METRICS = {
    "expired_job_lease_transitions_total": (
        "ojos_orchestrator_expired_job_lease_transitions_total"
    ),
    "operation_over_300_seconds_transitions_total": (
        "ojos_orchestrator_operation_over_300_seconds_transitions_total"
    ),
    "operation_invalid_updated_at_transitions_total": (
        "ojos_orchestrator_operation_invalid_updated_at_transitions_total"
    ),
    "observation_errors_total": (
        "ojos_orchestrator_control_plane_anomaly_observation_errors_total"
    ),
    "process_starts_total": "ojos_orchestrator_control_plane_process_starts_total",
    "state_loaded": "ojos_orchestrator_control_plane_anomaly_state_loaded",
    "process_start_time_seconds": "ojos_orchestrator_process_start_time_seconds",
}
ANOMALY_COUNTER_FIELDS = {
    "expired_job_lease_transitions_total",
    "operation_over_300_seconds_transitions_total",
    "operation_invalid_updated_at_transitions_total",
    "observation_errors_total",
    "process_starts_total",
}
REDACTED_CONFIGURATION_FIELDS = {
    "base_origin_sha256",
    "ca_configured",
    "authentication",
    "internal_token_configured",
    "environment_evidence",
    "nodes",
    "deployments",
    "topology_resources",
    "concurrent_operations",
    "soak_seconds",
    "warmup_seconds",
    "sample_seconds",
    "operation_interval_seconds",
    "control_plane_image",
    "agent_image",
    "fixture_image",
    "image_workflow_run_id",
    "image_provenance_record_sha256",
    "environment_observer_program_sha256",
}
RUNNER_NAME_PATTERN = re.compile(r"^[A-Za-z0-9_.-]{1,128}$")
RUNNER_SERVICE_UNIT_PATTERN = re.compile(
    r"^actions\.runner\.(?:[A-Za-z0-9_.:@-]|\\x[0-9A-Fa-f]{2})+\.service$"
)
RUNNER_INVOCATION_ID_PATTERN = re.compile(r"^[0-9a-f]{32}$")
LINUX_BOOT_ID_PATTERN = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
)
DOCKER_RFC3339_NANO_PATTERN = re.compile(
    r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?Z$"
)
RUNNER_SERVICE_CONTINUITY_FIELDS = (
    "unit",
    "boot_id",
    "control_group",
    "process_control_group",
    "active_enter_timestamp",
    "active_enter_monotonic_usec",
    "exec_main_start_timestamp",
    "exec_main_start_monotonic_usec",
    "invocation_id",
    "main_pid",
    "listener_pid",
    "listener_start_ticks",
    "listener_clock_ticks_per_second",
    "listener_control_group",
    "listener_executable",
    "listener_start_boottime_usec",
    "listener_ancestor_depth",
    "observer_pid",
    "observer_start_ticks",
)


def mapping(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be an object")
    return value


def sequence(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise ValueError(f"{label} must be an array")
    return value


def number(value: Any, label: str) -> float:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        raise ValueError(f"{label} must be numeric")
    value = float(value)
    if not math.isfinite(value):
        raise ValueError(f"{label} must be finite")
    return value


def nonempty_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{label} must be a non-empty string")
    return value.strip()


def positive_integer(value: Any, label: str) -> int:
    parsed = number(value, label)
    if parsed <= 0 or not parsed.is_integer():
        raise ValueError(f"{label} must be a positive integer")
    return int(parsed)


def integer(value: Any, label: str) -> int:
    parsed = number(value, label)
    if not parsed.is_integer():
        raise ValueError(f"{label} must be an integer")
    return int(parsed)


def rfc3339_epoch(value: Any, label: str) -> float:
    timestamp = nonempty_string(value, label)
    if not timestamp.endswith("Z"):
        raise ValueError(f"{label} must be an RFC 3339 UTC timestamp")
    try:
        parsed = datetime.datetime.fromisoformat(timestamp[:-1] + "+00:00")
    except ValueError as error:
        raise ValueError(f"{label} must be an RFC 3339 UTC timestamp") from error
    return parsed.timestamp()


def docker_started_at_key(value: Any, label: str) -> tuple[int, int]:
    timestamp = nonempty_string(value, label)
    match = DOCKER_RFC3339_NANO_PATTERN.fullmatch(timestamp)
    if match is None:
        raise ValueError(f"{label} must be Docker RFC3339Nano")
    try:
        seconds = int(
            datetime.datetime.strptime(match.group(1), "%Y-%m-%dT%H:%M:%S")
            .replace(tzinfo=datetime.timezone.utc)
            .timestamp()
        )
    except ValueError as error:
        raise ValueError(f"{label} must be Docker RFC3339Nano") from error
    return seconds, int((match.group(2) or "0").ljust(9, "0"))


def http_date_epoch(value: Any, label: str) -> float:
    timestamp = nonempty_string(value, label)
    try:
        parsed = email.utils.parsedate_to_datetime(timestamp)
    except (TypeError, ValueError) as error:
        raise ValueError(f"{label} must be a valid HTTP Date") from error
    if parsed.tzinfo is None or parsed.utcoffset() != datetime.timedelta(0):
        raise ValueError(f"{label} must be UTC")
    return parsed.timestamp()


def clock_bracket(
    monotonic_lower_value: Any,
    monotonic_upper_value: Any,
    boottime_value: Any,
    label: str,
) -> dict[str, int]:
    monotonic_lower = positive_integer(
        monotonic_lower_value, f"{label}.monotonic_lower_usec"
    )
    monotonic_upper = positive_integer(
        monotonic_upper_value, f"{label}.monotonic_upper_usec"
    )
    boottime = positive_integer(boottime_value, f"{label}.boottime_usec")
    if monotonic_upper < monotonic_lower:
        raise ValueError(f"{label} monotonic bracket moved backwards")
    return {
        "monotonic_lower_usec": monotonic_lower,
        "monotonic_upper_usec": monotonic_upper,
        "boottime_usec": boottime,
        "offset_lower_usec": boottime - monotonic_upper,
        "offset_upper_usec": boottime - monotonic_lower,
    }


def intersect_clock_offsets(
    first: dict[str, int], second: dict[str, int], label: str
) -> tuple[int, int]:
    lower = max(first["offset_lower_usec"], second["offset_lower_usec"])
    upper = min(first["offset_upper_usec"], second["offset_upper_usec"])
    if lower > upper:
        if lower - upper > CLOCK_OFFSET_TOLERANCE_USEC:
            raise ValueError(f"{label} proves a host suspend")
        midpoint = (lower + upper) // 2
        return midpoint, midpoint
    return lower, upper


def clock_offsets_are_continuous(
    reference: dict[str, Any], current: dict[str, Any]
) -> bool:
    return not (
        current["clock_offset_lower_usec"]
        > reference["clock_offset_upper_usec"] + CLOCK_OFFSET_TOLERANCE_USEC
        or current["clock_offset_upper_usec"]
        < reference["clock_offset_lower_usec"] - CLOCK_OFFSET_TOLERANCE_USEC
    )


def runner_service_observation(
    value: Any, label: str, runner_name: str
) -> dict[str, Any]:
    service = mapping(value, label)
    if service.get("schema_version") != 1:
        raise ValueError(f"{label}.schema_version must be 1")
    unit = nonempty_string(service.get("unit"), f"{label}.unit")
    if not RUNNER_SERVICE_UNIT_PATTERN.fullmatch(unit):
        raise ValueError(f"{label}.unit is not an actions.runner systemd service")
    if not unit.endswith(f".{runner_name}.service"):
        raise ValueError(f"{label}.unit does not match identity.runner.name")
    boot_id = nonempty_string(service.get("boot_id"), f"{label}.boot_id")
    if not LINUX_BOOT_ID_PATTERN.fullmatch(boot_id):
        raise ValueError(f"{label}.boot_id is invalid")
    control_group = nonempty_string(
        service.get("control_group"), f"{label}.control_group"
    )
    process_control_group = nonempty_string(
        service.get("process_control_group"),
        f"{label}.process_control_group",
    )
    if (
        not control_group.startswith("/")
        or control_group == "/"
        or "//" in control_group
        or "/../" in f"{control_group}/"
    ):
        raise ValueError(f"{label}.control_group is invalid")
    if control_group.rstrip("/").rsplit("/", 1)[-1] != unit:
        raise ValueError(f"{label}.control_group does not identify its unit")
    if process_control_group != control_group and not process_control_group.startswith(
        control_group + "/"
    ):
        raise ValueError(f"{label} process cgroup is outside systemd ControlGroup")
    for field, expected in (
        ("load_state", "loaded"),
        ("active_state", "active"),
        ("sub_state", "running"),
    ):
        if service.get(field) != expected:
            raise ValueError(f"{label}.{field} must be {expected}")
    nonempty_string(
        service.get("active_enter_timestamp"),
        f"{label}.active_enter_timestamp",
    )
    nonempty_string(
        service.get("exec_main_start_timestamp"),
        f"{label}.exec_main_start_timestamp",
    )
    invocation_id = nonempty_string(
        service.get("invocation_id"), f"{label}.invocation_id"
    )
    if not RUNNER_INVOCATION_ID_PATTERN.fullmatch(invocation_id):
        raise ValueError(f"{label}.invocation_id is invalid")
    active_enter = positive_integer(
        service.get("active_enter_monotonic_usec"),
        f"{label}.active_enter_monotonic_usec",
    )
    exec_start = positive_integer(
        service.get("exec_main_start_monotonic_usec"),
        f"{label}.exec_main_start_monotonic_usec",
    )
    positive_integer(service.get("main_pid"), f"{label}.main_pid")
    positive_integer(service.get("listener_pid"), f"{label}.listener_pid")
    listener_start_ticks = positive_integer(
        service.get("listener_start_ticks"), f"{label}.listener_start_ticks"
    )
    listener_clock_ticks = positive_integer(
        service.get("listener_clock_ticks_per_second"),
        f"{label}.listener_clock_ticks_per_second",
    )
    listener_control_group = nonempty_string(
        service.get("listener_control_group"),
        f"{label}.listener_control_group",
    )
    if listener_control_group != control_group and not listener_control_group.startswith(
        control_group + "/"
    ):
        raise ValueError(f"{label} Runner.Listener cgroup is outside ControlGroup")
    listener_executable = nonempty_string(
        service.get("listener_executable"), f"{label}.listener_executable"
    )
    if re.split(r"[/\\]", listener_executable)[-1] != "Runner.Listener":
        raise ValueError(f"{label}.listener_executable is not Runner.Listener")
    listener_start_boottime = positive_integer(
        service.get("listener_start_boottime_usec"),
        f"{label}.listener_start_boottime_usec",
    )
    if listener_start_boottime != (
        listener_start_ticks * 1_000_000 // listener_clock_ticks
    ):
        raise ValueError(f"{label}.listener_start_boottime_usec is inconsistent")
    positive_integer(
        service.get("listener_ancestor_depth"),
        f"{label}.listener_ancestor_depth",
    )
    positive_integer(service.get("observer_pid"), f"{label}.observer_pid")
    positive_integer(
        service.get("observer_start_ticks"), f"{label}.observer_start_ticks"
    )

    clock_before = clock_bracket(
        service.get("clock_before_monotonic_lower_usec"),
        service.get("clock_before_monotonic_upper_usec"),
        service.get("clock_before_boottime_usec"),
        f"{label}.clock_before",
    )
    clock_after = clock_bracket(
        service.get("clock_after_monotonic_lower_usec"),
        service.get("clock_after_monotonic_upper_usec"),
        service.get("clock_after_boottime_usec"),
        f"{label}.clock_after",
    )
    if (
        clock_after["monotonic_lower_usec"]
        < clock_before["monotonic_upper_usec"]
        or clock_after["boottime_usec"] < clock_before["boottime_usec"]
    ):
        raise ValueError(f"{label} observation clocks moved backwards")
    offset_lower, offset_upper = intersect_clock_offsets(
        clock_before, clock_after, f"{label} clock brackets"
    )
    if integer(
        service.get("clock_offset_lower_usec"),
        f"{label}.clock_offset_lower_usec",
    ) != offset_lower or integer(
        service.get("clock_offset_upper_usec"),
        f"{label}.clock_offset_upper_usec",
    ) != offset_upper:
        raise ValueError(f"{label} clock offset interval is inconsistent")

    observed_monotonic_before = clock_before["monotonic_lower_usec"]
    observed_monotonic_after = clock_after["monotonic_upper_usec"]
    observed_boottime_before = clock_before["boottime_usec"]
    observed_boottime_after = clock_after["boottime_usec"]
    if positive_integer(
        service.get("observed_monotonic_before_usec"),
        f"{label}.observed_monotonic_before_usec",
    ) != observed_monotonic_before:
        raise ValueError(f"{label}.observed_monotonic_before_usec is inconsistent")
    if positive_integer(
        service.get("observed_monotonic_after_usec"),
        f"{label}.observed_monotonic_after_usec",
    ) != observed_monotonic_after:
        raise ValueError(f"{label}.observed_monotonic_after_usec is inconsistent")
    if positive_integer(
        service.get("observed_boottime_before_usec"),
        f"{label}.observed_boottime_before_usec",
    ) != observed_boottime_before:
        raise ValueError(f"{label}.observed_boottime_before_usec is inconsistent")
    if positive_integer(
        service.get("observed_boottime_after_usec"),
        f"{label}.observed_boottime_after_usec",
    ) != observed_boottime_after:
        raise ValueError(f"{label}.observed_boottime_after_usec is inconsistent")
    if positive_integer(
        service.get("observed_monotonic_usec"),
        f"{label}.observed_monotonic_usec",
    ) != observed_monotonic_after:
        raise ValueError(f"{label}.observed_monotonic_usec is inconsistent")
    if positive_integer(
        service.get("observed_boottime_usec"),
        f"{label}.observed_boottime_usec",
    ) != observed_boottime_after:
        raise ValueError(f"{label}.observed_boottime_usec is inconsistent")
    observed_at = number(
        service.get("observed_at_epoch_seconds"),
        f"{label}.observed_at_epoch_seconds",
    )
    active_uptime = number(
        service.get("active_uptime_seconds"),
        f"{label}.active_uptime_seconds",
    )
    process_uptime = number(
        service.get("main_process_uptime_seconds"),
        f"{label}.main_process_uptime_seconds",
    )
    active_enter_epoch = number(
        service.get("active_enter_epoch_seconds"),
        f"{label}.active_enter_epoch_seconds",
    )
    service_start = max(active_enter, exec_start)
    if service_start > observed_monotonic_before:
        raise ValueError(f"{label} contains a future service start timestamp")
    if positive_integer(
        service.get("service_start_monotonic_usec"),
        f"{label}.service_start_monotonic_usec",
    ) != service_start:
        raise ValueError(f"{label}.service_start_monotonic_usec is inconsistent")
    expected_active_uptime = (
        observed_monotonic_after - active_enter
    ) / 1_000_000
    expected_process_uptime = (
        observed_monotonic_after - exec_start
    ) / 1_000_000
    expected_age_lower_bound = (
        observed_monotonic_before - service_start
    ) / 1_000_000
    expected_listener_age_lower_bound = (
        observed_boottime_before - listener_start_boottime
    ) / 1_000_000
    if expected_listener_age_lower_bound < 0:
        raise ValueError(f"{label} contains a future Runner.Listener start timestamp")
    if abs(active_uptime - expected_active_uptime) > 0.001:
        raise ValueError(f"{label}.active_uptime_seconds is inconsistent")
    if abs(process_uptime - expected_process_uptime) > 0.001:
        raise ValueError(f"{label}.main_process_uptime_seconds is inconsistent")
    if abs(
        number(
            service.get("active_age_lower_bound_seconds"),
            f"{label}.active_age_lower_bound_seconds",
        )
        - expected_age_lower_bound
    ) > 0.001:
        raise ValueError(f"{label}.active_age_lower_bound_seconds is inconsistent")
    if abs(
        number(
            service.get("listener_age_lower_bound_seconds"),
            f"{label}.listener_age_lower_bound_seconds",
        )
        - expected_listener_age_lower_bound
    ) > 0.001:
        raise ValueError(f"{label}.listener_age_lower_bound_seconds is inconsistent")
    if abs(active_enter_epoch - (observed_at - active_uptime)) > 1.0:
        raise ValueError(f"{label}.active_enter_epoch_seconds is inconsistent")
    if abs(
        number(
            service.get("service_start_epoch_seconds"),
            f"{label}.service_start_epoch_seconds",
        )
        - (
            observed_at
            - (observed_monotonic_after - service_start) / 1_000_000
        )
    ) > 1.0:
        raise ValueError(f"{label}.service_start_epoch_seconds is inconsistent")
    return service


def validate_runner_service_evidence(
    report: dict[str, Any], failures: list[str]
) -> None:
    try:
        identity = mapping(report.get("identity"), "identity")
        workflow = mapping(identity.get("workflow"), "identity.workflow")
        if workflow.get("api_verified") is not True:
            failures.append("workflow dispatch metadata was not verified by the Actions API")
        if workflow.get("event") != "workflow_dispatch":
            failures.append("capacity evidence was not created by workflow_dispatch")
        if workflow.get("run_attempt") != "1":
            failures.append("production capacity evidence cannot come from a workflow rerun")
        if workflow.get("head_branch") != "main" or workflow.get("ref") != "refs/heads/main":
            failures.append("capacity evidence was not dispatched from main")
        created_at = rfc3339_epoch(
            workflow.get("created_at"), "identity.workflow.created_at"
        )
        reported_created_at = number(
            workflow.get("created_at_epoch_seconds"),
            "identity.workflow.created_at_epoch_seconds",
        )
        if abs(created_at - reported_created_at) > 0.001:
            failures.append("workflow created_at representations are inconsistent")

        api_date = http_date_epoch(
            workflow.get("api_date"), "identity.workflow.api_date"
        )
        reported_api_date = number(
            workflow.get("api_date_epoch_seconds"),
            "identity.workflow.api_date_epoch_seconds",
        )
        if abs(api_date - reported_api_date) > 0.001:
            failures.append("GitHub API Date representations are inconsistent")
        if api_date + HTTP_DATE_RESOLUTION_SECONDS < created_at:
            failures.append("GitHub API Date predates workflow creation")
        local_received = number(
            workflow.get("api_local_received_at_epoch_seconds"),
            "identity.workflow.api_local_received_at_epoch_seconds",
        )
        reported_skew = number(
            workflow.get("api_local_clock_skew_seconds"),
            "identity.workflow.api_local_clock_skew_seconds",
        )
        if abs(reported_skew - (local_received - api_date)) > 0.001:
            failures.append("GitHub API Date local clock skew is inconsistent")
        if abs(reported_skew) > MAX_LOCAL_CLOCK_SKEW_SECONDS:
            failures.append("local wall clock differs from GitHub API Date by over 30 seconds")

        api_request_clock = clock_bracket(
            workflow.get("api_request_monotonic_lower_usec"),
            workflow.get("api_request_monotonic_upper_usec"),
            workflow.get("api_request_boottime_usec"),
            "identity.workflow.api_request",
        )
        api_response_clock = clock_bracket(
            workflow.get("api_response_monotonic_lower_usec"),
            workflow.get("api_response_monotonic_upper_usec"),
            workflow.get("api_response_boottime_usec"),
            "identity.workflow.api_response",
        )
        if (
            api_response_clock["monotonic_lower_usec"]
            < api_request_clock["monotonic_upper_usec"]
            or api_response_clock["boottime_usec"]
            < api_request_clock["boottime_usec"]
        ):
            raise ValueError("GitHub API clock evidence moved backwards")
        api_offset_lower, api_offset_upper = intersect_clock_offsets(
            api_request_clock,
            api_response_clock,
            "GitHub API request clock brackets",
        )
        if integer(
            workflow.get("api_clock_offset_lower_usec"),
            "identity.workflow.api_clock_offset_lower_usec",
        ) != api_offset_lower or integer(
            workflow.get("api_clock_offset_upper_usec"),
            "identity.workflow.api_clock_offset_upper_usec",
        ) != api_offset_upper:
            failures.append("GitHub API clock offset interval is inconsistent")

        runner = mapping(identity.get("runner"), "identity.runner")
        runner_name = nonempty_string(runner.get("name"), "identity.runner.name")
        if not RUNNER_NAME_PATTERN.fullmatch(runner_name):
            raise ValueError("identity.runner.name cannot identify a runner service")
        expected_unit = nonempty_string(
            runner.get("expected_service_unit"),
            "identity.runner.expected_service_unit",
        )
        if (
            not RUNNER_SERVICE_UNIT_PATTERN.fullmatch(expected_unit)
            or not expected_unit.endswith(f".{runner_name}.service")
        ):
            raise ValueError(
                "identity.runner.expected_service_unit is invalid or mismatches runner name"
            )
        baseline = runner_service_observation(
            runner.get("service"), "identity.runner.service", runner_name
        )
        if baseline.get("unit") != expected_unit:
            failures.append(
                "runner service unit does not equal identity.runner.expected_service_unit"
            )
        if api_request_clock["monotonic_lower_usec"] < baseline[
            "clock_after_monotonic_upper_usec"
        ] or api_request_clock["boottime_usec"] < baseline[
            "clock_after_boottime_usec"
        ]:
            failures.append("GitHub API clock sample predates the runner baseline")
        api_clock_interval = {
            "clock_offset_lower_usec": api_offset_lower,
            "clock_offset_upper_usec": api_offset_upper,
        }
        if not clock_offsets_are_continuous(baseline, api_clock_interval):
            failures.append("runner host suspended between baseline and GitHub API verification")
        if baseline["active_age_lower_bound_seconds"] < 3_600:
            failures.append("runner service monotonic age lower bound is below one hour")
        if baseline["listener_age_lower_bound_seconds"] < 3_600:
            failures.append("Runner.Listener age lower bound is below one hour")

        service_age_at_api_request = (
            api_request_clock["monotonic_lower_usec"]
            - baseline["service_start_monotonic_usec"]
        ) / 1_000_000
        listener_age_at_api_request = (
            api_request_clock["boottime_usec"]
            - baseline["listener_start_boottime_usec"]
        ) / 1_000_000
        if service_age_at_api_request < 0 or listener_age_at_api_request < 0:
            failures.append("runner start evidence postdates the GitHub API request")
        if abs(
            number(
                baseline.get("active_at_api_request_lower_seconds"),
                "identity.runner.service.active_at_api_request_lower_seconds",
            )
            - service_age_at_api_request
        ) > 0.001:
            failures.append("runner service API-request age lower bound is inconsistent")
        if abs(
            number(
                baseline.get("listener_age_at_api_request_lower_seconds"),
                "identity.runner.service.listener_age_at_api_request_lower_seconds",
            )
            - listener_age_at_api_request
        ) > 0.001:
            failures.append("Runner.Listener API-request age lower bound is inconsistent")
        api_date_upper_bound = api_date + HTTP_DATE_RESOLUTION_SECONDS
        active_before_dispatch = (
            service_age_at_api_request + created_at - api_date_upper_bound
        )
        listener_active_before_dispatch = (
            listener_age_at_api_request + created_at - api_date_upper_bound
        )
        reported_service_prior = number(
            baseline.get("active_before_dispatch_seconds"),
            "identity.runner.service.active_before_dispatch_seconds",
        )
        reported_listener_prior = number(
            baseline.get("listener_active_before_dispatch_seconds"),
            "identity.runner.service.listener_active_before_dispatch_seconds",
        )
        if abs(active_before_dispatch - reported_service_prior) > 0.001:
            failures.append("runner service pre-dispatch duration is inconsistent")
        if abs(listener_active_before_dispatch - reported_listener_prior) > 0.001:
            failures.append("Runner.Listener pre-dispatch duration is inconsistent")
        if active_before_dispatch < 3_600:
            failures.append(
                "runner service was active for less than one hour before workflow dispatch"
            )
        if listener_active_before_dispatch < 3_600:
            failures.append(
                "Runner.Listener was active for less than one hour before workflow dispatch"
            )

        samples = sequence(report.get("samples"), "samples")
        observations: list[tuple[str, dict[str, Any]]] = [
            ("identity.runner.service", baseline)
        ]
        for index, sample_value in enumerate(samples):
            sample = mapping(sample_value, f"samples[{index}]")
            service = runner_service_observation(
                sample.get("runner_service"),
                f"samples[{index}].runner_service",
                runner_name,
            )
            sampled_at = number(
                sample.get("sampled_at_epoch_seconds"),
                f"samples[{index}].sampled_at_epoch_seconds",
            )
            if abs(service["observed_at_epoch_seconds"] - sampled_at) > 5:
                failures.append(
                    f"samples[{index}] runner service observation is not contemporaneous"
                )
                return
            observations.append((f"samples[{index}].runner_service", service))
        evidence = mapping(report.get("evidence"), "evidence")
        if evidence.get("sampling_clock") != "CLOCK_BOOTTIME":
            failures.append("production sampling did not use CLOCK_BOOTTIME")
        final = runner_service_observation(
            evidence.get("runner_service_final"),
            "evidence.runner_service_final",
            runner_name,
        )
        observations.append(("evidence.runner_service_final", final))
        expected_count = len(samples) + 2
        if positive_integer(
            evidence.get("runner_service_observations"),
            "evidence.runner_service_observations",
        ) != expected_count:
            failures.append("runner service observation count does not cover the complete gate")

        prior_monotonic_after = -1
        prior_boottime_after = -1
        prior_uptime = -1.0
        prior_listener_age = -1.0
        for label, observation in observations:
            for field in RUNNER_SERVICE_CONTINUITY_FIELDS:
                if observation.get(field) != baseline.get(field):
                    failures.append(f"{label}.{field} breaks runner service continuity")
                    return
            monotonic_before = observation["observed_monotonic_before_usec"]
            boottime_before = observation["observed_boottime_before_usec"]
            if (
                monotonic_before <= prior_monotonic_after
                or boottime_before <= prior_boottime_after
            ):
                failures.append("runner service observations are not strictly increasing")
                return
            if prior_monotonic_after >= 0:
                if not clock_offsets_are_continuous(baseline, observation):
                    failures.append(
                        "runner host clock offset discontinuity proves a suspend between service observations"
                    )
                    return
            if observation["active_uptime_seconds"] < prior_uptime:
                failures.append("runner service active uptime moved backwards")
                return
            if observation["listener_age_lower_bound_seconds"] < prior_listener_age:
                failures.append("Runner.Listener age moved backwards")
                return
            prior_monotonic_after = observation["observed_monotonic_after_usec"]
            prior_boottime_after = observation["observed_boottime_after_usec"]
            prior_uptime = observation["active_uptime_seconds"]
            prior_listener_age = observation["listener_age_lower_bound_seconds"]
    except ValueError as error:
        failures.append(str(error))


def validate_identity(
    report: dict[str, Any], expected_commit: str, failures: list[str]
) -> None:
    try:
        identity = mapping(report.get("identity"), "identity")
        if identity.get("source_commit") != expected_commit:
            failures.append("identity.source_commit does not match the candidate commit")
        for name in ("oci_revision", "provenance_commit"):
            if identity.get(name) != expected_commit:
                failures.append(f"identity.{name} does not match the candidate commit")

        build = mapping(identity.get("server_build"), "identity.server_build")
        if build.get("commit_sha") != expected_commit:
            failures.append("server readiness build commit does not match the candidate commit")
        if build.get("profile") != "production":
            failures.append("server readiness build profile is not production")
        if build.get("version") != "1.0.0":
            failures.append("server readiness build version is not 1.0.0")
        target = nonempty_string(build.get("target"), "identity.server_build.target")
        if "x86_64" not in target.lower() or "linux" not in target.lower():
            failures.append("server readiness build target is not Linux x86_64")

        workflow = mapping(identity.get("workflow"), "identity.workflow")
        if workflow.get("sha") != expected_commit:
            failures.append("workflow SHA does not match the candidate commit")
        for name in ("repository", "workflow", "run_id", "run_attempt", "job", "ref"):
            nonempty_string(workflow.get(name), f"identity.workflow.{name}")
        repository = str(workflow.get("repository"))
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
            failures.append("workflow repository is not a canonical owner/repository")
        if workflow.get("workflow") != "Orchestrator capacity and soak gate":
            failures.append("workflow identity is not the production capacity gate")
        if workflow.get("path") != CAPACITY_WORKFLOW_PATH:
            failures.append(
                f"workflow path is not the fixed production capacity workflow {CAPACITY_WORKFLOW_PATH}"
            )
        positive_integer(workflow.get("workflow_id"), "identity.workflow.workflow_id")
        if workflow.get("job") != "production-soak":
            failures.append("workflow job is not production-soak")
        run_id = str(workflow.get("run_id"))
        if not run_id.isdigit() or int(run_id) <= 0:
            failures.append("workflow run_id is not a positive decimal integer")
        if workflow.get("run_attempt") != "1":
            failures.append("workflow run_attempt is not the first attempt")
        if workflow.get("ref") != "refs/heads/main":
            failures.append("workflow ref is not refs/heads/main")

        runner = mapping(identity.get("runner"), "identity.runner")
        for name in ("name", "os", "arch", "environment"):
            nonempty_string(runner.get(name), f"identity.runner.{name}")
        if runner.get("os") != "Linux":
            failures.append("runner OS is not Linux")
        if str(runner.get("arch")).lower() not in {"x64", "x86_64"}:
            failures.append("runner architecture is not x64")
        if str(runner.get("environment")).lower() != "self-hosted":
            failures.append("runner environment is not self-hosted")
        labels = runner.get("labels")
        if not isinstance(labels, list) or not REQUIRED_RUNNER_LABELS.issubset(
            {str(label).lower() for label in labels}
        ):
            failures.append("runner identity does not contain the required soak labels")
        image_provenance = mapping(
            identity.get("image_provenance"), "identity.image_provenance"
        )
        if set(image_provenance) != {
            "control_plane_image",
            "agent_image",
            "fixture_image",
            "source_workflow_run_id",
            "record_sha256",
            "source_workflow",
            "source_workflow_run_attempt",
        }:
            failures.append("image provenance identity has unexpected fields")
        for name in ("control_plane_image", "agent_image", "fixture_image"):
            reference = nonempty_string(
                image_provenance.get(name), f"identity.image_provenance.{name}"
            )
            if not IMMUTABLE_OCI_PATTERN.fullmatch(reference):
                failures.append(f"identity.image_provenance.{name} is not immutable")
        require_sha256(
            image_provenance.get("record_sha256"),
            "identity.image_provenance.record_sha256",
        )
        image_run_id = nonempty_string(
            image_provenance.get("source_workflow_run_id"),
            "identity.image_provenance.source_workflow_run_id",
        )
        if not image_run_id.isdigit() or int(image_run_id) <= 0:
            failures.append("candidate image workflow run ID is not positive")
        if (
            image_provenance.get("source_workflow")
            != ".github/workflows/orchestrator-candidate-images.yml"
            or image_provenance.get("source_workflow_run_attempt") != 1
        ):
            failures.append("candidate image provenance is not from attempt 1 of the fixed workflow")
    except ValueError as error:
        failures.append(str(error))


def validate_configuration(report: dict[str, Any], failures: list[str]) -> None:
    try:
        configuration = mapping(report.get("configuration"), "configuration")
        redacted = mapping(configuration.get("redacted"), "configuration.redacted")
        if set(redacted) != REDACTED_CONFIGURATION_FIELDS:
            failures.append("redacted configuration fields do not match the production schema")
        fingerprint = nonempty_string(
            configuration.get("fingerprint_sha256"),
            "configuration.fingerprint_sha256",
        )
        canonical = json.dumps(redacted, separators=(",", ":"), sort_keys=True).encode(
            "utf-8"
        )
        if fingerprint != hashlib.sha256(canonical).hexdigest():
            failures.append("redacted configuration fingerprint is invalid")
        forbidden = {"token", "secret", "password", "authorization"}
        for key in redacted:
            lowered = str(key).lower()
            if any(word in lowered for word in forbidden) and not lowered.endswith(
                "_configured"
            ):
                failures.append(f"redacted configuration exposes forbidden field {key!r}")
        if redacted.get("authentication") != "refreshing_oidc_helper":
            failures.append("production evidence did not use the refreshing OIDC helper")
        if redacted.get("internal_token_configured") is not False:
            failures.append("production evidence used a static internal token")
        if redacted.get("environment_evidence") != "protected_argv_helper":
            failures.append(
                "production evidence did not use the protected environment argv helper"
            )
        if redacted.get("ca_configured") is not True:
            failures.append("production evidence did not configure a trusted TLS CA")
        require_sha256(
            redacted.get("base_origin_sha256"),
            "configuration.redacted.base_origin_sha256",
        )
        require_sha256(
            redacted.get("environment_observer_program_sha256"),
            "configuration.redacted.environment_observer_program_sha256",
        )

        expected = mapping(report.get("expected"), "expected")
        evidence = mapping(report.get("evidence"), "evidence")
        identity = mapping(report.get("identity"), "identity")
        image_provenance = mapping(
            identity.get("image_provenance"), "identity.image_provenance"
        )
        environment_identity = mapping(
            evidence.get("environment_identity"), "evidence.environment_identity"
        )
        exact_values = {
            "nodes": expected.get("nodes"),
            "deployments": expected.get("deployments"),
            "topology_resources": expected.get("topology_resources"),
            "concurrent_operations": expected.get("concurrent_operations"),
            "soak_seconds": evidence.get("soak_seconds_requested"),
            "warmup_seconds": evidence.get("warmup_seconds"),
            "sample_seconds": evidence.get("sample_seconds"),
            "operation_interval_seconds": evidence.get(
                "operation_interval_seconds"
            ),
            "control_plane_image": image_provenance.get("control_plane_image"),
            "agent_image": image_provenance.get("agent_image"),
            "fixture_image": image_provenance.get("fixture_image"),
            "image_workflow_run_id": image_provenance.get(
                "source_workflow_run_id"
            ),
            "image_provenance_record_sha256": image_provenance.get(
                "record_sha256"
            ),
            "base_origin_sha256": environment_identity.get(
                "control_plane_origin_sha256"
            ),
        }
        for name, expected_value in exact_values.items():
            if redacted.get(name) != expected_value:
                failures.append(
                    f"configuration.redacted.{name} does not match report evidence"
                )
    except ValueError as error:
        failures.append(str(error))


def validate_checkpoint_history(
    report: dict[str, Any], failures: list[str]
) -> None:
    try:
        evidence = mapping(report.get("evidence"), "evidence")
        if number(
            evidence.get("checkpoint_interval_seconds"),
            "checkpoint interval",
        ) != CHECKPOINT_INTERVAL_SECONDS:
            failures.append("production checkpoint interval is not 30 seconds")
        if evidence.get("checkpoint_clock") != "CLOCK_BOOTTIME":
            failures.append("production checkpoints did not use CLOCK_BOOTTIME")
        history = [
            mapping(value, f"evidence.checkpoint_history[{index}]")
            for index, value in enumerate(
                sequence(evidence.get("checkpoint_history"), "checkpoint history")
            )
        ]
        if not history:
            failures.append("production checkpoint history is empty")
            return
        epochs: list[float] = []
        clocks: list[float] = []
        for index, checkpoint in enumerate(history):
            if set(checkpoint) != {"sequence", "epoch_seconds", "clock_seconds"}:
                failures.append(f"checkpoint {index} has unexpected fields")
                return
            if integer(checkpoint.get("sequence"), "checkpoint sequence") != index + 1:
                failures.append("checkpoint sequence is not contiguous")
                return
            epochs.append(number(checkpoint.get("epoch_seconds"), "checkpoint epoch"))
            clocks.append(number(checkpoint.get("clock_seconds"), "checkpoint clock"))
        if any(right <= left for left, right in zip(epochs, epochs[1:])):
            failures.append("checkpoint epoch timestamps are not strictly increasing")
        if any(right <= left for left, right in zip(clocks, clocks[1:])):
            failures.append("checkpoint BOOTTIME timestamps are not strictly increasing")
        gaps = [right - left for left, right in zip(clocks, clocks[1:])]
        if gaps and max(gaps) > MAX_CHECKPOINT_GAP_SECONDS:
            failures.append(
                f"checkpoint history contains a gap over {MAX_CHECKPOINT_GAP_SECONDS} seconds"
            )
        started_at = rfc3339_epoch(report.get("started_at"), "report.started_at")
        completed_at = rfc3339_epoch(
            evidence.get("completed_at"), "evidence.completed_at"
        )
        if epochs[0] < started_at - 1 or epochs[0] > started_at + MAX_CHECKPOINT_GAP_SECONDS:
            failures.append("checkpoint history does not begin with the gate")
        if any(epoch < started_at - 1 or epoch > completed_at for epoch in epochs):
            failures.append("checkpoint history falls outside the report lifetime")
        if abs(epochs[-1] - completed_at) > 0.001:
            failures.append("final checkpoint does not define gate completion")
        if integer(evidence.get("checkpoint_count"), "checkpoint count") != len(
            history
        ):
            failures.append("checkpoint count does not match full history")
        if len(history) < math.floor((completed_at - started_at) / 35) + 1:
            failures.append("checkpoint history is too sparse for the full gate duration")
        checkpointed_at = rfc3339_epoch(
            evidence.get("checkpointed_at"), "evidence.checkpointed_at"
        )
        if abs(checkpointed_at - epochs[-1]) > 0.001:
            failures.append("checkpointed_at does not identify the final checkpoint")
    except ValueError as error:
        failures.append(str(error))


def sample_measurements(
    sample: dict[str, Any], label: str, permanent_running_seconds: float
) -> dict[str, float | bool]:
    process = mapping(sample.get("process"), f"{label}.process")
    storage = mapping(sample.get("storage"), f"{label}.storage")
    jobs = mapping(sample.get("jobs"), f"{label}.jobs")
    anomalies = mapping(sample.get("anomalies"), f"{label}.anomalies")
    if set(anomalies) != set(ANOMALY_SAMPLE_METRICS):
        raise ValueError(f"{label}.anomalies has unexpected fields")
    values: dict[str, float | bool] = {
        "rss_bytes": number(process.get("rss_bytes"), f"{label}.process.rss_bytes"),
        "threads": number(process.get("threads"), f"{label}.process.threads"),
        "active_requests": number(
            process.get("active_requests"), f"{label}.process.active_requests"
        ),
        "pool_connections": number(
            storage.get("pool_connections"), f"{label}.storage.pool_connections"
        ),
        "pool_idle_connections": number(
            storage.get("pool_idle_connections"),
            f"{label}.storage.pool_idle_connections",
        ),
        "collection_error": number(
            jobs.get("collection_error"), f"{label}.jobs.collection_error"
        ),
        "expired_leases": number(
            jobs.get("expired_leases"), f"{label}.jobs.expired_leases"
        ),
        "oldest_leased_heartbeat_age_seconds": number(
            jobs.get("oldest_leased_heartbeat_age_seconds"),
            f"{label}.jobs.oldest_leased_heartbeat_age_seconds",
        ),
    }
    for name in ANOMALY_SAMPLE_METRICS:
        values[name] = number(anomalies.get(name), f"{label}.anomalies.{name}")
    for name in ANOMALY_COUNTER_FIELDS:
        if values[name] < 0 or not float(values[name]).is_integer():
            raise ValueError(f"{label}.anomalies.{name} is not a non-negative counter")
    if values["state_loaded"] != 1:
        raise ValueError(f"{label}.anomalies.state_loaded is not 1")
    if values["process_start_time_seconds"] <= 0:
        raise ValueError(f"{label}.anomalies.process_start_time_seconds is invalid")
    values["valid"] = bool(
        values["rss_bytes"] > 0
        and values["threads"] > 0
        and values["collection_error"] == 0
        and values["expired_leases"] == 0
        and values["oldest_leased_heartbeat_age_seconds"]
        <= permanent_running_seconds
        and values["pool_connections"] > 0
        and 0
        <= values["pool_idle_connections"]
        <= values["pool_connections"]
    )
    return values


def process_extrema(
    measurements: list[dict[str, float | bool]], label: str
) -> dict[str, float]:
    if not measurements:
        raise ValueError(f"{label} has no samples")
    first = measurements[0]
    return {
        "baseline_rss_bytes": float(first["rss_bytes"]),
        "max_rss_bytes": max(float(value["rss_bytes"]) for value in measurements),
        "baseline_threads": float(first["threads"]),
        "max_threads": max(float(value["threads"]) for value in measurements),
        "baseline_pool_connections": float(first["pool_connections"]),
        "max_pool_connections": max(
            float(value["pool_connections"]) for value in measurements
        ),
        "baseline_active_requests": float(first["active_requests"]),
        "max_active_requests": max(
            float(value["active_requests"]) for value in measurements
        ),
        "max_pool_idle_connections": max(
            float(value["pool_idle_connections"]) for value in measurements
        ),
    }


def require_exact_process_extrema(
    report: dict[str, Any], extrema: dict[str, float], label: str
) -> None:
    process = mapping(report.get("process"), "process")
    for name, expected_value in extrema.items():
        if number(process.get(name), f"process.{name}") != expected_value:
            raise ValueError(f"process.{name} does not match {label}")


def validate_samples(
    report: dict[str, Any], failures: list[str]
) -> dict[str, Any] | None:
    try:
        samples = sequence(report.get("samples"), "samples")
        evidence = mapping(report.get("evidence"), "evidence")
        permanent_running_seconds = number(
            evidence.get("permanent_running_seconds"), "permanent state limit"
        )
        warmup: list[dict[str, Any]] = []
        boundaries: list[dict[str, Any]] = []
        soak: list[dict[str, Any]] = []
        measurements: list[dict[str, float | bool]] = []
        previous_sampled_at = -1.0
        previous_sample_clock = -1.0
        phase_elapsed: dict[str, float] = {
            "warmup": -1.0,
            "soak_boundary": -1.0,
            "soak": -1.0,
        }
        phase_rank = {"warmup": 0, "soak_boundary": 1, "soak": 2}
        highest_phase_rank = 0
        for index, raw in enumerate(samples):
            label = f"samples[{index}]"
            sample = mapping(raw, label)
            expected_sequence = index + 1
            if integer(sample.get("sequence"), f"{label}.sequence") != expected_sequence:
                raise ValueError("sample sequence is not contiguous")
            phase = sample.get("phase")
            if phase not in phase_rank:
                raise ValueError(f"{label}.phase is invalid")
            if phase_rank[phase] < highest_phase_rank:
                raise ValueError("sample phases are not ordered warmup/boundary/soak")
            highest_phase_rank = phase_rank[phase]
            sampled_at = number(
                sample.get("sampled_at_epoch_seconds"),
                f"{label}.sampled_at_epoch_seconds",
            )
            if sampled_at <= previous_sampled_at:
                raise ValueError("sample timestamps are not strictly increasing")
            previous_sampled_at = sampled_at
            sample_clock = number(
                sample.get("sample_clock_seconds"),
                f"{label}.sample_clock_seconds",
            )
            if sample_clock <= previous_sample_clock:
                raise ValueError("sample BOOTTIME timestamps are not strictly increasing")
            previous_sample_clock = sample_clock
            elapsed = number(
                sample.get("phase_elapsed_seconds"),
                f"{label}.phase_elapsed_seconds",
            )
            if elapsed < 0 or elapsed <= phase_elapsed[phase]:
                raise ValueError(f"{phase} sample phase timestamps are not strictly increasing")
            phase_elapsed[phase] = elapsed
            metrics = mapping(sample.get("metrics"), f"{label}.metrics")
            if metrics.get("snapshot_kind") != "prometheus_snapshots_ndjson":
                raise ValueError(f"{label} has no indexed Prometheus snapshot")
            if integer(
                metrics.get("snapshot_record"), f"{label}.metrics.snapshot_record"
            ) != expected_sequence:
                raise ValueError("Prometheus snapshot_record does not match sample sequence")
            values = sample_measurements(sample, label, permanent_running_seconds)
            if sample.get("valid") is not values["valid"]:
                raise ValueError(f"{label}.valid does not match recomputed sample validity")
            if values["valid"] is not True:
                raise ValueError(f"{label} contains invalid process, storage, or Job evidence")
            measurements.append(values)
            if phase == "warmup":
                warmup.append(sample)
            elif phase == "soak_boundary":
                boundaries.append(sample)
            else:
                soak.append(sample)

        if not warmup:
            failures.append("production report has no warmup samples")
        if len(boundaries) != 1:
            raise ValueError("production report must contain exactly one soak boundary sample")
        valid_soak = [sample for sample in soak if sample.get("valid") is True]
        if len(valid_soak) < MINIMUM_VALID_SAMPLES:
            failures.append(
                f"production soak has {len(valid_soak)} valid samples; {MINIMUM_VALID_SAMPLES} required"
            )
        exact_counts = {
            "warmup_samples": len(warmup),
            "soak_samples": len(soak),
            "valid_soak_samples": len(valid_soak),
        }
        for name, expected_value in exact_counts.items():
            if integer(evidence.get(name), f"evidence.{name}") != expected_value:
                failures.append(f"evidence.{name} does not match report samples")

        soak_measurements = [
            measurements[index]
            for index, sample in enumerate(samples)
            if isinstance(sample, dict) and sample.get("phase") == "soak"
        ]
        if not soak_measurements:
            raise ValueError("production report has no soak samples")
        boundary_sample = boundaries[0]
        boundary_index = samples.index(boundary_sample)
        boundary_measurement = measurements[boundary_index]
        boundary_evidence = mapping(
            evidence.get("soak_boundary"), "evidence.soak_boundary"
        )
        if set(boundary_evidence) != {
            "sample_sequence",
            "prometheus_snapshot_record",
            "sampled_at_epoch_seconds",
            "sample_clock_seconds",
            "environment_record",
            "environment_completed_at_epoch_seconds",
            "environment_aggregate_sha256",
            "anomalies",
        }:
            raise ValueError("evidence.soak_boundary has unexpected fields")
        if integer(
            boundary_evidence.get("sample_sequence"),
            "evidence.soak_boundary.sample_sequence",
        ) != integer(boundary_sample.get("sequence"), "boundary sample sequence"):
            raise ValueError("soak boundary sample sequence is not bound")
        if integer(
            boundary_evidence.get("prometheus_snapshot_record"),
            "evidence.soak_boundary.prometheus_snapshot_record",
        ) != integer(
            mapping(boundary_sample.get("metrics"), "boundary sample metrics").get(
                "snapshot_record"
            ),
            "boundary snapshot record",
        ):
            raise ValueError("soak boundary Prometheus record is not bound")
        if number(
            boundary_evidence.get("sampled_at_epoch_seconds"),
            "evidence.soak_boundary.sampled_at_epoch_seconds",
        ) != number(
            boundary_sample.get("sampled_at_epoch_seconds"),
            "boundary sample timestamp",
        ):
            raise ValueError("soak boundary timestamp is not bound")
        if number(
            boundary_evidence.get("sample_clock_seconds"),
            "evidence.soak_boundary.sample_clock_seconds",
        ) != number(
            boundary_sample.get("sample_clock_seconds"),
            "boundary sample BOOTTIME",
        ):
            raise ValueError("soak boundary BOOTTIME is not bound")
        environment_checks = [
            mapping(value, f"environment_checks[{index}]")
            for index, value in enumerate(
                sequence(report.get("environment_checks"), "environment_checks")
            )
        ]
        boundary_environment_record = positive_integer(
            boundary_evidence.get("environment_record"),
            "evidence.soak_boundary.environment_record",
        )
        if boundary_environment_record > len(environment_checks):
            raise ValueError("soak boundary environment record does not exist")
        boundary_environment = environment_checks[boundary_environment_record - 1]
        if (
            integer(
                boundary_environment.get("sequence"),
                "soak boundary environment sequence",
            )
            != boundary_environment_record
            or boundary_environment.get("phase") != "soak_boundary"
            or boundary_environment.get("operation_round_index") is not None
            or boundary_environment.get("post_warmup_baseline") is not True
        ):
            raise ValueError("soak boundary environment record is not an exact baseline")
        boundary_environment_completed = number(
            boundary_evidence.get("environment_completed_at_epoch_seconds"),
            "evidence.soak_boundary.environment_completed_at_epoch_seconds",
        )
        if boundary_environment_completed != number(
            boundary_environment.get("completed_at_epoch_seconds"),
            "soak boundary environment completion",
        ):
            raise ValueError("soak boundary environment completion is not bound")
        boundary_sampled_at = number(
            boundary_sample.get("sampled_at_epoch_seconds"),
            "boundary sample timestamp",
        )
        if boundary_environment_completed > boundary_sampled_at:
            raise ValueError("soak boundary environment completed after its sample")
        if require_sha256(
            boundary_evidence.get("environment_aggregate_sha256"),
            "evidence.soak_boundary.environment_aggregate_sha256",
        ) != require_sha256(
            boundary_environment.get("aggregate_sha256"),
            "soak boundary environment aggregate_sha256",
        ):
            raise ValueError("soak boundary environment digest is not bound")
        soak_rounds = [
            mapping(value, f"operation_rounds[{index}]")
            for index, value in enumerate(
                sequence(report.get("operation_rounds"), "operation_rounds")
            )
            if isinstance(value, dict) and value.get("phase") == "soak"
        ]
        if soak_rounds and number(
            soak_rounds[0].get("started_at_epoch_seconds"),
            "first soak Operation started_at_epoch_seconds",
        ) < max(boundary_sampled_at, boundary_environment_completed):
            raise ValueError("the soak boundary was captured after the first soak Operation began")
        boundary_anomalies = mapping(
            boundary_evidence.get("anomalies"), "evidence.soak_boundary.anomalies"
        )
        if boundary_anomalies != mapping(
            boundary_sample.get("anomalies"), "boundary sample anomalies"
        ):
            raise ValueError("soak boundary anomaly counters are not bound")
        anomaly_baseline = mapping(
            evidence.get("anomaly_counter_baseline"),
            "evidence.anomaly_counter_baseline",
        )
        if set(anomaly_baseline) != set(ANOMALY_SAMPLE_METRICS):
            raise ValueError("evidence.anomaly_counter_baseline has unexpected fields")
        for name in ANOMALY_SAMPLE_METRICS:
            baseline_value = number(
                anomaly_baseline.get(name), f"evidence.anomaly_counter_baseline.{name}"
            )
            if baseline_value != boundary_measurement[name]:
                failures.append(
                    f"evidence.anomaly_counter_baseline.{name} does not match the soak boundary"
                )
        for index, values in enumerate(soak_measurements, start=1):
            for name in ANOMALY_COUNTER_FIELDS:
                if values[name] != boundary_measurement[name]:
                    raise ValueError(
                        f"soak sample {index} anomaly counter {name} changed after baseline"
                    )
            if values["state_loaded"] != 1:
                raise ValueError(f"soak sample {index} anomaly state is not loaded")
            if (
                values["process_start_time_seconds"]
                != boundary_measurement["process_start_time_seconds"]
            ):
                raise ValueError(
                    f"soak sample {index} process start time changed after baseline"
                )

        timestamps = [
            number(sample.get("sample_clock_seconds"), "sample BOOTTIME")
            for sample in valid_soak
        ]
        gaps = [right - left for left, right in zip(timestamps, timestamps[1:])]
        max_gap = max(gaps, default=0.0)
        if number(
            evidence.get("max_observed_sample_gap_seconds"),
            "evidence.max_observed_sample_gap_seconds",
        ) != max_gap:
            failures.append(
                "evidence.max_observed_sample_gap_seconds does not match report samples"
            )
        if max_gap > MAX_SAMPLE_GAP_SECONDS:
            failures.append(
                f"production soak sample gap {max_gap:.2f}s exceeds {MAX_SAMPLE_GAP_SECONDS}s"
            )
        global_timestamps = [
            number(sample.get("sample_clock_seconds"), "sample BOOTTIME")
            for sample in samples
            if isinstance(sample, dict) and sample.get("valid") is True
        ]
        global_gaps = [
            right - left
            for left, right in zip(global_timestamps, global_timestamps[1:])
        ]
        max_global_gap = max(global_gaps, default=0.0)
        if number(
            evidence.get("max_observed_global_sample_gap_seconds"),
            "evidence.max_observed_global_sample_gap_seconds",
        ) != max_global_gap:
            failures.append(
                "evidence.max_observed_global_sample_gap_seconds does not match report samples"
            )
        if max_global_gap > MAX_SAMPLE_GAP_SECONDS:
            failures.append(
                "global warmup/boundary/soak sample gap "
                f"{max_global_gap:.2f}s exceeds {MAX_SAMPLE_GAP_SECONDS}s"
            )
        soak_elapsed = number(evidence.get("soak_elapsed_seconds"), "soak elapsed")
        soak_phase_elapsed = [
            number(sample.get("phase_elapsed_seconds"), "sample phase elapsed")
            for sample in valid_soak
        ]
        if soak_phase_elapsed and (
            soak_phase_elapsed[0] > MAX_SAMPLE_GAP_SECONDS
            or soak_elapsed - soak_phase_elapsed[-1] > MAX_SAMPLE_GAP_SECONDS
        ):
            failures.append("production soak samples do not cover the complete 24-hour window")

        extrema = process_extrema(
            [boundary_measurement, *soak_measurements], "production soak"
        )
        require_exact_process_extrema(report, extrema, "report sample extrema")
        return {
            "samples": samples,
            "warmup_samples": warmup,
            "soak_boundary_sample": boundary_sample,
            "soak_boundary_measurement": boundary_measurement,
            "soak_samples": soak,
            "valid_soak_samples": valid_soak,
            "measurements": measurements,
            "soak_extrema": extrema,
        }
    except ValueError as error:
        failures.append(str(error))
        return None


def prometheus_metric_value(
    metrics: dict[str, Any], metric_name: str, label: str
) -> float:
    matches = [
        value
        for key, value in metrics.items()
        if key == metric_name or str(key).startswith(f"{metric_name}{{")
    ]
    if len(matches) != 1:
        raise ValueError(f"{label} must contain exactly one {metric_name} metric")
    return number(matches[0], f"{label}.{metric_name}")


def validate_prometheus_sidecar(
    snapshots: list[Any],
    report: dict[str, Any],
    sample_summary: dict[str, Any] | None,
    failures: list[str],
) -> None:
    try:
        if sample_summary is None:
            raise ValueError("report samples are invalid; Prometheus sidecar cannot be bound")
        samples = sample_summary["samples"]
        if len(snapshots) != len(samples):
            raise ValueError("Prometheus snapshot log does not match report cardinality")
        sidecar_measurements: list[dict[str, float | bool]] = []
        permanent_running_seconds = number(
            mapping(report.get("evidence"), "evidence").get(
                "permanent_running_seconds"
            ),
            "permanent state limit",
        )
        for index, (raw_snapshot, raw_sample) in enumerate(zip(snapshots, samples)):
            label = f"Prometheus snapshot log record {index + 1}"
            snapshot = mapping(raw_snapshot, label)
            if set(snapshot) != {
                "sequence",
                "phase",
                "sampled_at_epoch_seconds",
                "sample_clock_seconds",
                "metrics",
                "storage",
            }:
                raise ValueError(f"{label} has unexpected fields")
            sample = mapping(raw_sample, f"samples[{index}]")
            sample_sequence = integer(
                sample.get("sequence"), f"samples[{index}].sequence"
            )
            if integer(snapshot.get("sequence"), f"{label}.sequence") != sample_sequence:
                raise ValueError(f"{label} sequence does not match its report sample")
            if snapshot.get("phase") != sample.get("phase"):
                raise ValueError(f"{label} phase does not match its report sample")
            if number(
                snapshot.get("sampled_at_epoch_seconds"), f"{label}.timestamp"
            ) != number(
                sample.get("sampled_at_epoch_seconds"),
                f"samples[{index}].sampled_at_epoch_seconds",
            ):
                raise ValueError(f"{label} timestamp does not match its report sample")
            if number(
                snapshot.get("sample_clock_seconds"), f"{label}.sample_clock_seconds"
            ) != number(
                sample.get("sample_clock_seconds"),
                f"samples[{index}].sample_clock_seconds",
            ):
                raise ValueError(f"{label} BOOTTIME does not match its report sample")
            sample_metrics = mapping(
                sample.get("metrics"), f"samples[{index}].metrics"
            )
            if integer(
                sample_metrics.get("snapshot_record"),
                f"samples[{index}].metrics.snapshot_record",
            ) != sample_sequence:
                raise ValueError(f"{label} snapshot_record does not match its sequence")

            raw_metrics = mapping(snapshot.get("metrics"), f"{label}.metrics")
            storage = mapping(snapshot.get("storage"), f"{label}.storage")
            if set(storage) != {"pool_connections", "pool_idle_connections"}:
                raise ValueError(f"{label}.storage has unexpected fields")
            values: dict[str, float | bool] = {
                field: prometheus_metric_value(raw_metrics, metric_name, label)
                for field, metric_name in PROMETHEUS_SAMPLE_METRICS.items()
            }
            values.update(
                {
                    field: prometheus_metric_value(raw_metrics, metric_name, label)
                    for field, metric_name in ANOMALY_SAMPLE_METRICS.items()
                }
            )
            values["pool_connections"] = number(
                storage.get("pool_connections"), f"{label}.storage.pool_connections"
            )
            values["pool_idle_connections"] = number(
                storage.get("pool_idle_connections"),
                f"{label}.storage.pool_idle_connections",
            )
            values["valid"] = bool(
                values["rss_bytes"] > 0
                and values["threads"] > 0
                and values["collection_error"] == 0
                and values["expired_leases"] == 0
                and values["oldest_leased_heartbeat_age_seconds"]
                <= permanent_running_seconds
                and values["pool_connections"] > 0
                and 0
                <= values["pool_idle_connections"]
                <= values["pool_connections"]
            )
            report_values = sample_summary["measurements"][index]
            for field in (
                "rss_bytes",
                "threads",
                "active_requests",
                "pool_connections",
                "pool_idle_connections",
                "collection_error",
                "expired_leases",
                "oldest_leased_heartbeat_age_seconds",
                *ANOMALY_SAMPLE_METRICS,
            ):
                if values[field] != report_values[field]:
                    raise ValueError(
                        f"{label} {field} does not match its report sample"
                    )
            if values["valid"] is not sample.get("valid"):
                raise ValueError(f"{label} validity does not match its report sample")
            if values["valid"] is not True:
                raise ValueError(f"{label} contains invalid production metrics")
            sidecar_measurements.append(values)

        soak_measurements = [
            sidecar_measurements[index]
            for index, sample in enumerate(samples)
            if sample.get("phase") == "soak"
        ]
        if not soak_measurements:
            raise ValueError("Prometheus sidecar has no soak samples")
        boundary_sample = sample_summary["soak_boundary_sample"]
        boundary_index = samples.index(boundary_sample)
        baseline = sidecar_measurements[boundary_index]
        extrema = process_extrema(
            [baseline, *soak_measurements], "Prometheus soak sidecar"
        )
        evidence_baseline = mapping(
            mapping(report.get("evidence"), "evidence").get(
                "anomaly_counter_baseline"
            ),
            "evidence.anomaly_counter_baseline",
        )
        for name in ANOMALY_SAMPLE_METRICS:
            if number(
                evidence_baseline.get(name),
                f"evidence.anomaly_counter_baseline.{name}",
            ) != baseline[name]:
                raise ValueError(
                    f"Prometheus sidecar {name} baseline does not match report evidence"
                )
        for index, values in enumerate(soak_measurements, start=1):
            for name in ANOMALY_COUNTER_FIELDS:
                if values[name] != baseline[name]:
                    raise ValueError(
                        f"Prometheus soak record {index} anomaly counter {name} changed"
                    )
            if values["state_loaded"] != 1:
                raise ValueError(
                    f"Prometheus soak record {index} anomaly state is not loaded"
                )
            if values["process_start_time_seconds"] != baseline[
                "process_start_time_seconds"
            ]:
                raise ValueError(
                    f"Prometheus soak record {index} process start time changed"
                )
        if extrema != sample_summary["soak_extrema"]:
            raise ValueError("Prometheus sidecar extrema do not match report sample extrema")
        require_exact_process_extrema(report, extrema, "Prometheus sidecar extrema")
    except ValueError as error:
        failures.append(str(error))


def validate_inventory(report: dict[str, Any], failures: list[str]) -> None:
    try:
        checks = sequence(report.get("inventory_checks"), "inventory_checks")
        if len(checks) < MINIMUM_OPERATION_ROUNDS + 1:
            failures.append("too few full inventory checks for the 24-hour gate")
        soak_timestamps: list[float] = []
        for index, raw in enumerate(checks):
            check = mapping(raw, f"inventory_checks[{index}]")
            number(check.get("sampled_at_epoch_seconds"), "inventory timestamp")
            if check.get("phase") == "soak":
                soak_timestamps.append(
                    number(
                        check.get("phase_elapsed_seconds"),
                        "inventory phase elapsed",
                    )
                )
            nodes = number(check.get("nodes_total"), "inventory nodes_total")
            ready = number(check.get("nodes_ready"), "inventory nodes_ready")
            deployments = number(
                check.get("deployments_total"), "inventory deployments_total"
            )
            running = number(
                check.get("deployments_running"), "inventory deployments_running"
            )
            topologies = number(
                check.get("topologies_total"), "inventory topologies_total"
            )
            in_sync = number(
                check.get("topologies_in_sync"), "inventory topologies_in_sync"
            )
            if check.get("ok") is not True:
                failures.append(f"inventory_checks[{index}] is not successful")
            if nodes < 100 or ready != nodes:
                failures.append(f"inventory_checks[{index}] does not prove all Nodes ready")
            if deployments < 2_000 or running != deployments:
                failures.append(
                    f"inventory_checks[{index}] does not prove every Deployment RUNNING"
                )
            if number(check.get("topology_resources"), "topology resources") < 10_000:
                failures.append(f"inventory_checks[{index}] is below topology scale")
            if topologies <= 0 or in_sync != topologies:
                failures.append(f"inventory_checks[{index}] is not fully IN_SYNC")
            if number(check.get("topology_drift"), "topology drift") != 0:
                failures.append(f"inventory_checks[{index}] reports topology drift")
            if check.get("permanent_operations") != []:
                failures.append(f"inventory_checks[{index}] reports permanent Operations")
            if failures and failures[-1].startswith(f"inventory_checks[{index}]"):
                break
        if len(soak_timestamps) < MINIMUM_OPERATION_ROUNDS:
            failures.append("too few full inventory checks during the soak phase")
        gaps = [
            right - left
            for left, right in zip(soak_timestamps, soak_timestamps[1:])
        ]
        if gaps and max(gaps) > MAX_OPERATION_GAP_SECONDS:
            failures.append("full inventory checks contain an interval longer than 390 seconds")
        evidence = mapping(report.get("evidence"), "evidence")
        soak_elapsed = number(evidence.get("soak_elapsed_seconds"), "soak elapsed")
        if soak_timestamps and (
            soak_timestamps[0] > MAX_OPERATION_GAP_SECONDS
            or soak_elapsed - soak_timestamps[-1] > MAX_OPERATION_GAP_SECONDS
        ):
            failures.append("full inventory checks do not cover the complete soak window")
    except ValueError as error:
        failures.append(str(error))


def validate_operation_rounds(report: dict[str, Any], failures: list[str]) -> None:
    try:
        rounds = sequence(report.get("operation_rounds"), "operation_rounds")
        soak_rounds = [round_ for round_ in rounds if isinstance(round_, dict) and round_.get("phase") == "soak"]
        evidence = mapping(report.get("evidence"), "evidence")
        if integer(
            evidence.get("soak_operation_rounds"),
            "evidence.soak_operation_rounds",
        ) != len(soak_rounds):
            failures.append(
                "evidence.soak_operation_rounds does not match Operation rounds"
            )
        if len(soak_rounds) < MINIMUM_OPERATION_ROUNDS:
            failures.append(
                f"only {len(soak_rounds)} soak Operation rounds; {MINIMUM_OPERATION_ROUNDS} required"
            )
        environment_by_round: dict[int, dict[str, Any]] = {}
        for raw_check in sequence(report.get("environment_checks"), "environment_checks"):
            if not isinstance(raw_check, dict) or raw_check.get("phase") != "operation_round":
                continue
            round_index = positive_integer(
                raw_check.get("operation_round_index"),
                "environment operation_round_index",
            )
            if round_index in environment_by_round:
                raise ValueError("environment operation_round_index is duplicated")
            environment_by_round[round_index] = raw_check
        timestamps: list[float] = []
        for index, round_ in enumerate(soak_rounds):
            round_number = positive_integer(
                round_.get("round"), f"soak Operation round {index}.round"
            )
            if round_number != index + 1:
                raise ValueError("soak Operation round numbers are not contiguous")
            timestamps.append(
                number(
                    round_.get("phase_elapsed_seconds"),
                    "Operation round phase elapsed",
                )
            )
            if round_.get("ok") is not True:
                failures.append(f"soak Operation round {index} failed")
                break
            for name in ("requested_operations", "created_operations", "event_streams_observed"):
                if integer(round_.get(name), f"Operation round {name}") != 50:
                    failures.append(f"soak Operation round {index} does not have exactly 50 {name}")
                    break
            if number(round_.get("failed_requests"), "Operation round failed_requests") != 0:
                failures.append(f"soak Operation round {index} contains failed requests")
                break
            for name in (
                "target_nodes",
                "target_deployments",
                "target_containers",
                "unique_created_operations",
            ):
                if integer(round_.get(name), f"Operation round {name}") != 50:
                    failures.append(
                        f"soak Operation round {index} does not prove 50 distinct {name}"
                    )
                    break
            if round_.get("unique_created_operations") != round_.get(
                "created_operations"
            ):
                failures.append(
                    f"soak Operation round {index} contains duplicate Operation IDs"
                )
                break
            target_identities = [
                mapping(value, f"soak Operation round {index}.target_identities")
                for value in sequence(
                    round_.get("target_identities"),
                    f"soak Operation round {index}.target_identities",
                )
            ]
            if len(target_identities) != 50:
                raise ValueError(
                    f"soak Operation round {index} does not contain 50 target identities"
                )
            normalized_targets: list[dict[str, str]] = []
            for target_index, target in enumerate(target_identities):
                if set(target) != {"deployment_id", "node_id", "container_id"}:
                    raise ValueError(
                        f"soak Operation round {index} target {target_index} has unexpected fields"
                    )
                normalized_targets.append(
                    {
                        name: nonempty_string(
                            target.get(name),
                            f"soak Operation round {index} target {target_index}.{name}",
                        )
                        for name in ("deployment_id", "node_id", "container_id")
                    }
                )
            sorted_targets = sorted(
                normalized_targets,
                key=lambda value: (
                    value["deployment_id"],
                    value["node_id"],
                    value["container_id"],
                ),
            )
            if normalized_targets != sorted_targets:
                raise ValueError(
                    f"soak Operation round {index} target identities are not sorted"
                )
            distinct_counts = {
                "target_deployments": len(
                    {value["deployment_id"] for value in normalized_targets}
                ),
                "target_nodes": len({value["node_id"] for value in normalized_targets}),
                "target_containers": len(
                    {value["container_id"] for value in normalized_targets}
                ),
            }
            for name, count in distinct_counts.items():
                if count != round_.get(name):
                    raise ValueError(
                        f"soak Operation round {index} {name} is not its distinct identity count"
                    )
            operation_ids = [
                nonempty_string(value, f"soak Operation round {index}.operation_ids")
                for value in sequence(
                    round_.get("operation_ids"),
                    f"soak Operation round {index}.operation_ids",
                )
            ]
            if (
                len(operation_ids) != 50
                or len(set(operation_ids)) != 50
                or operation_ids != sorted(operation_ids)
            ):
                raise ValueError(
                    f"soak Operation round {index} Operation IDs are not 50 unique sorted values"
                )
            canonical_target_digest = hashlib.sha256(
                json.dumps(
                    normalized_targets, sort_keys=True, separators=(",", ":")
                ).encode("utf-8")
            ).hexdigest()
            canonical_operation_digest = hashlib.sha256(
                json.dumps(operation_ids, separators=(",", ":")).encode("utf-8")
            ).hexdigest()
            if require_sha256(
                round_.get("target_identities_sha256"),
                f"soak Operation round {index}.target_identities_sha256",
            ) != canonical_target_digest:
                raise ValueError(
                    f"soak Operation round {index} target identity digest is invalid"
                )
            if require_sha256(
                round_.get("operation_ids_sha256"),
                f"soak Operation round {index}.operation_ids_sha256",
            ) != canonical_operation_digest:
                raise ValueError(
                    f"soak Operation round {index} Operation ID digest is invalid"
                )
            environment = environment_by_round.get(round_number)
            if environment is None:
                failures.append(
                    f"soak Operation round {index} has no matching environment observation"
                )
                break
            if positive_integer(
                round_.get("environment_record"),
                f"soak Operation round {index}.environment_record",
            ) != positive_integer(
                environment.get("sequence"),
                f"environment check for Operation round {index}.sequence",
            ):
                failures.append(
                    f"soak Operation round {index} environment record does not match"
                )
                break
            environment_hashes = {
                "environment_engine_aggregate_sha256": "aggregate_sha256",
                "environment_node_ids_sha256": "node_ids_sha256",
                "environment_deployment_ids_sha256": "deployment_ids_sha256",
                "environment_container_ids_sha256": "container_ids_sha256",
            }
            for round_field, environment_field in environment_hashes.items():
                digest = require_sha256(
                    round_.get(round_field),
                    f"soak Operation round {index}.{round_field}",
                )
                if digest != environment.get(environment_field):
                    failures.append(
                        f"soak Operation round {index} {round_field} does not match its environment observation"
                    )
                    break
        gaps = [right - left for left, right in zip(timestamps, timestamps[1:])]
        if gaps and max(gaps) > MAX_OPERATION_GAP_SECONDS:
            failures.append("Operation rounds contain an interval longer than 390 seconds")
        soak_elapsed = number(evidence.get("soak_elapsed_seconds"), "soak elapsed")
        if timestamps and (
            timestamps[0] > MAX_OPERATION_GAP_SECONDS
            or soak_elapsed - timestamps[-1] > MAX_OPERATION_GAP_SECONDS
        ):
            failures.append("Operation rounds do not cover the complete soak window")
    except ValueError as error:
        failures.append(str(error))


def require_sha256(value: Any, label: str) -> str:
    text = nonempty_string(value, label)
    if not SHA256_PATTERN.fullmatch(text):
        raise ValueError(f"{label} must be 64 lowercase hexadecimal characters")
    return text


def operation_event_cursor(value: Any, label: str) -> dict[str, Any]:
    text = nonempty_string(value, label)
    if (
        len(text) > MAX_OPERATION_EVENT_CURSOR_BYTES * 2
        or len(text) % 2
        or any(character not in "0123456789abcdef" for character in text)
    ):
        raise ValueError(f"{label} is not a canonical Operation event cursor")
    try:
        decoded = json.loads(bytes.fromhex(text).decode("utf-8"))
    except (ValueError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is not a valid Operation event cursor") from error
    cursor = mapping(decoded, label)
    if set(cursor) != {"operation_revision", "job_sequences"}:
        raise ValueError(f"{label} has unexpected cursor fields")
    revision = integer(cursor.get("operation_revision"), f"{label}.operation_revision")
    jobs = mapping(cursor.get("job_sequences"), f"{label}.job_sequences")
    if revision < 0 or any(
        not isinstance(job_id, str)
        or not job_id
        or integer(job_sequence, f"{label}.job_sequences[{job_id!r}]") < 0
        for job_id, job_sequence in jobs.items()
    ):
        raise ValueError(f"{label} has invalid cursor values")
    canonical = json.dumps(cursor, sort_keys=True, separators=(",", ":")).encode(
        "utf-8"
    ).hex()
    if text != canonical:
        raise ValueError(f"{label} is not canonically encoded")
    return cursor


def validate_restart_probe_evidence(
    evidence: dict[str, Any], failures: list[str]
) -> None:
    try:
        pre = mapping(evidence.get("restart_probe_pre"), "evidence.restart_probe_pre")
        post = mapping(
            evidence.get("restart_probe_post"), "evidence.restart_probe_post"
        )
        fields = {
            "operation_id",
            "status",
            "action",
            "revision",
            "request_sha256",
            "operation_sha256",
            "event_cursor",
        }
        for label, snapshot in (
            ("evidence.restart_probe_pre", pre),
            ("evidence.restart_probe_post", post),
        ):
            if set(snapshot) != fields:
                raise ValueError(f"{label} has unexpected fields")
            operation_id = nonempty_string(
                snapshot.get("operation_id"), f"{label}.operation_id"
            )
            if operation_id != evidence.get("restart_probe_operation_id"):
                raise ValueError(
                    f"{label}.operation_id does not match restart_probe_operation_id"
                )
            if nonempty_string(snapshot.get("status"), f"{label}.status") != "PLANNED":
                raise ValueError(f"{label}.status is not PLANNED")
            if (
                nonempty_string(snapshot.get("action"), f"{label}.action")
                != "deployment.health"
            ):
                raise ValueError(f"{label}.action is not deployment.health")
            revision = positive_integer(snapshot.get("revision"), f"{label}.revision")
            require_sha256(snapshot.get("request_sha256"), f"{label}.request_sha256")
            require_sha256(
                snapshot.get("operation_sha256"), f"{label}.operation_sha256"
            )
            cursor = operation_event_cursor(
                snapshot.get("event_cursor"), f"{label}.event_cursor"
            )
            if cursor["operation_revision"] != revision:
                raise ValueError(f"{label}.event_cursor revision does not match snapshot")
        if pre != post:
            failures.append("restart probe Operation changed across the control-plane restart")
    except ValueError as error:
        failures.append(str(error))


ENVIRONMENT_IDENTITY_FIELDS = (
    "configuration_fingerprint_sha256",
    "fixture_image",
    "node_ids_sha256",
    "deployment_ids_sha256",
    "container_ids_sha256",
    "endpoint_ids_sha256",
    "link_ids_sha256",
    "observer_identity_sha256",
    "provenance_record_sha256",
    "image_workflow_run_id",
    "control_plane_image",
    "agent_image",
    "provenance_fixture_image",
    "control_plane_origin_sha256",
    "restart_argv_sha256",
    "topology_id",
    "topology_revision_id",
    "topology_identity_sha256",
    "runtime_provision_manifest_sha256",
    "runtime_host_identity_sha256",
    "runner_machine_id_sha256",
    "control_plane_image_id",
    "control_plane_container_id",
    "control_plane_started_at",
    "control_plane_configuration_sha256",
    "postgres_image",
    "postgres_image_id",
    "postgres_container_id",
    "postgres_started_at",
    "postgres_configuration_sha256",
    "postgres_server_leaf_sha256",
    "agent_image_id",
    "agent_node_ids_sha256",
    "agent_container_ids_sha256",
    "agent_started_at_sha256",
    "agent_spiffe_ids_sha256",
    "agent_certificate_fingerprints_sha256",
    "agent_ledger_identities_sha256",
    "agent_independent_mtls_identities",
    "agent_independent_sqlite_ledgers",
    "docker_engine_image",
    "docker_engine_image_id",
    "engine_outer_container_ids_sha256",
    "engine_inner_daemon_ids_sha256",
    "engine_socket_volumes_sha256",
    "engine_data_volumes_sha256",
)

ENVIRONMENT_SHA256_FIELDS = {
    "configuration_fingerprint_sha256",
    "node_ids_sha256",
    "deployment_ids_sha256",
    "container_ids_sha256",
    "endpoint_ids_sha256",
    "link_ids_sha256",
    "observer_identity_sha256",
    "provenance_record_sha256",
    "control_plane_origin_sha256",
    "restart_argv_sha256",
    "topology_identity_sha256",
    "runtime_provision_manifest_sha256",
    "runtime_host_identity_sha256",
    "runner_machine_id_sha256",
    "control_plane_configuration_sha256",
    "postgres_container_id",
    "postgres_configuration_sha256",
    "postgres_server_leaf_sha256",
    "agent_node_ids_sha256",
    "agent_container_ids_sha256",
    "agent_started_at_sha256",
    "agent_spiffe_ids_sha256",
    "agent_certificate_fingerprints_sha256",
    "agent_ledger_identities_sha256",
    "engine_outer_container_ids_sha256",
    "engine_inner_daemon_ids_sha256",
    "engine_socket_volumes_sha256",
    "engine_data_volumes_sha256",
}

ENVIRONMENT_INTEGER_FIELDS = {
    "agent_independent_mtls_identities",
    "agent_independent_sqlite_ledgers",
}


def validate_environment_checks(
    report: dict[str, Any], failures: list[str]
) -> list[dict[str, Any]]:
    try:
        checks = [
            mapping(value, f"environment_checks[{index}]")
            for index, value in enumerate(
                sequence(report.get("environment_checks"), "environment_checks")
            )
        ]
        rounds = [
            value
            for value in sequence(report.get("operation_rounds"), "operation_rounds")
            if isinstance(value, dict) and value.get("phase") == "soak"
        ]
        expected_records = len(rounds) + 4
        if len(checks) != expected_records:
            failures.append(
                "environment observations must contain pre/post-restart, the soak "
                "boundary, every soak Operation round, and final records"
            )
            return checks
        if checks[0].get("phase") != "pre_restart" or checks[0].get(
            "operation_round_index"
        ) is not None:
            failures.append("first environment observation is not pre-restart")
        if checks[1].get("phase") != "post_restart" or checks[1].get(
            "operation_round_index"
        ) is not None:
            failures.append("second environment observation is not post-restart")
        if (
            checks[2].get("phase") != "soak_boundary"
            or checks[2].get("operation_round_index") is not None
            or checks[2].get("post_warmup_baseline") is not True
        ):
            failures.append("third environment observation is not the soak boundary")
        if checks[-1].get("phase") != "final" or checks[-1].get(
            "operation_round_index"
        ) is not None:
            failures.append("last environment observation is not final")
        operation_checks = checks[3:-1]

        baseline_identity: dict[str, str] | None = None
        stable_process_identity: dict[str, str] | None = None
        completions: list[float] = []
        for index, check in enumerate(checks):
            if check.get("sequence") != index + 1:
                failures.append("environment observation sequence is not contiguous")
                break
            if index != 2 and check.get("post_warmup_baseline") is not False:
                failures.append("post-warmup baseline marker appears more than once")
                break
            started = number(
                check.get("started_at_epoch_seconds"),
                f"environment_checks[{index}].started_at_epoch_seconds",
            )
            completed = number(
                check.get("completed_at_epoch_seconds"),
                f"environment_checks[{index}].completed_at_epoch_seconds",
            )
            if completed < started or completed - started > 85:
                failures.append(
                    f"environment_checks[{index}] exceeds the 85-second observer budget"
                )
            identity = {
                field: (
                    require_sha256(
                        check.get(field), f"environment_checks[{index}].{field}"
                    )
                    if field in ENVIRONMENT_SHA256_FIELDS
                    else positive_integer(
                        check.get(field), f"environment_checks[{index}].{field}"
                    )
                    if field in ENVIRONMENT_INTEGER_FIELDS
                    else nonempty_string(
                        check.get(field), f"environment_checks[{index}].{field}"
                    )
                )
                for field in ENVIRONMENT_IDENTITY_FIELDS
                if field not in {"control_plane_container_id", "control_plane_started_at"}
            }
            for image_field in (
                "fixture_image",
                "control_plane_image",
                "agent_image",
                "provenance_fixture_image",
                "postgres_image",
                "docker_engine_image",
            ):
                if not IMMUTABLE_OCI_PATTERN.fullmatch(identity[image_field]):
                    failures.append(f"environment {image_field} is not digest pinned")
            for image_id_field in (
                "control_plane_image_id",
                "postgres_image_id",
                "agent_image_id",
                "docker_engine_image_id",
            ):
                if not re.fullmatch(
                    r"sha256:[0-9a-f]{64}", identity[image_id_field]
                ):
                    failures.append(f"environment {image_id_field} is invalid")
            docker_started_at_key(
                check.get("postgres_started_at"),
                f"environment_checks[{index}].postgres_started_at",
            )
            for count_field in ENVIRONMENT_INTEGER_FIELDS:
                if identity[count_field] != 100:
                    failures.append(f"environment {count_field} is not 100")
            if baseline_identity is None:
                baseline_identity = identity
            elif identity != baseline_identity:
                failures.append("environment resource identity changed during the gate")
                break
            process_identity = {
                "container_id": nonempty_string(
                    check.get("control_plane_container_id"),
                    f"environment_checks[{index}].control_plane_container_id",
                ),
                "started_at": nonempty_string(
                    check.get("control_plane_started_at"),
                    f"environment_checks[{index}].control_plane_started_at",
                ),
            }
            process_started_at = docker_started_at_key(
                process_identity["started_at"],
                f"environment_checks[{index}].control_plane_started_at",
            )
            if not re.fullmatch(r"[0-9a-f]{64}", process_identity["container_id"]):
                failures.append("control-plane container ID is invalid")
            if index == 1:
                previous = checks[0]
                if process_started_at <= docker_started_at_key(
                    previous.get("control_plane_started_at"),
                    "pre-restart control-plane StartedAt",
                ):
                    failures.append(
                        "controlled restart did not change control-plane StartedAt"
                    )
                stable_process_identity = process_identity
            elif index >= 2 and process_identity != stable_process_identity:
                failures.append("control-plane process identity changed after restart")
                break
            for field, expected in (
                ("workers", 10),
                ("engines", 100),
                ("containers", 2_000),
                ("running_containers", 2_000),
                ("healthy_containers", 2_000),
                ("endpoint_checks_total", 2_000),
                ("endpoint_checks_healthy", 2_000),
                ("endpoint_checks_failed", 0),
                ("link_probes_total", 8_000),
                ("link_probes_healthy", 8_000),
                ("link_probes_failed", 0),
                ("drift", 0),
            ):
                if number(check.get(field), f"environment {field}") != expected:
                    failures.append(
                        f"environment_checks[{index}].{field} is not {expected}"
                    )
                    break
            require_sha256(
                check.get("aggregate_sha256"),
                f"environment_checks[{index}].aggregate_sha256",
            )
            if check.get("ok") is not True:
                failures.append(f"environment_checks[{index}] is not successful")
            if 2 < index < len(checks) - 1:
                if check.get("phase") != "operation_round":
                    failures.append(
                        f"environment_checks[{index}] is not an Operation-round observation"
                    )
                if check.get("operation_round_index") != index - 2:
                    failures.append("environment Operation-round indexes are not contiguous")
            completions.append(completed)

        coverage = completions[2:]
        gaps = [right - left for left, right in zip(coverage, coverage[1:])]
        max_gap = max(gaps, default=0.0)
        if max_gap > MAX_OPERATION_GAP_SECONDS:
            failures.append(
                "environment observations contain an interval longer than 390 seconds"
            )
        evidence = mapping(report.get("evidence"), "evidence")
        if number(
            evidence.get("environment_observations"),
            "environment observation count",
        ) != len(checks):
            failures.append("environment evidence summary count is inconsistent")
        for field, expected in (
            ("environment_first_record", 1),
            ("environment_last_record", len(checks)),
            ("environment_final_record", len(checks)),
        ):
            if number(evidence.get(field), field) != expected:
                failures.append(f"evidence.{field} is inconsistent")
        if abs(
            number(
                evidence.get("environment_max_observation_gap_seconds"),
                "environment maximum observation gap",
            )
            - max_gap
        ) > 0.001:
            failures.append("environment observation gap summary is inconsistent")
        if baseline_identity is not None:
            if (
                evidence.get("environment_configuration_fingerprint_sha256")
                != baseline_identity["configuration_fingerprint_sha256"]
            ):
                failures.append("environment configuration fingerprint summary changed")
            summary_identity = mapping(
                evidence.get("environment_identity"), "evidence.environment_identity"
            )
            if summary_identity != {
                **{
                    key: baseline_identity[key]
                    for key in ENVIRONMENT_IDENTITY_FIELDS[1:]
                    if key not in {
                        "control_plane_container_id",
                        "control_plane_started_at",
                    }
                },
                "control_plane_container_id": stable_process_identity["container_id"],
                "control_plane_started_at": stable_process_identity["started_at"],
            }:
                failures.append("environment resource identity summary is inconsistent")
        return checks
    except ValueError as error:
        failures.append(str(error))
        return []


def validate_unified_timeline(
    report: dict[str, Any], failures: list[str]
) -> None:
    """Bind all wall-clock evidence to one report and runner BOOTTIME epoch."""

    try:
        identity = mapping(report.get("identity"), "identity")
        workflow = mapping(identity.get("workflow"), "identity.workflow")
        runner = mapping(identity.get("runner"), "identity.runner")
        evidence = mapping(report.get("evidence"), "evidence")
        started_at = rfc3339_epoch(report.get("started_at"), "report.started_at")
        completed_at = rfc3339_epoch(
            evidence.get("completed_at"), "evidence.completed_at"
        )
        dispatch_at = rfc3339_epoch(
            workflow.get("created_at"), "identity.workflow.created_at"
        )
        if dispatch_at > started_at:
            failures.append("workflow dispatch postdates report start")
        if completed_at < started_at:
            failures.append("report completion predates report start")
            return

        api_received_at = number(
            workflow.get("api_local_received_at_epoch_seconds"),
            "identity.workflow.api_local_received_at_epoch_seconds",
        )
        if not started_at - 1 <= api_received_at <= completed_at:
            failures.append("workflow API verification falls outside the report lifetime")

        baseline = mapping(runner.get("service"), "identity.runner.service")
        baseline_epoch = number(
            baseline.get("observed_at_epoch_seconds"),
            "identity.runner.service.observed_at_epoch_seconds",
        )
        baseline_boottime = number(
            baseline.get("observed_boottime_usec"),
            "identity.runner.service.observed_boottime_usec",
        ) / 1_000_000
        if not started_at - 1 <= baseline_epoch <= api_received_at:
            failures.append("runner baseline falls outside report startup/API verification")
        runner_wall_boottime_offset = baseline_epoch - baseline_boottime

        api_response_boottime = number(
            workflow.get("api_response_boottime_usec"),
            "identity.workflow.api_response_boottime_usec",
        ) / 1_000_000
        if abs(
            (api_received_at - api_response_boottime)
            - runner_wall_boottime_offset
        ) > TIMELINE_TOLERANCE_SECONDS:
            failures.append("workflow API epoch is inconsistent with runner BOOTTIME")

        samples = [
            mapping(value, f"samples[{index}]")
            for index, value in enumerate(sequence(report.get("samples"), "samples"))
        ]
        phase_wall_origins: dict[str, list[float]] = {
            "warmup": [],
            "soak_boundary": [],
            "soak": [],
        }
        phase_boottime_origins: dict[str, list[float]] = {
            "warmup": [],
            "soak_boundary": [],
            "soak": [],
        }
        for index, sample in enumerate(samples):
            label = f"samples[{index}]"
            sampled_at = number(
                sample.get("sampled_at_epoch_seconds"),
                f"{label}.sampled_at_epoch_seconds",
            )
            if not started_at <= sampled_at <= completed_at:
                failures.append(f"{label} falls outside the report lifetime")
                break
            phase = sample.get("phase")
            if phase not in phase_wall_origins:
                continue
            elapsed = number(
                sample.get("phase_elapsed_seconds"),
                f"{label}.phase_elapsed_seconds",
            )
            service = mapping(sample.get("runner_service"), f"{label}.runner_service")
            observed_epoch = number(
                service.get("observed_at_epoch_seconds"),
                f"{label}.runner_service.observed_at_epoch_seconds",
            )
            observed_boottime = number(
                service.get("observed_boottime_usec"),
                f"{label}.runner_service.observed_boottime_usec",
            ) / 1_000_000
            sample_clock = number(
                sample.get("sample_clock_seconds"),
                f"{label}.sample_clock_seconds",
            )
            if not started_at <= observed_epoch <= completed_at:
                failures.append(f"{label} runner observation is outside the report lifetime")
                break
            if abs(
                (observed_epoch - observed_boottime)
                - runner_wall_boottime_offset
            ) > TIMELINE_TOLERANCE_SECONDS:
                failures.append(f"{label} epoch is inconsistent with runner BOOTTIME")
                break
            if abs(sample_clock - observed_boottime) > TIMELINE_TOLERANCE_SECONDS:
                failures.append(
                    f"{label} sample BOOTTIME is not contemporaneous with its runner observation"
                )
                break
            phase_wall_origins[phase].append(sampled_at - elapsed)
            phase_boottime_origins[phase].append(sample_clock - elapsed)

        phase_origins: dict[str, float] = {}
        for phase in ("warmup", "soak_boundary", "soak"):
            wall_origins = phase_wall_origins[phase]
            boottime_origins = phase_boottime_origins[phase]
            if not wall_origins:
                continue
            if max(wall_origins) - min(wall_origins) > TIMELINE_TOLERANCE_SECONDS:
                failures.append(f"{phase} epoch/phase_elapsed timeline is inconsistent")
            if max(boottime_origins) - min(boottime_origins) > TIMELINE_TOLERANCE_SECONDS:
                failures.append(f"{phase} BOOTTIME/phase_elapsed timeline is inconsistent")
            if any(
                abs((wall - boot) - runner_wall_boottime_offset)
                > TIMELINE_TOLERANCE_SECONDS
                for wall, boot in zip(wall_origins, boottime_origins)
            ):
                failures.append(f"{phase} phase origin is not bound to runner BOOTTIME")
            phase_origins[phase] = min(wall_origins)
            if not started_at <= phase_origins[phase] <= completed_at:
                failures.append(f"{phase} phase origin falls outside the report lifetime")
        ordered_origins = [
            phase_origins[phase]
            for phase in ("warmup", "soak_boundary", "soak")
            if phase in phase_origins
        ]
        if any(right < left for left, right in zip(ordered_origins, ordered_origins[1:])):
            failures.append("sample phase origins are not ordered warmup/boundary/soak")

        checks = [
            mapping(value, f"environment_checks[{index}]")
            for index, value in enumerate(
                sequence(report.get("environment_checks"), "environment_checks")
            )
        ]
        previous_environment_completion = started_at
        for index, check in enumerate(checks):
            check_started = number(
                check.get("started_at_epoch_seconds"),
                f"environment_checks[{index}].started_at_epoch_seconds",
            )
            check_completed = number(
                check.get("completed_at_epoch_seconds"),
                f"environment_checks[{index}].completed_at_epoch_seconds",
            )
            if not (
                started_at
                <= check_started
                <= check_completed
                <= completed_at
            ):
                failures.append(
                    f"environment_checks[{index}] falls outside the report lifetime"
                )
                break
            if check_started < previous_environment_completion:
                failures.append("environment observations overlap or move backwards")
                break
            if check_completed < previous_environment_completion:
                failures.append("environment observation timeline moved backwards")
                break
            previous_environment_completion = check_completed
        if "warmup" in phase_origins and len(checks) >= 3:
            boundary_environment_started = number(
                checks[2].get("started_at_epoch_seconds"),
                "soak boundary environment start",
            )
            boundary_environment_completed = number(
                checks[2].get("completed_at_epoch_seconds"),
                "soak boundary environment completion",
            )
            requested_warmup = number(
                evidence.get("warmup_seconds"), "evidence.warmup_seconds"
            )
            if boundary_environment_started < (
                phase_origins["warmup"] + requested_warmup - 1
            ):
                failures.append("soak boundary environment began before warmup completed")
            observed_warmup = number(
                evidence.get("warmup_elapsed_seconds"),
                "evidence.warmup_elapsed_seconds",
            )
            if abs(
                observed_warmup
                - (boundary_environment_completed - phase_origins["warmup"])
            ) > TIMELINE_TOLERANCE_SECONDS:
                failures.append("warmup elapsed summary is inconsistent with the boundary")

        history = [
            mapping(value, f"evidence.checkpoint_history[{index}]")
            for index, value in enumerate(
                sequence(evidence.get("checkpoint_history"), "checkpoint history")
            )
        ]
        for index, checkpoint in enumerate(history):
            epoch = number(
                checkpoint.get("epoch_seconds"), f"checkpoint {index} epoch"
            )
            clock = number(
                checkpoint.get("clock_seconds"), f"checkpoint {index} BOOTTIME"
            )
            if not started_at <= epoch <= completed_at:
                failures.append(f"checkpoint {index} falls outside the report lifetime")
                break
            if abs(
                (epoch - clock) - runner_wall_boottime_offset
            ) > TIMELINE_TOLERANCE_SECONDS:
                failures.append("checkpoint epoch is inconsistent with runner BOOTTIME")
                break

        final_service = mapping(
            evidence.get("runner_service_final"), "evidence.runner_service_final"
        )
        final_epoch = number(
            final_service.get("observed_at_epoch_seconds"),
            "evidence.runner_service_final.observed_at_epoch_seconds",
        )
        final_boottime = number(
            final_service.get("observed_boottime_usec"),
            "evidence.runner_service_final.observed_boottime_usec",
        ) / 1_000_000
        last_sample_at = max(
            (
                number(sample.get("sampled_at_epoch_seconds"), "sample timestamp")
                for sample in samples
            ),
            default=started_at,
        )
        if not last_sample_at <= final_epoch <= completed_at:
            failures.append("final runner observation falls outside report completion")
        if abs(
            (final_epoch - final_boottime) - runner_wall_boottime_offset
        ) > TIMELINE_TOLERANCE_SECONDS:
            failures.append("final runner epoch is inconsistent with BOOTTIME")
    except ValueError as error:
        failures.append(str(error))


def validate_runtime_sidecar(
    runtime: dict[str, Any],
    check: dict[str, Any],
    expected_commit: str,
    label: str,
) -> None:
    if set(runtime) != {
        "schema_version",
        "candidate_sha",
        "provision_manifest_sha256",
        "host_count",
        "host_identity_sha256",
        "hosts",
        "control_plane",
        "postgres",
        "restart_identity",
        "agents",
        "engines",
    } or (
        runtime.get("schema_version") != 2
        or runtime.get("candidate_sha") != expected_commit
        or runtime.get("host_count") != 13
        or runtime.get("provision_manifest_sha256")
        != check.get("runtime_provision_manifest_sha256")
        or runtime.get("host_identity_sha256")
        != check.get("runtime_host_identity_sha256")
    ):
        raise ValueError(f"{label} runtime identity/cardinality is invalid")

    hosts = sequence(runtime.get("hosts"), f"{label}.runtime_evidence.hosts")
    expected_roles = {
        "control-plane",
        "postgres",
        "runner",
        *(f"worker-{ordinal:02d}" for ordinal in range(10)),
    }
    roles: set[str] = set()
    machines: set[str] = set()
    hosts_by_role: dict[str, dict[str, Any]] = {}
    host_lines: list[str] = []
    if len(hosts) != 13:
        raise ValueError(f"{label} runtime host count is not 13")
    for index, raw_host in enumerate(hosts):
        host = mapping(raw_host, f"{label}.runtime_evidence.hosts[{index}]")
        if set(host) != {"role", "machine_id_sha256", "boot_id"}:
            raise ValueError(f"{label} runtime host fields are invalid")
        role = nonempty_string(host.get("role"), "runtime host role")
        machine = require_sha256(host.get("machine_id_sha256"), "runtime machine ID")
        boot = nonempty_string(host.get("boot_id"), "runtime boot ID")
        if not LINUX_BOOT_ID_PATTERN.fullmatch(boot):
            raise ValueError(f"{label} runtime boot ID is invalid")
        roles.add(role)
        machines.add(machine)
        hosts_by_role[role] = host
        host_lines.append(f"{role}\0{machine}\0{boot}")
    digest = hashlib.sha256()
    for host_line in sorted(host_lines):
        digest.update(host_line.encode())
        digest.update(b"\n")
    if (
        roles != expected_roles
        or len(machines) != 13
        or digest.hexdigest() != runtime.get("host_identity_sha256")
    ):
        raise ValueError(f"{label} runtime host identity set is invalid")
    if (
        hosts_by_role["runner"].get("machine_id_sha256")
        != check.get("runner_machine_id_sha256")
    ):
        raise ValueError(f"{label} runner host is not the executing gate machine")

    manifest_hash = runtime["provision_manifest_sha256"]
    control_plane = mapping(runtime.get("control_plane"), f"{label}.control_plane")
    cp_image = mapping(control_plane.get("image"), f"{label}.control_plane.image")
    cp_container = mapping(
        control_plane.get("container"), f"{label}.control_plane.container"
    )
    cp_configuration = mapping(
        control_plane.get("configuration"), f"{label}.control_plane.configuration"
    )
    cp_tls = mapping(
        control_plane.get("database_tls_identity"),
        f"{label}.control_plane.database_tls_identity",
    )
    if (
        set(control_plane)
        != {
            "schema_version",
            "candidate_sha",
            "provision_manifest_sha256",
            "host",
            "image",
            "container",
            "configuration",
            "database_tls_identity",
        }
        or control_plane.get("schema_version") != 2
        or control_plane.get("candidate_sha") != expected_commit
        or control_plane.get("provision_manifest_sha256") != manifest_hash
        or mapping(control_plane.get("host"), "control-plane host").get("role")
        != "control-plane"
        or set(cp_image)
        != {"reference", "repo_digest", "image_id", "oci_revision"}
        or cp_image.get("reference") != check.get("control_plane_image")
        or cp_image.get("repo_digest") != cp_image.get("reference")
        or cp_image.get("image_id") != check.get("control_plane_image_id")
        or cp_image.get("oci_revision") != expected_commit
        or set(cp_container)
        != {"container_id", "container_name", "started_at", "state"}
        or cp_container.get("container_id")
        != check.get("control_plane_container_id")
        or cp_container.get("started_at") != check.get("control_plane_started_at")
        or cp_container.get("state") != "RUNNING"
        or not nonempty_string(cp_container.get("container_name"), "container name")
        or set(cp_configuration)
        != {"effective_sha256", "provisioned_sha256", "non_sensitive"}
        or cp_configuration.get("effective_sha256")
        != check.get("control_plane_configuration_sha256")
        or cp_configuration.get("effective_sha256")
        != cp_configuration.get("provisioned_sha256")
        or not isinstance(cp_configuration.get("non_sensitive"), dict)
        or set(cp_tls)
        != {
            "verified_hostname",
            "port",
            "peer_leaf_sha256",
            "root_certificates_sha256",
            "tls_version",
        }
    ):
        raise ValueError(f"{label} control-plane runtime evidence is invalid")
    docker_started_at_key(cp_container.get("started_at"), "control-plane StartedAt")
    require_sha256(cp_tls.get("peer_leaf_sha256"), "PostgreSQL peer certificate")
    roots = sequence(cp_tls.get("root_certificates_sha256"), "PostgreSQL roots")
    if not roots:
        raise ValueError(f"{label} PostgreSQL root certificate set is empty")
    for root in roots:
        require_sha256(root, "PostgreSQL root certificate")

    postgres = mapping(runtime.get("postgres"), f"{label}.postgres")
    pg_image = mapping(postgres.get("image"), f"{label}.postgres.image")
    pg_container = mapping(postgres.get("container"), f"{label}.postgres.container")
    pg_configuration = mapping(
        postgres.get("configuration"), f"{label}.postgres.configuration"
    )
    pg_settings = mapping(postgres.get("settings"), f"{label}.postgres.settings")
    if (
        set(postgres)
        != {
            "schema_version",
            "candidate_sha",
            "provision_manifest_sha256",
            "host",
            "image",
            "container",
            "configuration",
            "server_leaf_sha256",
            "root_certificates_sha256",
            "settings",
        }
        or postgres.get("schema_version") != 2
        or postgres.get("candidate_sha") != expected_commit
        or postgres.get("provision_manifest_sha256") != manifest_hash
        or mapping(postgres.get("host"), "PostgreSQL host").get("role") != "postgres"
        or set(pg_image)
        != {"reference", "repo_digest", "image_id", "oci_revision"}
        or pg_image.get("reference") != check.get("postgres_image")
        or pg_image.get("repo_digest") != pg_image.get("reference")
        or pg_image.get("image_id") != check.get("postgres_image_id")
        or set(pg_container)
        != {"container_id", "container_name", "started_at", "state", "health"}
        or pg_container.get("container_id") != check.get("postgres_container_id")
        or pg_container.get("started_at") != check.get("postgres_started_at")
        or pg_container.get("state") != "RUNNING"
        or pg_container.get("health") != "HEALTHY"
        or not nonempty_string(pg_container.get("container_name"), "PostgreSQL name")
        or set(pg_configuration)
        != {"effective_sha256", "provisioned_sha256", "non_sensitive"}
        or pg_configuration.get("effective_sha256")
        != check.get("postgres_configuration_sha256")
        or pg_configuration.get("effective_sha256")
        != pg_configuration.get("provisioned_sha256")
        or not isinstance(pg_configuration.get("non_sensitive"), dict)
        or postgres.get("server_leaf_sha256")
        != check.get("postgres_server_leaf_sha256")
        or postgres.get("server_leaf_sha256") != cp_tls.get("peer_leaf_sha256")
        or postgres.get("root_certificates_sha256") != roots
        or set(pg_settings)
        != {
            "ssl",
            "ssl_cert_file",
            "ssl_key_file",
            "ssl_ca_file",
            "data_directory",
            "port",
            "postmaster_started_at",
        }
        or pg_settings.get("ssl") != "on"
        or pg_settings.get("port") != "5432"
        or not nonempty_string(
            pg_settings.get("postmaster_started_at"), "PostgreSQL postmaster start"
        )
    ):
        raise ValueError(f"{label} PostgreSQL runtime evidence is invalid")
    docker_started_at_key(pg_container.get("started_at"), "PostgreSQL StartedAt")

    agents = mapping(runtime.get("agents"), f"{label}.agents")
    agent_image = mapping(agents.get("image"), f"{label}.agents.image")
    agent_ids = sequence(agent_image.get("image_ids"), f"{label}.agents.image_ids")
    if (
        set(agents)
        != {
            "count",
            "running",
            "control_plane_origin",
            "image",
            "node_ids_sha256",
            "container_ids_sha256",
            "started_at_sha256",
            "spiffe_ids_sha256",
            "certificate_fingerprints_sha256",
            "ledger_identities_sha256",
            "independent_mtls_identities",
            "independent_sqlite_ledgers",
        }
        or agents.get("count") != 100
        or agents.get("running") != 100
        or agents.get("independent_mtls_identities")
        != check.get("agent_independent_mtls_identities")
        or agents.get("independent_sqlite_ledgers")
        != check.get("agent_independent_sqlite_ledgers")
        or hashlib.sha256(
            nonempty_string(
                agents.get("control_plane_origin"), "Agent control-plane origin"
            ).encode()
        ).hexdigest()
        != check.get("control_plane_origin_sha256")
        or set(agent_image)
        != {"reference", "repo_digest", "image_ids", "oci_revision"}
        or agent_image.get("reference") != check.get("agent_image")
        or agent_image.get("repo_digest") != agent_image.get("reference")
        or agent_image.get("oci_revision") != expected_commit
        or agent_ids != [check.get("agent_image_id")]
        or agents.get("node_ids_sha256") != check.get("agent_node_ids_sha256")
        or agents.get("container_ids_sha256")
        != check.get("agent_container_ids_sha256")
        or agents.get("started_at_sha256") != check.get("agent_started_at_sha256")
        or agents.get("spiffe_ids_sha256") != check.get("agent_spiffe_ids_sha256")
        or agents.get("certificate_fingerprints_sha256")
        != check.get("agent_certificate_fingerprints_sha256")
        or agents.get("ledger_identities_sha256")
        != check.get("agent_ledger_identities_sha256")
    ):
        raise ValueError(f"{label} Agent runtime evidence is invalid")

    engines = mapping(runtime.get("engines"), f"{label}.engines")
    engine_image = mapping(engines.get("image"), f"{label}.engines.image")
    engine_ids = sequence(engine_image.get("image_ids"), f"{label}.engines.image_ids")
    if (
        set(engines)
        != {
            "count",
            "running",
            "healthy",
            "inner_daemon_count",
            "container_count",
            "image",
            "outer_container_ids_sha256",
            "inner_daemon_ids_sha256",
            "socket_volumes_sha256",
            "data_volumes_sha256",
        }
        or engines.get("count") != 100
        or engines.get("running") != 100
        or engines.get("healthy") != 100
        or engines.get("inner_daemon_count") != 100
        or engines.get("container_count") != 2_000
        or set(engine_image) != {"reference", "repo_digest", "image_ids"}
        or engine_image.get("reference") != check.get("docker_engine_image")
        or engine_image.get("repo_digest") != engine_image.get("reference")
        or engine_ids != [check.get("docker_engine_image_id")]
        or engines.get("outer_container_ids_sha256")
        != check.get("engine_outer_container_ids_sha256")
        or engines.get("inner_daemon_ids_sha256")
        != check.get("engine_inner_daemon_ids_sha256")
        or engines.get("socket_volumes_sha256")
        != check.get("engine_socket_volumes_sha256")
        or engines.get("data_volumes_sha256")
        != check.get("engine_data_volumes_sha256")
    ):
        raise ValueError(f"{label} Docker Engine runtime evidence is invalid")

    restart = mapping(runtime.get("restart_identity"), f"{label}.restart_identity")
    if restart != {
        "container_id": cp_container["container_id"],
        "container_name": cp_container["container_name"],
        "started_at": cp_container["started_at"],
        "image_id": cp_image["image_id"],
        "repo_digest": cp_image["repo_digest"],
    }:
        raise ValueError(f"{label} restart identity is inconsistent")


def validate_environment_sidecar_record(
    raw: Any,
    check: dict[str, Any],
    expected_commit: str,
    label: str,
) -> None:
    record = mapping(raw, label)
    if set(record) != {
        "sequence",
        "phase",
        "operation_round_index",
        "recorded_at_epoch_seconds",
        "observation",
    }:
        raise ValueError(f"{label} has unexpected fields")
    if (
        record.get("sequence") != check.get("sequence")
        or record.get("phase") != check.get("phase")
        or record.get("operation_round_index")
        != check.get("operation_round_index")
    ):
        raise ValueError(f"{label} does not match its report check")
    observation = mapping(record.get("observation"), f"{label}.observation")
    if set(observation) != {
        "schema_version",
        "candidate_sha",
        "started_at_epoch_seconds",
        "completed_at_epoch_seconds",
        "configuration_fingerprint_sha256",
        "observer_identity",
        "provenance_identity",
        "deployment_identity",
        "engine_evidence",
        "network_evidence",
        "runtime_evidence",
    }:
        raise ValueError(f"{label}.observation has unexpected fields")
    if observation.get("schema_version") != 1 or observation.get(
        "candidate_sha"
    ) != expected_commit:
        raise ValueError(f"{label}.observation candidate identity is invalid")
    if (
        number(observation.get("started_at_epoch_seconds"), "observer start")
        != number(check.get("started_at_epoch_seconds"), "check start")
        or number(observation.get("completed_at_epoch_seconds"), "observer completion")
        != number(check.get("completed_at_epoch_seconds"), "check completion")
    ):
        raise ValueError(f"{label}.observation timestamps do not match the report")
    recorded_at = number(record.get("recorded_at_epoch_seconds"), "recorded timestamp")
    completed_at = number(observation.get("completed_at_epoch_seconds"), "completion")
    if recorded_at < completed_at - MAX_LOCAL_CLOCK_SKEW_SECONDS or recorded_at > (
        completed_at + MAX_LOCAL_CLOCK_SKEW_SECONDS
    ):
        raise ValueError(f"{label} was not recorded contemporaneously")
    if (
        observation.get("configuration_fingerprint_sha256")
        != check.get("configuration_fingerprint_sha256")
    ):
        raise ValueError(f"{label} configuration fingerprint does not match")
    observer = mapping(observation.get("observer_identity"), f"{label}.observer_identity")
    if require_sha256(
        hashlib.sha256(
            json.dumps(observer, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest(),
        f"{label} observer identity hash",
    ) != check.get("observer_identity_sha256"):
        raise ValueError(f"{label} observer identity does not match")
    provenance = mapping(
        observation.get("provenance_identity"), f"{label}.provenance_identity"
    )
    for observation_key, check_key in (
        ("record_sha256", "provenance_record_sha256"),
        ("source_workflow_run_id", "image_workflow_run_id"),
        ("control_plane_reference", "control_plane_image"),
        ("agent_reference", "agent_image"),
        ("fixture_reference", "provenance_fixture_image"),
    ):
        if provenance.get(observation_key) != check.get(check_key):
            raise ValueError(f"{label} provenance identity does not match")
    deployment = mapping(
        observation.get("deployment_identity"), f"{label}.deployment_identity"
    )
    for key, check_key in (
        ("control_plane_origin_sha256", "control_plane_origin_sha256"),
        ("restart_argv_sha256", "restart_argv_sha256"),
        ("topology_id", "topology_id"),
        ("topology_revision_id", "topology_revision_id"),
        ("topology_identity_sha256", "topology_identity_sha256"),
    ):
        if deployment.get(key) != check.get(check_key):
            raise ValueError(f"{label} deployment identity does not match")
    runtime = mapping(observation.get("runtime_evidence"), f"{label}.runtime_evidence")
    validate_runtime_sidecar(runtime, check, expected_commit, label)
    control_plane = mapping(
        runtime.get("control_plane"), f"{label}.runtime_evidence.control_plane"
    )
    runtime_image = mapping(control_plane.get("image"), f"{label}.control_plane.image")
    runtime_container = mapping(
        control_plane.get("container"), f"{label}.control_plane.container"
    )
    agents = mapping(runtime.get("agents"), f"{label}.runtime_evidence.agents")
    agent_runtime_image = mapping(agents.get("image"), f"{label}.agents.image")
    for observed, expected, field in (
        (runtime.get("host_identity_sha256"), check.get("runtime_host_identity_sha256"), "host identity"),
        (runtime_image.get("image_id"), check.get("control_plane_image_id"), "control-plane image"),
        (runtime_container.get("container_id"), check.get("control_plane_container_id"), "control-plane container"),
        (runtime_container.get("started_at"), check.get("control_plane_started_at"), "control-plane StartedAt"),
        ((agent_runtime_image.get("image_ids") or [None])[0], check.get("agent_image_id"), "Agent image"),
        (agents.get("node_ids_sha256"), check.get("agent_node_ids_sha256"), "Agent Node set"),
        (agents.get("container_ids_sha256"), check.get("agent_container_ids_sha256"), "Agent container set"),
        (agents.get("started_at_sha256"), check.get("agent_started_at_sha256"), "Agent StartedAt set"),
    ):
        if observed != expected:
            raise ValueError(f"{label} {field} does not match")
    engine = mapping(observation.get("engine_evidence"), f"{label}.engine_evidence")
    if set(engine) != {
        "fixture_image",
        "worker_count",
        "engine_count",
        "container_count",
        "running_containers",
        "healthy_containers",
        "oldest_worker_observed_at_epoch_seconds",
        "newest_worker_observed_at_epoch_seconds",
        "worker_collection_spread_seconds",
        "aggregate_sha256",
        "node_ids_sha256",
        "deployment_ids_sha256",
        "container_ids_sha256",
    }:
        raise ValueError(f"{label}.engine_evidence has unexpected fields")
    network = mapping(
        observation.get("network_evidence"), f"{label}.network_evidence"
    )
    if set(network) != {
        "checked_at_epoch_seconds",
        "endpoint_checks_total",
        "endpoint_checks_healthy",
        "endpoint_checks_failed",
        "link_probes_total",
        "link_probes_healthy",
        "link_probes_failed",
        "drift",
        "endpoint_ids_sha256",
        "link_ids_sha256",
    }:
        raise ValueError(f"{label}.network_evidence has unexpected fields")
    field_map = {
        "fixture_image": engine.get("fixture_image"),
        "aggregate_sha256": engine.get("aggregate_sha256"),
        "node_ids_sha256": engine.get("node_ids_sha256"),
        "deployment_ids_sha256": engine.get("deployment_ids_sha256"),
        "container_ids_sha256": engine.get("container_ids_sha256"),
        "endpoint_ids_sha256": network.get("endpoint_ids_sha256"),
        "link_ids_sha256": network.get("link_ids_sha256"),
        "workers": engine.get("worker_count"),
        "engines": engine.get("engine_count"),
        "containers": engine.get("container_count"),
        "running_containers": engine.get("running_containers"),
        "healthy_containers": engine.get("healthy_containers"),
        "endpoint_checks_total": network.get("endpoint_checks_total"),
        "endpoint_checks_healthy": network.get("endpoint_checks_healthy"),
        "endpoint_checks_failed": network.get("endpoint_checks_failed"),
        "link_probes_total": network.get("link_probes_total"),
        "link_probes_healthy": network.get("link_probes_healthy"),
        "link_probes_failed": network.get("link_probes_failed"),
        "drift": network.get("drift"),
    }
    if any(check.get(key) != value for key, value in field_map.items()):
        raise ValueError(f"{label} Engine/network result does not match the report")
    oldest = number(
        engine.get("oldest_worker_observed_at_epoch_seconds"), "oldest worker"
    )
    newest = number(
        engine.get("newest_worker_observed_at_epoch_seconds"), "newest worker"
    )
    spread = number(engine.get("worker_collection_spread_seconds"), "worker spread")
    if newest < oldest or spread > 90 or abs((newest - oldest) - spread) > 1.0:
        raise ValueError(f"{label} Engine collection window is invalid")
    checked_at = number(network.get("checked_at_epoch_seconds"), "network check")
    started_at = number(observation.get("started_at_epoch_seconds"), "start")
    if oldest < started_at - 30 or newest > completed_at + 30:
        raise ValueError(f"{label} Engine observations are outside the helper window")
    if checked_at < started_at - 30 or checked_at > completed_at + 30:
        raise ValueError(f"{label} network observation is outside the helper window")


def validate(
    report: dict[str, Any],
    expected_commit: str,
    evidence_directory: Path | None = None,
) -> list[str]:
    failures: list[str] = []
    if not COMMIT_SHA_PATTERN.fullmatch(expected_commit):
        return ["expected commit must be 40 lowercase hex characters"]
    if report.get("schema_version") != REPORT_SCHEMA_VERSION:
        failures.append("capacity report schema_version must be 2")
    if report.get("profile") != "production":
        failures.append("profile must be production")
    reported_failures = report.get("failures")
    if not isinstance(reported_failures, list) or reported_failures:
        failures.append(f"capacity report contains failures: {reported_failures!r}")

    try:
        expected = mapping(report.get("expected"), "expected")
        observed = mapping(report.get("observed"), "observed")
        for name, minimum in MINIMUMS.items():
            if number(expected.get(name), f"expected.{name}") < minimum:
                failures.append(f"expected.{name} is below {minimum}")
            if number(observed.get(name), f"observed.{name}") < minimum:
                failures.append(f"observed.{name} is below {minimum}")
    except ValueError as error:
        failures.append(str(error))

    try:
        thresholds = mapping(report.get("thresholds_ms"), "thresholds_ms")
        measurements = mapping(report.get("measurements_ms"), "measurements_ms")
        for name, maximum in MAX_THRESHOLDS_MS.items():
            if number(thresholds.get(name), f"thresholds_ms.{name}") > maximum:
                failures.append(f"thresholds_ms.{name} is weaker than {maximum}")
            if number(measurements.get(name), f"measurements_ms.{name}") > maximum:
                failures.append(f"measurements_ms.{name} exceeds {maximum}")
    except ValueError as error:
        failures.append(str(error))

    validate_identity(report, expected_commit, failures)
    validate_runner_service_evidence(report, failures)
    validate_configuration(report, failures)
    validate_checkpoint_history(report, failures)

    try:
        evidence = mapping(report.get("evidence"), "evidence")
        if evidence.get("source_commit") != expected_commit:
            failures.append("evidence source_commit does not match the candidate commit")
        if number(evidence.get("soak_seconds_requested"), "soak request") < MINIMUM_SOAK_SECONDS:
            failures.append("production soak request was shorter than 24 hours")
        if number(evidence.get("soak_elapsed_seconds"), "soak elapsed") < MINIMUM_SOAK_SECONDS:
            failures.append("observed production soak was shorter than 24 hours")
        if number(evidence.get("warmup_seconds"), "warmup seconds") != MINIMUM_WARMUP_SECONDS:
            failures.append("production warmup request must be exactly 600 seconds")
        if number(
            evidence.get("warmup_elapsed_seconds"), "warmup elapsed"
        ) < MINIMUM_WARMUP_SECONDS:
            failures.append("observed production warmup was shorter than 600 seconds")
        if number(evidence.get("sample_seconds"), "sample seconds") != SAMPLE_SECONDS:
            failures.append("production sample interval was not 30 seconds")
        if number(evidence.get("operation_interval_seconds"), "Operation interval") != OPERATION_INTERVAL_SECONDS:
            failures.append("production Operation interval was not 300 seconds")
        if number(evidence.get("permanent_running_seconds"), "permanent state limit") > 300:
            failures.append("production permanent-state limit was weaker than 300 seconds")
        if number(evidence.get("valid_soak_samples"), "valid soak samples") < MINIMUM_VALID_SAMPLES:
            failures.append("evidence summary contains too few valid samples")
        if number(evidence.get("max_observed_sample_gap_seconds"), "sample gap") > MAX_SAMPLE_GAP_SECONDS:
            failures.append("evidence summary contains a sample gap over 90 seconds")
        if number(
            evidence.get("max_observed_global_sample_gap_seconds"),
            "global sample gap",
        ) > MAX_SAMPLE_GAP_SECONDS:
            failures.append("evidence summary contains a global sample gap over 90 seconds")
        if number(evidence.get("soak_operation_rounds"), "soak Operation rounds") < MINIMUM_OPERATION_ROUNDS:
            failures.append("evidence summary contains too few Operation rounds")
        if evidence.get("restart_triggered") is not True:
            failures.append("production evidence did not trigger a real control-plane restart")
        if evidence.get("restart_unavailable_observed") is not True:
            failures.append("control-plane readiness never became unavailable during restart")
        if evidence.get("restart_probe_recovered") is not True:
            failures.append("the durable restart probe Operation was not recovered")
        if not evidence.get("restart_probe_operation_id"):
            failures.append("production evidence has no restart probe Operation id")
        if number(evidence.get("token_refresh_count"), "token refresh count") < 2:
            failures.append("production evidence did not prove OIDC token refresh")
        if not evidence.get("completed_at") or not evidence.get("checkpointed_at"):
            failures.append("capacity evidence has no completion/checkpoint timestamp")
        validate_restart_probe_evidence(evidence, failures)
    except ValueError as error:
        failures.append(str(error))

    try:
        process = mapping(report.get("process"), "process")
        baseline_rss = number(process.get("baseline_rss_bytes"), "baseline RSS")
        max_rss = number(process.get("max_rss_bytes"), "max RSS")
        baseline_threads = number(process.get("baseline_threads"), "baseline threads")
        max_threads = number(process.get("max_threads"), "max threads")
        baseline_pool = number(process.get("baseline_pool_connections"), "baseline pool")
        max_pool = number(process.get("max_pool_connections"), "max pool")
        baseline_active = number(process.get("baseline_active_requests"), "baseline active requests")
        max_active = number(process.get("max_active_requests"), "max active requests")
        if baseline_rss <= 0 or max_rss > baseline_rss * 1.10:
            failures.append("RSS evidence is missing or exceeds 10% growth")
        if baseline_threads <= 0 or max_threads > baseline_threads + 2:
            failures.append("thread evidence is missing or exceeds the allowed growth")
        if baseline_pool <= 0 or max_pool > baseline_pool + 2:
            failures.append("connection-pool evidence is missing or exceeds the allowed growth")
        if baseline_active < 0 or max_active > baseline_active + 2:
            failures.append("active-request evidence exceeds the allowed growth")
    except ValueError as error:
        failures.append(str(error))

    sample_summary = validate_samples(report, failures)
    validate_inventory(report, failures)
    validate_operation_rounds(report, failures)
    environment_checks = validate_environment_checks(report, failures)
    validate_unified_timeline(report, failures)

    try:
        logs = mapping(report.get("logs"), "logs")
        index = sequence(logs.get("index"), "logs.index")
        if len(index) != 3:
            failures.append("capacity evidence log index must contain exactly three sidecars")
        kinds = {
            str(entry.get("kind")) for entry in index if isinstance(entry, dict)
        }
        for required_kind in (
            "capacity_events_ndjson",
            "prometheus_snapshots_ndjson",
            "environment_observations_ndjson",
        ):
            if required_kind not in kinds:
                failures.append(f"capacity evidence is missing {required_kind}")
        for position, raw in enumerate(index):
            entry = mapping(raw, f"logs.index[{position}]")
            nonempty_string(entry.get("path"), "log path")
            digest = nonempty_string(entry.get("sha256"), "log sha256")
            if not re.fullmatch(r"[0-9a-f]{64}", digest):
                failures.append(f"logs.index[{position}] has an invalid SHA-256")
            if number(entry.get("bytes"), "log bytes") <= 0:
                failures.append(f"logs.index[{position}] is empty")
            kind = entry.get("kind")
            records = number(entry.get("records"), "log records")
            if kind in {
                "capacity_events_ndjson",
                "prometheus_snapshots_ndjson",
            } and records < MINIMUM_VALID_SAMPLES:
                failures.append(f"logs.index[{position}] contains too few records")
            if kind == "environment_observations_ndjson" and records != len(
                environment_checks
            ):
                failures.append(
                    "environment observation record count does not match the report"
                )
            if kind == "prometheus_snapshots_ndjson":
                sample_count = len(sequence(report.get("samples"), "samples"))
                if number(entry.get("records"), "Prometheus records") != sample_count:
                    failures.append(
                        "Prometheus snapshot record count does not match report samples"
                    )
            if evidence_directory is not None:
                relative = Path(entry["path"])
                if relative.is_absolute() or ".." in relative.parts:
                    failures.append(f"logs.index[{position}] contains an unsafe path")
                    continue
                log_path = evidence_directory / relative
                if not log_path.is_file():
                    failures.append(f"logs.index[{position}] file is missing")
                    continue
                raw_log = log_path.read_bytes()
                if len(raw_log) != int(number(entry.get("bytes"), "log bytes")):
                    failures.append(f"logs.index[{position}] byte count does not match")
                if hashlib.sha256(raw_log).hexdigest() != digest:
                    failures.append(f"logs.index[{position}] digest does not match")
                actual_records = sum(1 for line in raw_log.splitlines() if line.strip())
                if actual_records != int(number(entry.get("records"), "log records")):
                    failures.append(f"logs.index[{position}] record count does not match")
                if kind == "prometheus_snapshots_ndjson":
                    try:
                        snapshots = [
                            json.loads(line)
                            for line in raw_log.splitlines()
                            if line.strip()
                        ]
                    except json.JSONDecodeError:
                        failures.append("Prometheus snapshot log contains invalid JSON")
                        continue
                    validate_prometheus_sidecar(
                        snapshots, report, sample_summary, failures
                    )
                if kind == "environment_observations_ndjson":
                    try:
                        observations = [
                            json.loads(line)
                            for line in raw_log.splitlines()
                            if line.strip()
                        ]
                    except json.JSONDecodeError:
                        failures.append(
                            "environment observation log contains invalid JSON"
                        )
                        continue
                    if len(observations) != len(environment_checks):
                        failures.append(
                            "environment observation log does not match report cardinality"
                        )
                        continue
                    for observation_index, (observation, check) in enumerate(
                        zip(observations, environment_checks)
                    ):
                        try:
                            validate_environment_sidecar_record(
                                observation,
                                check,
                                expected_commit,
                                f"environment log record {observation_index + 1}",
                            )
                            observer = mapping(
                                mapping(
                                    mapping(
                                        observation,
                                        f"environment log record {observation_index + 1}",
                                    ).get("observation"),
                                    "environment observation",
                                ).get("observer_identity"),
                                "environment observer identity",
                            )
                            redacted = mapping(
                                mapping(
                                    report.get("configuration"), "configuration"
                                ).get("redacted"),
                                "configuration.redacted",
                            )
                            if observer.get("program_sha256") != redacted.get(
                                "environment_observer_program_sha256"
                            ):
                                raise ValueError(
                                    "environment observer program does not match redacted configuration"
                                )
                        except ValueError as error:
                            failures.append(str(error))
                            break
    except ValueError as error:
        failures.append(str(error))
    return failures


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("report", type=Path)
    parser.add_argument("--expected-commit", required=True)
    args = parser.parse_args()
    report = json.loads(args.report.read_text(encoding="utf-8"))
    failures = validate(
        mapping(report, "report"), args.expected_commit, args.report.parent
    )
    if failures:
        for failure in failures:
            print(f"GA evidence rejected: {failure}", file=sys.stderr)
        return 1
    print(f"GA capacity evidence accepted for {args.expected_commit}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
