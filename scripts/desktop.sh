#!/usr/bin/env bash
# 构建桌面外壳的安装包（Linux：DEB；Mac：DMG）。产物复制到 dist/。
# 先决条件：scripts/fetch-vendor.sh、构建好的界面（pnpm --dir web build）、系统的窗口开发库（见 docs/RELEASE.md）。
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) platform=linux-x64; lib=libpdfium.so; bundles=deb ;;
  Linux-aarch64) platform=linux-arm64; lib=libpdfium.so; bundles=deb ;;
  Darwin-arm64) platform=mac-arm64; lib=libpdfium.dylib; bundles=dmg ;;
  Darwin-x86_64) platform=mac-x64; lib=libpdfium.dylib; bundles=dmg ;;
  *) echo "不支持的平台：$(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
pdfium="vendor/pdfium/$platform/lib/$lib"
[ -e "$pdfium" ] || { echo "缺少 $pdfium，请先运行 scripts/fetch-vendor.sh。" >&2; exit 1; }
grep -q 'assets/index-' web/dist/index.html 2>/dev/null || { echo "界面还没构建，请先运行 pnpm --dir web build。" >&2; exit 1; }

# 外壳的版本号跟随主程序
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# PDF 解析库作为资源打进安装包；程序启动时到资源目录里找它
config=$(printf '{"version":"%s","bundle":{"resources":{"../../%s":"%s"}}}' "$version" "$pdfium" "$lib")

cd apps/desktop
npx --yes @tauri-apps/cli@2 build --bundles "$bundles" --config "$config" -- --locked
cd ../..

mkdir -p dist
find apps/desktop/target/release/bundle -maxdepth 2 \( -name '*.deb' -o -name '*.dmg' \) -newer apps/desktop/tauri.conf.json -exec cp {} dist/ \;
ls -l dist | grep -E '\.(deb|dmg)$'
