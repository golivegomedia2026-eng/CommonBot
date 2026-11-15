#!/usr/bin/env bash
# 组装一个可以直接使用的离线包：程序（内含界面）+ PDF 解析库 + 使用说明 + 安装脚本。
# 先决条件（本脚本不代做，缺了就报错）：
#   scripts/fetch-vendor.sh                     取 PDF 解析库
#   pnpm --dir web install --frozen-lockfile && pnpm --dir web build
#   cargo build --release --locked -p commonbot-server
# 用法：scripts/package.sh            产物在 dist/commonbot-<版本>-<平台>.tar.gz
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) platform=linux-x64; lib=libpdfium.so ;;
  Linux-aarch64) platform=linux-arm64; lib=libpdfium.so ;;
  Darwin-arm64) platform=mac-arm64; lib=libpdfium.dylib ;;
  Darwin-x86_64) platform=mac-x64; lib=libpdfium.dylib ;;
  *) echo "不支持的平台：$(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
bin=target/release/commonbot
pdfium="vendor/pdfium/$platform/lib/$lib"

for f in "$bin" "$pdfium" web/dist/index.html; do
  [ -e "$f" ] || { echo "缺少 $f，请先按文件开头的说明准备。" >&2; exit 1; }
done
# 界面必须是真的构建产物，不能是占位页
grep -q 'assets/index-' web/dist/index.html || { echo "web/dist 是占位页，请先构建界面再重新编译程序。" >&2; exit 1; }
# 程序里嵌入的界面要和 web/dist 一致（先构建界面、后编译程序）
if [ web/dist/index.html -nt "$bin" ]; then
  echo "界面比程序新：请重新执行 cargo build --release，把最新界面嵌进去。" >&2; exit 1
fi

name="commonbot-$version-$platform"
out="dist/$name"
rm -rf "$out" "dist/$name.tar.gz"
mkdir -p "$out/lib" "$out/docs" "$out/LICENSES"

cp "$bin" "$out/commonbot"
cp "$pdfium" "$out/lib/"
cp "vendor/pdfium/$platform/LICENSE" "$out/LICENSES/pdfium.txt" 2>/dev/null || true
cp -R "vendor/pdfium/$platform/licenses" "$out/LICENSES/pdfium-third-party" 2>/dev/null || true
cp web/src/guide/zh-CN.md "$out/docs/使用说明.md"
cp web/src/guide/zh-TW.md "$out/docs/使用說明.md"
cp web/src/guide/en.md "$out/docs/User guide.md"
cp web/src/guide/ja.md "$out/docs/使い方.md"
echo "$version" > "$out/VERSION"

case "$platform" in
  linux-*)
    cp packaging/linux/install.sh packaging/linux/uninstall.sh packaging/linux/commonbot.service "$out/"
    chmod +x "$out/install.sh" "$out/uninstall.sh"
    ;;
  mac-*)
    cp "packaging/mac/Start CommonBot.command" "$out/"
    chmod +x "$out/Start CommonBot.command"
    ;;
esac
chmod +x "$out/commonbot"

tar -C dist -czf "dist/$name.tar.gz" "$name"
( cd dist && { sha256sum "$name.tar.gz" 2>/dev/null || shasum -a 256 "$name.tar.gz"; } > "$name.tar.gz.sha256" )
echo "已生成 dist/$name.tar.gz（$(du -h "dist/$name.tar.gz" | cut -f1)）"
cat "dist/$name.tar.gz.sha256"
