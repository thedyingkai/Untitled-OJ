from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("service_publish_gate.py")
SPEC = importlib.util.spec_from_file_location("service_publish_gate", SCRIPT)
assert SPEC and SPEC.loader
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, sort_keys=True, separators=(",", ":")), encoding="utf-8"
    )


class ServicePublishGateTests(unittest.TestCase):
    def test_semver_and_same_version_baseline_policy_is_fail_closed(self) -> None:
        self.assertEqual(GATE.stable_version("1.2.3", "fixture"), (1, 2, 3))
        self.assertEqual(GATE.next_patch("1.2.3"), "1.2.4")
        for version in ("1.2", "v1.2.3", "1.2.3-rc.1", "01.2.3"):
            with self.subTest(version=version), self.assertRaises(GATE.PublishGateError):
                GATE.stable_version(version, "fixture")

    def test_baseline_revision_rejects_non_commit_input(self) -> None:
        root = Path(tempfile.mkdtemp(prefix="ojos-baseline-revision-"))
        self.addCleanup(shutil.rmtree, root, ignore_errors=True)
        for revision in ("", "HEAD", "-deadbeef", "abc/def"):
            with self.subTest(revision=revision), self.assertRaisesRegex(
                GATE.PublishGateError, "full hexadecimal commit id"
            ):
                GATE.baseline_revision(root, revision, root / "output")

    def test_base_tree_tampering_is_rejected_before_compatibility(self) -> None:
        root, paths = self.fixture()
        metadata = paths["catalog"] / "metadata" / "alpha-0.1.0.release.json"
        metadata.write_text(metadata.read_text(encoding="utf-8") + " ", encoding="utf-8")
        with self.assertRaisesRegex(GATE.PublishGateError, "metadata digest"):
            self.verify(paths)

        _root, paths = self.fixture()
        trust = paths["catalog"] / "trust.json"
        write_json(trust, {paths["key"]: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="})
        catalog = json.loads((paths["catalog"] / "catalog.json").read_text())
        self.assertNotEqual(
            trust.read_text(encoding="utf-8"),
            json.dumps(catalog["signatures"], sort_keys=True),
        )

    def test_compatibility_arguments_require_external_trust_and_version_progress(self) -> None:
        root, paths = self.fixture()
        baseline = root / "baseline"
        metadata = baseline / "metadata"
        shutil.copytree(paths["catalog"], baseline)
        trust = root / "operator-trust.json"
        shutil.copyfile(baseline / "trust.json", trust)
        baseline_contract = metadata / "alpha-0.1.0.service.contract.json"
        build = {"serviceVersion": "0.1.0"}

        args, manifest = GATE.compatibility_args(
            paths["service"], (baseline, trust, build), root / "scratch"
        )
        self.assertEqual(args, ["--previous-catalog", str(baseline), "--previous-trust", str(trust)])
        self.assertNotEqual(manifest.parent, paths["service"])
        self.assertIn("version: 0.1.1", manifest.read_text(encoding="utf-8"))

        current = json.loads((paths["service"] / "gen" / "service.contract.json").read_text())
        current["displayName"] = "tampered"
        write_json(paths["service"] / "gen" / "service.contract.json", current)
        with self.assertRaisesRegex(GATE.PublishGateError, "without increasing service version"):
            GATE.compatibility_args(
                paths["service"], (baseline, trust, build), root / "scratch-two"
            )

        current["serviceVersion"] = "0.0.9"
        write_json(paths["service"] / "gen" / "service.contract.json", current)
        with self.assertRaisesRegex(GATE.PublishGateError, "older than trusted baseline"):
            GATE.compatibility_args(
                paths["service"], (baseline, trust, build), root / "scratch-three"
            )

    def test_generic_resolver_covers_every_role_with_immutable_references(self) -> None:
        document = GATE.resolved_artifact_fixture(
            {
                "serviceId": "alpha",
                "artifactRequirements": [
                    {"role": "runtime", "slot": "alpha-runtime"},
                    {"role": "migration:schema-v1", "slot": "alpha-migration"},
                    {"role": "frontend-bundle:alpha.user", "slot": "alpha-frontend"},
                    {
                        "role": "contract",
                        "slot": "contract",
                        "expectedDigest": "sha256:" + "a" * 64,
                        "expectedSize": 42,
                    },
                    {"role": "sbom", "slot": "sbom"},
                    {"role": "provenance", "slot": "provenance"},
                ],
            }
        )
        self.assertEqual(
            set(document["artifacts"]),
            {
                "alpha-runtime",
                "alpha-migration",
                "alpha-frontend",
                "contract",
                "sbom",
                "provenance",
            },
        )
        self.assertRegex(
            document["artifacts"]["alpha-runtime"]["reference"],
            r"@sha256:[a-f0-9]{64}$",
        )
        self.assertRegex(
            document["artifacts"]["alpha-frontend"]["reference"],
            r"^https://fixture\.invalid/__ojos/extensions/[a-f0-9]{64}/bundle\.js$",
        )
        self.assertEqual(document["artifacts"]["contract"]["size"], 42)

    def fixture(self) -> tuple[Path, dict[str, object]]:
        root = Path(tempfile.mkdtemp(prefix="ojos-publish-verify-"))
        self.addCleanup(shutil.rmtree, root, ignore_errors=True)
        service = root / "services" / "alpha"
        (service / "ojos.service.yaml").parent.mkdir(parents=True, exist_ok=True)
        (service / "ojos.service.yaml").write_text(
            "apiVersion: ojos.dev/v1\n"
            "kind: Service\n"
            "metadata:\n"
            "  id: alpha\n"
            "  version: 0.1.0\n",
            encoding="utf-8",
        )
        contract = {
            "schemaVersion": "ojos.dev/service-contract/v3",
            "serviceId": "alpha",
            "serviceVersion": "0.1.0",
        }
        contract_path = service / "gen" / "service.contract.json"
        write_json(contract_path, contract)
        contract_digest = GATE.sha256_digest(contract_path)
        artifacts = {
            "contract": {
                "mediaType": "application/json",
                "digest": contract_digest,
                "size": contract_path.stat().st_size,
                "reference": f"https://fixture.invalid/{contract_digest[7:]}/contract",
            },
            "runtime": {
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": "sha256:" + "a" * 64,
                "size": 1,
                "reference": "example.invalid/runtime@sha256:" + "a" * 64,
            },
            "sbom": {
                "mediaType": "application/json",
                "digest": "sha256:" + "b" * 64,
                "size": 1,
                "reference": "https://fixture.invalid/" + "b" * 64 + "/sbom",
            },
            "provenance": {
                "mediaType": "application/json",
                "digest": "sha256:" + "c" * 64,
                "size": 1,
                "reference": "https://fixture.invalid/" + "c" * 64 + "/provenance",
            },
        }
        requirements = [
            {"role": role, "slot": slot}
            for role, slot in (
                ("contract", "contract"),
                ("runtime", "runtime"),
                ("sbom", "sbom"),
                ("provenance", "provenance"),
            )
        ]
        build_input = {
            "schemaVersion": "ojos.dev/build-input/v1",
            "serviceId": "alpha",
            "serviceVersion": "0.1.0",
            "contractDigest": contract_digest,
            "artifactRequirements": requirements,
        }
        build_path = service / "gen" / "build-input.json"
        write_json(build_path, build_input)
        resolved_path = root / "resolved.json"
        write_json(
            resolved_path,
            {"schemaVersion": "ojos.dev/resolved-artifacts/v1", "artifacts": artifacts},
        )
        lock = {
            "schemaVersion": "ojos.dev/release-lock/v1",
            "serviceId": "alpha",
            "serviceVersion": "0.1.0",
            "sourceDigest": "sha256:" + "d" * 64,
            "contractDigest": contract_digest,
            "artifacts": artifacts,
            "bindings": requirements,
        }
        lock_path = root / "release.lock.json"
        write_json(lock_path, lock)
        catalog_dir = root / "catalog"
        metadata_dir = catalog_dir / "metadata"
        published_lock = metadata_dir / "alpha-0.1.0.release.lock.json"
        published_contract = metadata_dir / "alpha-0.1.0.service.contract.json"
        metadata_path = metadata_dir / "alpha-0.1.0.release.json"
        published_lock.parent.mkdir(parents=True, exist_ok=True)
        published_lock.write_bytes(lock_path.read_bytes())
        published_contract.write_bytes(contract_path.read_bytes())
        lock_digest = GATE.sha256_digest(lock_path)
        subjects = [
            {"slot": slot, "roles": [role], **artifacts[slot]}
            for role, slot in (
                ("contract", "contract"),
                ("provenance", "provenance"),
                ("runtime", "runtime"),
                ("sbom", "sbom"),
            )
        ]
        metadata = {
            "platform": {
                "contractDigest": contract_digest,
                "releaseLockDigest": lock_digest,
                "artifactSubjects": subjects,
            }
        }
        write_json(metadata_path, metadata)
        key_id = "ci-alpha-ephemeral"
        catalog_id = "ci-alpha"
        catalog = {
            "schema_version": 2,
            "id": catalog_id,
            "modules": [
                {
                    "id": "alpha",
                    "releases": [
                        {
                            "version": "0.1.0",
                            "metadata": {
                                "sha256": GATE.sha256_digest(metadata_path),
                                "url": "https://fixture.invalid/metadata",
                            },
                        }
                    ],
                }
            ],
            "signatures": [
                {
                    "key_id": key_id,
                    "algorithm": "Ed25519",
                    "signature": "",
                }
            ],
        }
        import base64

        catalog["signatures"][0]["signature"] = base64.b64encode(b"s" * 64).decode()
        write_json(catalog_dir / "catalog.json", catalog)
        write_json(catalog_dir / "trust.json", {key_id: base64.b64encode(b"k" * 32).decode()})
        write_json(
            catalog_dir / "catalog-source.json",
            [{"id": catalog_id, "required_key_id": key_id}],
        )
        return root, {
            "service": service,
            "build": build_path,
            "resolved": resolved_path,
            "lock": lock_path,
            "catalog": catalog_dir,
            "key": key_id,
            "catalog_id": catalog_id,
        }

    def verify(self, paths: dict[str, object]) -> None:
        GATE.verify_publication(
            paths["service"],
            paths["build"],
            paths["resolved"],
            paths["lock"],
            paths["catalog"],
            paths["key"],
            paths["catalog_id"],
        )

    def test_accepts_complete_digest_bound_publication(self) -> None:
        _root, paths = self.fixture()
        self.verify(paths)

    def test_rejects_tampered_metadata_digest(self) -> None:
        _root, paths = self.fixture()
        metadata = paths["catalog"] / "metadata" / "alpha-0.1.0.release.json"
        metadata.write_text(metadata.read_text() + " ", encoding="utf-8")
        with self.assertRaisesRegex(GATE.PublishGateError, "metadata digest"):
            self.verify(paths)

    def test_rejects_artifact_subject_or_published_lock_substitution(self) -> None:
        for target in ("subjects", "lock"):
            with self.subTest(target=target):
                _root, paths = self.fixture()
                if target == "subjects":
                    metadata = paths["catalog"] / "metadata" / "alpha-0.1.0.release.json"
                    value = json.loads(metadata.read_text())
                    value["platform"]["artifactSubjects"][0]["digest"] = "sha256:" + "f" * 64
                    write_json(metadata, value)
                    catalog_path = paths["catalog"] / "catalog.json"
                    catalog = json.loads(catalog_path.read_text())
                    catalog["modules"][0]["releases"][0]["metadata"]["sha256"] = GATE.sha256_digest(metadata)
                    write_json(catalog_path, catalog)
                    message = "artifact subjects"
                else:
                    lock = paths["catalog"] / "metadata" / "alpha-0.1.0.release.lock.json"
                    lock.write_text(lock.read_text() + " ", encoding="utf-8")
                    message = "published release lock differs"
                with self.assertRaisesRegex(GATE.PublishGateError, message):
                    self.verify(paths)


if __name__ == "__main__":
    unittest.main()
