import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest


OPS = Path(__file__).resolve().parents[1]
SCRIPT = OPS / "backup-manifest.py"
SPEC = importlib.util.spec_from_file_location("backup_manifest", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
backup_manifest = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(backup_manifest)


def add_bytes(bundle: tarfile.TarFile, name: str, contents: bytes, kind=None, linkname=""):
    member = tarfile.TarInfo(name)
    member.size = len(contents)
    if kind is not None:
        member.type = kind
        member.linkname = linkname
        member.size = 0
    bundle.addfile(member, io.BytesIO(contents) if member.isreg() else None)


class BackupManifestTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name) / "backup"
        self.root.mkdir()

    def tearDown(self):
        self.temporary.cleanup()

    def run_cli(self, *arguments, ok=True):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), *map(str, arguments)],
            capture_output=True,
            text=True,
            check=False,
        )
        if ok and result.returncode != 0:
            self.fail(f"command failed: {result.stderr}")
        if not ok and result.returncode == 0:
            self.fail(f"command unexpectedly passed: {result.stdout}")
        return result

    def make_fixture(self, *, redis=True, local=True, minio=True):
        for name in backup_manifest.DATABASES:
            self.write(f"postgres/{name}.dump", f"dump-{name}".encode())
            self.write(f"postgres/{name}.dump.list", f"list-{name}".encode())
        if redis:
            self.write("redis/dump.rdb", b"REDIS0011fixture")
            self.write("redis/dump.rdb.check", b"RDB looks OK\n")
        source = Path(self.temporary.name) / "source"
        source.mkdir()
        self.write_at(source / "a.txt", b"alpha")
        self.write_at(source / "nested" / "b.bin", b"\x00\x01\x02")
        if local:
            archive = self.root / "storage/storage-root.tar.gz"
            archive.parent.mkdir(parents=True, exist_ok=True)
            with tarfile.open(archive, "w:gz") as bundle:
                add_bytes(bundle, "./a.txt", b"alpha")
                add_bytes(bundle, "./nested/b.bin", b"\x00\x01\x02")
        buckets = []
        if minio:
            buckets = ["problems", "submissions"]
            self.write("storage/minio/problems/object-a", b"problem")
            self.write("storage/minio/submissions/one/result.json", b"{}")
        retained = Path(self.temporary.name) / "problem-retained-volume"
        retained.mkdir()
        self.write_at(retained / "live" / "package.yaml", b"id: 42\nrevision: 7\n")
        retained_inventory = backup_manifest.tree_inventory(retained)
        self.write(
            "retained/problem-packages.inventory.json",
            (json.dumps(retained_inventory, indent=2, sort_keys=True) + "\n").encode(),
        )
        archive = self.root / "retained/problem-packages.tar.gz"
        archive.parent.mkdir(parents=True, exist_ok=True)
        with tarfile.open(archive, "w:gz") as bundle:
            bundle.add(retained / "live" / "package.yaml", arcname="./live/package.yaml")
        owner = "service-instance-problem"
        name_digest = hashlib.sha256(
            f"{owner}\0problem-service\0problem-packages".encode()
        ).hexdigest()
        stable_identity = {
            "schema_version": backup_manifest.RETAINED_IDENTITY_SCHEMA,
            "volume_name": f"ojos-retain-{name_digest[:32]}",
            "driver": "local",
            "scope": "local",
            "labels": {
                "ojos.managed_by": "orchestrator-agent",
                "ojos.service_id": "problem-service",
                "ojos.runtime_profile_sha256": backup_manifest.STANDARD_PROFILE_SHA256,
                "ojos.volume_logical_name": "problem-packages",
                "ojos.volume_lifecycle": "retain",
                "ojos.owner_instance_id": owner,
                "ojos.volume_target": "/data/ojos/problems",
            },
        }
        stable_identity["identity_sha256"] = hashlib.sha256(
            json.dumps(stable_identity, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()
        self.write(
            "retained/problem-packages.identity.json",
            (json.dumps(stable_identity, indent=2, sort_keys=True) + "\n").encode(),
        )
        self.retained_source = retained
        arguments = [
            "create",
            "--root",
            self.root,
            "--environment",
            "production",
            "--source-id",
            "source-a",
            "--created-at",
            "2026-08-12T12:34:56Z",
            "--fence-id-sha256",
            "a" * 64,
            "--redis",
            str(redis).lower(),
            "--local-storage",
            str(local).lower(),
            "--minio",
            str(minio).lower(),
            "--buckets-json",
            json.dumps(buckets),
            "--problem-retained-volume-source",
            retained,
        ]
        if local:
            arguments += ["--local-storage-source", source]
        self.run_cli(*arguments)
        self.write_checksums()
        return source

    def write(self, relative, contents):
        self.write_at(self.root / relative, contents)

    @staticmethod
    def write_at(path, contents):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(contents)

    def write_checksums(self):
        lines = []
        for path in sorted(path for path in self.root.rglob("*") if path.is_file()):
            if path.name == "SHA256SUMS":
                continue
            relative = path.relative_to(self.root).as_posix()
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            lines.append(f"{digest}  {relative}\n")
        (self.root / "SHA256SUMS").write_text("".join(lines), encoding="utf-8")

    def verify(self, *, ok=True, source="source-a", environment="production"):
        return self.run_cli(
            "verify",
            "--root",
            self.root,
            "--environment",
            environment,
            "--expected-source-id",
            source,
            ok=ok,
        )

    def test_create_and_verify_full_manifest(self):
        self.make_fixture()
        self.verify()
        value = json.loads((self.root / "manifest.json").read_text(encoding="utf-8"))
        self.assertEqual(value["schema_version"], backup_manifest.SCHEMA_VERSION)
        self.assertEqual(value["source_id"], "source-a")
        self.assertEqual(
            [item["name"] for item in value["components"]["postgres"]["databases"]],
            list(backup_manifest.DATABASES),
        )
        listed = {item["path"] for item in value["payload_files"]}
        self.assertNotIn("manifest.json", listed)
        self.assertNotIn("SHA256SUMS", listed)
        checksums = (self.root / "SHA256SUMS").read_text(encoding="utf-8")
        self.assertIn("  manifest.json\n", checksums)
        self.assertNotIn("  SHA256SUMS\n", checksums)

    def test_explicit_redis_and_storage_exclusions(self):
        self.make_fixture(redis=False, local=False, minio=False)
        self.verify()
        components = json.loads((self.root / "manifest.json").read_text())["components"]
        self.assertEqual(components["redis"]["excluded_reason"], "explicitly_excluded")
        self.assertEqual(components["storage"]["local"]["excluded_reason"], "explicitly_excluded")
        self.assertEqual(components["storage"]["minio"]["excluded_reason"], "explicitly_excluded")

    def test_source_and_environment_are_strict(self):
        self.make_fixture()
        self.assertIn("source_id mismatch", self.verify(ok=False, source="source-b").stderr)
        self.assertIn("environment mismatch", self.verify(ok=False, environment="staging").stderr)

    def test_payload_tamper_extra_and_missing_are_rejected(self):
        for mutation in ("tamper", "extra", "missing"):
            with self.subTest(mutation=mutation):
                self.tearDown()
                self.setUp()
                self.make_fixture()
                if mutation == "tamper":
                    self.write("postgres/auth.dump", b"tampered")
                elif mutation == "extra":
                    self.write("unexpected.txt", b"extra")
                else:
                    (self.root / "postgres/auth.dump").unlink()
                self.verify(ok=False)

    def test_manifest_component_declaration_cannot_be_patched_in_isolation(self):
        self.make_fixture()
        path = self.root / "manifest.json"
        value = json.loads(path.read_text())
        value["components"]["redis"]["included"] = False
        path.write_text(json.dumps(value), encoding="utf-8")
        self.write_checksums()
        self.assertIn("excluded Redis", self.verify(ok=False).stderr)

    def test_retained_volume_identity_inventory_and_archive_are_strict(self):
        for mutation in ("identity", "inventory", "archive"):
            with self.subTest(mutation=mutation):
                self.tearDown()
                self.setUp()
                self.make_fixture()
                if mutation == "identity":
                    path = self.root / "retained/problem-packages.identity.json"
                    value = json.loads(path.read_text())
                    value["labels"]["ojos.owner_instance_id"] = "foreign-instance"
                    path.write_text(json.dumps(value), encoding="utf-8")
                elif mutation == "inventory":
                    path = self.root / "retained/problem-packages.inventory.json"
                    value = json.loads(path.read_text())
                    value["files"][0]["bytes"] += 1
                    path.write_text(json.dumps(value), encoding="utf-8")
                else:
                    path = self.root / "retained/problem-packages.tar.gz"
                    path.write_bytes(path.read_bytes() + b"tamper")
                self.write_checksums()
                self.verify(ok=False)

    def test_retained_archive_path_traversal_and_symlink_are_rejected(self):
        self.make_fixture()
        path = self.root / "retained/problem-packages.tar.gz"
        for member_name, kind in (("../escape", None), ("unsafe-link", tarfile.SYMTYPE)):
            with self.subTest(member=member_name):
                with tarfile.open(path, "w:gz") as bundle:
                    add_bytes(bundle, member_name, b"x", kind, "target")
                self.write_checksums()
                self.verify(ok=False)

    def test_checksums_require_exact_set_and_safe_unique_paths(self):
        cases = {
            "missing": lambda lines: lines[:-1],
            "duplicate": lambda lines: lines + [lines[0]],
            "escape": lambda lines: lines + [f"{'0' * 64}  ../escape\n"],
            "absolute": lambda lines: lines + [f"{'0' * 64}  /etc/passwd\n"],
            "self": lambda lines: lines + [f"{'0' * 64}  SHA256SUMS\n"],
        }
        for name, mutate in cases.items():
            with self.subTest(name=name):
                self.tearDown()
                self.setUp()
                self.make_fixture()
                path = self.root / "SHA256SUMS"
                lines = path.read_text().splitlines(keepends=True)
                path.write_text("".join(mutate(lines)), encoding="utf-8")
                self.verify(ok=False)

    def test_create_requires_local_source_and_exact_archive(self):
        for mode in ("missing-source", "different-archive"):
            with self.subTest(mode=mode):
                self.tearDown()
                self.setUp()
                for name in backup_manifest.DATABASES:
                    self.write(f"postgres/{name}.dump", b"d")
                    self.write(f"postgres/{name}.dump.list", b"l")
                source = Path(self.temporary.name) / "source"
                self.write_at(source / "a", b"a")
                archive = self.root / "storage/storage-root.tar.gz"
                archive.parent.mkdir(parents=True, exist_ok=True)
                with tarfile.open(archive, "w:gz") as bundle:
                    add_bytes(bundle, "a", b"different")
                args = [
                    "create", "--root", self.root, "--environment", "production",
                    "--source-id", "source-a", "--created-at", "2026-08-12T00:00:00Z",
                    "--fence-id-sha256", "a" * 64, "--redis", "false",
                    "--local-storage", "true", "--minio", "false", "--buckets-json", "[]",
                ]
                if mode == "different-archive":
                    args += ["--local-storage-source", source]
                self.run_cli(*args, ok=False)

    def test_verify_tar_and_tree_round_trip(self):
        source = self.make_fixture()
        manifest = json.loads((self.root / "manifest.json").read_text())
        expected = json.dumps(manifest["components"]["storage"]["local"]["tree"])
        tar_result = self.run_cli(
            "verify-tar", "--archive", self.root / "storage/storage-root.tar.gz",
            "--expected-summary-json", expected,
        )
        tree_result = self.run_cli(
            "verify-tree", "--root", source, "--expected-summary-json", expected,
        )
        self.assertEqual(json.loads(tar_result.stdout), json.loads(tree_result.stdout))
        wrong = json.loads(expected)
        wrong["bytes"] += 1
        self.run_cli(
            "verify-tree", "--root", source,
            "--expected-summary-json", json.dumps(wrong), ok=False,
        )

    def test_verify_inventory_detects_per_file_substitution(self):
        self.make_fixture()
        inventory = self.root / "retained/problem-packages.inventory.json"
        self.run_cli(
            "verify-inventory", "--root", self.retained_source,
            "--inventory", inventory,
        )
        path = self.retained_source / "live/package.yaml"
        original = path.read_bytes()
        path.write_bytes(b"x" * len(original))
        self.run_cli(
            "verify-inventory", "--root", self.retained_source,
            "--inventory", inventory, ok=False,
        )

    def test_unsafe_tar_members_are_rejected(self):
        cases = [
            ("../escape", None, ""),
            ("/absolute", None, ""),
            ("C:/drive", None, ""),
            ("link", tarfile.SYMTYPE, "target"),
            ("hard", tarfile.LNKTYPE, "target"),
            ("device", tarfile.CHRTYPE, ""),
            ("fifo", tarfile.FIFOTYPE, ""),
        ]
        expected_empty = json.dumps(
            {"regular_files": 0, "bytes": 0, "sha256": hashlib.sha256().hexdigest()}
        )
        for name, kind, link in cases:
            with self.subTest(name=name, kind=kind):
                archive = Path(self.temporary.name) / "unsafe.tar"
                with tarfile.open(archive, "w") as bundle:
                    add_bytes(bundle, name, b"x", kind, link)
                self.run_cli(
                    "verify-tar", "--archive", archive,
                    "--expected-summary-json", expected_empty, ok=False,
                )

    def test_tar_duplicate_and_tree_symlink_are_rejected(self):
        archive = Path(self.temporary.name) / "duplicate.tar"
        with tarfile.open(archive, "w") as bundle:
            add_bytes(bundle, "a", b"one")
            add_bytes(bundle, "./a", b"two")
        expected = json.dumps({"regular_files": 2, "bytes": 6, "sha256": "0" * 64})
        self.run_cli(
            "verify-tar", "--archive", archive,
            "--expected-summary-json", expected, ok=False,
        )

        tree = Path(self.temporary.name) / "tree"
        tree.mkdir()
        (tree / "target").write_bytes(b"target")
        try:
            (tree / "link").symlink_to("target")
        except (OSError, NotImplementedError):
            self.skipTest("symbolic links unavailable on this platform")
        self.run_cli(
            "verify-tree", "--root", tree,
            "--expected-summary-json", expected, ok=False,
        )


if __name__ == "__main__":
    unittest.main()
