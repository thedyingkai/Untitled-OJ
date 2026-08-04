import pathlib
import unittest


ROOT = pathlib.Path(__file__).parents[3]
INSTALLER_MANIFEST = ROOT / "manager" / "installer" / "Cargo.toml"
INSTALLER_SOURCE = ROOT / "manager" / "installer" / "src" / "lib.rs"
PORTABLE_WORKFLOW = ROOT / ".github" / "workflows" / "orchestrator-portable.yml"


class PortableInstallTests(unittest.TestCase):
    def test_native_installer_is_the_only_root_install_surface(self) -> None:
        for forbidden in ("install.sh", "install.ps1", "install.bat", "install.cmd"):
            self.assertFalse((ROOT / forbidden).exists(), forbidden)
        manifest = INSTALLER_MANIFEST.read_text(encoding="utf-8")
        self.assertIn('name = "ojos-orchestrator-installer"', manifest)
        self.assertIn('name = "ojos-orchestrator"', manifest)

    def test_native_installer_enforces_complete_verified_payloads(self) -> None:
        source = INSTALLER_SOURCE.read_text(encoding="utf-8")
        for required in (
            "try_lock_exclusive",
            "InstallJournal",
            "JournalPhase::Prepared",
            "JournalPhase::OldMoved",
            "JournalPhase::NewPublished",
            "acquire_runtime_install_guard",
            "verify_manifest_files",
            "sha256_file",
            "validate_runtime_references",
            "local://",
            "migrations",
            "platform/shared/go",
            "WebView2Loader.dll",
            "manager/web/dist/index.html",
        ):
            self.assertIn(required, source)

    def test_portable_workflow_has_no_installer_script_or_release_infrastructure(self) -> None:
        workflow = PORTABLE_WORKFLOW.read_text(encoding="utf-8")
        for forbidden in (
            "install.sh",
            "install.ps1",
            ".cmd",
            "self-hosted",
            "orchestrator-capacity",
            "orchestrator-rc-signing",
            "azure/",
            "artifact-signing",
            "id-token:",
            ".msi",
            "Authenticode",
        ):
            self.assertNotIn(forbidden, workflow)
        self.assertIn("ojos-orchestrator", workflow)
        self.assertIn(" install ", workflow)
        self.assertIn(" verify ", workflow)
        self.assertIn("OJOS_DESKTOP_SMOKE", workflow)
        self.assertIn("upgrade succeeded while an installed daemon held the runtime lock", workflow)


if __name__ == "__main__":
    unittest.main()
