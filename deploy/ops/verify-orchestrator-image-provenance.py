#!/usr/bin/env python3
"""Validate downloaded candidate-image identities and export their exact subjects."""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import shutil
from typing import Any


DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
SHA40 = re.compile(r"^[0-9a-f]{40}$")
RUN_ID = re.compile(r"^[1-9][0-9]*$")
REPOSITORY = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
WORKFLOW = ".github/workflows/orchestrator-candidate-images.yml"
IDENTITY_KEYS = {
    "schema_version",
    "component",
    "image",
    "digest",
    "reference",
    "commit_sha",
    "workflow_run_id",
    "workflow_run_attempt",
    "repository",
    "workflow_file",
}
IMAGE_NAMES = {
    "control-plane": "ojos-orchestrator-control-plane",
    "agent": "ojos-orchestrator-agent",
    "capacity-fixture": "ojos-orchestrator-capacity-fixture",
}


def load(path: pathlib.Path) -> dict[str, Any]:
    raw = path.read_bytes()
    if len(raw) > 1_048_576:
        raise ValueError(f"oversized candidate identity: {path}")
    value = json.loads(raw)
    if not isinstance(value, dict):
        raise ValueError(f"candidate identity is not an object: {path}")
    return value


def validate(args: argparse.Namespace) -> dict[str, str]:
    if (
        not SHA40.fullmatch(args.candidate_sha)
        or not RUN_ID.fullmatch(args.workflow_run_id)
        or not REPOSITORY.fullmatch(args.repository)
    ):
        raise ValueError("candidate SHA, repository, or source workflow run ID is invalid")
    owner = args.repository.split("/", 1)[0].lower()
    identities: dict[str, dict[str, Any]] = {}
    for component in ("control-plane", "agent", "capacity-fixture"):
        identity = load(
            args.root
            / f"orchestrator-candidate-image-{component}"
            / f"{component}.json"
        )
        expected = {
            "schema_version": 2,
            "component": component,
            "commit_sha": args.candidate_sha,
            "workflow_run_id": args.workflow_run_id,
            "workflow_run_attempt": "1",
            "repository": args.repository,
            "workflow_file": WORKFLOW,
        }
        if set(identity) != IDENTITY_KEYS or any(
            identity.get(key) != value for key, value in expected.items()
        ):
            raise ValueError(f"{component} identity does not match the selected run")
        digest = identity.get("digest")
        image = identity.get("image")
        expected_image = f"ghcr.io/{owner}/{IMAGE_NAMES[component]}"
        if (
            not isinstance(digest, str)
            or not DIGEST.fullmatch(digest)
            or not isinstance(image, str)
            or image != expected_image
            or identity.get("reference") != f"{image}@{digest}"
        ):
            raise ValueError(f"{component} identity has an invalid OCI subject")
        identities[component] = identity

    record_path = (
        args.root
        / "orchestrator-candidate-image-provenance"
        / "candidate-image-provenance.json"
    )
    raw = record_path.read_bytes()
    if len(raw) > 1_048_576:
        raise ValueError("candidate provenance record is oversized")
    record = json.loads(raw)
    expected_keys = {
        "schema_version",
        "candidate_sha",
        "repository",
        "source_workflow",
        "source_workflow_run_id",
        "source_workflow_run_attempt",
        "github_oidc_issuer",
        "control_plane",
        "agent",
        "capacity_fixture",
    }
    if (
        not isinstance(record, dict)
        or set(record) != expected_keys
        or record.get("schema_version") != 1
        or record.get("candidate_sha") != args.candidate_sha
        or record.get("repository") != args.repository
        or record.get("source_workflow") != WORKFLOW
        or record.get("source_workflow_run_id") != args.workflow_run_id
        or record.get("source_workflow_run_attempt") != 1
        or record.get("github_oidc_issuer")
        != "https://token.actions.githubusercontent.com"
    ):
        raise ValueError("candidate provenance record does not match the selected run")
    for component, record_key in (
        ("control-plane", "control_plane"),
        ("agent", "agent"),
        ("capacity-fixture", "capacity_fixture"),
    ):
        identity = identities[component]
        if record.get(record_key) != {
            "reference": identity["reference"],
            "digest": identity["digest"],
        }:
            raise ValueError(f"provenance record does not bind {component}")
    return {
        "ORCHESTRATOR_GATE_CONTROL_PLANE_IMAGE": identities["control-plane"][
            "reference"
        ],
        "ORCHESTRATOR_GATE_AGENT_IMAGE": identities["agent"]["reference"],
        "ORCHESTRATOR_GATE_FIXTURE_IMAGE": identities["capacity-fixture"][
            "reference"
        ],
        "ORCHESTRATOR_GATE_IMAGE_WORKFLOW_RUN_ID": args.workflow_run_id,
        "ORCHESTRATOR_GATE_IMAGE_PROVENANCE_RECORD_SHA256": hashlib.sha256(
            raw
        ).hexdigest(),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=pathlib.Path, required=True)
    parser.add_argument("--candidate-sha", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--workflow-run-id", required=True)
    parser.add_argument("--github-env", type=pathlib.Path, required=True)
    parser.add_argument("--record-output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    values = validate(args)
    args.record_output.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(
        args.root
        / "orchestrator-candidate-image-provenance"
        / "candidate-image-provenance.json",
        args.record_output,
    )
    with args.github_env.open("a", encoding="utf-8", newline="\n") as output:
        for name, value in values.items():
            if "\n" in value or "\r" in value:
                raise ValueError("candidate identity contains a line break")
            output.write(f"{name}={value}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
