#!/bin/sh
# 현장 점검 (실제 서버, 물리 USB): 대상 시스템에는 쓰지 않고, 결과는 모두 저장 폴더(USB)에 남김
#
#   sudo sh field-check.sh <CLI 실행 파일> <USB 저장 폴더> [시험할 장치]
#   예) sudo sh /mnt/usb/tools/field-check.sh /mnt/usb/WSCollector-CLI-linux-aarch64 /mnt/usb/Cases /dev/nvme0n1
#
# 하는 일 (모두 읽기만):
#   1. 시스템 정보, 커널, lockdown, 디스크 목록, 역할, 시리얼 (list --out)
#   2. 메모리 수집 가능 여부와 이유
#   3. 장치를 지정하면: 앞 256MB만 E01로 이미징 + 검증 + 속도 (sha256sum 원본 앞 256MB와 대조)
#   4. 저장 디스크 자체를 이미징하려 하면 거부되는지
# 결과: <저장 폴더>/field-check-<호스트>-<시각>/field-check.txt  → 이 파일을 개발자에게 보내 주세요
set -u
CLI=${1:?CLI 실행 파일 경로}
OUT=${2:?USB 저장 폴더}
DEV=${3:-}
[ "$(id -u)" = 0 ] || { echo "root로 실행하세요 (sudo)"; exit 2; }
[ -d "$OUT" ] || { echo "저장 폴더가 없습니다: $OUT"; exit 2; }
# USB가 noexec로 마운트되면 실행이 안 됨 → 미리 확인 (그때는 /dev/shm 등으로 복사해 실행)
"$CLI" --version >/dev/null 2>&1 || { echo "CLI를 실행할 수 없습니다: $CLI (noexec 마운트면 cp \"$CLI\" /dev/shm/ 후 그 경로로)"; exit 2; }
H=$(hostname 2>/dev/null || echo host)
D="$OUT/field-check-$H-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$D" || exit 2
R="$D/field-check.txt"
say() { printf '%s\n' "$*" | tee -a "$R"; }
run() { say "\$ $*"; "$@" >>"$R" 2>&1; say "(종료 코드 $?)"; }

say "=== WS Collector 현장 점검 $(date)"
say "=== 시스템"
run uname -a
[ -r /etc/os-release ] && run sh -c '. /etc/os-release; echo "$PRETTY_NAME"'
say "lockdown: $(cat /sys/kernel/security/lockdown 2>/dev/null || echo '파일 없음')"
say "Secure Boot: $( (dmesg 2>/dev/null | grep -i -m1 'secure boot') || echo '표시 없음')"
say "udev 데이터: $(ls /run/udev/data 2>/dev/null | wc -l)개"
run sha256sum "$CLI"
say "=== 디스크, 메모리 목록 (저장 위치 표시)"
run "$CLI" list --out "$OUT"
say "=== 마운트"
run sh -c "grep -v -E ' (proc|sysfs|cgroup2?|devpts|mqueue|debugfs|tracefs|securityfs|pstore|bpf|configfs|fusectl|hugetlbfs) ' /proc/self/mountinfo"

if [ -n "$DEV" ]; then
    say "=== $DEV 앞 256MB E01 이미징 + 검증"
    start=$(date +%s)
    run "$CLI" image -s "$DEV" -o "$D" --case FIELD --evidence E1 --examiner field-check --notes "현장 점검 (앞 256MB)" --max-bytes 256M -y
    say "소요 $(( $(date +%s) - start ))초"
    # 캐시 없는 읽기(iflag=direct)가 안 되는 dd(busybox 등)는 일반 읽기로. 실패하면 빈 입력 해시가 나오므로 걸러냄
    H1=$(dd if="$DEV" bs=1M count=256 iflag=direct 2>/dev/null | sha256sum | cut -c1-64)
    case "$H1" in e3b0c44298fc1c149afbf4c8996fb924*) H1=$(dd if="$DEV" bs=1M count=256 2>/dev/null | sha256sum | cut -c1-64) ;; esac
    case "$H1" in e3b0c44298fc1c149afbf4c8996fb924*) say "원본 앞 256MB sha256: dd로 읽지 못함 (대조 생략)" ;;
        *) say "원본 앞 256MB sha256: $H1"
           say "(위 결과의 SHA-256과 같아야 함. 마운트되어 쓰이는 중인 장치는 그 사이 내용이 바뀌어 다를 수 있음)" ;;
    esac
    say "마운트 여부: $(grep -c "^$DEV" /proc/mounts)개 (0이 아니면 사용 중)"
fi

say "=== 저장 위치가 있는 디스크 이미징 시도 (거부돼야 정상)"
OUTDEV=$(df -P "$OUT" | awk 'NR==2{print $1}')
say "저장 위치 장치: $OUTDEV"
case "$OUTDEV" in
    /dev/*) run "$CLI" image -s "$OUTDEV" -o "$D" --case FIELD --evidence X1 --examiner field-check --max-bytes 1M -y ;;
    *) say "블록 장치가 아니라 건너뜀" ;;
esac
# 시험용 이미지는 지우고 로그, 보고서만 남김 (대상 데이터가 USB에 남지 않게)
# (조각, 끝나지 않은 파일 포함. 파일만 지우므로 USB 빈 영역에는 남을 수 있음: 시험 뒤 USB를 지우고 쓰세요)
find "$D" \( -name '*.E[0-9][0-9]' -o -name '*.E[A-Z][A-Z]' -o -name '*.incomplete' -o -name '*.dd' -o -name '*.[0-9][0-9][0-9]' \) -type f -delete 2>/dev/null
say "=== 끝. 결과 파일: $R"
