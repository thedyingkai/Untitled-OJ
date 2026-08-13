from __future__ import annotations

import json
import os
import pathlib
import shutil
import subprocess
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[3]
MISSING = object()
LOAD_DRILL = (ROOT / "deploy" / "ops" / "basic-load-soak.sh").read_text(
    encoding="utf-8"
)


def shell_function(name: str) -> str:
    marker = f"{name}() {{\n"
    start = LOAD_DRILL.index(marker)
    end = LOAD_DRILL.index("\n}\n", start) + len("\n}\n")
    return LOAD_DRILL[start:end]


def bash_executable() -> str:
    if os.name != "nt":
        executable = shutil.which("bash")
        if executable:
            return executable
        raise AssertionError("bash is required for the basic load gate tests")

    candidates: list[pathlib.Path] = []
    jq = shutil.which("jq")
    if jq:
        candidates.append(pathlib.Path(jq).resolve().with_name("bash.exe"))
    git = shutil.which("git")
    if git:
        git_path = pathlib.Path(git).resolve()
        candidates.extend(
            [
                git_path.with_name("bash.exe"),
                git_path.parent.parent / "bin" / "bash.exe",
                git_path.parent.parent / "usr" / "bin" / "bash.exe",
            ]
        )
    program_files = pathlib.Path(os.environ.get("ProgramFiles", r"C:\Program Files"))
    candidates.append(program_files / "Git" / "bin" / "bash.exe")
    for candidate in candidates:
        if candidate.is_file():
            return str(candidate)
    raise AssertionError("Git Bash or MSYS2 bash is required for the basic load gate tests")


class BasicLoadSoakGateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.bash = bash_executable()
        cls.gate = shell_function("validate_load_gate")

    def run_gate(
        self,
        *,
        judge_total: int = 1,
        judge_ok: int = 1,
        worker_processed_count: object = 1,
        final_pending_count: object = 0,
        queue_pending_max: int = 0,
        success_rate: object = 1.0,
    ) -> subprocess.CompletedProcess[str]:
        metrics: dict[str, object] = {
            "by_operation": {
                "judge-submit": {"total": judge_total, "ok": judge_ok}
            },
            "queue_pending_max": queue_pending_max,
        }
        if success_rate is not MISSING:
            metrics["success_rate"] = success_rate
        if worker_processed_count is not MISSING:
            metrics["worker_processed_count"] = worker_processed_count
        queue_after: dict[str, object] = {}
        if final_pending_count is not MISSING:
            queue_after["pending_count"] = final_pending_count
        script = f"""\
set -Eeuo pipefail
{self.gate}
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT
printf '%s\\n' "$METRICS_JSON" >"$fixture_dir/metrics.json"
printf '%s\\n' "$QUEUE_AFTER_JSON" >"$fixture_dir/queue-after.json"
validate_load_gate "$fixture_dir/metrics.json" "$fixture_dir/queue-after.json" 0.95
"""
        env = os.environ.copy()
        env["METRICS_JSON"] = json.dumps(metrics, separators=(",", ":"))
        env["QUEUE_AFTER_JSON"] = json.dumps(queue_after, separators=(",", ":"))
        bash_dir = str(pathlib.Path(self.bash).parent)
        env["PATH"] = bash_dir + os.pathsep + env.get("PATH", "")
        return subprocess.run(
            [self.bash, "-c", script],
            cwd=ROOT,
            env=env,
            check=False,
            capture_output=True,
            text=True,
        )

    def test_gate_accepts_completed_judge_work_with_transient_pending(self) -> None:
        result = self.run_gate(queue_pending_max=4)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_gate_rejects_failed_judge_submit_even_above_aggregate_threshold(self) -> None:
        result = self.run_gate(judge_ok=0, success_rate=0.97561)
        self.assertNotEqual(result.returncode, 0)

    def test_gate_rejects_zero_worker_result_delta(self) -> None:
        result = self.run_gate(worker_processed_count=0)
        self.assertNotEqual(result.returncode, 0)

    def test_gate_rejects_final_pending_work(self) -> None:
        result = self.run_gate(final_pending_count=1, queue_pending_max=1)
        self.assertNotEqual(result.returncode, 0)

    def test_gate_rejects_malformed_success_rate(self) -> None:
        for value in (MISSING, None, "garbage", [], {}):
            with self.subTest(value=value):
                result = self.run_gate(success_rate=value)
                self.assertNotEqual(result.returncode, 0)

    def test_gate_rejects_malformed_worker_result_delta(self) -> None:
        for value in (MISSING, None, "garbage", [], {}):
            with self.subTest(value=value):
                result = self.run_gate(worker_processed_count=value)
                self.assertNotEqual(result.returncode, 0)

    def test_gate_rejects_malformed_final_pending_count(self) -> None:
        for value in (MISSING, None, "garbage", [], {}):
            with self.subTest(value=value):
                result = self.run_gate(final_pending_count=value)
                self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
