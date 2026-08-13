from __future__ import annotations

import importlib.util
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("service_ci_matrix.py")
SPEC = importlib.util.spec_from_file_location("service_ci_matrix", SCRIPT)
assert SPEC and SPEC.loader
MATRIX = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MATRIX)


def write(path: Path, text: str = "fixture\n") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def write_json(path: Path, value: object) -> None:
    write(path, json.dumps(value, sort_keys=True, separators=(",", ":")))


def create_service(
    root: Path,
    service_id: str,
    *,
    migration: bool = False,
    frontend: bool = False,
) -> Path:
    service = root / "services" / service_id
    manifest = (
        "apiVersion: ojos.dev/v1\n"
        "kind: Service\n"
        f"metadata:\n  id: {service_id}\n  version: 0.1.0\n"
        "runtime:\n"
        "  profile: standard-container-v1\n"
        f"  artifact: {service_id}-runtime\n"
    )
    if migration:
        manifest += (
            "migrations:\n"
            f"  - id: {service_id}-schema-v1\n"
            f"    artifact: {service_id}-migration\n"
        )
    if frontend:
        manifest += (
            "frontends:\n"
            "  - target: user-shell\n"
            "    manifest: frontend/user/manifest.json\n"
        )
    write(service / "ojos.service.yaml", manifest)
    write(service / "Dockerfile", "FROM scratch\n")
    write(service / "gen" / "go" / "go.mod", f"module example.invalid/{service_id}\n")
    write(service / "gen" / "rust" / "Cargo.toml", "[package]\nname='fixture'\nversion='0.1.0'\n")
    write_json(service / "gen" / "ts" / "package.json", {"name": service_id})
    for name in ("resolved-artifacts-fixture.ps1",):
        write(service / "scripts" / name)

    runtime_artifact = f"{service_id}-runtime"
    migrations = []
    roles = [
        {"role": "runtime", "slot": runtime_artifact},
        {"role": "contract", "slot": "contract"},
        {"role": "sbom", "slot": "sbom"},
        {"role": "provenance", "slot": "provenance"},
    ]
    if migration:
        write(service / "migrations" / "Dockerfile", "FROM scratch\n")
        migrations.append(
            {"id": f"{service_id}-schema-v1", "artifact": f"{service_id}-migration"}
        )
        roles.append(
            {
                "role": f"migration:{service_id}-schema-v1",
                "slot": f"{service_id}-migration",
            }
        )
    frontends = []
    if frontend:
        manifest = {
            "schemaVersion": "ojos.frontend/v1",
            "moduleId": f"{service_id}.user",
            "target": "user-shell",
            "artifact": f"{service_id}-frontend",
            "hostApiRange": "^1.0",
            "routes": [],
        }
        write_json(service / "frontend" / "user" / "manifest.json", manifest)
        write(service / "frontend" / "user" / "bundle.js", "export default {};\n")
        frontends.append(
            {
                "target": "user-shell",
                "manifest": {"path": "frontend/user/manifest.json"},
                "module": manifest,
            }
        )
        roles.append(
            {
                "role": f"frontend-bundle:{service_id}.user",
                "slot": f"{service_id}-frontend",
            }
        )
    contract = {
        "schemaVersion": "ojos.dev/service-contract/v3",
        "serviceId": service_id,
        "serviceVersion": "0.1.0",
        "runtime": {"artifact": runtime_artifact},
        "migrations": migrations,
        "frontends": frontends,
    }
    contract_path = service / "gen" / "service.contract.json"
    write_json(contract_path, contract)
    contract_digest = f"sha256:{hashlib.sha256(contract_path.read_bytes()).hexdigest()}"
    write_json(
        service / "gen" / "build-input.json",
        {
            "schemaVersion": "ojos.dev/build-input/v1",
            "serviceId": service_id,
            "serviceVersion": "0.1.0",
            "contractDigest": contract_digest,
            "artifactRequirements": roles,
        },
    )
    return service


class ServiceCiMatrixTests(unittest.TestCase):
    def fixture(self) -> Path:
        root = Path(tempfile.mkdtemp(prefix="ojos-service-ci-"))
        self.addCleanup(shutil.rmtree, root, ignore_errors=True)
        write(
            root / ".github" / "toolchains.env",
            "GO_VERSION=1.26.5\nRUST_VERSION=1.92.0\nNODE_VERSION=24.11.0\n"
            "TYPESCRIPT_VERSION=5.9.2\nGOVULNCHECK_VERSION=v1.1.4\n"
            "CARGO_AUDIT_VERSION=0.22.2\n",
        )
        write(root / ".github" / "scripts" / "service_publish_gate.py")
        write(root / ".github" / "scripts" / "test_service_publish_gate.py")
        create_service(root, "alpha", migration=True, frontend=True)
        create_service(root, "zeta")
        return root

    def test_discovers_sorted_services_and_capabilities(self) -> None:
        matrix = MATRIX.render_matrix(self.fixture())
        services = matrix["services"]["include"]
        self.assertEqual([item["service"] for item in services], ["alpha", "zeta"])
        self.assertEqual(matrix["serviceCount"], 2)
        self.assertEqual(
            [item["service"] for item in matrix["images"]["include"]],
            ["alpha", "zeta"],
        )
        self.assertEqual(
            [item["service"] for item in matrix["publish"]["include"]],
            ["alpha", "zeta"],
        )
        self.assertTrue(services[0]["hasGo"])
        self.assertTrue(services[0]["hasRust"])
        self.assertTrue(services[0]["hasNode"])
        self.assertEqual(
            services[0]["dockerfiles"],
            ["services/alpha/Dockerfile", "services/alpha/migrations/Dockerfile"],
        )

    def test_service_change_selects_only_that_service(self) -> None:
        matrix = MATRIX.render_matrix(self.fixture(), ["services/zeta/internal/service.go"])
        self.assertFalse(matrix["globalChange"])
        self.assertEqual(
            [item["service"] for item in matrix["affected"]["include"]], ["zeta"]
        )

    def test_compiler_change_selects_all_services(self) -> None:
        matrix = MATRIX.render_matrix(self.fixture(), ["tools/ojos-service/src/lib.rs"])
        self.assertTrue(matrix["globalChange"])
        self.assertEqual(matrix["affectedCount"], 2)

    def test_unrelated_change_selects_no_services(self) -> None:
        matrix = MATRIX.render_matrix(self.fixture(), ["README.md"])
        self.assertEqual(matrix["affectedCount"], 0)
        self.assertEqual(matrix["affected"], {"include": []})

    def test_deleted_manifest_cannot_silently_drop_a_service(self) -> None:
        root = self.fixture()
        (root / "services" / "alpha" / "ojos.service.yaml").unlink()
        with self.assertRaises(MATRIX.DiscoveryError):
            MATRIX.render_matrix(
                root, ["services/alpha/ojos.service.yaml"]
            )

    def test_deleted_service_directory_cannot_silently_empty_the_matrix(self) -> None:
        root = self.fixture()
        shutil.rmtree(root / "services" / "alpha")
        with self.assertRaisesRegex(MATRIX.DiscoveryError, "changed service manifests"):
            MATRIX.render_matrix(root, ["services/alpha/ojos.service.yaml"])

    def test_github_outputs_are_compact_json(self) -> None:
        root = self.fixture()
        output = root / "github-output.txt"
        matrix = MATRIX.render_matrix(root)
        MATRIX.write_github_outputs(output, matrix)
        values = dict(line.split("=", 1) for line in output.read_text().splitlines())
        self.assertEqual(values["service_count"], "2")
        self.assertEqual(len(json.loads(values["go_matrix"])["include"]), 2)

    def test_toolchain_file_requires_every_pin(self) -> None:
        root = self.fixture()
        values = MATRIX.validate_toolchains(root / ".github" / "toolchains.env")
        self.assertEqual(values["RUST_VERSION"], "1.92.0")
        write(root / ".github" / "toolchains.env", "GO_VERSION=1.26.5\n")
        with self.assertRaises(MATRIX.DiscoveryError):
            MATRIX.validate_toolchains(root / ".github" / "toolchains.env")

    def test_runtime_dockerfile_is_required_and_pseudo_files_are_ignored(self) -> None:
        root = self.fixture()
        service = root / "services" / "zeta"
        (service / "Dockerfile").unlink()
        write(service / "node_modules" / "dependency" / "Dockerfile")
        write(service / "gen" / "target" / "Dockerfile.fake")
        with self.assertRaisesRegex(MATRIX.DiscoveryError, "runtime Dockerfile"):
            MATRIX.render_matrix(root)

    def test_declared_migration_requires_exact_dockerfile(self) -> None:
        root = self.fixture()
        (root / "services" / "alpha" / "migrations" / "Dockerfile").unlink()
        with self.assertRaisesRegex(MATRIX.DiscoveryError, "migration Dockerfile"):
            MATRIX.render_matrix(root)

    def test_source_declared_migration_cannot_hide_behind_stale_generated_contract(self) -> None:
        root = self.fixture()
        manifest = root / "services" / "zeta" / "ojos.service.yaml"
        manifest.write_text(
            manifest.read_text(encoding="utf-8")
            + "migrations:\n  - id: added-schema\n    artifact: added-migration\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(MATRIX.DiscoveryError, "migration Dockerfile"):
            MATRIX.render_matrix(root)

    def test_declared_frontend_requires_bundle(self) -> None:
        root = self.fixture()
        (root / "services" / "alpha" / "frontend" / "user" / "bundle.js").unlink()
        with self.assertRaisesRegex(MATRIX.DiscoveryError, "frontend bundle"):
            MATRIX.render_matrix(root)

    def test_source_declared_frontend_cannot_hide_behind_stale_generated_contract(self) -> None:
        root = self.fixture()
        service = root / "services" / "zeta"
        manifest = service / "ojos.service.yaml"
        manifest.write_text(
            manifest.read_text(encoding="utf-8")
            + "frontends:\n  - target: user-shell\n    manifest: frontend/user/manifest.json\n",
            encoding="utf-8",
        )
        write_json(
            service / "frontend" / "user" / "manifest.json",
            {"schemaVersion": "ojos.frontend/v1"},
        )
        with self.assertRaisesRegex(MATRIX.DiscoveryError, "frontend bundle"):
            MATRIX.render_matrix(root)

    def test_generated_build_input_and_publish_gates_are_required(self) -> None:
        root = self.fixture()
        build_input = root / "services" / "zeta" / "gen" / "build-input.json"
        build_input.unlink()
        with self.assertRaisesRegex(MATRIX.DiscoveryError, "generated build-input"):
            MATRIX.render_matrix(root)

        root = self.fixture()
        publish_gate = root / ".github" / "scripts" / "service_publish_gate.py"
        publish_gate.unlink()
        with self.assertRaisesRegex(MATRIX.DiscoveryError, "repository publish gate"):
            MATRIX.render_matrix(root)

    def test_build_input_must_bind_runtime_migration_and_frontend_slots(self) -> None:
        for role in (
            "runtime",
            "migration:alpha-schema-v1",
            "frontend-bundle:alpha.user",
        ):
            with self.subTest(role=role):
                root = self.fixture()
                path = root / "services" / "alpha" / "gen" / "build-input.json"
                value = json.loads(path.read_text(encoding="utf-8"))
                value["artifactRequirements"] = [
                    item for item in value["artifactRequirements"] if item["role"] != role
                ]
                write_json(path, value)
                with self.assertRaises(MATRIX.DiscoveryError):
                    MATRIX.render_matrix(root)

    def test_required_fan_in_allows_skips_only_for_empty_matrix(self) -> None:
        skipped = {name: "skipped" for name in MATRIX.MATRIX_GATE_NAMES}
        MATRIX.validate_required_results(0, "success", skipped)
        with self.assertRaises(MATRIX.DiscoveryError):
            MATRIX.validate_required_results(1, "success", skipped)

        success = {name: "success" for name in MATRIX.MATRIX_GATE_NAMES}
        MATRIX.validate_required_results(2, "success", success)
        for bad_result in ("skipped", "failure", "cancelled", ""):
            with self.subTest(result=bad_result):
                results = dict(success)
                results["publish"] = bad_result
                with self.assertRaises(MATRIX.DiscoveryError):
                    MATRIX.validate_required_results(2, "success", results)

    def test_required_fan_in_rejects_missing_gate_or_failed_discovery(self) -> None:
        results = {name: "success" for name in MATRIX.MATRIX_GATE_NAMES}
        results.pop("image")
        with self.assertRaises(MATRIX.DiscoveryError):
            MATRIX.validate_required_results(1, "success", results)
        with self.assertRaises(MATRIX.DiscoveryError):
            MATRIX.validate_required_results(
                1,
                "failure",
                {name: "success" for name in MATRIX.MATRIX_GATE_NAMES},
            )

    def test_workflow_cannot_conditionally_skip_publish_or_accept_partial_fan_in(self) -> None:
        workflow = SCRIPT.parent.parent / "workflows" / "service-contract-ci.yml"
        source = workflow.read_text(encoding="utf-8")
        self.assertNotIn("if: matrix.hasPublishFixture", source)
        self.assertIn("python3 .github/scripts/test_service_publish_gate.py", source)
        self.assertIn(
            "python3 .github/scripts/service_publish_gate.py",
            source,
        )
        self.assertIn('--service "${{ matrix.directory }}"', source)
        self.assertIn('--baseline-revision "$baseline_revision"', source)
        self.assertIn('baseline_revision="$PR_BASE_SHA"', source)
        self.assertIn('baseline_revision="$BEFORE_SHA"', source)
        self.assertIn('baseline_revision="$(git rev-parse HEAD^)"', source)
        publish_job = source.split("  publish-gate:\n", 1)[1].split("  required:\n", 1)[0]
        self.assertIn("fetch-depth: 0", publish_job)
        self.assertNotIn("publish-fixture.test.ps1", publish_job)
        self.assertNotIn("resolved-artifacts-fixture.test.ps1", publish_job)
        self.assertIn("--verify-required", source)
        required_job = source.split("  required:\n", 1)[1]
        self.assertIn("- uses: actions/checkout@v6", required_job)
        for gate in MATRIX.MATRIX_GATE_NAMES:
            self.assertIn(f'--gate-result "{gate}=$', source)
        self.assertNotIn("join(needs.*.result", source)


if __name__ == "__main__":
    unittest.main()
