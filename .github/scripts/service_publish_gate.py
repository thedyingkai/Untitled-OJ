#!/usr/bin/env python3
"""Run and independently verify the signed Catalog publish gate for one service."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from typing import Any, Sequence


class PublishGateError(RuntimeError):
    pass


def sha256_digest(path: Path) -> str:
    return f"sha256:{hashlib.sha256(path.read_bytes()).hexdigest()}"


def load_object(path: Path, purpose: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise PublishGateError(f"invalid {purpose} {path}: {error}") from error
    if not isinstance(value, dict):
        raise PublishGateError(f"invalid {purpose} {path}: expected JSON object")
    return value


def require_file(path: Path, purpose: str) -> Path:
    if not path.is_file() or path.stat().st_size == 0:
        raise PublishGateError(f"missing non-empty {purpose}: {path}")
    return path


def run(command: Sequence[str], repo: Path) -> None:
    try:
        result = subprocess.run(command, cwd=repo, check=False)
    except OSError as error:
        raise PublishGateError(f"cannot execute {command[0]}: {error}") from error
    if result.returncode != 0:
        raise PublishGateError(
            f"command failed with exit code {result.returncode}: {' '.join(command)}"
        )


def stable_version(value: Any, purpose: str) -> tuple[int, int, int]:
    if not isinstance(value, str):
        raise PublishGateError(f"{purpose} has no stable semantic version")
    parts = value.split(".")
    if (
        len(parts) != 3
        or any(not part.isdigit() for part in parts)
        or any(len(part) > 1 and part.startswith("0") for part in parts)
    ):
        raise PublishGateError(
            f"{purpose} version {value!r} is not a stable MAJOR.MINOR.PATCH release"
        )
    return tuple(int(part) for part in parts)  # type: ignore[return-value]


def next_patch(value: str) -> str:
    major, minor, patch = stable_version(value, "baseline contract")
    return f"{major}.{minor}.{patch + 1}"


def copy_service_for_version(service_dir: Path, destination: Path, version: str) -> Path:
    shutil.copytree(
        service_dir,
        destination,
        ignore=shutil.ignore_patterns("target", "node_modules", ".git"),
    )
    manifest = destination / "ojos.service.yaml"
    source = require_file(manifest, "service manifest").read_text(encoding="utf-8")
    lines = source.splitlines(keepends=True)
    metadata_index = next(
        (index for index, line in enumerate(lines) if line.rstrip("\r\n") == "metadata:"),
        None,
    )
    if metadata_index is None:
        raise PublishGateError("service manifest has no metadata block")
    version_lines = [
        index
        for index in range(metadata_index + 1, len(lines))
        if lines[index].startswith("  version:")
    ]
    if len(version_lines) != 1:
        raise PublishGateError("service manifest must have exactly one metadata.version")
    index = version_lines[0]
    newline = "\r\n" if lines[index].endswith("\r\n") else "\n"
    lines[index] = f"  version: {version}{newline}"
    manifest.write_text("".join(lines), encoding="utf-8", newline="")
    return manifest


def resolved_artifact_fixture(build_input: dict[str, Any]) -> dict[str, Any]:
    service_id = build_input.get("serviceId")
    requirements = build_input.get("artifactRequirements")
    if not isinstance(service_id, str) or not service_id:
        raise PublishGateError("build-input has no serviceId")
    if not isinstance(requirements, list) or not requirements:
        raise PublishGateError("build-input has no artifactRequirements")
    artifacts: dict[str, Any] = {}
    for requirement in requirements:
        if not isinstance(requirement, dict):
            raise PublishGateError("build-input contains an invalid artifact requirement")
        role = requirement.get("role")
        slot = requirement.get("slot")
        if not isinstance(role, str) or not role or not isinstance(slot, str) or not slot:
            raise PublishGateError("artifact requirement needs a non-empty role and slot")
        if slot in artifacts:
            raise PublishGateError(f"artifact slot {slot!r} is required more than once")
        payload = f"ci-fixture:{service_id}:{role}:{slot}".encode("utf-8")
        expected_digest = requirement.get("expectedDigest")
        expected_size = requirement.get("expectedSize")
        digest = (
            expected_digest
            if isinstance(expected_digest, str) and expected_digest
            else f"sha256:{hashlib.sha256(payload).hexdigest()}"
        )
        if not (
            isinstance(digest, str)
            and len(digest) == 71
            and digest.startswith("sha256:")
            and all(character in "0123456789abcdef" for character in digest[7:])
        ):
            raise PublishGateError(f"artifact requirement {role!r} has invalid digest")
        if expected_size is not None and (
            not isinstance(expected_size, int)
            or isinstance(expected_size, bool)
            or expected_size <= 0
        ):
            raise PublishGateError(f"artifact requirement {role!r} has invalid size")
        size = expected_size if expected_size is not None else len(payload)
        digest_hex = digest[7:]
        if role == "runtime" or role.startswith("migration:"):
            media_type = "application/vnd.oci.image.manifest.v1+json"
            reference = f"example.invalid/ojos/{service_id}/{digest_hex[:16]}@{digest}"
        elif role.startswith("frontend-bundle:"):
            media_type = "text/javascript"
            reference = f"https://fixture.invalid/__ojos/extensions/{digest_hex}/bundle.js"
        else:
            media_type = "application/vnd.ojos.fixture+octet-stream"
            reference = f"https://fixture.invalid/__ojos/artifacts/{digest_hex}/{slot}"
        artifacts[slot] = {
            "mediaType": media_type,
            "digest": digest,
            "size": size,
            "reference": reference,
        }
    return {"schemaVersion": "ojos.dev/resolved-artifacts/v1", "artifacts": artifacts}


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )


def artifact_map(document: dict[str, Any], purpose: str) -> dict[str, Any]:
    artifacts = document.get("artifacts")
    if not isinstance(artifacts, dict) or not artifacts:
        raise PublishGateError(f"{purpose} has no artifact map")
    return artifacts


def verify_publication(
    service_dir: Path,
    build_input_path: Path,
    resolved_path: Path,
    release_lock_path: Path,
    catalog_dir: Path,
    key_id: str,
    catalog_id: str,
) -> None:
    build_input = load_object(build_input_path, "build-input")
    resolved = load_object(resolved_path, "resolved artifacts")
    lock = load_object(release_lock_path, "release lock")
    contract_path = require_file(
        service_dir / "gen" / "service.contract.json", "generated contract"
    )
    contract = load_object(contract_path, "generated contract")
    service_id = service_dir.name
    service_version = contract.get("serviceVersion")
    if not isinstance(service_version, str) or not service_version:
        raise PublishGateError("generated contract has no serviceVersion")
    if build_input.get("serviceId") != service_id or contract.get("serviceId") != service_id:
        raise PublishGateError("service identity mismatch across generated inputs")
    contract_digest = sha256_digest(contract_path)
    if build_input.get("contractDigest") != contract_digest:
        raise PublishGateError("build-input does not bind generated contract bytes")
    if lock.get("schemaVersion") != "ojos.dev/release-lock/v1":
        raise PublishGateError("release lock schemaVersion is invalid")
    if lock.get("serviceId") != service_id or lock.get("serviceVersion") != service_version:
        raise PublishGateError("release lock service identity/version mismatch")
    if lock.get("contractDigest") != contract_digest:
        raise PublishGateError("release lock does not bind generated contract digest")

    resolved_artifacts = artifact_map(resolved, "resolved artifact document")
    lock_artifacts = artifact_map(lock, "release lock")
    if resolved.get("schemaVersion") != "ojos.dev/resolved-artifacts/v1":
        raise PublishGateError("resolved artifact schemaVersion is invalid")
    if lock_artifacts != resolved_artifacts:
        raise PublishGateError("release lock artifact map differs from resolved inputs")
    requirements = build_input.get("artifactRequirements")
    bindings = lock.get("bindings")
    if not isinstance(requirements, list) or not isinstance(bindings, list):
        raise PublishGateError("build-input requirements or release lock bindings are invalid")
    expected_bindings = sorted(
        (item.get("role"), item.get("slot"))
        for item in requirements
        if isinstance(item, dict)
    )
    actual_bindings = sorted(
        (item.get("role"), item.get("slot")) for item in bindings if isinstance(item, dict)
    )
    if len(expected_bindings) != len(requirements) or expected_bindings != actual_bindings:
        raise PublishGateError("release lock bindings do not exactly match build-input roles")
    required_roles = {"contract", "runtime", "sbom", "provenance"}
    roles = {role for role, _slot in actual_bindings}
    if not required_roles.issubset(roles):
        raise PublishGateError(f"release lock is missing required roles: {required_roles - roles}")

    metadata_name = f"{service_id}-{service_version}.release.json"
    lock_name = f"{service_id}-{service_version}.release.lock.json"
    contract_name = f"{service_id}-{service_version}.service.contract.json"
    catalog_path = require_file(catalog_dir / "catalog.json", "signed Catalog")
    trust_path = require_file(catalog_dir / "trust.json", "Catalog trust document")
    source_path = require_file(catalog_dir / "catalog-source.json", "Catalog source")
    metadata_path = require_file(catalog_dir / "metadata" / metadata_name, "release metadata")
    published_lock = require_file(catalog_dir / "metadata" / lock_name, "published release lock")
    published_contract = require_file(
        catalog_dir / "metadata" / contract_name, "published service contract"
    )
    if published_lock.read_bytes() != release_lock_path.read_bytes():
        raise PublishGateError("published release lock differs from sealed release lock")
    if published_contract.read_bytes() != contract_path.read_bytes():
        raise PublishGateError("published contract differs from generated contract")

    catalog = load_object(catalog_path, "signed Catalog")
    trust = load_object(trust_path, "Catalog trust document")
    source = json.loads(source_path.read_text(encoding="utf-8"))
    if catalog.get("schema_version") != 2 or catalog.get("id") != catalog_id:
        raise PublishGateError("published Catalog identity/schema is invalid")
    signatures = catalog.get("signatures")
    if not isinstance(signatures, list) or len(signatures) != 1:
        raise PublishGateError("published Catalog must have exactly one ephemeral signature")
    signature = signatures[0]
    if (
        not isinstance(signature, dict)
        or signature.get("key_id") != key_id
        or signature.get("algorithm") != "Ed25519"
    ):
        raise PublishGateError("published Catalog signature identity is invalid")
    try:
        signature_bytes = base64.b64decode(signature.get("signature", ""), validate=True)
        public_key = base64.b64decode(trust.get(key_id, ""), validate=True)
    except (ValueError, TypeError) as error:
        raise PublishGateError("Catalog signature/trust key is not canonical base64") from error
    if len(signature_bytes) != 64 or len(public_key) != 32:
        raise PublishGateError("Catalog signature or trust key has invalid Ed25519 length")
    if (
        not isinstance(source, list)
        or len(source) != 1
        or source[0].get("required_key_id") != key_id
        or source[0].get("id") != catalog_id
    ):
        raise PublishGateError("Catalog source does not bind the ephemeral signing key")

    modules = catalog.get("modules")
    if not isinstance(modules, list) or len(modules) != 1 or modules[0].get("id") != service_id:
        raise PublishGateError("Catalog does not contain exactly the affected service module")
    releases = modules[0].get("releases")
    if not isinstance(releases, list) or len(releases) != 1:
        raise PublishGateError("Catalog module does not contain exactly one release")
    release = releases[0]
    if release.get("version") != service_version:
        raise PublishGateError("Catalog release version mismatch")
    metadata_ref = release.get("metadata")
    if not isinstance(metadata_ref, dict) or metadata_ref.get("sha256") != sha256_digest(metadata_path):
        raise PublishGateError("Catalog metadata digest does not bind release metadata bytes")

    metadata = load_object(metadata_path, "release metadata")
    platform = metadata.get("platform")
    if not isinstance(platform, dict):
        raise PublishGateError("release metadata has no platform contract")
    canonical_lock_digest = f"sha256:{hashlib.sha256(release_lock_path.read_bytes()).hexdigest()}"
    if platform.get("releaseLockDigest") != canonical_lock_digest:
        raise PublishGateError("release metadata does not bind the canonical release lock")
    if platform.get("contractDigest") != contract_digest:
        raise PublishGateError("release metadata does not bind the generated contract")
    subjects = platform.get("artifactSubjects")
    if not isinstance(subjects, list):
        raise PublishGateError("release metadata has no artifactSubjects")
    actual_subjects = {
        item.get("slot"): {
            "mediaType": item.get("mediaType"),
            "digest": item.get("digest"),
            "size": item.get("size"),
            "reference": item.get("reference"),
        }
        for item in subjects
        if isinstance(item, dict) and isinstance(item.get("slot"), str)
    }
    if len(actual_subjects) != len(subjects) or actual_subjects != lock_artifacts:
        raise PublishGateError("signed metadata artifact subjects differ from release lock")


def build_signed_baseline(
    repo: Path,
    baseline_repo: Path,
    service_name: str,
    scratch: Path,
) -> tuple[Path, Path, dict[str, Any]] | None:
    baseline_service = baseline_repo / "services" / service_name
    baseline_manifest = baseline_service / "ojos.service.yaml"
    if not baseline_manifest.is_file():
        return None

    baseline_generated = scratch / "baseline-generated"
    baseline_resolved = scratch / "baseline-resolved.json"
    baseline_lock = scratch / "baseline.release.lock.json"
    baseline_catalog = scratch / "baseline-catalog"
    baseline_seed = scratch / "baseline-ed25519-seed.txt"
    baseline_trust = scratch / "operator-trust.json"
    baseline_tool_manifest = baseline_repo / "Cargo.toml"
    baseline_tool = baseline_repo / "tools" / "ojos-service" / "Cargo.toml"
    baseline_workdir = baseline_repo
    if not baseline_tool.is_file():
        # One-time bootstrap for the revision that first introduces Service Contract v3.
        # Every later baseline carries its own compiler, so an ordinary PR cannot alter
        # how the protected base revision is compiled.
        baseline_tool_manifest = require_file(repo / "Cargo.toml", "workspace manifest")
        baseline_workdir = repo
    else:
        require_file(baseline_tool_manifest, "baseline workspace manifest")
    run(
        [
            "cargo", "run", "--locked", "--quiet", "--manifest-path",
            str(baseline_tool_manifest), "-p", "ojos-service", "--", "service",
            "build", str(baseline_manifest), "--output", str(baseline_generated),
        ],
        baseline_workdir,
    )
    baseline_build = load_object(
        require_file(baseline_generated / "build-input.json", "baseline build-input"),
        "baseline build-input",
    )
    write_json(baseline_resolved, resolved_artifact_fixture(baseline_build))
    baseline_seed.write_text(
        base64.b64encode(hashlib.sha256(f"ojos-ci-baseline:{service_name}".encode()).digest()).decode("ascii")
        + "\n",
        encoding="ascii",
    )
    key_id = f"ci-baseline-{service_name}"
    run(
        [
            "cargo", "run", "--locked", "--quiet", "--manifest-path",
            str(baseline_tool_manifest), "-p", "ojos-service", "--", "service",
            "publish", str(baseline_manifest), "--artifacts", str(baseline_resolved),
            "--output", str(baseline_lock), "--catalog-output", str(baseline_catalog),
            "--signing-key-file", str(baseline_seed), "--key-id", key_id,
            "--catalog-id", f"ci-baseline-{service_name}", "--public-base-url",
            f"https://baseline.invalid/{service_name}",
        ],
        baseline_workdir,
    )
    shutil.copyfile(
        require_file(baseline_catalog / "trust.json", "baseline Catalog trust document"),
        baseline_trust,
    )
    if baseline_trust.resolve().is_relative_to(baseline_catalog.resolve()):
        raise PublishGateError("operator trust must remain outside baseline Catalog")
    return baseline_catalog, baseline_trust, baseline_build


def compatibility_args(
    service_dir: Path,
    baseline: tuple[Path, Path, dict[str, Any]] | None,
    scratch: Path,
) -> tuple[list[str], Path]:
    if baseline is None:
        return [], service_dir / "ojos.service.yaml"
    catalog, trust, baseline_build = baseline
    current = load_object(
        require_file(service_dir / "gen" / "service.contract.json", "generated contract"),
        "generated contract",
    )
    baseline_version = baseline_build.get("serviceVersion")
    current_version = current.get("serviceVersion")
    baseline_tuple = stable_version(baseline_version, "baseline build-input")
    current_tuple = stable_version(current_version, "generated contract")
    if current_tuple < baseline_tuple:
        raise PublishGateError(
            f"current service version {current_version} is older than trusted baseline {baseline_version}"
        )
    manifest = service_dir / "ojos.service.yaml"
    if current_tuple == baseline_tuple:
        baseline_contract = load_object(
            require_file(
                catalog / "metadata" / f"{service_dir.name}-{baseline_version}.service.contract.json",
                "baseline service contract",
            ),
            "baseline service contract",
        )
        if current != baseline_contract:
            raise PublishGateError(
                f"service contract changed without increasing service version {current_version}"
            )
        manifest = copy_service_for_version(
            service_dir,
            scratch / "compatibility-source" / service_dir.name,
            next_patch(str(current_version)),
        )
    return ["--previous-catalog", str(catalog), "--previous-trust", str(trust)], manifest


def baseline_revision(repository: Path, revision: str, destination: Path) -> None:
    if not revision or revision.startswith("-") or any(
        character not in "0123456789abcdefABCDEF" for character in revision
    ):
        raise PublishGateError("baseline revision must be a full hexadecimal commit id")
    try:
        result = subprocess.run(
            [
                "git", "-c", "core.autocrlf=false", "archive", "--format=tar",
                revision,
            ],
            cwd=repository,
            check=False,
            stdout=subprocess.PIPE,
        )
    except OSError as error:
        raise PublishGateError(f"cannot execute git archive: {error}") from error
    if result.returncode != 0:
        raise PublishGateError(
            f"cannot export trusted baseline revision {revision}: git archive exited {result.returncode}"
        )
    destination.mkdir(parents=True, exist_ok=False)
    try:
        extracted = subprocess.run(
            ["tar", "-xf", "-", "-C", str(destination)],
            input=result.stdout,
            cwd=repository,
            check=False,
        )
    except OSError as error:
        raise PublishGateError(f"cannot execute tar for baseline export: {error}") from error
    if extracted.returncode != 0:
        raise PublishGateError(
            f"cannot extract trusted baseline revision {revision}: tar exited {extracted.returncode}"
        )


def execute_gate(
    repo: Path,
    service_dir: Path,
    baseline_repo: Path | None = None,
    keep_output: Path | None = None,
) -> None:
    manifest = require_file(service_dir / "ojos.service.yaml", "service manifest")
    scratch_owner = tempfile.TemporaryDirectory(prefix=f"ojos-publish-gate-{service_dir.name}-")
    scratch = Path(scratch_owner.name)
    try:
        generated = scratch / "generated"
        resolved = scratch / "resolved-artifacts.json"
        release_lock = scratch / "release.lock.json"
        catalog = scratch / "catalog"
        signing_key = scratch / "ed25519-seed.txt"
        baseline = (
            build_signed_baseline(repo, baseline_repo, service_dir.name, scratch)
            if baseline_repo is not None
            else None
        )
        previous, compatibility_manifest = compatibility_args(
            service_dir, baseline, scratch
        )
        if baseline_repo is not None and baseline is None:
            print(
                f"service-publish-gate: {service_dir.name} has no prior manifest; treating as first publication",
                file=sys.stderr,
            )
        run(
            [
                "cargo", "run", "--locked", "--quiet", "-p", "ojos-service", "--",
                "service", "check", str(compatibility_manifest), *previous,
            ],
            repo,
        )
        publication_service = compatibility_manifest.parent
        publication_generated = publication_service / "gen"
        if publication_service != service_dir:
            run(
                [
                    "cargo", "run", "--locked", "--quiet", "-p", "ojos-service", "--",
                    "service", "build", str(compatibility_manifest), "--output",
                    str(publication_generated),
                ],
                repo,
            )
        run(
            ["cargo", "run", "--locked", "--quiet", "-p", "ojos-service", "--", "service", "build", str(manifest), "--output", str(generated)],
            repo,
        )
        generated_build = require_file(generated / "build-input.json", "fresh build-input")
        checked_build = require_file(
            service_dir / "gen" / "build-input.json", "checked-in build-input"
        )
        if generated_build.read_bytes() != checked_build.read_bytes():
            raise PublishGateError("fresh build-input differs from checked-in generated output")
        publication_build = require_file(
            publication_generated / "build-input.json", "publication build-input"
        )
        write_json(
            resolved,
            resolved_artifact_fixture(load_object(publication_build, "publication build-input")),
        )
        require_file(resolved, "resolved artifacts")
        signing_key.write_text(
            base64.b64encode(os.urandom(32)).decode("ascii") + "\n", encoding="ascii"
        )
        key_id = f"ci-{service_dir.name}-ephemeral"
        catalog_id = f"ci-{service_dir.name}"
        run(
            [
                "cargo", "run", "--locked", "--quiet", "-p", "ojos-service", "--",
                "service", "publish", str(compatibility_manifest), "--artifacts", str(resolved),
                "--output", str(release_lock), "--catalog-output", str(catalog),
                "--signing-key-file", str(signing_key), "--key-id", key_id,
                "--catalog-id", catalog_id, "--public-base-url",
                f"https://fixture.invalid/{service_dir.name}", *previous,
            ],
            repo,
        )
        verify_publication(
            publication_service,
            publication_build,
            resolved,
            release_lock,
            catalog,
            key_id,
            catalog_id,
        )
        if keep_output is not None:
            if keep_output.exists():
                raise PublishGateError(f"refusing to overwrite gate output {keep_output}")
            shutil.copytree(scratch, keep_output)
    finally:
        scratch_owner.cleanup()


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--service", type=Path, required=True)
    parser.add_argument(
        "--baseline-revision",
        help="trusted base commit exported with git archive; absent service manifests are first publications",
    )
    parser.add_argument("--keep-output", type=Path)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    repo = args.repo.resolve()
    service = (repo / args.service).resolve() if not args.service.is_absolute() else args.service.resolve()
    try:
        service.relative_to(repo / "services")
    except ValueError as error:
        raise PublishGateError("service directory must stay below repository services/") from error
    baseline_owner: tempfile.TemporaryDirectory[str] | None = None
    baseline_repo: Path | None = None
    try:
        if args.baseline_revision:
            baseline_owner = tempfile.TemporaryDirectory(prefix="ojos-ci-baseline-tree-")
            baseline_repo = Path(baseline_owner.name) / "repository"
            baseline_revision(repo, args.baseline_revision, baseline_repo)
        execute_gate(repo, service, baseline_repo, args.keep_output)
    finally:
        if baseline_owner is not None:
            baseline_owner.cleanup()
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except PublishGateError as error:
        print(f"service-publish-gate: {error}", file=sys.stderr)
        raise SystemExit(2)
