#!/usr/bin/env bash
set -Eeuo pipefail

directory="${1:?artifact directory is required}"
platform="${OJOS_RELEASE_PLATFORM:?OJOS_RELEASE_PLATFORM is required}"
version="${OJOS_RELEASE_VERSION:?OJOS_RELEASE_VERSION is required}"
channel="${OJOS_RELEASE_CHANNEL:?OJOS_RELEASE_CHANNEL is required}"
version="${version#v}"
cd "$directory"
base="ojos-orchestrator-$version-$channel-$platform"
[[ -s "$base.SHA256SUMS" ]] || { echo "missing platform checksum manifest" >&2; exit 1; }
sha256sum -c "$base.SHA256SUMS"
[[ -s "$base.spdx.json" ]] || { echo "missing SPDX SBOM" >&2; exit 1; }
[[ -s "$base.provenance.json" ]] || { echo "missing provenance" >&2; exit 1; }
checksum_contains() {
  awk -v expected="$1" '
    {
      name = $2
      sub(/^\*/, "", name)
      if (name == expected) found = 1
    }
    END { exit(found ? 0 : 1) }
  ' "$base.SHA256SUMS" || {
    echo "checksum manifest does not cover $1" >&2
    exit 1
  }
}
checksum_contains "$base.spdx.json"
checksum_contains "$base.provenance.json"
provenance_contains() {
  grep -Fq "\"name\":\"$1\"" "$base.provenance.json" || {
    echo "provenance does not cover $1" >&2
    exit 1
  }
}
provenance_contains "$base.spdx.json"
if [[ "$platform" == "windows-x64" ]]; then
  [[ -s "$base.zip" && -s "ojos-orchestrator-$version-windows-x64.msi" ]] || exit 1
  checksum_contains "$base.zip"
  checksum_contains "ojos-orchestrator-$version-windows-x64.msi"
  provenance_contains "$base.zip"
  provenance_contains "ojos-orchestrator-$version-windows-x64.msi"
else
  [[ -s "$base.tar.gz" && -s "ojos-orchestrator-$version-linux-x86_64.deb" && \
     -s "ojos-orchestrator-$version-linux-x86_64.AppImage" ]] || exit 1
  checksum_contains "$base.tar.gz"
  checksum_contains "ojos-orchestrator-$version-linux-x86_64.deb"
  checksum_contains "ojos-orchestrator-$version-linux-x86_64.AppImage"
  provenance_contains "$base.tar.gz"
  provenance_contains "ojos-orchestrator-$version-linux-x86_64.deb"
  provenance_contains "ojos-orchestrator-$version-linux-x86_64.AppImage"
fi
if [[ "${OJOS_REQUIRE_SIGNATURES:-0}" == "1" ]]; then
  while IFS= read -r file; do
    [[ -s "$file.sigstore.json" ]] || { echo "missing signature bundle for $file" >&2; exit 1; }
  done < <(find . -maxdepth 1 -type f \( -name '*.msi' -o -name '*.zip' -o -name '*.deb' -o -name '*.AppImage' -o -name '*.tar.gz' -o -name '*.spdx.json' -o -name '*.provenance.json' -o -name '*.SHA256SUMS' \) -printf '%f\n' | sort)
fi
echo "artifact assertion passed: $base"
