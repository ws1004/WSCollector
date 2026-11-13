#!/bin/bash
# Linux GUI 시험 빌드 (WSL, Linux에서): 화면 점검 통로(devbridge)를 허용하는 CSP로 빌드
#   1) Windows 또는 Linux에서 화면 빌드: VITE_WS_BRIDGE=9334 npm run build   (dist/)
#   2) bash tools/linux-gui-build-test.sh   → $CARGO_TARGET_DIR/debug/WSCollector
# 실제 배포 빌드에는 쓰지 않음 (unsafe-eval, 127.0.0.1:9334 허용)
set -eu
cd "${REPO:-$(dirname "$0")/..}"
CSP="default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; font-src 'self' data:; script-src 'self' 'unsafe-eval'; connect-src ipc: http://ipc.localhost http://127.0.0.1:9334"
export TAURI_CONFIG="{\"app\":{\"security\":{\"csp\":\"$CSP\"}}}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/root/wsc-target}"
"${CARGO:-$HOME/.cargo/bin/cargo}" build -p ws-gui --features custom-protocol
echo "빌드: $CARGO_TARGET_DIR/debug/WSCollector"
