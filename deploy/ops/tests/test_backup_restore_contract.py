import pathlib
import re
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[3]
BACKUP = ROOT / "deploy" / "ops" / "backup.sh"
RESTORE = ROOT / "deploy" / "ops" / "restore.sh"
MANIFEST = ROOT / "deploy" / "ops" / "backup-manifest.py"
DRILL = ROOT / "deploy" / "ops" / "tests" / "full-stack-backup-restore-drill.sh"
STAGING_WORKFLOW = ROOT / ".github" / "workflows" / "staging-drill.yml"
RELEASE_WORKFLOW = ROOT / ".github" / "workflows" / "release.yml"


class FullStackBackupRestoreContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.backup = BACKUP.read_text(encoding="utf-8")
        cls.restore = RESTORE.read_text(encoding="utf-8")

    def test_backup_requires_a_verified_fence_and_publishes_atomically(self) -> None:
        self.assertIn("OJOS_CONFIRM_QUIESCED_BACKUP", self.backup)
        self.assertIn("OJOS_BACKUP_FENCE_CHECK_COMMAND", self.backup)
        self.assertIn("OJOS_BACKUP_FENCE_TOKEN", self.backup)
        self.assertIn('temporary="$backup_root/.${stamp}.tmp.$$"', self.backup)
        self.assertIn('[[ ! -e "$final" && ! -e "$temporary" ]]', self.backup)
        self.assertIn('trap cleanup EXIT', self.backup)
        self.assertIn("trap 'exit 143' TERM", self.backup)
        self.assertIn('mv "$temporary" "$final"', self.backup)
        self.assertLess(
            self.backup.index('backup-manifest.py" "${create_args[@]}"'),
            self.backup.index('find . -type f ! -name SHA256SUMS'),
        )
        self.assertLess(
            self.backup.index('sha256sum -c SHA256SUMS'),
            self.backup.index('mv "$temporary" "$final"'),
        )

    def test_every_database_dump_is_catalog_verified(self) -> None:
        self.assertIn("orchestrator:ORCHESTRATOR_DATABASE_URL", self.backup)
        self.assertIn("auth:AUTH_DATABASE_URL", self.backup)
        self.assertIn("problem:PROBLEM_DATABASE_URL", self.backup)
        self.assertIn("judge:JUDGE_DATABASE_URL", self.backup)
        self.assertIn("user:USER_DATABASE_URL", self.backup)
        self.assertIn('pg_restore --list "$temporary/postgres/$name.dump"', self.backup)
        self.assertIn("redis-check-rdb", self.backup)

    def test_problem_retained_volume_is_a_required_agent_owned_component(self) -> None:
        self.assertIn("OJOS_PROBLEM_RETAINED_VOLUME_OWNER_INSTANCE_ID", self.backup)
        self.assertIn("OJOS_PROBLEM_RETAINED_VOLUME_NAME", self.backup)
        self.assertIn('docker volume inspect "$retained_volume_name"', self.backup)
        self.assertIn('--filter "volume=$retained_volume_name" --filter status=running', self.backup)
        self.assertGreaterEqual(self.backup.count('capture_retained_volume "'), 2)
        self.assertIn("problem-packages.identity.json", self.backup)
        self.assertIn("problem-packages.inventory.json", self.backup)
        self.assertIn("problem-packages.tar.gz", self.backup)
        self.assertIn("Problem retained volume live tree changed during backup", self.backup)
        self.assertNotIn("BACKUP_SKIP_RETAINED", self.backup)

    def test_restore_is_verify_first_clean_target_only_and_transactional(self) -> None:
        verify = self.restore.index('backup-manifest.py" verify')
        confirm = self.restore.index("OJOS_CONFIRM_RESTORE")
        first_restore = self.restore.index("pg_restore --no-owner")
        self.assertLess(verify, confirm)
        self.assertLess(confirm, first_restore)
        self.assertIn("same-environment in-place restore is forbidden", self.restore)
        self.assertIn("OJOS_CONFIRM_CLEAN_TARGET", self.restore)
        self.assertIn("target database is not empty", self.restore)
        self.assertIn("--single-transaction --exit-on-error", self.restore)
        self.assertNotIn("pg_restore --clean", self.restore)
        self.assertIn("verify-tar", self.restore)
        self.assertIn("verify-tree", self.restore)
        self.assertIn("Redis target RDB already exists", self.restore)
        self.assertIn("MinIO target bucket", self.restore)
        self.assertIn("OJOS_RESTORE_MINIO_TARGET_ID", self.restore)

    def test_retained_restore_is_clean_staged_exact_and_reinspected(self) -> None:
        self.assertIn("OJOS_RESTORE_PROBLEM_RETAINED_VOLUME_OWNER_INSTANCE_ID", self.restore)
        self.assertIn("OJOS_RESTORE_RETAINED_VOLUME_TARGET_ID", self.restore)
        self.assertIn("target Problem retained volume is not empty", self.restore)
        self.assertIn("staged retained-volume archive digest", self.restore)
        self.assertGreaterEqual(self.restore.count('backup-manifest.py" verify-inventory'), 2)
        self.assertGreaterEqual(
            self.restore.count('inspect_target_retained_volume "$retained_inspect'), 3
        )
        self.assertIn("acquired a running mount during restore", self.restore)
        self.assertIn("Problem retained volume identity changed during restore", self.restore)
        self.assertLess(
            self.restore.index("target Problem retained volume is not empty"),
            self.restore.index('cp -a -- "$retained_stage/."'),
        )

    def test_cutover_has_a_mandatory_paired_rollback_and_post_check(self) -> None:
        self.assertIn("OJOS_RESTORE_CUTOVER_COMMAND", self.restore)
        self.assertIn("OJOS_RESTORE_ROLLBACK_COMMAND", self.restore)
        self.assertIn("OJOS_RESTORE_POST_CUTOVER_CHECK_COMMAND", self.restore)
        self.assertIn("OJOS_RESTORE_POST_ROLLBACK_CHECK_COMMAND", self.restore)
        self.assertIn("OJOS_RESTORE_FAILED_TARGET_CLEANUP_COMMAND", self.restore)
        self.assertIn('if [[ $rc -ne 0 && "$cutover_started" == "1" ]]', self.restore)
        self.assertLess(
            self.restore.index('cutover_started=1'),
            self.restore.index('bash -Eeuo pipefail -c "$cutover_command"'),
        )
        self.assertLess(
            self.restore.index('bash -Eeuo pipefail -c "$cutover_command"'),
            self.restore.index('bash -Eeuo pipefail -c "$OJOS_RESTORE_POST_CUTOVER_CHECK_COMMAND"'),
        )

    def test_restore_exposes_only_bounded_test_failpoints(self) -> None:
        expected = {
            "after-databases",
            "after-redis",
            "after-storage",
            "after-retained-volume",
            "after-components",
        }
        actual = set(re.findall(r'!= "(after-[a-z-]+)"', self.restore))
        self.assertEqual(actual, expected)
        self.assertNotIn("eval ", self.backup)
        self.assertNotIn("eval ", self.restore)

    def test_manifest_helper_is_a_required_runtime_asset(self) -> None:
        self.assertTrue(MANIFEST.is_file())
        self.assertIn("backup-manifest.py", self.backup)
        self.assertIn("backup-manifest.py", self.restore)

    def test_clean_target_drill_calls_the_production_scripts_with_dedicated_targets(self) -> None:
        drill = DRILL.read_text(encoding="utf-8")
        self.assertIn("full-stack-clean-target-drill-v1", drill)
        self.assertIn("ojos_${name}_backup_restore_drill_source", drill)
        self.assertIn("ojos_${name}_backup_restore_drill_target", drill)
        self.assertIn('bash "$repo_root/deploy/ops/backup.sh"', drill)
        self.assertEqual(drill.count('bash "$repo_root/deploy/ops/restore.sh"'), 2)
        self.assertIn("OJOS_RESTORE_VERIFY_ONLY=1", drill)
        self.assertIn("OJOS_ENV_FILE=", drill)
        self.assertGreaterEqual(drill.count("env -i PATH="), 3)
        self.assertIn("traffic_changed=no", drill)
        self.assertIn("orchestrator_api_bindings", drill)
        self.assertIn("integration_outbox", drill)
        self.assertIn("integration_inbox", drill)
        self.assertIn("problem_artifact_sha256", drill)
        self.assertIn("source_problem_retained", drill)
        self.assertIn("target_problem_retained", drill)
        self.assertIn("problem_mutation_journal=reconciled", drill)
        self.assertIn("problem_artifact_reference=reconciled", drill)
        self.assertIn("submission_artifacts_and_database_references=reconciled", drill)
        self.assertIn('domain-reconciliation.json', drill)
        self.assertNotIn("DROP DATABASE", drill.upper())
        self.assertNotIn("rm -rf", drill)
        workflow = STAGING_WORKFLOW.read_text(encoding="utf-8")
        self.assertIn("full-stack-backup-restore-drill.sh", workflow)
        self.assertIn("OJOS_DRILL_SOURCE_REDIS_URL", workflow)

    def test_container_evidence_is_runner_owned_and_upload_is_preflighted(self) -> None:
        workflow = STAGING_WORKFLOW.read_text(encoding="utf-8")
        self.assertEqual(workflow.count('runner_uid="$(id -u)"'), 2)
        self.assertEqual(workflow.count('runner_gid="$(id -g)"'), 2)
        self.assertEqual(
            workflow.count('--user "$runner_uid:$runner_gid"'),
            2,
        )
        self.assertIn("- name: Validate recovery artifact suite", workflow)
        self.assertIn('find "$root" -type d -print0', workflow)
        self.assertIn('find "$root" -type f -print0', workflow)
        self.assertIn('[[ -r "$path" && -x "$path" ]]', workflow)
        self.assertIn('[[ -r "$path" ]]', workflow)
        self.assertIn('tar -czf "$archive" "${evidence[@]}"', workflow)
        self.assertIn("archive_bytes", workflow)
        self.assertIn("FROM postgres:17", workflow)
        self.assertIn('docker build --tag "$drill_image"', workflow)
        upload = workflow.split("- name: Upload staging drill evidence", 1)[1]
        self.assertIn("steps.recovery-artifact-suite.outcome == 'success'", upload)
        self.assertIn("if-no-files-found: error", upload)
        self.assertIn("include-hidden-files: true", upload)
        self.assertNotIn("continue-on-error", workflow)

    def test_full_stack_source_storage_is_created_parent_first_with_private_modes(
        self,
    ) -> None:
        drill = DRILL.read_text(encoding="utf-8")
        root = 'mkdir -m 0700 "$source_storage"'
        parents = (
            'mkdir -m 0700 "$source_storage/problems" '
            '"$source_storage/submissions"'
        )
        leaves = (
            'mkdir -m 0700 "$source_storage/problems/drill" '
            '"$source_storage/submissions/drill"'
        )
        self.assertIn(root, drill)
        self.assertIn(parents, drill)
        self.assertIn(leaves, drill)
        self.assertLess(drill.index(root), drill.index(parents))
        self.assertLess(drill.index(parents), drill.index(leaves))
        self.assertNotIn('mkdir -p "$source_storage', drill)

    def test_release_recovery_evidence_uses_the_same_ownership_contract(self) -> None:
        workflow = RELEASE_WORKFLOW.read_text(encoding="utf-8")
        drill = workflow.split(
            "- name: Real PostgreSQL and artifact backup/restore drill", 1
        )[1].split("- name: Rust, Web, TUI and operations contract gates", 1)[0]
        self.assertIn('runner_uid="$(id -u)"', drill)
        self.assertIn('runner_gid="$(id -g)"', drill)
        self.assertIn('--user "$runner_uid:$runner_gid"', drill)
        upload = workflow.split("- name: Upload recovery drill evidence", 1)[1]
        self.assertIn("if-no-files-found: error", upload)
        self.assertIn("include-hidden-files: true", upload)


if __name__ == "__main__":
    unittest.main()
