from __future__ import annotations

import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[3]
HARNESS_PATH = ROOT / "deploy" / "ops" / "contest-service-real-vertical-e2e.sh"
HARNESS = HARNESS_PATH.read_text(encoding="utf-8")


class ContestRealVerticalPolicyTests(unittest.TestCase):
    def test_run_root_uses_trusted_tmp_and_stays_private_until_handoff(self) -> None:
        initialization_end = HARNESS.index('scratch_root="$run_root/workload"')
        initialization = HARNESS[:initialization_end]
        for marker in (
            'run_parent="/tmp"',
            'run_parent_canonical="$(realpath -e -- "$run_parent"',
            '"$run_parent_canonical" != "$run_parent"',
            '! -d "$run_parent"',
            '-L "$run_parent"',
            '"$run_parent_contract" != "0:0:1777"',
            'mktemp -d -p "$run_parent" ojos-contest-real-vertical.XXXXXXXX',
            '"$(stat -c \'%u:%g:%a\' -- "$run_root")" != "$invoking_uid:$invoking_gid:700"',
        ):
            self.assertIn(marker, initialization)
        self.assertNotIn("RUNNER_TEMP", initialization)
        self.assertNotIn("${TMPDIR", initialization)

        private_check = HARNESS.index(
            '"$(stat -c \'%u:%g:%a\' -- "$run_root")" != "$invoking_uid:$invoking_gid:700"'
        )
        handoff = HARNESS.index('chmod 0755 "$run_root"')
        handoff_check = HARNESS.index(
            '"$(stat -c \'%u:%g:%a\' -- "$run_root")" != "$invoking_uid:$invoking_gid:755"'
        )
        self.assertLess(private_check, handoff)
        self.assertLess(handoff, handoff_check)

    def test_workload_output_preclean_is_runner_owned_and_path_constrained(self) -> None:
        self.assertNotIn('sudo -u "#$workload_uid"', HARNESS)
        mkdir = HARNESS.index(
            'mkdir -p "$scratch_root/home" "$scratch_root/tmp" "$output_root"'
        )
        preclean = HARNESS.index('rm -f -- "$evidence"', mkdir)
        chown = HARNESS.index(
            'sudo chown -R "$workload_uid:$workload_gid"', preclean
        )
        self.assertLess(mkdir, preclean)
        self.assertLess(preclean, chown)
        preclean_block = HARNESS[mkdir:chown]
        for marker in (
            '"$(dirname -- "$evidence")" != "$output_root"',
            '"$(basename -- "$evidence")" != "live-evidence.json"',
            "refusing unsafe contest vertical evidence preclean",
            'rm -f -- "$evidence"',
        ):
            self.assertIn(marker, preclean_block)
        self.assertNotIn("sudo rm", preclean_block)
        self.assertNotIn("setpriv", preclean_block)

    def test_real_test_keeps_exact_workload_identity_and_socket_group(self) -> None:
        home_marker = '  "HOME=$scratch_root/home"'
        execution_start = HARNESS.rindex(
            "sudo env -i", 0, HARNESS.index(home_marker)
        )
        execution_end = HARNESS.index(
            '      "$staged_test_binary" --nocapture --test-threads=1',
            execution_start,
        )
        execution = HARNESS[execution_start:execution_end]
        for marker in (
            '--reuid "$workload_uid" --regid "$workload_gid"',
            '--groups "$workload_groups"',
            "--bounding-set=-all --inh-caps=-all --ambient-caps=-all",
            "--no-new-privs",
        ):
            self.assertIn(marker, execution)
        self.assertIn("workload_uid=65532", HARNESS)
        self.assertIn("workload_gid=65532", HARNESS)
        self.assertNotIn("--umask", execution)
        self.assertIn(
            "/bin/bash -c 'umask 0022; exec \"$@\"' contest-vertical",
            execution,
        )
        self.assertNotIn('eval ', execution)
        self.assertNotIn(
            '      "$test_binary" --nocapture --test-threads=1', execution
        )

    def test_cargo_test_binary_is_identity_checked_and_staged_read_only(self) -> None:
        for marker in (
            'cargo metadata --no-deps --format-version=1',
            '"services/orchestrator/backend/tests/contest_service_real_vertical_e2e.rs"',
            'os.path.commonpath((target_root, resolved)) != target_root',
            'r"contest_service_real_vertical_e2e-[0-9a-f]{16,64}"',
            'if len(set(executables)) != 1',
            'staged_test_binary="$run_root/contest-service-real-vertical-e2e"',
            'install -m 0555 -- "$test_binary" "$staged_test_binary"',
            '"$(stat -c \'%a\' "$staged_test_binary")" != 555',
            'cmp -s -- "$test_binary" "$staged_test_binary"',
        ):
            self.assertIn(marker, HARNESS)
        self.assertNotIn("chmod -R", HARNESS)
        self.assertNotIn("chmod o+x", HARNESS)

    def test_privileged_cleanup_remains_confined_to_validated_mktemp_root(self) -> None:
        cleanup_start = HARNESS.index("cleanup() {")
        cleanup_end = HARNESS.index("\n}\ntrap cleanup EXIT", cleanup_start)
        cleanup = HARNESS[cleanup_start:cleanup_end]
        for marker in (
            'cleanup_root="$(realpath -e -- "$run_root" 2>/dev/null || true)"',
            '"$cleanup_root" == "$run_root"',
            '"$run_parent" == /tmp',
            '"$(realpath -e -- "$run_parent" 2>/dev/null || true)" == "$run_parent"',
            '"$(stat -c \'%u:%g:%a\' -- "$run_parent" 2>/dev/null || true)" == "0:0:1777"',
            '"$(dirname -- "$cleanup_root")" == "$run_parent"',
            '^ojos-contest-real-vertical\\.[A-Za-z0-9]{8}$',
            'sudo rm -rf -- "$cleanup_root"',
            "refusing unsafe privileged contest vertical cleanup",
        ):
            self.assertIn(marker, cleanup)


if __name__ == "__main__":
    unittest.main()
