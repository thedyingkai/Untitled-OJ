#!/usr/bin/env python3
"""Build the flat GitHub Wiki mirror and validate every generated link."""

from __future__ import annotations

import argparse
import posixpath
import re
import shutil
from pathlib import Path, PurePosixPath
from urllib.parse import quote, unquote, urlsplit


INLINE_LINK_RE = re.compile(
    r"(?P<prefix>!?\[[^\]\n]*\]\()(?P<target>[^)\n]+)(?P<suffix>\))"
)
REFERENCE_LINK_RE = re.compile(
    r"(?m)^(?P<prefix>[ \t]{0,3}\[[^\]\n]+\]:[ \t]*)(?P<target>\S+)(?P<suffix>.*)$"
)
FENCE_RE = re.compile(r"^[ \t]{0,3}(?P<marker>`{3,}|~{3,})")
FULL_COMMIT_SHA_RE = re.compile(r"^[0-9a-fA-F]{40}$")
REPOSITORY_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")

EXTERNAL_PAGES = (
    (PurePosixPath("deploy/cross-machine/README.md"), "Deployment-Cross-machine-v2"),
    (PurePosixPath("deploy/worker/README.md"), "Deployment-Judge-worker"),
    (PurePosixPath("sdk/service-sdk/README.md"), "SDK-Service-context"),
)


class WikiBuildError(RuntimeError):
    pass


def slug_for(relative_doc: PurePosixPath) -> str:
    """Convert docs/foo/bar.md to the flat Wiki slug Foo-Bar."""
    without_suffix = relative_doc.with_suffix("")
    parts = []
    for part in without_suffix.parts:
        parts.append(part[:1].upper() + part[1:])
    return "-".join(parts)


def split_target(raw_target: str) -> tuple[str, str]:
    """Split a Markdown destination from its optional title."""
    if raw_target.startswith("<"):
        closing = raw_target.find(">")
        if closing != -1:
            return raw_target[1:closing], raw_target[closing + 1 :]

    match = re.match(r"([^\s]+)(.*)", raw_target, re.DOTALL)
    if not match:
        return raw_target, ""
    return match.group(1), match.group(2)


def fenced_segments(text: str) -> list[tuple[bool, str]]:
    """Split Markdown into fenced-code and ordinary text segments."""
    segments: list[tuple[bool, str]] = []
    ordinary: list[str] = []
    fenced: list[str] = []
    active_marker: str | None = None

    def flush(buffer: list[str], is_fenced: bool) -> None:
        if buffer:
            segments.append((is_fenced, "".join(buffer)))
            buffer.clear()

    for line in text.splitlines(keepends=True):
        match = FENCE_RE.match(line)
        if active_marker is None:
            if match:
                flush(ordinary, False)
                active_marker = match.group("marker")
                fenced.append(line)
            else:
                ordinary.append(line)
            continue

        fenced.append(line)
        if (
            match
            and match.group("marker")[0] == active_marker[0]
            and len(match.group("marker")) >= len(active_marker)
        ):
            flush(fenced, True)
            active_marker = None

    if active_marker is None:
        flush(ordinary, False)
    else:
        flush(fenced, True)
    return segments


class WikiBuilder:
    def __init__(
        self,
        root: Path,
        output: Path,
        repository: str,
        branch: str,
        commit_sha: str,
        run_url: str,
    ) -> None:
        self.root = root.resolve()
        self.output = output.resolve()
        self.repository = repository.strip("/")
        self.branch = branch.strip()
        self.commit_sha = commit_sha.strip().lower()
        self.run_url = run_url.strip()
        if not REPOSITORY_RE.fullmatch(self.repository):
            raise WikiBuildError("repository must use the owner/name form")
        if not self.branch:
            raise WikiBuildError("branch must not be empty")
        if not FULL_COMMIT_SHA_RE.fullmatch(self.commit_sha):
            raise WikiBuildError("commit SHA must contain exactly 40 hexadecimal characters")
        parsed_run_url = urlsplit(self.run_url)
        if (
            parsed_run_url.scheme != "https"
            or not parsed_run_url.netloc
            or parsed_run_url.username is not None
            or parsed_run_url.password is not None
        ):
            raise WikiBuildError("run URL must be an absolute HTTPS URL without credentials")
        self.page_for_source: dict[PurePosixPath, str] = {}
        self.rewritten_links = 0
        self.validated_links = 0

    def discover_pages(self) -> list[tuple[Path, PurePosixPath, str]]:
        pages: list[tuple[Path, PurePosixPath, str]] = []
        readme = self.root / "README.md"
        if not readme.is_file():
            raise WikiBuildError("README.md is missing")

        self.page_for_source[PurePosixPath("README.md")] = "Home"
        pages.append((readme, PurePosixPath("README.md"), "Home"))

        docs_root = self.root / "docs"
        docs = sorted(
            docs_root.rglob("*.md"),
            key=lambda path: path.relative_to(docs_root).as_posix(),
        )
        for source in docs:
            relative_doc = PurePosixPath(source.relative_to(docs_root).as_posix())
            source_path = PurePosixPath("docs") / relative_doc
            slug = (
                "Docs"
                if relative_doc == PurePosixPath("README.md")
                else slug_for(relative_doc)
            )
            self.page_for_source[source_path] = slug
            pages.append((source, source_path, slug))

        for source_path, slug in EXTERNAL_PAGES:
            source = self.root.joinpath(*source_path.parts)
            if not source.is_file():
                raise WikiBuildError(f"explicit Wiki source is missing: {source_path}")
            self.page_for_source[source_path] = slug
            pages.append((source, source_path, slug))

        collisions: dict[str, list[PurePosixPath]] = {}
        for _, source_path, slug in pages:
            collisions.setdefault(slug.casefold(), []).append(source_path)
        duplicates = {
            slug: paths for slug, paths in collisions.items() if len(paths) > 1
        }
        if duplicates:
            details = "; ".join(
                f"{slug}: {', '.join(map(str, paths))}"
                for slug, paths in duplicates.items()
            )
            raise WikiBuildError(f"Wiki slug collision: {details}")
        return pages

    def source_identity(self) -> str:
        return "\n".join(
            [
                "# Source Identity",
                "",
                f"- Repository: `{self.repository}`",
                f"- Branch: `{self.branch}`",
                f"- Commit SHA: `{self.commit_sha}`",
                f"- Workflow run: <{self.run_url}>",
                "",
            ]
        )

    def resolve_target(
        self, target: str, source_path: PurePosixPath, *, is_image: bool
    ) -> str:
        parsed = urlsplit(target)
        if parsed.scheme or parsed.netloc or target.startswith(("#", "//")):
            return target
        if not parsed.path:
            return target

        decoded_path = unquote(parsed.path).replace("\\", "/")
        if decoded_path.startswith("/"):
            return target

        normalized = posixpath.normpath(str(source_path.parent / decoded_path))
        if normalized == ".." or normalized.startswith("../"):
            raise WikiBuildError(
                f"{source_path}: link escapes the repository: {target}"
            )
        repository_path = PurePosixPath(normalized)

        if repository_path in self.page_for_source:
            rewritten = self.page_for_source[repository_path]
        else:
            local_target = self.root.joinpath(*repository_path.parts)
            if not local_target.exists():
                raise WikiBuildError(
                    f"{source_path}: link target does not exist: {target}"
                )

            encoded_repo = quote(self.repository, safe="/")
            encoded_branch = quote(self.branch, safe="")
            encoded_path = quote(repository_path.as_posix(), safe="/")
            if is_image and local_target.is_file():
                rewritten = f"https://raw.githubusercontent.com/{encoded_repo}/{encoded_branch}/{encoded_path}"
            else:
                view = "tree" if local_target.is_dir() else "blob"
                rewritten = f"https://github.com/{encoded_repo}/{view}/{encoded_branch}/{encoded_path}"

        if parsed.query:
            rewritten += f"?{parsed.query}"
        if parsed.fragment:
            rewritten += f"#{parsed.fragment}"
        return rewritten

    def rewrite_markdown(self, text: str, source_path: PurePosixPath) -> str:
        def rewrite_inline(match: re.Match[str]) -> str:
            raw_target = match.group("target")
            destination, title = split_target(raw_target)
            is_image = match.group("prefix").startswith("!")
            rewritten = self.resolve_target(destination, source_path, is_image=is_image)
            if rewritten != destination:
                self.rewritten_links += 1
            return f"{match.group('prefix')}{rewritten}{title}{match.group('suffix')}"

        def rewrite_reference(match: re.Match[str]) -> str:
            raw_target = match.group("target")
            wrapped = raw_target.startswith("<") and raw_target.endswith(">")
            destination = raw_target[1:-1] if wrapped else raw_target
            rewritten = self.resolve_target(destination, source_path, is_image=False)
            if rewritten != destination:
                self.rewritten_links += 1
            if wrapped:
                rewritten = f"<{rewritten}>"
            return f"{match.group('prefix')}{rewritten}{match.group('suffix')}"

        output: list[str] = []
        for is_fenced, segment in fenced_segments(text):
            if is_fenced:
                output.append(segment)
                continue
            segment = INLINE_LINK_RE.sub(rewrite_inline, segment)
            segment = REFERENCE_LINK_RE.sub(rewrite_reference, segment)
            output.append(segment)
        return "".join(output)

    @staticmethod
    def title_for(text: str, fallback: str) -> str:
        for line in text.splitlines():
            if line.startswith("# "):
                return line[2:].strip()
        return fallback.replace("-", " ").replace("_", " ")

    def validate_generated_links(self) -> None:
        valid_pages = {path.stem.casefold() for path in self.output.glob("*.md")}
        failures: list[str] = []

        def check_target(raw_target: str, page: Path) -> None:
            destination, _ = split_target(raw_target)
            if destination.startswith("<") and destination.endswith(">"):
                destination = destination[1:-1]
            parsed = urlsplit(destination)
            if (
                parsed.scheme
                or parsed.netloc
                or destination.startswith(("#", "//"))
                or not parsed.path
            ):
                return
            self.validated_links += 1
            target_page = unquote(parsed.path).removesuffix(".md").casefold()
            if "/" in target_page or target_page not in valid_pages:
                failures.append(f"{page.name}: {destination}")

        for page in sorted(self.output.glob("*.md")):
            for is_fenced, segment in fenced_segments(page.read_text(encoding="utf-8")):
                if is_fenced:
                    continue
                for match in INLINE_LINK_RE.finditer(segment):
                    check_target(match.group("target"), page)
                for match in REFERENCE_LINK_RE.finditer(segment):
                    check_target(match.group("target"), page)

        if failures:
            raise WikiBuildError("Broken generated Wiki links:\n" + "\n".join(failures))

    def build(self) -> None:
        pages = self.discover_pages()
        if self.output == self.root or self.root not in self.output.parents:
            raise WikiBuildError("Output directory must be inside the repository root")
        if self.output.exists():
            shutil.rmtree(self.output)
        self.output.mkdir(parents=True)

        sidebar_entries: list[tuple[str, str]] = []
        for source, source_path, slug in pages:
            original = source.read_text(encoding="utf-8")
            rewritten = self.rewrite_markdown(original, source_path)
            (self.output / f"{slug}.md").write_text(
                rewritten, encoding="utf-8", newline="\n"
            )
            if slug != "Home":
                sidebar_entries.append((self.title_for(original, slug), slug))

        (self.output / "Source-Identity.md").write_text(
            self.source_identity(), encoding="utf-8", newline="\n"
        )

        sidebar = [
            "# OJOS Wiki",
            "",
            "- [首页](Home)",
            "- [Source Identity](Source-Identity)",
        ]
        sidebar.extend(f"- [{title}]({slug})" for title, slug in sidebar_entries)
        (self.output / "_Sidebar.md").write_text(
            "\n".join(sidebar) + "\n", encoding="utf-8", newline="\n"
        )

        self.validate_generated_links()
        file_count = len(list(self.output.glob("*.md")))
        print(
            f"Built {file_count} Wiki files; rewrote {self.rewritten_links} local links; "
            f"validated {self.validated_links} Wiki links."
        )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repository", required=True, help="GitHub owner/repository")
    parser.add_argument("--branch", default="main")
    parser.add_argument("--commit-sha", required=True)
    parser.add_argument("--run-url", required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        WikiBuilder(
            args.root,
            args.output,
            args.repository,
            args.branch,
            args.commit_sha,
            args.run_url,
        ).build()
    except WikiBuildError as error:
        print(f"Wiki build failed: {error}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
