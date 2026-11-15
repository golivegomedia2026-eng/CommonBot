#!/usr/bin/env bash
# 存档：记一行进度、提交、推送并核对远端。
# 用法：scripts/checkpoint.sh "<阶段>" "<完成了什么>" "<下一步>"
set -euo pipefail

if [ $# -ne 3 ]; then
  echo "用法：$0 \"<阶段>\" \"<完成了什么>\" \"<下一步>\"" >&2
  exit 2
fi

cd "$(git rev-parse --show-toplevel)"
branch=$(git rev-parse --abbrev-ref HEAD)
if [ "$branch" = "main" ] || [ "$branch" = "master" ]; then
  echo "拒绝在 $branch 上存档，请切到开发分支。" >&2
  exit 1
fi

progress=docs/v1/PROGRESS.md
# 表格里的竖线会破坏列，替换成全角
esc() { printf '%s' "$1" | tr '\n' ' ' | sed 's/|/｜/g'; }
printf '| %s | %s | %s | %s |\n' "$(date -u '+%Y-%m-%d %H:%M UTC')" "$(esc "$1")" "$(esc "$2")" "$(esc "$3")" >> "$progress"

git add -A
git commit -q -m "$1：$2

下一步：$3"

ok=0
for wait in 0 5 15; do
  sleep "$wait"
  if git push -q -u origin "$branch"; then ok=1; break; fi
  echo "推送失败，稍后重试…" >&2
done
if [ "$ok" -ne 1 ]; then
  echo "推送失败：这次存档只保存在本地。" >&2
  exit 1
fi

local_sha=$(git rev-parse HEAD)
remote_sha=$(git ls-remote origin "refs/heads/$branch" | cut -f1)
if [ "$local_sha" != "$remote_sha" ]; then
  echo "远端分支（$remote_sha）与本地（$local_sha）不一致。" >&2
  exit 1
fi
echo "已存档并核对：$branch @ ${local_sha:0:7}"
