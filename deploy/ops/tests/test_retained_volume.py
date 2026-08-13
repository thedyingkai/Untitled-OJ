import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


OPS = Path(__file__).resolve().parents[1]
SCRIPT = OPS / "retained-volume.py"
PROFILE = "sha256:56c8ec1e421205dbebb97ad40cbda30bf468d198dd8c3fc50151e39465ea573f"


class RetainedVolumeIdentityTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve() / "volume"
        self.root.mkdir()
        self.owner = "service-instance-problem"
        digest = hashlib.sha256(
            f"{self.owner}\0problem-service\0problem-packages".encode()
        ).hexdigest()
        self.name = "ojos-retain-" + digest[:32]

    def tearDown(self):
        self.temporary.cleanup()

    def inspect(self, **mutations):
        value = {
            "Name": self.name,
            "Driver": "local",
            "Mountpoint": self.root.as_posix(),
            "Scope": "local",
            "Options": {},
            "Labels": {
                "ojos.managed_by": "orchestrator-agent",
                "ojos.service_id": "problem-service",
                "ojos.runtime_profile_sha256": PROFILE,
                "ojos.volume_logical_name": "problem-packages",
                "ojos.volume_lifecycle": "retain",
                "ojos.owner_instance_id": self.owner,
                "ojos.volume_target": "/data/ojos/problems",
            },
        }
        for path, replacement in mutations.items():
            if path.startswith("label_"):
                value["Labels"][path.removeprefix("label_").replace("__", ".")] = replacement
            else:
                value[path] = replacement
        inspect = Path(self.temporary.name) / f"inspect-{hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()[:12]}.json"
        inspect.write_text(json.dumps([value]), encoding="utf-8")
        return inspect

    def run_cli(self, inspect, *, ok=True, root=None, owner=None):
        command = [
            sys.executable,
            str(SCRIPT),
            "--inspect",
            str(inspect),
            "--owner-instance-id",
            owner or self.owner,
            "--root",
            str(root or self.root),
        ]
        result = subprocess.run(command, capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode == 0, ok, result.stderr)
        return result

    def test_exact_agent_identity_and_derived_name_pass(self):
        value = json.loads(self.run_cli(self.inspect()).stdout)
        self.assertEqual(value["volume_name"], self.name)
        self.assertRegex(value["identity_sha256"], r"^[0-9a-f]{64}$")

    def test_foreign_label_wrong_owner_and_wrong_mountpoint_fail_closed(self):
        cases = [
            (self.inspect(label_ojos__service_id="other-service"), self.root, self.owner),
            (self.inspect(), self.root, "different-owner"),
        ]
        other = Path(self.temporary.name) / "other"
        other.mkdir()
        cases.append((self.inspect(), other, self.owner))
        for inspect, root, owner in cases:
            with self.subTest(root=root, owner=owner):
                self.run_cli(inspect, ok=False, root=root, owner=owner)

    def test_symlink_root_is_rejected(self):
        link = Path(self.temporary.name) / "volume-link"
        try:
            link.symlink_to(self.root, target_is_directory=True)
        except (OSError, NotImplementedError):
            self.skipTest("directory symlinks unavailable")
        self.run_cli(self.inspect(Mountpoint=link.as_posix()), ok=False, root=link)


if __name__ == "__main__":
    unittest.main()
