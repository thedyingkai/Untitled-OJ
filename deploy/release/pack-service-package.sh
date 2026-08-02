#!/usr/bin/env bash
# 打包单个 Service 为可安装的 release 包（zip），用于上传到 GitHub Release 资产。
# 包内至少包含 release.yaml；service.yaml 与 migrations/ 一并打入（如存在）。
# 用法：deploy/release/pack-service-package.sh <service-name> [输出目录]
set -euo pipefail

SERVICE="${1:?用法: pack-service-package.sh <service-name> [输出目录]}"
OUT_DIR="${2:-dist/service-packages}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SERVICE_DIR="$ROOT/services/$SERVICE"

[ -f "$SERVICE_DIR/release.yaml" ] || {
  echo "错误: $SERVICE_DIR/release.yaml 不存在" >&2
  exit 1
}

VERSION="$(sed -n 's/^version:[[:space:]]*//p' "$SERVICE_DIR/release.yaml" | head -1 | tr -d '\"' )"
[ -n "$VERSION" ] || { echo "错误: 无法从 release.yaml 读取 version" >&2; exit 1; }

mkdir -p "$ROOT/$OUT_DIR"
PKG="$ROOT/$OUT_DIR/$SERVICE-$VERSION.zip"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

cp "$SERVICE_DIR/release.yaml" "$STAGE/"
[ -f "$SERVICE_DIR/service.yaml" ] && cp "$SERVICE_DIR/service.yaml" "$STAGE/"
if [ -d "$SERVICE_DIR/migrations" ]; then
  mkdir -p "$STAGE/services/$SERVICE"
  cp -r "$SERVICE_DIR/migrations" "$STAGE/services/$SERVICE/"
fi

rm -f "$PKG"
(cd "$STAGE" && zip -qr "$PKG" .)

CHECKSUM="sha256:$(sha256sum "$PKG" | cut -d' ' -f1)"
echo "打包完成: $PKG"
echo "checksum: $CHECKSUM"
echo "上传到 GitHub Release 后，商店索引条目可填:"
echo "  \"repo\": \"<owner>/<repo>\"  或  \"source_url\": \"<资产下载 URL>\", \"checksum\": \"$CHECKSUM\""
