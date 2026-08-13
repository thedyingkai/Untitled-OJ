from __future__ import annotations

import pathlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[3]
WORKFLOW_PATH = ROOT / ".github" / "workflows" / "release.yml"
WORKFLOW = WORKFLOW_PATH.read_text(encoding="utf-8")
IMAGE_WORKFLOW_PATH = (
    ROOT / ".github" / "workflows" / "orchestrator-candidate-images.yml"
)
IMAGE_WORKFLOW = IMAGE_WORKFLOW_PATH.read_text(encoding="utf-8")
DOCKER_E2E_WORKFLOW_PATH = (
    ROOT / ".github" / "workflows" / "orchestrator-docker-e2e.yml"
)
DOCKER_E2E_WORKFLOW = DOCKER_E2E_WORKFLOW_PATH.read_text(encoding="utf-8")
COMPOSE_PATH = ROOT / "deploy" / "compose" / "docker-compose.yml"
COMPOSE = COMPOSE_PATH.read_text(encoding="utf-8")
COMPOSE_DEV = (
    ROOT / "deploy" / "compose" / "docker-compose.dev.yml"
).read_text(encoding="utf-8")
OPS_CI_POLICY = (ROOT / "deploy" / "ops" / "ci-policy.sh").read_text(
    encoding="utf-8"
)
TRACE_DRILL = (ROOT / "deploy" / "ops" / "trace-e2e-drill.sh").read_text(
    encoding="utf-8"
)
LOAD_DRILL = (ROOT / "deploy" / "ops" / "basic-load-soak.sh").read_text(
    encoding="utf-8"
)
CI_WORKFLOW_PATH = ROOT / ".github" / "workflows" / "orchestrator-ci.yml"
CI_WORKFLOW = CI_WORKFLOW_PATH.read_text(encoding="utf-8")
RELEASE_POLICY_PATH = ROOT / "docs" / "release" / "candidate-promotion.md"
RELEASE_POLICY = RELEASE_POLICY_PATH.read_text(encoding="utf-8")


def workflow_job(name: str, document: str = WORKFLOW) -> str:
    """Return one top-level job without depending on a third-party YAML parser."""
    match = re.search(rf"(?m)^  {re.escape(name)}:\s*$", document)
    if match is None:
        raise AssertionError(f"release workflow has no {name!r} job")
    following = re.search(r"(?m)^  [a-zA-Z0-9_-]+:\s*$", document[match.end() :])
    end = len(document) if following is None else match.end() + following.start()
    return document[match.start() : end]


def workflow_step(name: str, document: str) -> str:
    match = re.search(rf"(?m)^      - name: {re.escape(name)}\s*$", document)
    if match is None:
        raise AssertionError(f"workflow has no {name!r} step")
    following = re.search(r"(?m)^      - (?:name:|uses:)", document[match.end() :])
    end = len(document) if following is None else match.end() + following.start()
    return document[match.start() : end]


def assert_in_order(test: unittest.TestCase, body: str, *needles: str) -> None:
    positions = []
    for needle in needles:
        position = body.find(needle)
        test.assertNotEqual(position, -1, f"release policy is missing {needle!r}")
        positions.append(position)
    test.assertEqual(
        positions,
        sorted(positions),
        f"release policy order is wrong for {needles!r}",
    )


class ReleaseWorkflowPolicyTests(unittest.TestCase):
    def test_docker_e2e_runs_judge_permission_namespace_postgres_contract(self) -> None:
        step = workflow_step(
            "Auth Judge permission namespace PostgreSQL contract",
            DOCKER_E2E_WORKFLOW,
        )
        self.assertNotIn("if:", step)
        self.assertIn("AUTH_JUDGE_PERMISSION_TEST_DATABASE_URL:", step)
        self.assertIn(
            "go test ./internal/repository -run "
            "'^TestJudgePermissionNamespacePostgres' -count=1",
            step,
        )

    def test_prometheus_validation_mounts_a_nonempty_read_only_token_fixture(self) -> None:
        step = workflow_step(
            "Validate Prometheus configuration and alert rules",
            CI_WORKFLOW,
        )
        self.assertIn('promtool_fixture_dir="$(mktemp -d)"', step)
        self.assertIn(
            'promtool_token_file="$promtool_fixture_dir/orchestrator-observability-token"',
            step,
        )
        self.assertIn(
            "printf '%s\\n' 'promtool-ci-observability-token-fixture-v1' "
            '>"$promtool_token_file"',
            step,
        )
        self.assertIn('test -s "$promtool_token_file"', step)
        self.assertIn(
            '-v "$promtool_token_file:/etc/prometheus/secrets/'
            'orchestrator-observability-token:ro"',
            step,
        )
        for ca_name in ("orchestrator-ca.crt", "gateway-ca.crt"):
            self.assertIn(
                '-v "/etc/ssl/certs/ca-certificates.crt:'
                f'/etc/prometheus/tls/{ca_name}:ro"',
                step,
            )
        self.assertIn("check config /etc/prometheus/prometheus.yml", step)
        self.assertIn("check rules /etc/ojos-monitoring/alerts.yml", step)
        self.assertIn("test rules alert-tests.yml", step)

    def test_security_scans_are_required_and_kept_as_explicit_steps(self) -> None:
        opt_in = "if: ${{ vars.ORCHESTRATOR_RUN_SECURITY_GATES == 'true' }}"
        for name in (
            "Scan Go modules for reachable vulnerabilities",
            "Audit Rust dependencies",
            "Audit gateway frontend dependencies",
        ):
            self.assertNotIn(opt_in, workflow_step(name, CI_WORKFLOW), name)
        self.assertNotIn(
            opt_in,
            workflow_step("Audit gateway frontend dependencies", DOCKER_E2E_WORKFLOW),
        )
        self.assertNotIn(
            "npm audit",
            workflow_step("Build gateway frontend", CI_WORKFLOW),
        )
        self.assertNotIn(
            "npm audit",
            workflow_step("Gateway frontend checks", DOCKER_E2E_WORKFLOW),
        )

    def test_docker_e2e_installs_desktop_native_dependencies_before_rust(self) -> None:
        install = DOCKER_E2E_WORKFLOW.index(
            "Prepare PostgreSQL live schema and Desktop build dependencies"
        )
        rust_checks = DOCKER_E2E_WORKFLOW.index("- name: Rust checks")
        self.assertLess(install, rust_checks)
        for package in (
            "libayatana-appindicator3-dev",
            "librsvg2-dev",
            "libssl-dev",
            "libwebkit2gtk-4.1-dev",
            "libxdo-dev",
        ):
            self.assertIn(package, DOCKER_E2E_WORKFLOW[install:rust_checks])
        embedded_web = DOCKER_E2E_WORKFLOW.index(
            "- name: Build embedded Orchestrator Web UI"
        )
        self.assertLess(install, embedded_web)
        self.assertLess(embedded_web, rust_checks)
        self.assertIn("working-directory: manager/web", DOCKER_E2E_WORKFLOW[embedded_web:rust_checks])
        self.assertIn("npm ci --registry=https://registry.npmjs.org", DOCKER_E2E_WORKFLOW[embedded_web:rust_checks])
        self.assertIn("npm run build", DOCKER_E2E_WORKFLOW[embedded_web:rust_checks])

    def test_docker_e2e_isolates_job_services_from_compose_drills(self) -> None:
        job = workflow_job("docker-e2e", DOCKER_E2E_WORKFLOW)
        self.assertIn(
            "COMPOSE_PROJECT_NAME: ojos-docker-e2e-${{ github.run_id }}-${{ github.run_attempt }}",
            job,
        )
        self.assertIn(
            "OJOS_E2E_MINIO_CONTAINER: ojos-e2e-minio-${{ github.run_id }}-${{ github.run_attempt }}",
            job,
        )
        self.assertIn("- 56379:6379", job)
        self.assertIn(
            "OJOS_REAL_REDIS_URL: redis://127.0.0.1:56379/0",
            job,
        )
        self.assertNotIn("- 6379:6379", job)
        self.assertIn("-p 19000:9000", job)
        self.assertIn("-p 19001:9001", job)
        self.assertIn("OJOS_REAL_MINIO_ENDPOINT: 127.0.0.1:19000", job)
        self.assertNotIn("--name ojos-e2e-minio ", job)

        contest_vertical = workflow_step("Contest service real Docker vertical", job)
        self.assertIn(
            'OJOS_CONTEST_E2E_REDIS_HOST_PORT: "56379"', contest_vertical
        )

        trace_drill = workflow_step("Compose trace E2E drill", job)
        self.assertNotIn("continue-on-error", trace_drill)
        self.assertIn('REDIS_HOST_PORT: "16379"', trace_drill)
        self.assertNotIn("\n          REDIS_URL:", trace_drill)
        load_drill = workflow_step("Compose basic load/soak drill", job)
        self.assertIn('REDIS_HOST_PORT: "16379"', load_drill)
        self.assertNotIn("\n          REDIS_URL:", load_drill)
        cleanup = workflow_step("Cleanup compose runtime drill", job)
        self.assertIn("if: ${{ always()", cleanup)
        self.assertIn('REDIS_HOST_PORT: "16379"', cleanup)
        self.assertIn('docker rm -f "$OJOS_E2E_MINIO_CONTAINER"', cleanup)
        self.assertIn('exit "$compose_status"', cleanup)

    def test_compose_redis_host_port_defaults_to_6379_and_is_overridable(self) -> None:
        self.assertIn(
            '"127.0.0.1:${REDIS_HOST_PORT:-6379}:6379"',
            COMPOSE,
        )
        self.assertNotIn('"127.0.0.1:6379:6379"', COMPOSE)
        self.assertIn("${REDIS_HOST_PORT:-6379}/0", TRACE_DRILL)
        self.assertIn("${REDIS_HOST_PORT:-6379}/0", LOAD_DRILL)

    def test_legacy_drills_capture_complete_compose_failures(self) -> None:
        for drill in (TRACE_DRILL, LOAD_DRILL):
            self.assertIn('docker_compose ps -a >"$evidence_dir/logs/compose-ps.txt"', drill)
            self.assertIn("auth-service gateway problem-service storage-service", drill)
            self.assertIn('compose_ps: "logs/compose-ps.txt"', drill)

    def test_legacy_drills_use_current_gateway_projection_without_orchestrator(self) -> None:
        gateway = COMPOSE_DEV.split("\n  gateway:\n", 1)[1].split("\n  auth-service:\n", 1)[0]
        self.assertIn("depends_on: !override", gateway)
        self.assertIn("auth-service:", gateway)
        self.assertNotIn("condition: service_healthy\n      orchestrator:", gateway)
        self.assertIn('ORCHESTRATOR_ENDPOINT: ""', gateway)
        self.assertIn('ORCHESTRATOR_INTERNAL_TOKEN: ""', gateway)
        self.assertIn('ORCHESTRATOR_NODE_ID: ""', gateway)
        for drill in (TRACE_DRILL, LOAD_DRILL):
            migration_block = drill.split("run_compose_migrations()", 1)[1].split("}\n", 1)[0]
            startup_block = drill.split("compose_up_args+=(", 1)[1].split(")", 1)[0]
            self.assertNotIn("orchestrator-migrations", migration_block)
            self.assertNotIn("\n    orchestrator\n", startup_block)
        self.assertNotIn(
            "development Compose must opt into the explicit ephemeral daemon",
            OPS_CI_POLICY,
        )
        self.assertIn(
            'set(gateway.get("depends_on", {})) == {"auth-service"}',
            OPS_CI_POLICY,
        )
        self.assertIn(
            'development Auth must clear {variable}',
            OPS_CI_POLICY,
        )
        self.assertEqual(
            COMPOSE_DEV.count(
                "OJOS_AUTH_PERMISSION_GATEWAY_ENDPOINT: http://gateway:8080"
            ),
            2,
        )
        self.assertIn(
            'development {service_name} must use the smoke-pushed delegated permission route',
            OPS_CI_POLICY,
        )

    def test_workflow_is_manual_only_and_candidate_is_the_safe_default(self) -> None:
        trigger = WORKFLOW.split("\nrun-name:", 1)[0]
        self.assertIn("\non:\n  workflow_dispatch:\n", f"\n{trigger}")
        self.assertNotRegex(trigger, r"(?m)^  (?:push|pull_request|schedule):")
        self.assertNotRegex(trigger, r"(?m)^\s+tags(?:-ignore)?:")
        self.assertRegex(
            trigger,
            r"(?ms)^      publish:\n(?:^        .*\n)*?^        default: false$",
        )
        self.assertRegex(
            trigger,
            r'(?ms)^      candidate_run_id:\n(?:^        .*\n)*?^        default: ""$',
        )

        resolver = workflow_job("resolve-dispatch")
        self.assertIn('[[ "$GITHUB_RUN_ATTEMPT" == "1" ]]', resolver)
        self.assertIn("candidate construction and promotion refuse workflow reruns", resolver)
        self.assertIn('if [[ "$INPUT_PUBLISH" == "false" ]]', resolver)
        self.assertIn('[[ -z "$INPUT_CANDIDATE_RUN_ID" ]]', resolver)
        self.assertIn("candidate_run_id must be empty when publish=false", resolver)
        self.assertIn("/commits/$GITHUB_SHA", resolver)
        self.assertIn(
            "feat(orchestrator): freeze v1 release candidate", resolver
        )
        self.assertIn('[[ "$INPUT_PUBLISH" == "true" ]]', resolver)
        self.assertIn("publish=true requires a numeric candidate_run_id", resolver)
        self.assertIn("jq -r '.run_attempt'", resolver)
        self.assertIn("selected candidate run is a rerun", resolver)

        evidence = workflow_job("production-evidence")
        self.assertIn("actions/runs/$run_id", evidence)
        self.assertIn("jq -r '.run_attempt'", evidence)
        self.assertIn('!= "1"', evidence)
        self.assertIn("jq -r '.event'", evidence)
        self.assertIn('!= "workflow_dispatch"', evidence)
        self.assertIn('"environment_observations_ndjson"', evidence)
        self.assertIn("if len(index) != 3", evidence)

    def test_candidate_path_cannot_publish_and_manifest_stays_evidence_only(self) -> None:
        candidate_jobs = (
            "candidate-gates",
            "production-evidence",
            "build-windows",
            "build-linux",
            "attest-and-sign-primary",
            "assemble-candidate",
        )
        for name in candidate_jobs:
            body = workflow_job(name)
            self.assertIn("outputs.mode == 'candidate'", body, name)
            self.assertNotIn("gh release create", body, name)

        assemble = workflow_job("assemble-candidate")
        self.assertIn("orchestrator-candidate.py create", assemble)
        self.assertIn("--output candidate/candidate-manifest.json", assemble)
        self.assertIn("--payload-dir candidate/payload", assemble)
        self.assertIn('--candidate-run-attempt "$GITHUB_RUN_ATTEMPT"', assemble)
        self.assertIn("test ! -e candidate/payload/candidate-manifest.json", assemble)
        self.assertIn("test ! -d candidate/payload/evidence", assemble)
        self.assertIn("name: orchestrator-v1-signed-candidate", assemble)
        self.assertIn("path: candidate", assemble)
        self.assertIn("id: signed-candidate-upload", assemble)
        self.assertIn("steps.signed-candidate-upload.outputs.artifact-id", assemble)
        self.assertIn("steps.signed-candidate-upload.outputs.artifact-digest", assemble)
        self.assertIn("for attempt in $(seq 1 6)", assemble)
        self.assertIn("sleep 5", assemble)
        self.assertIn("metadata was not complete after 6 attempts", assemble)
        self.assertIn('"$api_digest" == "sha256:$UPLOAD_ARTIFACT_DIGEST"', assemble)
        self.assertIn("name: orchestrator-v1-candidate-identity", assemble)

    def test_real_desktop_webview_and_web_soak_run_for_thirty_minutes_together(self) -> None:
        gates = workflow_job("candidate-gates")
        self.assertIn("cargo build -p ojos-orchestrator-desktop", gates)
        self.assertIn("OJOS_DESKTOP_SMOKE=1", gates)
        self.assertIn("OJOS_DESKTOP_SMOKE_DURATION_MS=1800000", gates)
        self.assertIn("./target/debug/ojos-orchestrator-desktop", gates)
        self.assertIn("xvfb-run -a", gates)
        self.assertIn("OJOS_E2E_SOAK_MS=1800000 npm run test:e2e", gates)
        self.assertIn('wait -n -p finished_pid "$desktop_pid" "$playwright_pid"', gates)

    def test_windows_oidc_and_artifact_signing_order_is_fail_closed(self) -> None:
        windows = workflow_job("build-windows")
        self.assertIn("environment: orchestrator-rc-signing", windows)
        self.assertIn("id-token: write", windows)
        self.assertNotIn("creds:", windows)
        self.assertNotIn("AZURE_CLIENT_SECRET", windows)
        self.assertEqual(windows.count("uses: azure/artifact-signing-action@v2"), 2)

        no_bundle = windows.index("cargo tauri build --no-bundle")
        login = windows.index("uses: azure/login@v3")
        signing = [
            match.start()
            for match in re.finditer(
                re.escape("uses: azure/artifact-signing-action@v2"), windows
            )
        ]
        msi_bundle = windows.index("cargo tauri bundle --bundles msi")
        package = windows.index("bash deploy/release/pack-orchestrator-v1.sh")
        verify = windows.index("deploy/release/verify-windows-authenticode.ps1")
        smoke = windows.index("deploy/release/smoke-orchestrator-v1-layout.ps1")
        assertion = windows.index("Assert Windows primary artifact set")
        self.assertLess(no_bundle, login)
        self.assertLess(login, signing[0])
        self.assertLess(signing[0], msi_bundle)
        self.assertLess(msi_bundle, signing[1])
        self.assertLess(signing[1], package)
        self.assertLess(package, verify)
        self.assertLess(verify, smoke)
        self.assertLess(smoke, assertion)

    def test_release_runbook_documents_exact_protected_inputs_and_dispatches(
        self,
    ) -> None:
        windows = workflow_job("build-windows")
        for name in sorted(set(re.findall(r"secrets\.([A-Z0-9_]+)", windows))):
            self.assertIn(f"`{name}`", RELEASE_POLICY)
        for name in sorted(set(re.findall(r"vars\.([A-Z0-9_]+)", windows))):
            self.assertIn(f"`{name}`", RELEASE_POLICY)

        self.assertIn("`orchestrator-rc-signing` GitHub Environment", RELEASE_POLICY)
        self.assertIn(
            "`repo:OWNER/REPOSITORY:environment:orchestrator-rc-signing`",
            RELEASE_POLICY,
        )
        candidate_section = RELEASE_POLICY.split(
            "只能用以下候选入口：", 1
        )[1].split("```bash", 1)[1].split("```", 1)[0]
        self.assertIn("gh workflow run release.yml", candidate_section)
        self.assertIn("-f publish=false", candidate_section)
        self.assertNotIn("candidate_run_id", candidate_section)

        promotion_section = RELEASE_POLICY.split(
            "只有届时才允许执行：", 1
        )[1].split("```bash", 1)[1].split("```", 1)[0]
        self.assertIn("gh workflow run release.yml", promotion_section)
        self.assertIn("-f publish=true", promotion_section)
        self.assertIn('-f candidate_run_id="$CANDIDATE_RUN_ID"', promotion_section)

    def test_exactly_eleven_primary_matrix_entries_are_signed_and_attested(self) -> None:
        trust = workflow_job("attest-and-sign-primary")
        entries = re.findall(r"(?m)^          - id: ([a-z0-9-]+)$", trust)
        templates = re.findall(r"(?m)^            template: (\S+)$", trust)
        self.assertEqual(len(entries), 11)
        self.assertEqual(len(set(entries)), 11)
        self.assertEqual(len(templates), 11)
        self.assertEqual(len(set(templates)), 11)
        self.assertEqual(sum("windows" in entry for entry in entries), 5)
        self.assertEqual(sum("linux" in entry for entry in entries), 6)
        self.assertIn(
            'cosign sign-blob --yes --bundle "$path.sigstore.json" "$path"', trust
        )
        self.assertIn("uses: actions/attest-build-provenance@v4", trust)
        assert_in_order(
            self,
            trust,
            "cosign sign-blob",
            "actions/attest-build-provenance@v4",
            "actions/upload-artifact@v7",
        )

        assemble = workflow_job("assemble-candidate")
        self.assertIn("pattern: orchestrator-v1-signed-primary-*", assemble)
        self.assertIn("merge-multiple: true", assemble)
        self.assertRegex(
            assemble,
            r'test "\$\(find candidate/payload .*\| wc -l\)" -eq 22',
        )

    def test_promotion_selects_one_candidate_run_and_never_rebuilds(self) -> None:
        promotion = workflow_job("promote-existing-candidate")
        self.assertIn("outputs.mode == 'promotion'", promotion)
        self.assertRegex(promotion, r"(?m)^    needs: resolve-dispatch$")
        self.assertIn("environment: orchestrator-ga-promotion", promotion)
        self.assertIn("CANDIDATE_RUN_ID:", promotion)
        self.assertIn("actions/runs/$CANDIDATE_RUN_ID/artifacts?per_page=100", promotion)
        self.assertIn("expected exactly one signed candidate artifact", promotion)
        self.assertIn("actions/artifacts/$artifact_id/zip", promotion)
        self.assertIn('archive_sha256="$(sha256sum "$archive"', promotion)
        self.assertIn('"sha256:$archive_sha256" == "$artifact_digest"', promotion)
        self.assertIn("--actual-artifact-archive-sha256", promotion)
        self.assertLess(
            promotion.index('archive_sha256="$(sha256sum "$archive"'),
            promotion.index('unzip -q "$archive"'),
        )
        self.assertIn('[[ "$GITHUB_RUN_ATTEMPT" == "1" ]]', promotion)
        self.assertNotIn("gh run download", promotion)
        self.assertIn("verify-orchestrator-v1-trust.sh", promotion)

        forbidden = (
            "cargo build",
            "cargo tauri",
            "npm --prefix manager/web run build",
            "pack-orchestrator-v1.sh",
            "syft",
            "cosign sign-blob",
            "attest-build-provenance",
            "artifact-signing-action",
        )
        for needle in forbidden:
            self.assertNotIn(needle, promotion, needle)

    def test_promotion_requires_protected_acceptance_for_exact_artifact_identity(self) -> None:
        promotion = workflow_job("promote-existing-candidate")
        self.assertIn("environment: orchestrator-ga-promotion", promotion)
        for variable in (
            "ORCHESTRATOR_SECURITY_ACCEPTED_CANDIDATE_SHA",
            "ORCHESTRATOR_SECURITY_ACCEPTED_CANDIDATE_RUN_ID",
            "ORCHESTRATOR_SECURITY_ACCEPTED_CANDIDATE_MANIFEST_SHA256",
            "ORCHESTRATOR_SECURITY_ACCEPTED_CANDIDATE_ARTIFACT_ID",
            "ORCHESTRATOR_SECURITY_ACCEPTED_CANDIDATE_ARTIFACT_DIGEST",
        ):
            self.assertIn(f"vars.{variable}", promotion)
        self.assertIn(
            'manifest_sha="$(jq -r \'.candidate_sha\' candidate/candidate-manifest.json)"',
            promotion,
        )
        self.assertIn('[[ "$manifest_sha" == "$CANDIDATE_SHA" ]]', promotion)
        self.assertIn('"$ACCEPTED_RUN_ID" == "$CANDIDATE_RUN_ID"', promotion)
        self.assertIn('"$artifact_id" == "$ACCEPTED_ARTIFACT_ID"', promotion)
        self.assertIn('"$artifact_digest" == "$ACCEPTED_ARTIFACT_DIGEST"', promotion)
        self.assertIn('"$manifest_sha256" == "$ACCEPTED_MANIFEST_SHA256"', promotion)
        self.assertIn("artifact belongs to another workflow run or commit", promotion)

    def test_promotion_publishes_only_the_verified_twenty_two_payload_files(self) -> None:
        promotion = workflow_job("promote-existing-candidate")
        publish = promotion[promotion.index("Publish only the verified 22-file payload") :]
        self.assertIn("orchestrator-candidate.py list-assets", publish)
        self.assertIn('assets+=("candidate/payload/$name"', publish)
        self.assertIn('"candidate/payload/$name.sigstore.json")', publish)
        self.assertIn('[[ "${#assets[@]}" -eq 22 ]]', publish)
        self.assertIn("gh release create", publish)
        self.assertIn("--verify-tag", publish)
        self.assertIn('"${assets[@]}"', publish)
        self.assertNotIn("candidate-manifest.json", publish)
        self.assertNotIn("candidate/evidence", publish)
        self.assertNotIn("gh release upload", publish)

    def test_candidate_images_are_same_main_sha_digest_based_and_attested(self) -> None:
        trigger = IMAGE_WORKFLOW.split("\nrun-name:", 1)[0]
        self.assertIn("\n  push:\n    branches:\n      - main\n", f"\n{trigger}")
        self.assertIn("\n  workflow_dispatch:\n", f"\n{trigger}")
        self.assertNotRegex(trigger, r"(?m)^  (?:pull_request|schedule):")

        guard = workflow_job("guard", IMAGE_WORKFLOW)
        self.assertIn("github.event_name == 'workflow_dispatch'", guard)
        self.assertIn(
            "github.event.head_commit.message == 'feat(orchestrator): freeze v1 release candidate'",
            guard,
        )
        self.assertIn('[[ "$GITHUB_REF" == refs/heads/main ]]', guard)
        self.assertIn('[[ "$GITHUB_SHA" =~ ^[0-9a-f]{40}$ ]]', guard)
        self.assertIn('[[ "$GITHUB_EVENT_NAME" == push ]]', guard)
        self.assertIn('[[ "$CANDIDATE_COMMIT_MESSAGE" == "$EXPECTED_COMMIT_MESSAGE" ]]', guard)
        self.assertIn(
            '[[ "$FIXTURE_BASE_IMAGE" =~ ^[^[:space:]@]+@sha256:[0-9a-f]{64}$ ]]',
            guard,
        )
        self.assertIn('image_name="${FIXTURE_BASE_IMAGE%@*}"', guard)
        self.assertIn('[[ "${image_name##*/}" != *:* ]]', guard)

        build = workflow_job("build", IMAGE_WORKFLOW)
        components = re.findall(r"(?m)^          - component: ([a-z0-9-]+)$", build)
        self.assertEqual(
            components,
            ["control-plane", "agent", "capacity-fixture"],
        )
        self.assertEqual(build.count("image: ojos-orchestrator-"), 3)
        self.assertIn("ref: ${{ github.sha }}", build)
        self.assertIn(":sha-${{ github.sha }}", build)
        self.assertIn("org.opencontainers.image.revision=${{ github.sha }}", build)
        self.assertIn("GITHUB_SHA=${{ github.sha }}", build)
        self.assertIn("OJOS_BUILD_COMMIT=${{ github.sha }}", build)
        self.assertIn(
            "CAPACITY_FIXTURE_BASE_IMAGE=${{ needs.guard.outputs.fixture_base_image }}",
            build,
        )
        self.assertIn("provenance: mode=max", build)
        self.assertIn("sbom: true", build)
        self.assertIn("uses: actions/attest-build-provenance@v4", build)
        self.assertIn("subject-digest: ${{ steps.build.outputs.digest }}", build)
        self.assertIn("push-to-registry: true", build)
        self.assertIn('reference="$IMAGE@$DIGEST"', build)
        self.assertIn("org.opencontainers.image.revision", build)
        self.assertIn('[[ "$revision" == "$GITHUB_SHA" ]]', build)
        self.assertIn('if [[ "$COMPONENT" == "agent" ]]', build)
        self.assertIn("docker image inspect --format '{{.Config.User}}'", build)
        self.assertIn('[[ "$runtime_user" == "65532:65532" ]]', build)
        for field in (
            "component:$component",
            "digest:$digest",
            "reference:$reference",
            "commit_sha:$commit_sha",
            "workflow_run_id:$workflow_run_id",
        ):
            self.assertIn(field, build)


if __name__ == "__main__":
    unittest.main()
