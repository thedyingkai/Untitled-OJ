#!/usr/bin/env bash
set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
cd "$repo_root"

die() { echo "pack-orchestrator-v1: $*" >&2; exit 1; }
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  else powershell -NoProfile -Command "(Get-FileHash -Algorithm SHA256 -LiteralPath '$1').Hash.ToLower()" | tr -d '\r\n'
  fi
}

version="${OJOS_RELEASE_VERSION:-1.0.0}"
version="${version#v}"
channel="${OJOS_RELEASE_CHANNEL:-ga}"
platform="${OJOS_RELEASE_PLATFORM:-}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || die "invalid release version"
[[ "$channel" == "compat" || "$channel" == "ga" ]] || die "channel must be compat or ga"
[[ "$platform" == "windows-x64" || "$platform" == "linux-x86_64" ]] || die "unsupported platform"
[[ -f manager/web/dist/index.html ]] || die "manager/web/dist/index.html is required"
target_dir="${OJOS_RELEASE_TARGET_DIR:-target/release}"
[[ -d "$target_dir" ]] || die "release target directory $target_dir does not exist"

output_root="${OJOS_RELEASE_OUTPUT_DIR:-$repo_root/dist/orchestrator-release/$version/$platform/$channel}"
mkdir -p "$output_root"
output_root="$(cd "$output_root" && pwd -P)"
bundle_name="ojos-orchestrator-$version-$channel-$platform"
stage="$(mktemp -d "$output_root/.stage.XXXXXX")"
cleanup() { rm -rf -- "$stage"; }
trap cleanup EXIT
bundle="$stage/$bundle_name"
mkdir -p "$bundle/bin" "$bundle/manager/web" "$bundle/platform/schemas" \
  "$bundle/services" "$bundle/sets" "$bundle/store" "$bundle/docs"

for binary in ojos-orchestrator-daemon ojos-orchestrator-tui ojos-orchestrator-agent ojos-orchestrator-desktop; do
  source="$target_dir/$binary"
  [[ "$platform" == "windows-x64" ]] && source="$source.exe"
  [[ -x "$source" || -f "$source" ]] || die "missing release binary $source"
  cp "$source" "$bundle/bin/"
done
if [[ "$platform" == "windows-x64" ]]; then
  webview_loader="$target_dir/WebView2Loader.dll"
  [[ -f "$webview_loader" ]] || die "missing Windows Desktop runtime $webview_loader"
  cp "$webview_loader" "$bundle/bin/WebView2Loader.dll"
fi
cp -R manager/web/dist "$bundle/manager/web/"
cp -R platform/schemas/orchestrator "$bundle/platform/schemas/"
while IFS= read -r -d '' manifest; do
  relative="${manifest#services/}"
  destination="$bundle/services/$(dirname "$relative")"
  mkdir -p "$destination"
  cp "$manifest" "$destination/"
done < <(find services -mindepth 2 -maxdepth 2 -type f \( \
  -name service.yaml -o -name release.yaml \) -print0 | sort -z)
cp -R sets/. "$bundle/sets/"
cp store/index.json "$bundle/store/index.json"
cp docs/orchestrator/operations-v1.md "$bundle/docs/"

cat >"$bundle/README.txt" <<EOF
OJOS Orchestrator $version ($channel, $platform)

Desktop is the default local entry point and embeds the Web UI, backend and
SQLite. Managed container execution is intentionally unavailable in Desktop;
use an enrolled standalone Agent with a verified workload-file ownership
boundary. The standalone daemon is production-only unless --ephemeral is
explicitly passed. Remote production configuration and recovery procedures:
docs/operations-v1.md

The compat channel contains the 0.2 legacy adapter and emits Deprecation,
Sunset and successor Link headers. The ga channel removes legacy behavior and
returns RFC 9457-style 410 problem responses with v1 successor paths.

Remote Node bootstrap (keep the identity directory and SQLite ledger durable):
  bin/ojos-orchestrator-agent enroll --help
  bin/ojos-orchestrator-agent run --help
EOF

for required in \
  bin/ojos-orchestrator-daemon \
  bin/ojos-orchestrator-tui \
  bin/ojos-orchestrator-agent \
  bin/ojos-orchestrator-desktop \
  manager/web/dist/index.html \
  platform/schemas/orchestrator/actions-v1.yaml \
  store/index.json \
  docs/operations-v1.md; do
  [[ "$platform" == "windows-x64" && "$required" == bin/* ]] && required="$required.exe"
  [[ -f "$bundle/$required" ]] || die "portable layout is missing $required"
done
if [[ "$platform" == "windows-x64" ]]; then
  [[ -f "$bundle/bin/WebView2Loader.dll" ]] || \
    die "portable Windows layout is missing WebView2Loader.dll"
fi
source_manifest_count="$(find services -mindepth 2 -maxdepth 2 -type f \( \
  -name service.yaml -o -name release.yaml \) | wc -l | tr -d ' ')"
bundle_manifest_count="$(find "$bundle/services" -mindepth 2 -maxdepth 2 -type f \( \
  -name service.yaml -o -name release.yaml \) | wc -l | tr -d ' ')"
[[ "$source_manifest_count" -gt 0 && "$bundle_manifest_count" -eq "$source_manifest_count" ]] || \
  die "portable layout service manifest count is incomplete"
source_set_count="$(find sets -maxdepth 1 -type f -name '*.yaml' | wc -l | tr -d ' ')"
bundle_set_count="$(find "$bundle/sets" -maxdepth 1 -type f -name '*.yaml' | wc -l | tr -d ' ')"
[[ "$source_set_count" -gt 0 && "$bundle_set_count" -eq "$source_set_count" ]] || \
  die "portable layout set count is incomplete"

archive="$output_root/$bundle_name.tar.gz"
if [[ "$platform" == "windows-x64" ]]; then
  archive="$output_root/$bundle_name.zip"
  if command -v zip >/dev/null 2>&1; then
    (cd "$stage" && zip -q -r "$archive" "$bundle_name")
  else
    windows_bundle="$bundle"
    windows_archive="$archive"
    if command -v cygpath >/dev/null 2>&1; then
      windows_bundle="$(cygpath -w "$bundle")"
      windows_archive="$(cygpath -w "$archive")"
    fi
    OJOS_PACK_BUNDLE="$windows_bundle" OJOS_PACK_ARCHIVE="$windows_archive" \
      powershell -NoProfile -Command \
      'Compress-Archive -LiteralPath $env:OJOS_PACK_BUNDLE -DestinationPath $env:OJOS_PACK_ARCHIVE -Force'
  fi
else
  tar --sort=name --mtime='UTC 2020-01-01' --owner=0 --group=0 --numeric-owner \
    -czf "$archive" -C "$stage" "$bundle_name"
fi

native_count=0
native_artifacts=()
if [[ "$platform" == "windows-x64" ]]; then
  while IFS= read -r -d '' artifact; do
    native="$output_root/ojos-orchestrator-$version-windows-x64.msi"
    cp "$artifact" "$native"
    native_artifacts+=("$native")
    native_count=$((native_count + 1))
  done < <(find "$target_dir/bundle/msi" -maxdepth 1 -type f -name '*.msi' -print0 2>/dev/null)
  [[ "$native_count" -eq 1 ]] || die "expected exactly one MSI, found $native_count"
else
  for type in deb appimage; do
    native_count=0
    extension="$type"
    [[ "$type" == "appimage" ]] && extension="AppImage"
    while IFS= read -r -d '' artifact; do
      native="$output_root/ojos-orchestrator-$version-linux-x86_64.$extension"
      cp "$artifact" "$native"
      native_artifacts+=("$native")
      native_count=$((native_count + 1))
    done < <(find "$target_dir/bundle/$type" -maxdepth 1 -type f -iname "*.$extension" -print0 2>/dev/null)
    [[ "$native_count" -eq 1 ]] || die "expected exactly one $type artifact, found $native_count"
  done
fi

sbom="$output_root/$bundle_name.spdx.json"
if command -v syft >/dev/null 2>&1; then
  syft "dir:$bundle" -o "spdx-json=$sbom" >/dev/null
elif [[ "${OJOS_RELEASE_ALLOW_MISSING_SBOM:-0}" != "1" ]]; then
  die "syft is required to generate the SPDX SBOM"
fi

commit="${GITHUB_SHA:-$(git rev-parse HEAD 2>/dev/null || printf unknown)}"
builder="${GITHUB_WORKFLOW_REF:-local}"
provenance="$output_root/$bundle_name.provenance.json"
provenance_subjects=("$archive" "${native_artifacts[@]}")
[[ -f "$sbom" ]] && provenance_subjects+=("$sbom")
subject_json=""
subject_separator=""
for artifact in "${provenance_subjects[@]}"; do
  subject_json+="$subject_separator{\"name\":\"$(basename "$artifact")\",\"digest\":{\"sha256\":\"$(sha256_of "$artifact")\"}}"
  subject_separator=","
done
cat >"$provenance" <<EOF
{
  "_type": "https://in-toto.io/Statement/v1",
  "subject": [$subject_json],
  "predicateType": "https://slsa.dev/provenance/v1",
  "predicate": {
    "buildDefinition": {
      "buildType": "https://github.com/ojos/orchestrator/release-v1",
      "externalParameters": {"version": "$version", "channel": "$channel", "platform": "$platform"},
      "resolvedDependencies": [{"uri": "git+https://github.com/${GITHUB_REPOSITORY:-local/ojos}", "digest": {"gitCommit": "$commit"}}]
    },
    "runDetails": {"builder": {"id": "$builder"}, "metadata": {"invocationId": "${GITHUB_RUN_ID:-local}"}}
  }
}
EOF

checksum="$output_root/$bundle_name.SHA256SUMS"
checksum_inputs=("$archive" "${native_artifacts[@]}" "$provenance")
[[ -f "$sbom" ]] && checksum_inputs+=("$sbom")
(
  cd "$output_root"
  : >"$(basename "$checksum")"
  for artifact in "${checksum_inputs[@]}"; do
    sha256sum "$(basename "$artifact")" >>"$(basename "$checksum")"
  done
  sha256sum -c "$(basename "$checksum")" >/dev/null
)
trap - EXIT
rm -rf -- "$stage"
echo "pack-orchestrator-v1: artifacts ready in $output_root"
