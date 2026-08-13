from __future__ import annotations

import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[3]
HARNESS_PATH = ROOT / "deploy" / "ops" / "contest-service-real-vertical-e2e.sh"
HARNESS = HARNESS_PATH.read_text(encoding="utf-8")


class ContestRealVerticalPolicyTests(unittest.TestCase):
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
            '    "$test_binary" --nocapture --test-threads=1', execution_start
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

    def test_privileged_cleanup_remains_confined_to_validated_mktemp_root(self) -> None:
        cleanup_start = HARNESS.index("cleanup() {")
        cleanup_end = HARNESS.index("\n}\ntrap cleanup EXIT", cleanup_start)
        cleanup = HARNESS[cleanup_start:cleanup_end]
        for marker in (
            'cleanup_root="$(realpath -e -- "$run_root" 2>/dev/null || true)"',
            '"$cleanup_root" == "$run_root"',
            '"$(dirname -- "$cleanup_root")" == "$run_parent"',
            '^ojos-contest-real-vertical\\.[A-Za-z0-9]{8}$',
            'sudo rm -rf -- "$cleanup_root"',
            "refusing unsafe privileged contest vertical cleanup",
        ):
            self.assertIn(marker, cleanup)


if __name__ == "__main__":
    unittest.main()
