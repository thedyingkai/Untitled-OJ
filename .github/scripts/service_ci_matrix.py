#!/usr/bin/env python3
"""Discover Service Contract v3 services and emit deterministic CI matrices."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from typing import Any, Iterable


SCHEMA_VERSION = "ojos.dev/service-ci-matrix/v1"
MANIFEST_NAME = "ojos.service.yaml"
GLOBAL_INPUTS = (
    ".github/scripts/service_ci_matrix.py",
    ".github/scripts/service_publish_gate.py",
    ".github/scripts/test_service_publish_gate.py",
    ".github/toolchains.env",
    ".github/workflows/service-contract-ci.yml",
    "Cargo.lock",
    "Cargo.toml",
    "platform/schemas/",
    "platform/shared/",
    "tools/ojos-service/",
)
GENERATED_CONTRACT = Path("gen/service.contract.json")
GENERATED_BUILD_INPUT = Path("gen/build-input.json")
RUNTIME_DOCKERFILE = Path("Dockerfile")
MIGRATION_DOCKERFILE = Path("migrations/Dockerfile")
REPOSITORY_GATE_FILES = (
    Path(".github/scripts/service_publish_gate.py"),
    Path(".github/scripts/test_service_publish_gate.py"),
)
MATRIX_GATE_NAMES = ("contract", "go", "rust", "node", "image", "publish")


class DiscoveryError(RuntimeError):
    pass


def posix(path: Path) -> str:
    return path.as_posix()


def discover_manifests(repo: Path) -> list[Path]:
    services = repo / "services"
    if not services.is_dir():
        raise DiscoveryError(f"services directory is missing below {repo}")
    manifests = sorted(
        (
            path
            for path in services.rglob(MANIFEST_NAME)
            if path.is_file()
            and not any(
                part in {"node_modules", "gen", "target"}
                for part in path.relative_to(services).parts[:-1]
            )
        ),
        key=lambda path: posix(path.relative_to(repo)),
    )
    seen: set[str] = set()
    for manifest in manifests:
        service_id = manifest.parent.name
        if service_id in seen:
            raise DiscoveryError(f"duplicate discovered service directory id {service_id!r}")
        seen.add(service_id)
    return manifests


def git_changed_paths(repo: Path, base: str, head: str) -> list[str]:
    command = ["git", "diff", "--name-only", "--diff-filter=ACDMRTUXB", base, head, "--"]
    result = subprocess.run(command, cwd=repo, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        raise DiscoveryError(
            f"git diff {base} {head} failed: {result.stderr.strip() or result.stdout.strip()}"
        )
    return sorted({line.strip().replace("\\", "/") for line in result.stdout.splitlines() if line.strip()})


def is_global_input(path: str) -> bool:
    return any(path == item or path.startswith(item) for item in GLOBAL_INPUTS)


def path_matches_service(path: str, service_dir: str) -> bool:
    return path == service_dir or path.startswith(f"{service_dir}/")


def has_files(directory: Path, name: str) -> bool:
    return any(path.is_file() for path in directory.rglob(name))


def require_nonempty_file(service_dir: Path, relative: Path, purpose: str) -> Path:
    path = service_dir / relative
    if not path.is_file() or path.stat().st_size == 0:
        raise DiscoveryError(
            f"{service_dir.name}: missing non-empty {purpose} at {relative.as_posix()}"
        )
    return path


def load_json_object(path: Path, purpose: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise DiscoveryError(f"invalid {purpose} {path}: {error}") from error
    if not isinstance(value, dict):
        raise DiscoveryError(f"invalid {purpose} {path}: expected a JSON object")
    return value


def safe_service_relative_path(service_dir: Path, value: Any, purpose: str) -> Path:
    if not isinstance(value, str) or not value or "\\" in value:
        raise DiscoveryError(f"{service_dir.name}: invalid {purpose} path {value!r}")
    relative = Path(value)
    if relative.is_absolute() or ".." in relative.parts:
        raise DiscoveryError(f"{service_dir.name}: {purpose} escapes the service directory")
    return relative


def manifest_scalar(service_dir: Path, raw: str, purpose: str) -> str:
    value = raw.strip()
    if not value:
        raise DiscoveryError(f"{service_dir.name}: empty {purpose}")
    if value.startswith('"'):
        try:
            decoded = json.loads(value)
        except json.JSONDecodeError as error:
            raise DiscoveryError(
                f"{service_dir.name}: invalid quoted {purpose}: {error}"
            ) from error
        if not isinstance(decoded, str) or not decoded:
            raise DiscoveryError(f"{service_dir.name}: invalid {purpose}")
        return decoded
    if value.startswith("'"):
        if len(value) < 2 or not value.endswith("'"):
            raise DiscoveryError(f"{service_dir.name}: invalid quoted {purpose}")
        decoded = value[1:-1].replace("''", "'")
        if not decoded:
            raise DiscoveryError(f"{service_dir.name}: empty {purpose}")
        return decoded
    if not re.fullmatch(r"[A-Za-z0-9_.:/@+^-]+", value):
        raise DiscoveryError(
            f"{service_dir.name}: {purpose} must use a simple YAML scalar"
        )
    return value


def manifest_section(
    service_dir: Path, lines: list[str], section: str
) -> tuple[str, list[str]] | None:
    matches: list[tuple[int, str]] = []
    for index, raw in enumerate(lines):
        if raw.startswith((" ", "\t")) or raw.lstrip().startswith("#"):
            continue
        match = re.fullmatch(rf"{re.escape(section)}:\s*(.*?)\s*", raw)
        if match:
            matches.append((index, match.group(1)))
    if not matches:
        return None
    if len(matches) != 1:
        raise DiscoveryError(f"{service_dir.name}: duplicate top-level {section} section")
    start, inline = matches[0]
    body: list[str] = []
    for raw in lines[start + 1 :]:
        if raw and not raw.startswith((" ", "\t")) and not raw.lstrip().startswith("#"):
            break
        body.append(raw)
    return inline, body


def manifest_mapping_scalar(
    service_dir: Path, lines: list[str], section: str, key: str
) -> str:
    selected = manifest_section(service_dir, lines, section)
    if selected is None:
        raise DiscoveryError(f"{service_dir.name}: missing top-level {section} section")
    inline, body = selected
    if inline:
        raise DiscoveryError(
            f"{service_dir.name}: top-level {section} must use a block mapping"
        )
    values = []
    for raw in body:
        match = re.fullmatch(rf"  {re.escape(key)}:\s*(.*?)\s*", raw)
        if match:
            values.append(manifest_scalar(service_dir, match.group(1), f"{section}.{key}"))
    if len(values) != 1:
        raise DiscoveryError(
            f"{service_dir.name}: {section} must declare exactly one {key}"
        )
    return values[0]


def manifest_sequence(
    service_dir: Path,
    lines: list[str],
    section: str,
    required_keys: tuple[str, ...],
) -> list[dict[str, str]]:
    selected = manifest_section(service_dir, lines, section)
    if selected is None:
        return []
    inline, body = selected
    if inline == "[]":
        return []
    if inline:
        raise DiscoveryError(
            f"{service_dir.name}: top-level {section} must use a block sequence"
        )
    entries: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    for raw in body:
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        item = re.fullmatch(r"  -(?:\s+([A-Za-z][A-Za-z0-9]*):\s*(.*?))?\s*", raw)
        if item:
            current = {}
            entries.append(current)
            if item.group(1):
                current[item.group(1)] = manifest_scalar(
                    service_dir, item.group(2), f"{section}.{item.group(1)}"
                )
            continue
        field = re.fullmatch(r"    ([A-Za-z][A-Za-z0-9]*):\s*(.*?)\s*", raw)
        if field and current is not None:
            key = field.group(1)
            if key in current:
                raise DiscoveryError(
                    f"{service_dir.name}: duplicate {section}.{key} declaration"
                )
            current[key] = manifest_scalar(
                service_dir, field.group(2), f"{section}.{key}"
            )
    if body and not entries:
        raise DiscoveryError(f"{service_dir.name}: {section} must be a block sequence")
    for entry in entries:
        missing = [key for key in required_keys if key not in entry]
        if missing:
            raise DiscoveryError(
                f"{service_dir.name}: {section} entry is missing {', '.join(missing)}"
            )
    return entries


def source_manifest_layout(manifest: Path) -> dict[str, Any]:
    service_dir = manifest.parent
    try:
        lines = manifest.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError) as error:
        raise DiscoveryError(f"cannot read UTF-8 service manifest {manifest}: {error}") from error
    return {
        "runtimeArtifact": manifest_mapping_scalar(
            service_dir, lines, "runtime", "artifact"
        ),
        "migrations": manifest_sequence(
            service_dir, lines, "migrations", ("id", "artifact")
        ),
        "frontends": manifest_sequence(
            service_dir, lines, "frontends", ("target", "manifest")
        ),
    }


def artifact_role_map(service_dir: Path, build_input: dict[str, Any]) -> dict[str, str]:
    requirements = build_input.get("artifactRequirements")
    if not isinstance(requirements, list):
        raise DiscoveryError(
            f"{service_dir.name}: generated build-input has no artifactRequirements array"
        )
    roles: dict[str, str] = {}
    for requirement in requirements:
        if not isinstance(requirement, dict):
            raise DiscoveryError(f"{service_dir.name}: invalid artifact requirement")
        role = requirement.get("role")
        slot = requirement.get("slot")
        if not isinstance(role, str) or not role or not isinstance(slot, str) or not slot:
            raise DiscoveryError(f"{service_dir.name}: artifact requirement needs role and slot")
        if role in roles:
            raise DiscoveryError(f"{service_dir.name}: duplicate artifact role {role!r}")
        roles[role] = slot
    return roles


def validate_service_layout(
    service_dir: Path, manifest: Path
) -> tuple[dict[str, Any], list[Path]]:
    source_layout = source_manifest_layout(manifest)
    contract_path = require_nonempty_file(
        service_dir, GENERATED_CONTRACT, "generated Service Contract v3"
    )
    build_input_path = require_nonempty_file(
        service_dir, GENERATED_BUILD_INPUT, "generated build-input"
    )
    contract = load_json_object(contract_path, "generated Service Contract")
    build_input = load_json_object(build_input_path, "generated build-input")
    service_id = service_dir.name
    if contract.get("schemaVersion") != "ojos.dev/service-contract/v3":
        raise DiscoveryError(f"{service_id}: generated contract is not Service Contract v3")
    if contract.get("serviceId") != service_id or build_input.get("serviceId") != service_id:
        raise DiscoveryError(f"{service_id}: generated contract/build-input identity mismatch")
    if build_input.get("schemaVersion") != "ojos.dev/build-input/v1":
        raise DiscoveryError(f"{service_id}: generated build-input schema is invalid")
    if build_input.get("serviceVersion") != contract.get("serviceVersion"):
        raise DiscoveryError(f"{service_id}: generated build-input version mismatch")
    contract_digest = f"sha256:{hashlib.sha256(contract_path.read_bytes()).hexdigest()}"
    if build_input.get("contractDigest") != contract_digest:
        raise DiscoveryError(f"{service_id}: generated build-input contract digest mismatch")

    roles = artifact_role_map(service_dir, build_input)
    runtime = contract.get("runtime")
    if not isinstance(runtime, dict) or not isinstance(runtime.get("artifact"), str):
        raise DiscoveryError(f"{service_id}: generated contract has no runtime artifact")
    if runtime["artifact"] != source_layout["runtimeArtifact"]:
        raise DiscoveryError(
            f"{service_id}: source and generated runtime artifact declarations differ"
        )
    if roles.get("runtime") != runtime["artifact"]:
        raise DiscoveryError(f"{service_id}: build-input does not bind the runtime artifact")
    runtime_dockerfile = require_nonempty_file(
        service_dir, RUNTIME_DOCKERFILE, "runtime Dockerfile"
    )
    dockerfiles = [runtime_dockerfile]

    source_migrations = source_layout["migrations"]
    migrations = contract.get("migrations", [])
    if not isinstance(migrations, list):
        raise DiscoveryError(f"{service_id}: generated contract migrations must be an array")
    if source_migrations:
        dockerfiles.append(
            require_nonempty_file(
                service_dir, MIGRATION_DOCKERFILE, "migration Dockerfile"
            )
        )
    source_migration_pairs = sorted(
        (migration["id"], migration["artifact"]) for migration in source_migrations
    )
    generated_migration_pairs = sorted(
        (migration.get("id"), migration.get("artifact"))
        for migration in migrations
        if isinstance(migration, dict)
    )
    if len(generated_migration_pairs) != len(migrations) or source_migration_pairs != generated_migration_pairs:
        raise DiscoveryError(
            f"{service_id}: source and generated migration declarations differ"
        )
    for migration in migrations:
        if not isinstance(migration, dict):
            raise DiscoveryError(f"{service_id}: invalid migration declaration")
        migration_id = migration.get("id")
        artifact = migration.get("artifact")
        if not isinstance(migration_id, str) or roles.get(f"migration:{migration_id}") != artifact:
            raise DiscoveryError(
                f"{service_id}: build-input does not bind migration {migration_id!r}"
            )

    source_frontends = source_layout["frontends"]
    frontends = contract.get("frontends", [])
    if not isinstance(frontends, list):
        raise DiscoveryError(f"{service_id}: generated contract frontends must be an array")
    for frontend in source_frontends:
        manifest_path = safe_service_relative_path(
            service_dir, frontend["manifest"], "frontend manifest"
        )
        require_nonempty_file(service_dir, manifest_path, "frontend manifest")
        require_nonempty_file(
            service_dir,
            manifest_path.parent / "bundle.js",
            f"frontend bundle for {frontend['target']}",
        )
    source_frontend_pairs = sorted(
        (frontend["target"], frontend["manifest"]) for frontend in source_frontends
    )
    generated_frontend_pairs = sorted(
        (frontend.get("target"), frontend.get("manifest", {}).get("path"))
        for frontend in frontends
        if isinstance(frontend, dict) and isinstance(frontend.get("manifest"), dict)
    )
    if len(generated_frontend_pairs) != len(frontends) or source_frontend_pairs != generated_frontend_pairs:
        raise DiscoveryError(
            f"{service_id}: source and generated frontend declarations differ"
        )
    for frontend in frontends:
        if not isinstance(frontend, dict) or not isinstance(frontend.get("manifest"), dict):
            raise DiscoveryError(f"{service_id}: invalid frontend declaration")
        manifest = frontend["manifest"]
        manifest_path = safe_service_relative_path(
            service_dir, manifest.get("path"), "frontend manifest"
        )
        require_nonempty_file(service_dir, manifest_path, "frontend manifest")
        module = frontend.get("module")
        if not isinstance(module, dict):
            raise DiscoveryError(f"{service_id}: frontend declaration has no module")
        module_id = module.get("moduleId")
        artifact = module.get("artifact")
        if not isinstance(module_id, str) or roles.get(f"frontend-bundle:{module_id}") != artifact:
            raise DiscoveryError(
                f"{service_id}: build-input does not bind frontend bundle {module_id!r}"
            )
        require_nonempty_file(
            service_dir, manifest_path.parent / "bundle.js", f"frontend bundle {module_id}"
        )

    for relative, purpose in (
        (Path("gen/go/go.mod"), "generated Go module"),
        (Path("gen/rust/Cargo.toml"), "generated Rust crate"),
        (Path("gen/ts/package.json"), "generated TypeScript package"),
    ):
        require_nonempty_file(service_dir, relative, purpose)
    return contract, dockerfiles


def service_entry(repo: Path, manifest: Path) -> dict[str, Any]:
    service_dir = manifest.parent
    _contract, required_dockerfiles = validate_service_layout(service_dir, manifest)
    relative_dir = posix(service_dir.relative_to(repo))
    relative_manifest = posix(manifest.relative_to(repo))
    generated = service_dir / "gen"
    go_modules = sorted(
        path for path in service_dir.rglob("go.mod") if "target" not in path.parts
    )
    rust_manifests = sorted(
        path for path in service_dir.rglob("Cargo.toml") if "target" not in path.parts
    )
    node_packages = sorted(
        path for path in service_dir.rglob("package.json") if "node_modules" not in path.parts
    )
    dockerfiles = sorted(required_dockerfiles)
    return {
        "service": service_dir.name,
        "directory": relative_dir,
        "manifest": relative_manifest,
        "generated": posix(generated.relative_to(repo)),
        "goModules": [
            posix(path.parent.relative_to(repo))
            for path in sorted(
                go_modules,
                key=lambda path: (len(path.relative_to(repo).parts), posix(path.relative_to(repo))),
            )
        ],
        "rustManifests": [posix(path.relative_to(repo)) for path in rust_manifests],
        "nodePackages": [posix(path.relative_to(repo)) for path in node_packages],
        "dockerfiles": [posix(path.relative_to(repo)) for path in dockerfiles],
        "hasGo": bool(go_modules),
        "hasRust": bool(rust_manifests),
        "hasNode": bool(node_packages),
        "hasDocker": bool(dockerfiles),
        "hasGenerated": generated.is_dir(),
        "hasOpenApi": has_files(service_dir, "openapi.yaml") or has_files(service_dir, "openapi.json"),
    }


def select_affected(
    services: list[dict[str, Any]], changed_paths: Iterable[str] | None
) -> tuple[list[dict[str, Any]], bool]:
    if changed_paths is None:
        return services, True
    changed = tuple(changed_paths)
    global_change = any(is_global_input(path) for path in changed)
    if global_change:
        return services, True
    selected = [
        service
        for service in services
        if any(path_matches_service(path, service["directory"]) for path in changed)
    ]
    return selected, False


def compact(include: list[dict[str, Any]]) -> dict[str, Any]:
    return {"include": include}


def render_matrix(
    repo: Path, changed_paths: Iterable[str] | None = None
) -> dict[str, Any]:
    for relative in REPOSITORY_GATE_FILES:
        path = repo / relative
        if not path.is_file() or path.stat().st_size == 0:
            raise DiscoveryError(f"repository publish gate is missing: {relative.as_posix()}")
    manifests = discover_manifests(repo)
    if changed_paths is not None:
        discovered = {posix(path.relative_to(repo)) for path in manifests}
        missing_changed_manifests = sorted(
            path
            for path in changed_paths
            if path.startswith("services/")
            and path.endswith(f"/{MANIFEST_NAME}")
            and path not in discovered
        )
        if missing_changed_manifests:
            raise DiscoveryError(
                "changed service manifests are missing from the checkout: "
                + ", ".join(missing_changed_manifests)
            )
    services = [service_entry(repo, manifest) for manifest in manifests]
    affected, global_change = select_affected(services, changed_paths)
    return {
        "schemaVersion": SCHEMA_VERSION,
        "serviceCount": len(services),
        "affectedCount": len(affected),
        "globalChange": global_change,
        "services": compact(services),
        "affected": compact(affected),
        "go": compact([service for service in affected if service["hasGo"]]),
        "rust": compact([service for service in affected if service["hasRust"]]),
        "node": compact([service for service in affected if service["hasNode"]]),
        "images": compact([service for service in affected if service["hasDocker"]]),
        "publish": compact(affected),
    }


def validate_required_results(
    affected_count: int, discover_result: str, gate_results: dict[str, str]
) -> None:
    if discover_result != "success":
        raise DiscoveryError(f"service discovery did not succeed: {discover_result or 'missing'}")
    if affected_count < 0:
        raise DiscoveryError("affected service count cannot be negative")
    missing = sorted(set(MATRIX_GATE_NAMES) - gate_results.keys())
    extra = sorted(gate_results.keys() - set(MATRIX_GATE_NAMES))
    if missing or extra:
        raise DiscoveryError(
            f"required gate result set mismatch; missing={missing}, extra={extra}"
        )
    expected = "skipped" if affected_count == 0 else "success"
    invalid = {
        gate: result
        for gate, result in gate_results.items()
        if result != expected
    }
    if invalid:
        scope = "empty matrix" if affected_count == 0 else f"{affected_count} affected service(s)"
        raise DiscoveryError(
            f"required service gates are incomplete for {scope}; expected {expected}: {invalid}"
        )


def write_github_outputs(path: Path, matrix: dict[str, Any]) -> None:
    outputs: dict[str, str] = {
        "service_count": str(matrix["serviceCount"]),
        "affected_count": str(matrix["affectedCount"]),
        "global_change": str(matrix["globalChange"]).lower(),
    }
    for key in ("services", "affected", "go", "rust", "node", "images", "publish"):
        outputs[f"{key}_matrix"] = json.dumps(matrix[key], sort_keys=True, separators=(",", ":"))
    with path.open("a", encoding="utf-8", newline="\n") as stream:
        for key, value in outputs.items():
            stream.write(f"{key}={value}\n")


def validate_toolchains(path: Path) -> dict[str, str]:
    required = {
        "GO_VERSION",
        "RUST_VERSION",
        "NODE_VERSION",
        "TYPESCRIPT_VERSION",
        "GOVULNCHECK_VERSION",
        "CARGO_AUDIT_VERSION",
    }
    values: dict[str, str] = {}
    for line_number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            raise DiscoveryError(f"{path}:{line_number}: expected NAME=value")
        name, value = line.split("=", 1)
        if name in values or name not in required or not value or any(char.isspace() for char in value):
            raise DiscoveryError(f"{path}:{line_number}: invalid pinned toolchain entry")
        values[name] = value
    missing = sorted(required - values.keys())
    if missing:
        raise DiscoveryError(f"{path}: missing pinned toolchains: {', '.join(missing)}")
    return values


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument("--base", default=None)
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--changed-file", type=Path)
    parser.add_argument("--all", action="store_true", help="select every discovered service")
    parser.add_argument("--github-output", type=Path)
    parser.add_argument("--toolchains", type=Path)
    parser.add_argument("--pretty", action="store_true")
    parser.add_argument("--verify-required", action="store_true")
    parser.add_argument("--affected-count", type=int)
    parser.add_argument("--discover-result")
    parser.add_argument("--gate-result", action="append", default=[])
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    if args.verify_required:
        if args.affected_count is None or args.discover_result is None:
            raise DiscoveryError(
                "--verify-required needs --affected-count and --discover-result"
            )
        results: dict[str, str] = {}
        for raw in args.gate_result:
            if "=" not in raw:
                raise DiscoveryError(f"invalid --gate-result {raw!r}; expected name=result")
            name, result = raw.split("=", 1)
            if name in results:
                raise DiscoveryError(f"duplicate --gate-result {name!r}")
            results[name] = result
        validate_required_results(args.affected_count, args.discover_result, results)
        return 0
    repo = args.repo.resolve()
    validate_toolchains(args.toolchains or repo / ".github" / "toolchains.env")
    changed_paths: list[str] | None
    if args.all:
        changed_paths = None
    elif args.changed_file:
        changed_paths = sorted(
            {
                line.strip().replace("\\", "/")
                for line in args.changed_file.read_text(encoding="utf-8").splitlines()
                if line.strip()
            }
        )
    elif args.base:
        changed_paths = git_changed_paths(repo, args.base, args.head)
    else:
        changed_paths = None
    matrix = render_matrix(repo, changed_paths)
    print(json.dumps(matrix, indent=2 if args.pretty else None, sort_keys=True))
    output = args.github_output or (Path(os.environ["GITHUB_OUTPUT"]) if "GITHUB_OUTPUT" in os.environ else None)
    if output:
        write_github_outputs(output, matrix)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except DiscoveryError as error:
        print(f"service-ci-discovery: {error}", file=sys.stderr)
        raise SystemExit(2)
