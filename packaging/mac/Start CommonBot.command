#!/bin/sh
# 双击启动 CommonBot，并在浏览器里打开。关闭这个窗口即退出。
cd "$(dirname "$0")"
( sleep 2; open "http://localhost:8600" ) &
exec ./commonbot
