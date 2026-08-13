import json
import pathlib
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.error
import urllib.request


ROOT = pathlib.Path(__file__).resolve().parents[3]
DRILL = ROOT / "deploy" / "ops" / "redis-recovery-drill.sh"
FIXTURE = ROOT / "deploy" / "ops" / "fixtures" / "redis_recovery_permission_fixture.py"


class RedisRecoveryDrillContractTests(unittest.TestCase):
    def test_drill_uses_remote_permission_checker_without_auth_schema(self) -> None:
        drill = DRILL.read_text(encoding="utf-8")
        self.assertIn("redis_recovery_permission_fixture.py", drill)
        self.assertIn('Endpoint: "$permission_endpoint"', drill)
        self.assertIn('AdminToken: "$permission_token"', drill)
        self.assertIn("/auth/admin/permission-check", FIXTURE.read_text(encoding="utf-8"))
        self.assertNotIn("auth-service/migrations", drill)
        self.assertNotIn("user_roles", drill)
        self.assertNotIn("role_bindings", drill)

    def test_permission_and_queue_evidence_remain_hard_gates(self) -> None:
        drill = DRILL.read_text(encoding="utf-8")
        self.assertIn("-H 'X-Auth-Verified: true'", drill)
        self.assertIn('"$permission_evidence" >/dev/null', drill)
        for marker in (
            '.authorization_verified == true',
            '.request.user_id == 1',
            '.request.permission == "judge.admin"',
            '.request.scope_type == "system"',
            '.decision == "allowed"',
            '.pending_count == 0',
            '.consumer_count > 0',
        ):
            self.assertIn(marker, drill)
        self.assertNotIn("continue-on-error", drill)


class RedisRecoveryPermissionFixtureTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        root = pathlib.Path(self.tempdir.name)
        self.ready = root / "ready.json"
        self.evidence = root / "permission.json"
        self.token = "fixture-secret"
        self.process = subprocess.Popen(
            [
                sys.executable,
                str(FIXTURE),
                "--host",
                "127.0.0.1",
                "--port",
                "0",
                "--token",
                self.token,
                "--ready-file",
                str(self.ready),
                "--evidence-file",
                str(self.evidence),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and not self.ready.exists():
            if self.process.poll() is not None:
                self.fail(f"fixture exited early: {self.process.stdout.read()}")
            time.sleep(0.02)
        self.assertTrue(self.ready.exists(), "fixture did not become ready")
        ready = json.loads(self.ready.read_text(encoding="utf-8"))
        self.url = f"http://127.0.0.1:{ready['port']}/auth/admin/permission-check"

    def tearDown(self) -> None:
        self.process.terminate()
        self.process.wait(timeout=5)
        if self.process.stdout is not None:
            self.process.stdout.close()
        self.tempdir.cleanup()

    def post(self, token: str, payload: dict) -> dict:
        request = urllib.request.Request(
            self.url,
            data=json.dumps(payload).encode("utf-8"),
            headers={
                "Authorization": f"Bearer {token}",
                "Content-Type": "application/json",
            },
            method="POST",
        )
        with urllib.request.urlopen(request, timeout=2) as response:
            return json.load(response)

    def test_fixture_allows_only_exact_authenticated_admin_check(self) -> None:
        with self.assertRaises(urllib.error.HTTPError) as unauthorized:
            self.post("wrong-token", {"user_id": 1, "permission": "judge.admin"})
        self.assertEqual(unauthorized.exception.code, 401)
        self.assertFalse(self.evidence.exists())

        denied = self.post(
            self.token,
            {
                "user_id": 1,
                "permission": "judge.submit",
                "scope_type": "system",
            },
        )
        self.assertFalse(denied["data"]["allowed"])

        allowed = self.post(
            self.token,
            {
                "user_id": 1,
                "permission": "judge.admin",
                "scope_type": "system",
            },
        )
        self.assertTrue(allowed["data"]["allowed"])
        evidence = json.loads(self.evidence.read_text(encoding="utf-8"))
        self.assertTrue(evidence["authorization_verified"])
        self.assertEqual(evidence["decision"], "allowed")
        self.assertEqual(evidence["request"]["permission"], "judge.admin")


if __name__ == "__main__":
    unittest.main()
