from __future__ import annotations

import importlib.util
import json
import pathlib
import tempfile
import types
import unittest


MODULE_PATH = pathlib.Path(__file__).parents[1] / "verify-orchestrator-image-provenance.py"
SPEC = importlib.util.spec_from_file_location("verify_image_provenance", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)

CANDIDATE = "a" * 40
RUN_ID = "123"
REPOSITORY = "Owner/repo"


def write_evidence(root: pathlib.Path) -> types.SimpleNamespace:
    subjects: dict[str, dict[str, str]] = {}
    for index, component in enumerate(
        ("control-plane", "agent", "capacity-fixture"), start=1
    ):
        image = f"ghcr.io/owner/{VERIFY.IMAGE_NAMES[component]}"
        digest = f"sha256:{index:064x}"
        identity = {
            "schema_version": 2,
            "component": component,
            "image": image,
            "digest": digest,
            "reference": f"{image}@{digest}",
            "commit_sha": CANDIDATE,
            "workflow_run_id": RUN_ID,
            "workflow_run_attempt": "1",
            "repository": REPOSITORY,
            "workflow_file": VERIFY.WORKFLOW,
        }
        directory = root / f"orchestrator-candidate-image-{component}"
        directory.mkdir(parents=True)
        (directory / f"{component}.json").write_text(
            json.dumps(identity), encoding="utf-8"
        )
        subjects[component] = {
            "reference": identity["reference"],
            "digest": digest,
        }
    provenance = root / "orchestrator-candidate-image-provenance"
    provenance.mkdir()
    (provenance / "candidate-image-provenance.json").write_text(
        json.dumps(
            {
                "schema_version": 1,
                "candidate_sha": CANDIDATE,
                "repository": REPOSITORY,
                "source_workflow": VERIFY.WORKFLOW,
                "source_workflow_run_id": RUN_ID,
                "source_workflow_run_attempt": 1,
                "github_oidc_issuer": "https://token.actions.githubusercontent.com",
                "control_plane": subjects["control-plane"],
                "agent": subjects["agent"],
                "capacity_fixture": subjects["capacity-fixture"],
            }
        ),
        encoding="utf-8",
    )
    return types.SimpleNamespace(
        root=root,
        candidate_sha=CANDIDATE,
        repository=REPOSITORY,
        workflow_run_id=RUN_ID,
    )


class ImageProvenanceTests(unittest.TestCase):
    def test_binds_exact_three_oci_subjects_and_combined_record(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            args = write_evidence(pathlib.Path(directory))
            values = VERIFY.validate(args)
            self.assertEqual(
                set(values),
                {
                    "ORCHESTRATOR_GATE_CONTROL_PLANE_IMAGE",
                    "ORCHESTRATOR_GATE_AGENT_IMAGE",
                    "ORCHESTRATOR_GATE_FIXTURE_IMAGE",
                    "ORCHESTRATOR_GATE_IMAGE_WORKFLOW_RUN_ID",
                    "ORCHESTRATOR_GATE_IMAGE_PROVENANCE_RECORD_SHA256",
                },
            )
            self.assertEqual(values["ORCHESTRATOR_GATE_IMAGE_WORKFLOW_RUN_ID"], RUN_ID)

    def test_rejects_extra_identity_fields_wrong_subject_and_record_tampering(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            args = write_evidence(root)
            identity_path = (
                root
                / "orchestrator-candidate-image-agent"
                / "agent.json"
            )
            identity = json.loads(identity_path.read_text(encoding="utf-8"))
            identity["untrusted"] = True
            identity_path.write_text(json.dumps(identity), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "selected run"):
                VERIFY.validate(args)

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            args = write_evidence(root)
            identity_path = (
                root
                / "orchestrator-candidate-image-agent"
                / "agent.json"
            )
            identity = json.loads(identity_path.read_text(encoding="utf-8"))
            identity["image"] = "ghcr.io/owner/unrelated"
            identity["reference"] = f"{identity['image']}@{identity['digest']}"
            identity_path.write_text(json.dumps(identity), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "invalid OCI subject"):
                VERIFY.validate(args)

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            args = write_evidence(root)
            record_path = (
                root
                / "orchestrator-candidate-image-provenance"
                / "candidate-image-provenance.json"
            )
            record = json.loads(record_path.read_text(encoding="utf-8"))
            record["agent"] = record["control_plane"]
            record_path.write_text(json.dumps(record), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "does not bind agent"):
                VERIFY.validate(args)


if __name__ == "__main__":
    unittest.main()
