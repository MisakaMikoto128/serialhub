#!/usr/bin/env bash
# qa-web-w2 链接矩阵核验脚本 (席位 707)
# 用法: bash check-links.sh   (仅依赖 curl; Git Bash 自带)
set -u
BASE="https://misakamikoto128.github.io/serialhub"

echo "== A. 站内资源 GET 核验 =="
ASSETS=(
  "assets/favicon.svg"
  "assets/icon.png"
  "assets/shot-cli.png"
  "assets/shot-create-bridge.png"
  "assets/shot-dashboard.png"
  "assets/shot-drawer.png"
  "assets/shot-flow-stats.png"
  "assets/shot-tray.png"
  "assets/demo.mp4"
  "assets/og-image.png"
)
for a in "${ASSETS[@]}"; do
  curl -s -o /dev/null -w "%{http_code}  %{size_download}B  %{content_type}  ${BASE}/${a}\n" "${BASE}/${a}"
done

echo
echo "== B. 外链 HEAD/GET 核验 (跟随跳转) =="
LINKS=(
  "https://github.com/MisakaMikoto128/serialhub"
  "https://github.com/MisakaMikoto128/serialhub/releases"
  "https://github.com/MisakaMikoto128/serialhub/blob/main/CHANGELOG.md"
  "https://github.com/MisakaMikoto128/serialhub/blob/main/LICENSE"
  "https://github.com/MisakaMikoto128/serialhub/blob/main/SECURITY.md"
  "https://github.com/MisakaMikoto128/serialhub/blob/main/docs/manual/%E7%94%A8%E6%88%B7%E4%BD%BF%E7%94%A8%E6%89%8B%E5%86%8C.md"
  "https://github.com/MisakaMikoto128/serialhub/releases/download/v2.0.2/serialhub-v2.0.2-windows-x86_64.zip"
  "https://github.com/MisakaMikoto128/serialhub/releases/download/v2.0.2/serialhub-v2.0.2-linux-x86_64.tar.gz"
  "https://github.com/MisakaMikoto128/serialhub/releases/download/v2.0.2/serialhub-v2.0.2-macos-aarch64-unsigned.tar.gz"
)
for u in "${LINKS[@]}"; do
  code=$(curl -s -o /dev/null -I -L -w "%{http_code}" --max-time 30 "$u")
  if [ "$code" = "000" ] || [ "$code" = "429" ] || [ "$code" = "403" ]; then
    sleep 3
    code2=$(curl -s -o /dev/null -I -L -w "%{http_code}" --max-time 30 "$u")
    echo "RETRY ${code}->${code2}  ${u}"
  else
    echo "${code}  ${u}"
  fi
done

echo
echo "== C. 新鲜度自检数据源 (api.github.com latest) =="
curl -s --max-time 30 "https://api.github.com/repos/MisakaMikoto128/serialhub/releases/latest" \
  | grep -E '"tag_name"|"name"|"digest"|"size"' | head -20
