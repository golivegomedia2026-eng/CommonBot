#!/bin/sh
# 把 CommonBot 装到 /opt/commonbot，并登记为开机自动运行的系统服务。
# 服务以运行 sudo 的那个用户的身份运行，数据放在他的主目录下（~/.local/share/CommonBot）。
# 重复执行即为升级：只替换程序文件，不动数据。
set -eu

if [ "$(id -u)" -ne 0 ]; then
  echo "请用 sudo 运行：sudo ./install.sh" >&2
  exit 1
fi
owner="${SUDO_USER:-}"
if [ -z "$owner" ] || [ "$owner" = root ]; then
  echo "请以普通用户的身份通过 sudo 运行（不要直接用 root 登录后运行），这样数据会放在你的主目录下。" >&2
  exit 1
fi
here="$(cd "$(dirname "$0")" && pwd)"
dest=/opt/commonbot

command -v systemctl >/dev/null 2>&1 || { echo "这台机器没有 systemd，无法登记为服务。可以直接运行 ./commonbot。" >&2; exit 1; }

systemctl stop commonbot 2>/dev/null || true
mkdir -p "$dest/lib" "$dest/docs" "$dest/LICENSES"
cp "$here/commonbot" "$dest/commonbot"
cp "$here"/lib/* "$dest/lib/"
cp -R "$here/docs/." "$dest/docs/"
cp -R "$here/LICENSES/." "$dest/LICENSES/" 2>/dev/null || true
cp "$here/VERSION" "$here/uninstall.sh" "$dest/"
chmod 755 "$dest/commonbot" "$dest/uninstall.sh"

# 算力包（推理运行时 + 模型）：和安装包放在同一个文件夹、或它的上一级时一并装上。
# 体积很大，所以是移动而不是复制；跨磁盘时系统会自动改为复制。
for c in "$here/compute" "$here/../compute"; do
  if [ -f "$c/pack.json" ]; then
    echo "正在安装算力包（$(du -sh "$c" | cut -f1)）……"
    rm -rf "$dest/compute"
    mv "$c" "$dest/compute"
    chown -R "$owner" "$dest/compute"
    break
  fi
done

sed "s/__USER__/$owner/" "$here/commonbot.service" > /etc/systemd/system/commonbot.service
systemctl daemon-reload
systemctl enable --now commonbot

echo "CommonBot $(cat "$dest/VERSION") 已安装并启动。"
echo "  在这台机器上打开：http://localhost:8600"
echo "  让其他设备访问：  sudo -u $owner $dest/commonbot --lan on && sudo systemctl restart commonbot"
echo "  查看运行情况：    systemctl status commonbot"
