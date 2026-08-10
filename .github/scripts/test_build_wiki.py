import tempfile
import unittest
from pathlib import Path, PurePosixPath

from build_wiki import WikiBuildError, WikiBuilder, slug_for


class WikiBuilderTests(unittest.TestCase):
    COMMIT_SHA = "a" * 40
    RUN_URL = "https://github.com/owner/project/actions/runs/123456"

    def test_slug_for_flattens_nested_document_path(self) -> None:
        self.assertEqual(
            slug_for(PurePosixPath("orchestrator/web-ui.md")), "Orchestrator-Web-ui"
        )

    def test_build_rewrites_wiki_and_repository_links(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            self.write(
                root / "README.md",
                "# Project\n\n[文档索引](docs/README.md)\n[开始](docs/guide/start.md)\n"
                "[运维脚本](deploy/ops/README.md)\n",
            )
            self.write(
                root / "docs/README.md", "# 文档索引\n\n[开始](guide/start.md)\n"
            )
            self.write(
                root / "docs/guide/start.md",
                "# 开始\n\n[下一步](next.md#part)\n\n```markdown\n[示例](missing.md)\n```\n",
            )
            self.write(root / "docs/guide/next.md", "# 下一步\n\n## Part\n")
            self.write(root / "deploy/ops/README.md", "# 运维脚本\n")

            self.write_required_external_pages(root)
            output = root / ".wiki-src"
            self.builder(root, output).build()

            home = (output / "Home.md").read_text(encoding="utf-8")
            start = (output / "Guide-Start.md").read_text(encoding="utf-8")
            self.assertIn("[文档索引](Docs)", home)
            self.assertIn("[开始](Guide-Start)", home)
            self.assertIn(
                "[运维脚本](https://github.com/owner/project/blob/main/deploy/ops/README.md)",
                home,
            )
            self.assertIn("[下一步](Guide-Next#part)", start)
            self.assertIn("[示例](missing.md)", start)
            self.assertTrue((output / "Docs.md").is_file())
            self.assertTrue((output / "Deployment-Cross-machine-v2.md").is_file())
            self.assertTrue((output / "Deployment-Judge-worker.md").is_file())
            self.assertTrue((output / "SDK-Service-context.md").is_file())
            cross_machine = (output / "Deployment-Cross-machine-v2.md").read_text(
                encoding="utf-8"
            )
            self.assertIn("[Worker](Deployment-Judge-worker)", cross_machine)

            identity = (output / "Source-Identity.md").read_text(encoding="utf-8")
            self.assertIn("- Repository: `owner/project`", identity)
            self.assertIn("- Branch: `main`", identity)
            self.assertIn(f"- Commit SHA: `{self.COMMIT_SHA}`", identity)
            self.assertIn(f"- Workflow run: <{self.RUN_URL}>", identity)

            sidebar = (output / "_Sidebar.md").read_text(encoding="utf-8")
            for slug in (
                "Source-Identity",
                "Deployment-Cross-machine-v2",
                "Deployment-Judge-worker",
                "SDK-Service-context",
            ):
                self.assertIn(f"]({slug})", sidebar)

    def test_build_fails_when_a_local_target_is_missing(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            self.write(root / "README.md", "# Project\n\n[不存在](docs/missing.md)\n")
            self.write(root / "docs/README.md", "# 文档索引\n")

            self.write_required_external_pages(root)
            with self.assertRaisesRegex(WikiBuildError, "link target does not exist"):
                self.builder(root, root / ".wiki-src").build()

    def test_build_fails_when_an_explicit_external_source_is_missing(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            self.write(root / "README.md", "# Project\n")
            self.write(root / "deploy/cross-machine/README.md", "# Cross machine\n")
            self.write(root / "deploy/worker/README.md", "# Worker\n")

            with self.assertRaisesRegex(
                WikiBuildError, "explicit Wiki source is missing: sdk/service-sdk/README.md"
            ):
                self.builder(root, root / ".wiki-src").build()

    def test_source_identity_rejects_noncanonical_inputs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            with self.assertRaisesRegex(WikiBuildError, "exactly 40 hexadecimal"):
                WikiBuilder(
                    root,
                    root / ".wiki-src",
                    "owner/project",
                    "main",
                    "abc123",
                    self.RUN_URL,
                )
            with self.assertRaisesRegex(WikiBuildError, "absolute HTTPS URL"):
                WikiBuilder(
                    root,
                    root / ".wiki-src",
                    "owner/project",
                    "main",
                    self.COMMIT_SHA,
                    "http://github.com/owner/project/actions/runs/1",
                )

    def test_workflow_tracks_sources_passes_identity_and_reads_remote_back(self) -> None:
        workflow = (
            Path(__file__).resolve().parents[1] / "workflows" / "sync-wiki.yml"
        ).read_text(encoding="utf-8")
        for source in (
            "deploy/cross-machine/README.md",
            "deploy/worker/README.md",
            "sdk/service-sdk/README.md",
        ):
            self.assertIn(f'      - "{source}"', workflow)
        self.assertIn('--commit-sha "${GITHUB_SHA}"', workflow)
        self.assertIn("${GITHUB_RUN_ID}", workflow)
        self.assertIn('git ls-remote "$wiki_url" HEAD', workflow)
        self.assertIn('git clone --depth 1 "$wiki_url" "$readback_dir"', workflow)
        self.assertIn("Source-Identity.md", workflow)

    def builder(self, root: Path, output: Path) -> WikiBuilder:
        return WikiBuilder(
            root,
            output,
            "owner/project",
            "main",
            self.COMMIT_SHA,
            self.RUN_URL,
        )

    def write_required_external_pages(self, root: Path) -> None:
        self.write(
            root / "deploy/cross-machine/README.md",
            "# Cross-machine deployment\n\n[Worker](../worker/README.md)\n",
        )
        self.write(root / "deploy/worker/README.md", "# Judge worker deployment\n")
        self.write(root / "sdk/service-sdk/README.md", "# Service context SDK\n")

    @staticmethod
    def write(path: Path, content: str) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    unittest.main()
