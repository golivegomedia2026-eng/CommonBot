#!/bin/sh
# 卸载 CommonBot 的程序和服务。数据（~/.local/share/CommonBot）不会被删除。
set -eu
if [ "$(id -u)" -ne 0 ]; then
  echo "请用 sudo 运行：sudo /opt/commonbot/uninstall.sh" >&2
  exit 1
fi
systemctl disable --now commonbot 2>/dev/null || true
rm -f /etc/systemd/system/commonbot.service
systemctl daemon-reload 2>/dev/null || true
# 只删除安装时放进去的东西
rm -rf /opt/commonbot/lib /opt/commonbot/docs /opt/commonbot/LICENSES
rm -f /opt/commonbot/commonbot /opt/commonbot/VERSION /opt/commonbot/uninstall.sh
rmdir /opt/commonbot 2>/dev/null || true
echo "已卸载。你的数据仍在主目录下的 .local/share/CommonBot，不需要时可以自行删除。"
