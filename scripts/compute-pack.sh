#!/usr/bin/env bash
# 组装「算力包」：推理运行时（Ollama）+ 模型，放到没有网络的机器上，CommonBot 会自动启动并用上它。
# 在一台能上网的机器上运行（任何系统都行：模型文件与平台无关，运行时按目标平台下载）。
#
# 用法：
#   scripts/compute-pack.sh <平台> [选项]
#     平台：linux-arm64（DGX Spark）、mac（Apple Silicon / Intel 通用）、linux-x64
#   选项：
#     --chat <模型>      日常对话模型          默认 qwen3:14b      （约 9.3 GB）
#     --extra <模型>     另外带上的对话模型，可重复  默认 qwen3:30b  （约 18.6 GB，更强，适合建造师）
#     --embed <模型>     向量化模型            默认 bge-m3         （约 1.2 GB）
#     --vision <模型>    看图 / 文字识别模型    默认 qwen2.5vl:7b   （约 6 GB）
#     --no-extra         不带额外的对话模型
#     --out <目录>       输出目录，默认 dist/
#     --tar              另外打成一个 .tar 文件，便于拷贝
#
# 默认这一套约 35 GB，按 48 GB 内存的机器选的：日常模型、看图模型、向量化模型可以同时留在内存里，
# 换用 30b 时 Ollama 会自动把别的模型换出去。128 GB 的 Spark 可以用 --chat / --extra 换更大的模型。
#
# 产物是一个 compute 文件夹，放到下列任一位置即可（见使用说明）：
#   与 commonbot 程序同一个文件夹下；或数据目录下（~/.local/share/CommonBot/compute、
#   ~/Library/Application Support/CommonBot/compute）。
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

platform="${1:-}"; shift || true
case "$platform" in
  linux-arm64) runtime_url="https://ollama.com/download/ollama-linux-arm64.tar.zst" ;;
  linux-x64) runtime_url="https://ollama.com/download/ollama-linux-amd64.tar.zst" ;;
  mac) runtime_url="https://ollama.com/download/ollama-darwin.tgz" ;;
  *) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac

chat="qwen3:14b"; extra=("qwen3:30b"); embed="bge-m3"; vision="qwen2.5vl:7b"; out="dist"; make_tar=0
extra_given=0
while [ $# -gt 0 ]; do
  case "$1" in
    --chat) chat="$2"; shift 2 ;;
    --extra) if [ $extra_given -eq 0 ]; then extra=(); extra_given=1; fi; extra+=("$2"); shift 2 ;;
    --no-extra) extra=(); extra_given=1; shift ;;
    --embed) embed="$2"; shift 2 ;;
    --vision) vision="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --tar) make_tar=1; shift ;;
    *) echo "不认识的选项：$1" >&2; exit 1 ;;
  esac
done

# 先确认解压工具在，免得下完 1 GB 多的运行时才发现解不开
case "$runtime_url" in
  *.tar.zst) command -v zstd >/dev/null || { echo "需要 zstd 来解压运行时，请先安装（apt install zstd / brew install zstd）。" >&2; exit 1; } ;;
esac
# 在终端里显示下载进度；输出被重定向时保持安静
if [ -t 1 ]; then progress="--progress-bar"; else progress="-sS"; fi

dest="$out/commonbot-compute-$platform/compute"
mkdir -p "$dest/runtime" "$dest/models/blobs"
sha() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

# ---- 运行时 ----
if [ ! -e "$dest/runtime/.done" ]; then
  echo "下载运行时：$runtime_url"
  tmp="$dest/runtime.download"
  curl -fL $progress --retry 3 -C - -o "$tmp" "$runtime_url"
  rm -rf "$dest/runtime"; mkdir -p "$dest/runtime"
  case "$runtime_url" in
    *.tar.zst) zstd -dc "$tmp" | tar -x -C "$dest/runtime" ;;
    *) tar -xzf "$tmp" -C "$dest/runtime" ;;
  esac
  rm -f "$tmp"
  touch "$dest/runtime/.done"
fi

# ---- 模型：直接从模型库取清单和文件，按 Ollama 的存放方式放好，不需要在这台机器上运行 Ollama ----
registry="https://registry.ollama.ai"
pull() {
  local ref="$1" name tag
  name="${ref%%:*}"; tag="${ref#*:}"; [ "$tag" = "$ref" ] && tag=latest
  case "$name" in */*) ;; *) name="library/$name" ;; esac
  local mdir="$dest/models/manifests/registry.ollama.ai/$name"
  mkdir -p "$mdir"
  echo "模型 $ref"
  curl -fsSL --retry 3 -H "Accept: application/vnd.docker.distribution.manifest.v2+json" "$registry/v2/$name/manifests/$tag" -o "$mdir/$tag.tmp" \
    || { echo "  模型库里没有 $ref，请检查名称。" >&2; exit 1; }
  for digest in $(grep -o '"digest":"sha256:[0-9a-f]*"' "$mdir/$tag.tmp" | cut -d'"' -f4 | sort -u); do
    local hex="${digest#sha256:}" blob="$dest/models/blobs/sha256-${digest#sha256:}"
    if [ -f "$blob" ] && [ "$(sha "$blob")" = "$hex" ]; then continue; fi
    # 大文件可能中断：断点续传，下完校验
    curl -fL $progress --retry 5 --retry-delay 3 -C - -o "$blob.part" "$registry/v2/$name/blobs/$digest"
    [ "$(sha "$blob.part")" = "$hex" ] || { echo "  $digest 校验不一致，请重新运行本脚本。" >&2; rm -f "$blob.part"; exit 1; }
    mv "$blob.part" "$blob"
  done
  mv "$mdir/$tag.tmp" "$mdir/$tag"
}
models=("$chat" ${extra[@]+"${extra[@]}"})
[ -n "$embed" ] && models+=("$embed")
[ -n "$vision" ] && models+=("$vision")
for m in "${models[@]}"; do pull "$m"; done

# ---- 说明文件：CommonBot 读它来知道各个模型派什么用 ----
json_list() { local first=1; printf '['; for x in "$@"; do [ $first -eq 1 ] || printf ', '; printf '"%s"' "$x"; first=0; done; printf ']'; }
cat > "$dest/pack.json" <<JSON
{
  "format": 1,
  "platform": "$platform",
  "built": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "chat": "$chat",
  "extra": $(json_list ${extra[@]+"${extra[@]}"}),
  "embed": "$embed",
  "vision": "$vision"
}
JSON

echo
echo "算力包已就绪：$dest（$(du -sh "$dest" | cut -f1)）"
if [ $make_tar -eq 1 ]; then
  tar -C "$out/commonbot-compute-$platform" -cf "$out/commonbot-compute-$platform.tar" compute
  echo "已打包：$out/commonbot-compute-$platform.tar"
fi
