import tempfile
import unittest
from pathlib import Path, PurePosixPath

from build_wiki import WikiBuildError, WikiBuilder, slug_for


class WikiBuilderTests(unittest.TestCase):
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

            output = root / ".wiki-src"
            WikiBuilder(root, output, "owner/project", "main").build()

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

    def test_build_fails_when_a_local_target_is_missing(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            self.write(root / "README.md", "# Project\n\n[不存在](docs/missing.md)\n")
            self.write(root / "docs/README.md", "# 文档索引\n")

            with self.assertRaisesRegex(WikiBuildError, "link target does not exist"):
                WikiBuilder(root, root / ".wiki-src", "owner/project", "main").build()

    @staticmethod
    def write(path: Path, content: str) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    unittest.main()
