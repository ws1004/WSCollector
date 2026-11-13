#!/bin/bash
# 터미널 화면(TUI) 점검: tmux 안에서 실행하고 키를 보내며 화면 글자를 캡처
#   bash tools/tui-check.sh start <CLI 실행 파일>     tui 실행 (120x40)
#   bash tools/tui-check.sh keys <tmux 키...>          키 보내기 (예: "2026-TUI" Enter Tab Space Down)
#   bash tools/tui-check.sh shot                       지금 화면 출력
#   bash tools/tui-check.sh stop
set -u
S=wsc-tui
case "${1:-}" in
    start)
        tmux kill-session -t $S 2>/dev/null
        # 경로에 공백이 있어도 되게 따옴표 처리
        tmux new-session -d -s $S -x 120 -y 40 "$(printf '%q' "$2") tui; echo; echo \"[종료 코드 \$?]\"; sleep 600"
        sleep 1
        ;;
    keys)
        shift
        for k in "$@"; do
            tmux send-keys -t $S -l "$k" 2>/dev/null
            sleep 0.15
        done
        ;;
    key)
        shift
        tmux send-keys -t $S "$@"
        sleep 0.3
        ;;
    shot)
        tmux capture-pane -t $S -p
        ;;
    stop)
        tmux kill-session -t $S 2>/dev/null
        ;;
esac
