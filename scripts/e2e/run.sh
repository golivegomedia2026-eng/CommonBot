#!/usr/bin/env bash
# 浏览器回归：用假模型启动真实的服务，在浏览器里把主要流程走一遍，并截图。
# 需要：已构建的界面（pnpm --dir web build）、cargo、Node 和 Playwright（含 Chromium）。
# 用法：scripts/e2e/run.sh [截图输出目录]
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
out="${1:-$(mktemp -d)}"
mkdir -p "$out"
work=$(mktemp -d)

cargo build -q -p commonbot-server -p commonbot-mock-llm
target/debug/commonbot-mock-llm 11434 > "$work/mock.log" 2>&1 &
mock=$!
# COMMONBOT_WEB_ALLOW_PRIVATE：假搜索服务在本机，测试时允许读取它的网页
COMMONBOT_WEB_ALLOW_PRIVATE=1 COMMONBOT_DATA_DIR="$work/data" target/debug/commonbot --port 8600 > "$work/server.log" 2>&1 &
server=$!
trap 'kill $mock $server 2>/dev/null || true' EXIT
for _ in $(seq 1 40); do curl -sf http://127.0.0.1:8600/api/health > /dev/null && break; sleep 0.25; done

node scripts/e2e/flows.js "$out" "$PWD/crates/core/tests/fixtures"
echo "截图在 $out"
