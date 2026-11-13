#!/bin/sh
# Linux 시험용 가짜 디스크 만들기 (WSL, VM에서 root로 실행)
#   - /root/wsc-test/disk.img (64MB, GPT: EFI FAT32 16MB + Linux ext4 나머지) → 루프 장치로 연결
#   - /root/wsc-test/bad.img + dm-error 구간 → 읽기 오류 섹터 시험용 장치 /dev/mapper/wsc-bad
#   - /root/wsc-test/out 저장 폴더
# 정리: sh linux-test-setup.sh clean
set -e
D=/root/wsc-test

if [ "$1" = "clean" ]; then
    dmsetup remove wsc-bad 2>/dev/null || true
    umount "$D/mnt" 2>/dev/null || true
    for l in $(losetup -j "$D/disk.img" -O NAME -n 2>/dev/null) $(losetup -j "$D/bad.img" -O NAME -n 2>/dev/null); do losetup -d "$l"; done
    rm -rf "$D"
    echo "정리 완료"
    exit 0
fi

mkdir -p "$D/out" "$D/mnt"
rm -f "$D/disk.img" "$D/bad.img"
truncate -s 64M "$D/disk.img"
sfdisk -q "$D/disk.img" <<EOF
label: gpt
start=2048, size=32768, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name="EFI"
start=34816, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name="data"
EOF
LOOP=$(losetup -f --show -P "$D/disk.img")
if command -v mkfs.vfat >/dev/null; then mkfs.vfat -F 32 -n WSCEFI "${LOOP}p1" >/dev/null; else mkfs.ext2 -q -L WSCEFI "${LOOP}p1"; fi
mkfs.ext4 -q -L wscdata "${LOOP}p2"
mount "${LOOP}p2" "$D/mnt"
echo "WS Collector 리눅스 시험 파일" > "$D/mnt/hello.txt"
dd if=/dev/urandom of="$D/mnt/random.bin" bs=1M count=8 status=none
sync
echo "루프 디스크: $LOOP (p2를 $D/mnt에 마운트)"

# 읽기 오류 장치: 16MB 중 4MB 지점부터 8섹터를 dm-error로
truncate -s 16M "$D/bad.img"
dd if=/dev/urandom of="$D/bad.img" bs=1M count=16 conv=notrunc status=none
BLOOP=$(losetup -f --show "$D/bad.img")
dmsetup create wsc-bad <<EOF
0 8192 linear $BLOOP 0
8192 8 error
8200 24568 linear $BLOOP 8200
EOF
echo "오류 장치: /dev/mapper/wsc-bad (섹터 8192~8199 읽기 실패)"
