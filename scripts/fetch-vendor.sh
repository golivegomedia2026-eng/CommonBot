#!/usr/bin/env bash
# 下载随产品一起分发的第三方运行库到 vendor/（不进仓库）。目前只有 PDF 解析库 pdfium。
# 用法：scripts/fetch-vendor.sh            按当前机器的系统和架构下载
#       scripts/fetch-vendor.sh mac-arm64  指定平台：linux-x64 linux-arm64 mac-arm64 mac-x64 win-x64
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# 默认取最新发行版；要固定版本时设置 PDFIUM_TAG，例如 PDFIUM_TAG=chromium/7543。
# 实际取到的版本记在 vendor/pdfium/<平台>/VERSION，打包前核对。
PDFIUM_TAG="${PDFIUM_TAG:-latest}"

platform="${1:-}"
if [ -z "$platform" ]; then
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) platform=linux-x64 ;;
    Linux-aarch64) platform=linux-arm64 ;;
    Darwin-arm64) platform=mac-arm64 ;;
    Darwin-x86_64) platform=mac-x64 ;;
    *) echo "不支持的平台：$(uname -s)-$(uname -m)" >&2; exit 1 ;;
  esac
fi

dest="vendor/pdfium/$platform"
if [ -f "$dest/.tag" ] && [ "$(cat "$dest/.tag")" = "$PDFIUM_TAG" ]; then
  echo "pdfium ($platform) 已就绪：$dest"
  exit 0
fi
rm -rf "$dest"; mkdir -p "$dest"
if [ "$PDFIUM_TAG" = latest ]; then
  url="https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-$platform.tgz"
else
  url="https://github.com/bblanchon/pdfium-binaries/releases/download/${PDFIUM_TAG//\//%2F}/pdfium-$platform.tgz"
fi
echo "下载 $url"
curl -fsSL --retry 3 "$url" -o "$dest/pdfium.tgz"
tar -xzf "$dest/pdfium.tgz" -C "$dest"
rm "$dest/pdfium.tgz"
# 只保留运行库、版本和许可文件
find "$dest" -mindepth 1 -maxdepth 1 ! -name lib ! -name bin ! -name LICENSE ! -name licenses ! -name VERSION -exec rm -rf {} +
echo "$PDFIUM_TAG" > "$dest/.tag"
cat "$dest/VERSION" 2>/dev/null || true
ls "$dest"/lib "$dest"/bin 2>/dev/null | head || true
