#!/usr/bin/env bash
# Package the OJOS Orchestrator v0.1.0 alpha.
#
# Produces, under dist/:
#   - dist/services/ojos-service-<name>-<version>.tar.gz  (downloadable per-service
#     release packages: each holds the service's release.yaml + service.yaml)
#   - dist/services/manifest.json                          (file + sha256 per service)
#   - dist/<bundle>/                                       (staged platform bundle)
#   - dist/<bundle>.(zip|tar.gz)                           (the archived bundle)
#
# The bundle contains Desktop, daemon and TUI binaries, the Web UI build output
# (manager/web/dist, served by the daemon at /), plus the runtime data the
# daemon/TUI read at --repo-root: platform/schemas/orchestrator,
# services/*/{service,release}.yaml, and sets/.
# Cross-platform: Git Bash on Windows and Linux CI.
set -Eeuo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
cd "$repo_root"

version="${OJOS_ALPHA_VERSION:-v0.1.0-alpha}"

platform="${OJOS_ALPHA_PLATFORM:-}"
if [ -z "$platform" ]; then
  case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) platform="windows-x64" ;;
    Linux*) platform="linux-x64" ;;
    Darwin*) platform="macos-x64" ;;
    *) platform="unknown" ;;
  esac
fi

dist="$repo_root/dist"
services_out="$dist/services"
rm -rf "$dist"
mkdir -p "$services_out"

win_path() { if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else printf '%s' "$1"; fi; }

sha256_of() {
  local f="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$f" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$f" | awk '{print $1}'
  else
    MSYS_NO_PATHCONV=1 powershell -NoProfile -Command \
      "(Get-FileHash -Algorithm SHA256 -LiteralPath '$(win_path "$f")').Hash.ToLower()" | tr -d '\r\n'
  fi
}

# Base URL the published packages are downloaded from. When set, each packaged
# release.yaml's source.url is rewritten to its own download URL so that a
# `release.install` with a matching source_url override passes the orchestrator's
# strict manifest-provenance check. The in-repo/bundle release.yaml files are left
# untouched (still local://). Example for GitHub:
#   OJOS_ALPHA_BASE_URL=https://github.com/<owner>/<repo>/releases/download/<tag>
base_url="${OJOS_ALPHA_BASE_URL:-}"

# --- per-service downloadable release packages ---
services="gateway auth-service judge-api problem-service user-service storage-service"
entries=""
for svc in $services; do
  if [ ! -f "services/$svc/release.yaml" ]; then
    echo "pack-alpha: skip $svc (no release.yaml)" >&2
    continue
  fi
  stage="$(mktemp -d)"
  mkdir -p "$stage/$svc"
  cp "services/$svc/release.yaml" "$stage/$svc/release.yaml"
  [ -f "services/$svc/service.yaml" ] && cp "services/$svc/service.yaml" "$stage/$svc/service.yaml"
  pkg="ojos-service-$svc-$version.tar.gz"
  if [ -n "$base_url" ]; then
    # Point the packaged manifest at its own download URL (matches the source_url
    # override used at install time). Only the source.url line is rewritten.
    sed -i "s|url: local://services/$svc|url: $base_url/$pkg|" "$stage/$svc/release.yaml"
  fi
  tar -czf "$services_out/$pkg" -C "$stage" "$svc"
  rm -rf "$stage"
  sum="$(sha256_of "$services_out/$pkg")"
  entry="  \"$svc\": { \"file\": \"$pkg\", \"sha256\": \"sha256:$sum\" }"
  if [ -z "$entries" ]; then entries="$entry"; else entries="$entries,
$entry"; fi
  echo "pack-alpha: packaged $pkg (sha256:$sum)"
done
printf '{\n%s\n}\n' "$entries" >"$services_out/manifest.json"

# --- platform binary bundle ---
bundle_name="ojos-orchestrator-$version-$platform"
bundle="$dist/$bundle_name"
mkdir -p "$bundle" "$bundle/platform/schemas" "$bundle/services" "$bundle/sets"

copied_bin=0
missing_bin=0
for b in ojos-orchestrator-daemon ojos-orchestrator-tui ojos-orchestrator-desktop; do
  if [ -f "target/release/$b.exe" ]; then
    cp "target/release/$b.exe" "$bundle/"
    copied_bin=$((copied_bin + 1))
  elif [ -f "target/release/$b" ]; then
    cp "target/release/$b" "$bundle/"
    copied_bin=$((copied_bin + 1))
  else
    echo "pack-alpha: missing required binary target/release/$b[.exe]" >&2
    missing_bin=$((missing_bin + 1))
  fi
done
if [ "$missing_bin" -ne 0 ] || [ "$copied_bin" -ne 3 ]; then
  echo "pack-alpha: build daemon, TUI and Desktop before packaging" >&2
  exit 1
fi

# Web UI: the daemon serves <repo-root>/manager/web/dist at /, so the bundle must
# carry the build output. Without it, opening 8090 only shows the bootstrap page.
if [ ! -f manager/web/dist/index.html ]; then
  echo "pack-alpha: no Web UI build output in manager/web/dist; run 'cd manager/web && npm ci && npm run build' first" >&2
  exit 1
fi
mkdir -p "$bundle/manager/web"
cp -R manager/web/dist "$bundle/manager/web/dist"

cp -R platform/schemas/orchestrator "$bundle/platform/schemas/orchestrator"
[ -d sets ] && cp -R sets/. "$bundle/sets/" 2>/dev/null || true
# Default OJOS_STORE_INDEX_URL is the repo-relative store/index.json; ship it so the
# Web UI store page works out of the box in the bundle.
if [ -f store/index.json ]; then
  mkdir -p "$bundle/store"
  cp store/index.json "$bundle/store/index.json"
fi
for svc_dir in services/*/; do
  name="$(basename "$svc_dir")"
  if [ -f "$svc_dir/service.yaml" ] || [ -f "$svc_dir/release.yaml" ]; then
    mkdir -p "$bundle/services/$name"
    [ -f "$svc_dir/service.yaml" ] && cp "$svc_dir/service.yaml" "$bundle/services/$name/"
    [ -f "$svc_dir/release.yaml" ] && cp "$svc_dir/release.yaml" "$bundle/services/$name/"
  fi
done

cat >"$bundle/README.txt" <<TXT
OJOS Orchestrator $version ($platform) —— alpha

在本目录直接运行编排器（--repo-root . 会读取同目录的 platform/、services/、sets/、store/、
manager/web/dist）：

  ojos-orchestrator-daemon  --repo-root . --bind 127.0.0.1:8090
  ojos-orchestrator-tui     --repo-root .
  ojos-orchestrator-desktop --repo-root .

图形主入口（本地 WebView，无需浏览器）：

  ojos-orchestrator-desktop --repo-root .

本 bundle 已内置 Web UI 构建产物（manager/web/dist），无需另行构建或部署；
Desktop 默认在进程内启动只监听 loopback 随机端口的控制面。也可用
--daemon-url https://host:port 连接已有 daemon；远端明文 HTTP 默认拒绝。

命令行入口：
  查看它管理的 services：  curl http://127.0.0.1:8090/services
  健康检查：              curl http://127.0.0.1:8090/health

【重要】数据持久化：未设置 ORCHESTRATOR_DATABASE_URL 时编排器使用内存 store，
daemon 一退出，所有拓扑 / Endpoint / Link / Operation 记录全部丢失。要保留数据必须
先建库并指向它（schema 见仓库 services/orchestrator/migrations/）：

  ORCHESTRATOR_DATABASE_URL=postgres://user:pass@127.0.0.1:5432/ojos_orchestrator?sslmode=disable \\
    ojos-orchestrator-daemon --repo-root . --bind 127.0.0.1:8090

【重要】访问控制：未设置 ORCHESTRATOR_INTERNAL_TOKEN 时 daemon 对所有 API fail-open
（任何能访问该端口的人都能改拓扑）。除本机试用外，请设置该变量，并在请求头
x-ojos-orchestrator-token 中携带同一个值（Web UI 首次访问会提示输入）。

拉取 service 下载（从 URL 拉取 release 包并注册）：
  设置 ORCHESTRATOR_RELEASE_PACKAGE_LOAD=1 再启动 daemon，然后：
  POST /releases/<service>/install  body: {"source_url":"<包URL>","release_checksum":"sha256:...","confirm":"true"}

完整说明见仓库 docs/alpha-quickstart.md。judge-worker 需 Linux + nsjail，见文档。
TXT

# --- archive the bundle ---
if [ "${platform%%-*}" = "windows" ]; then
  archive="$dist/$bundle_name.zip"
  if command -v zip >/dev/null 2>&1; then
    (cd "$dist" && zip -qr "$bundle_name.zip" "$bundle_name")
  else
    MSYS_NO_PATHCONV=1 powershell -NoProfile -Command \
      "Compress-Archive -Path '$(win_path "$bundle")\\*' -DestinationPath '$(win_path "$archive")' -Force"
  fi
else
  archive="$dist/$bundle_name.tar.gz"
  tar -czf "$archive" -C "$dist" "$bundle_name"
fi
bundle_sum="$(sha256_of "$archive")"

echo "pack-alpha: bundle $(basename "$archive") (sha256:$bundle_sum)"
echo "pack-alpha: done -> $dist"
ls -1 "$dist" "$services_out"
