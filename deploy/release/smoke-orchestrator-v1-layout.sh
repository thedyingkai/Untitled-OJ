#!/usr/bin/env bash
set -Eeuo pipefail

directory="${1:?artifact directory is required}"
version="${OJOS_RELEASE_VERSION:?OJOS_RELEASE_VERSION is required}"
channel="${OJOS_RELEASE_CHANNEL:?OJOS_RELEASE_CHANNEL is required}"
platform="${OJOS_RELEASE_PLATFORM:?OJOS_RELEASE_PLATFORM is required}"
version="${version#v}"
[[ "$platform" == "linux-x86_64" ]] || {
  echo "Linux layout smoke requires OJOS_RELEASE_PLATFORM=linux-x86_64" >&2
  exit 1
}
directory="$(cd "$directory" && pwd -P)"
base="ojos-orchestrator-$version-$channel-$platform"

for command_name in dpkg-deb timeout xvfb-run; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "$command_name is required for the Linux Desktop layout smoke" >&2
    exit 1
  }
done

temp_root="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
work="$(mktemp -d "$temp_root/ojos-orchestrator-layout.XXXXXX")"
cleanup() {
  [[ -n "$work" && "$work" == "$temp_root"/ojos-orchestrator-layout.* ]] && rm -rf -- "$work"
}
trap cleanup EXIT
mkdir -p "$work/cwd"

exact_desktop() {
  local root="$1"
  local -a matches=()
  mapfile -d '' matches < <(
    find "$root" -type f -name ojos-orchestrator-desktop -print0
  )
  [[ "${#matches[@]}" -eq 1 ]] || {
    echo "expected exactly one Desktop executable under $root, found ${#matches[@]}" >&2
    return 1
  }
  printf '%s' "${matches[0]}"
}

run_desktop() {
  local label="$1"
  local executable="$2"
  local data_dir="$work/data-$label"
  chmod +x "$executable"
  mkdir -p "$data_dir"
  echo "smoke-orchestrator-v1-layout: starting $label layout"
  (
    cd "$work/cwd"
    timeout 120s xvfb-run -a env OJOS_DESKTOP_SMOKE=1 \
      "$executable" --data-dir "$data_dir"
  )
}

archive="$directory/$base.tar.gz"
deb="$directory/ojos-orchestrator-$version-linux-x86_64.deb"
appimage="$directory/ojos-orchestrator-$version-linux-x86_64.AppImage"
[[ -s "$archive" && -s "$deb" && -s "$appimage" ]] || {
  echo "portable tar, DEB and AppImage are required in $directory" >&2
  exit 1
}

mkdir -p "$work/portable" "$work/deb"
tar -xzf "$archive" -C "$work/portable"
portable_desktop="$(exact_desktop "$work/portable")"
[[ "$portable_desktop" == */bin/ojos-orchestrator-desktop ]] || {
  echo "portable Desktop must be stored below the archive bin directory" >&2
  exit 1
}
run_desktop portable "$portable_desktop"

dpkg-deb -x "$deb" "$work/deb"
installed_desktop="$(exact_desktop "$work/deb")"
run_desktop deb "$installed_desktop"

chmod +x "$appimage"
mkdir -p "$work/data-appimage"
echo "smoke-orchestrator-v1-layout: starting AppImage layout"
(
  cd "$work/cwd"
  timeout 120s xvfb-run -a env APPIMAGE_EXTRACT_AND_RUN=1 \
    OJOS_DESKTOP_SMOKE=1 "$appimage" --data-dir "$work/data-appimage"
)

echo "Desktop portable, DEB and AppImage resource layouts started successfully"
