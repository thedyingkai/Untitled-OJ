import pathlib
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[3]


class AuthAdminBootstrapPolicyTests(unittest.TestCase):
    def read(self, relative_path: str) -> str:
        return (REPO_ROOT / relative_path).read_text(encoding="utf-8")

    def test_production_compose_uses_one_fixed_read_only_host_bind(self) -> None:
        compose = self.read("deploy/compose/docker-compose.yml")
        target = "/run/secrets/ojos-auth-admin-bootstrap"
        self.assertEqual(compose.count(f"target: {target}"), 1)
        self.assertIn(
            "AUTH_ADMIN_BOOTSTRAP_SECRET_FILE: " + target,
            compose,
        )
        self.assertIn(
            "source: ${AUTH_ADMIN_BOOTSTRAP_SECRET_FILE:?set "
            "AUTH_ADMIN_BOOTSTRAP_SECRET_FILE to the private one-time host token file}",
            compose,
        )
        target_index = compose.index(f"target: {target}")
        mount = compose[target_index : target_index + 180]
        self.assertIn("read_only: true", mount)
        self.assertIn("create_host_path: false", mount)
        auth_service = compose[compose.index("  auth-service:") : target_index]
        self.assertIn('user: "65532:65532"', auth_service)

    def test_development_overlay_clears_env_and_drops_bootstrap_mount(self) -> None:
        overlay = self.read("deploy/compose/docker-compose.dev.yml")
        self.assertIn('AUTH_ADMIN_BOOTSTRAP_SECRET_FILE: ""', overlay)
        self.assertIn("volumes: !override", overlay)
        self.assertNotIn("/run/secrets/ojos-auth-admin-bootstrap", overlay)

    def test_both_production_env_examples_expose_host_path(self) -> None:
        for path in (
            ".env.production.example",
            "deploy/ops/production.env.example",
        ):
            with self.subTest(path=path):
                example = self.read(path)
                self.assertEqual(
                    example.count("\nAUTH_ADMIN_BOOTSTRAP_SECRET_FILE=\n"), 1
                )

    def test_preflights_reject_inline_and_validate_private_token_file(self) -> None:
        for path in (
            "deploy/ops/secret-check.sh",
            "deploy/ops/orchestrator-preflight.sh",
        ):
            with self.subTest(path=path):
                script = self.read(path)
                for marker in (
                    "AUTH_ADMIN_BOOTSTRAP_SECRET is forbidden in production",
                    "OJOS_SECRET_ADMINBOOTSTRAP_SECRET is Agent-only and forbidden",
                    "must name a regular file, not a symlink",
                    "must be owned by exact Auth runtime uid/gid 65532:65532",
                    "must use exact mode 0600",
                    "character token plus at most one trailing newline",
                    "must use only URL-safe",
                    "must not reuse",
                ):
                    self.assertIn(marker, script)

    def test_linux_policy_keeps_owner_and_mode_negatives_required(self) -> None:
        policy = self.read("deploy/ops/ci-policy.sh")
        self.assertIn(
            "bootstrap_cases=(invalid public owner mode reused empty oversized symlink)",
            policy,
        )
        self.assertIn('owner) bootstrap_path="$wrong_owner_bootstrap_file"', policy)
        self.assertIn('mode) bootstrap_path="$wrong_mode_bootstrap_file"', policy)

    def test_compose_policy_handles_false_omission_without_losing_source_evidence(self) -> None:
        policy = self.read("deploy/ops/ci-policy.sh")
        self.assertIn(
            '"$rendered_json" "$repo_root/deploy/compose/docker-compose.yml"',
            policy,
        )
        self.assertIn('"create_host_path: false" in source_mount', policy)
        self.assertIn(
            "normalized_create_host_path is None or normalized_create_host_path is False",
            policy,
        )
        self.assertNotIn(
            'get("bind", {}).get("create_host_path") is False',
            policy,
        )
        self.assertIn('case "$platform" in', policy)
        self.assertIn('MSYS_*|MINGW*|CYGWIN*)', policy)

    def test_runbook_requires_route_removal_verification(self) -> None:
        for path in (
            "docs/ops/deployment-checklist.md",
            "docs/ops/ops-runbook.md",
        ):
            with self.subTest(path=path):
                document = self.read(path)
                self.assertIn("/api/auth/bootstrap/admin", document)
                self.assertIn("404", document)
                self.assertIn("--force-recreate auth-service", document)


if __name__ == "__main__":
    unittest.main()
