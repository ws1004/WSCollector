#!/bin/bash
# Linux GUI 시험 환경 (WSL, VM에서 root로): 가상 화면(Xvfb :99) + 화면 점검 중계기 + 시험 빌드 GUI
#   bash tools/linux-gui-test.sh start <GUI 실행 파일>   시작 (이미 떠 있으면 다시 띄움)
#   bash tools/linux-gui-test.sh stop                     모두 끄기
# 화면 조작, 캡처: node tools/ui-check.mjs bridge:9335 ... shot out.png
# GUI 시험 빌드: 화면을 VITE_WS_BRIDGE=9334로 빌드하고, TAURI_CONFIG로 CSP에 http://127.0.0.1:9334 허용
set -u
REPO="${REPO:-$(cd "$(dirname "$0")/.." && pwd)}"

stop() {
    # 이름으로 정확히 끔 (-f 패턴은 이 스크립트 자신의 명령줄까지 맞을 수 있음)
    pkill -x WSCollector 2>/dev/null
    pkill -f "^node .*ui-bridge.mjs" 2>/dev/null
    pkill -x Xvfb 2>/dev/null
    sleep 1
}

if [ "${1:-}" = "stop" ]; then
    stop
    echo "꺼짐"
    exit 0
fi

GUI="${2:?GUI 실행 파일 경로}"
stop
setsid nohup Xvfb :99 -screen 0 1440x900x24 >/tmp/xvfb.log 2>&1 < /dev/null &
setsid nohup node "$REPO/tools/ui-bridge.mjs" 9334 9335 >/tmp/bridge.log 2>&1 < /dev/null &
sleep 1
cd "$(dirname "$GUI")"
# 가상 화면에는 GPU가 없으므로 소프트웨어 그리기
DISPLAY=:99 GDK_BACKEND=x11 WEBKIT_DISABLE_DMABUF_RENDERER=1 LIBGL_ALWAYS_SOFTWARE=1 NO_AT_BRIDGE=1 \
    WS_TEST_MAX_BYTES="${WS_TEST_MAX_BYTES:-64M}" \
    setsid nohup "$GUI" >/tmp/gui.log 2>&1 < /dev/null &
for _ in $(seq 1 30); do
    if curl -s 127.0.0.1:9335/status | grep -q '"page":true'; then
        echo "화면 연결됨"
        exit 0
    fi
    sleep 1
done
echo "화면이 연결되지 않음. /tmp/gui.log:"
tail -20 /tmp/gui.log
exit 1
