# WS Collector

포터블 라이브 증거 수집 도구 (WS Series #1). 디스크 이미징, 메모리 덤프(Windows, Linux), 네트워크 전송을 지원합니다.

| 플랫폼 | GUI | 터미널 화면(TUI) | CLI | 실행 파일 |
|------|------|------|------|------|
| Windows 10 / Server 2016 이상 (x64) | ✅ | ✅ | ✅ | `WSCollector-GUI-windows-x86_64.exe`, `WSCollector-CLI-windows-x86_64.exe` |
| Linux x86_64 데스크톱: Ubuntu 22.04+, Debian 12+, RHEL, Rocky 9+ | ✅ | ✅ | ✅ | `WSCollector-GUI-linux-x86_64` (+ CLI) |
| Linux x86_64 (커널 3.2 이상, 오래된 배포판, 서버, SSH) | – | ✅ | ✅ | `WSCollector-CLI-linux-x86_64` |
| Linux ARM64 (커널 4.1 이상) | – | ✅ | ✅ | `WSCollector-CLI-linux-aarch64` |

- 메모리 덤프: Linux (AVML과 같은 방식, 드라이버 없음), Windows x64 (WinPmem 드라이버를 USB에서 적재, 끝나면 서비스 삭제).

## 지원 OS

### Windows

| OS | GUI | CLI, TUI |
|------|------|------|
| Windows 10 / 11 (x64) | ✅ | ✅ |
| Windows Server 2016 / 2019 / 2022 / 2025 | ✅ | ✅ |
| Windows 8.1 / Server 2012 R2 이하 | ❌ | ❌ |

GUI는 WebView2 런타임이 필요합니다 (Windows 11,최신 Windows 10에는 기본 설치).

### Linux CLI, TUI (`WSCollector-CLI-linux-x86_64`, `-aarch64`)

라이브러리를 모두 넣은 정적 빌드라서 배포판과 상관없이 **커널만 맞으면** 파일 하나로 실행됩니다. 최소 커널: x86_64(Intel/AMD) 3.2, ARM64 4.1.

| 배포판 | 기본 커널 | 지원 | 실제 시험 |
|------|------|------|------|
| RHEL / CentOS 7 | 3.10 | ✅ | ✅ (메모리 덤프 /dev/crash 포함) |
| RHEL / Rocky / Alma 8 | 4.18 | ✅ | |
| RHEL / Rocky / Alma 9 | 5.14 | ✅ | ✅ (Secure Boot 켜짐) |
| Ubuntu 14.04 / 16.04 / 18.04 / 20.04 | 3.13 / 4.4 / 4.15 / 5.4 | ✅ | |
| Ubuntu 22.04 / 24.04 | 5.15 / 6.8 | ✅ | ✅ (22.04: LUKS+LVM, ARM64 / 24.04: WSL) |
| Debian 8 / 9 / 10 / 11 / 12 | 3.16 / 4.9 / 4.19 / 5.10 / 6.1 | ✅ | |
| SUSE (SLES) 12 / 15 | 3.12 / 4.12 | ✅ | |
| Amazon Linux 2 / 2023 | 4.14,5.10 / 6.1 | ✅ | |
| CentOS 6, SLES 11 | 2.6.32 / 3.0 | ❌ 커널이 너무 오래됨 | |

"실제 시험"이 빈 칸인 배포판은 커널 조건상 동작해야 하지만 직접 돌려 보지는 않았습니다.

### Linux GUI (`WSCollector-GUI-linux-x86_64`)

화면 엔진(webkit2gtk 4.1)과 glibc 2.34 이상이 필요해서 최근 데스크톱 배포판에서만 동작합니다.

| 지원 | 지원 안 됨 (TUI 사용) |
|------|------|
| Ubuntu 22.04 이상 | Ubuntu 20.04 이하 |
| Debian 12 이상 | Debian 11 이하 |
| RHEL / Rocky / Alma 9 이상 | RHEL 8 이하, CentOS 7 |
| Fedora 36 이상 | 화면 없는 서버 |

`libwebkit2gtk-4.1-0`이 없는 시스템은 TUI(`WSCollector-CLI-linux-x86_64 tui`)를 쓰세요. 라이브 대상에 패키지를 설치하는 것은 흔적이 되므로 권하지 않습니다.

### Linux 메모리 덤프 조건

- root 권한 필요 (커널 모듈은 적재하지 않음)
- 커널 lockdown `integrity`(Secure Boot 기본)는 수집됨, `confidentiality`는 커널이 막아 수집 불가 (화면에 이유 표시)
- 컨테이너 안에서는 수집 불가

## 배포 폴더 구성 (USB)

`WSCollector` 폴더를 통째로 USB에 복사해서 씁니다 (약 34MB, 설치 없음).

```
<USB>/WSCollector/
  WSCollector-GUI-windows-x86_64.exe       Windows GUI (관리자 권한 자동 요청)
  WSCollector-CLI-windows-x86_64.exe   Windows CLI, TUI
  WSCollector-GUI-linux-x86_64             Linux GUI (sudo -E로 실행)
  WSCollector-CLI-linux-x86_64         Linux CLI, TUI (Intel/AMD)
  WSCollector-CLI-linux-aarch64        Linux CLI, TUI (ARM64)
  drivers/winpmem_x64.sys              Windows 메모리 덤프용 WinPmem 드라이버 (Velocidex, Apache 2.0, Microsoft 서명)
  drivers/WINPMEM-LICENSE.txt          WinPmem 라이선스
  tools/field-check.sh                 Linux 현장 점검 (읽기만, 결과는 USB에)
  README.md                            이 문서
  SHA256SUMS.txt                       위 파일들의 SHA-256 (도구가 바뀌지 않았는지 확인)
  runtime/                             실행 시 자동 생성: 화면 엔진 데이터, 캐시, 임시 폴더 (대상 PC의 사용자 폴더 대신)
  runtime/webview2/                    (선택, Windows) WebView2 Fixed Version 런타임
```

파일 이름 규칙: `WSCollector-{GUI|CLI}-<OS>-<CPU>`.

필요한 것만 들고 가도 됩니다.

| 대상 | 필요한 파일 |
|------|------|
| Windows PC | `WSCollector-GUI-windows-x86_64.exe`, `WSCollector-CLI-windows-x86_64.exe`, 메모리를 받으려면 `drivers/` 폴더 |
| Linux 서버 (화면 없음, SSH) | `WSCollector-CLI-linux-x86_64` (ARM64면 `WSCollector-CLI-linux-aarch64`) |
| Linux 데스크톱 | `WSCollector-GUI-linux-x86_64` + 위 CLI (화면 엔진이 없으면 TUI 사용) |

### USB 준비

- 도구 폴더와 증거 저장 폴더를 같은 USB에 둬도 됩니다. 예: `E:\WSCollector\`(도구), `E:\Cases\`(증거). 막는 것은 저장 위치가 이미징 대상과 겹칠 때뿐입니다.
- 파일시스템은 **exFAT 또는 NTFS**를 권합니다. FAT32면 파일 하나 4GB 한계 때문에 E01이 약 3.9GB마다 `.E02, .E03 …`으로 나뉩니다.
- Linux에서 USB가 `noexec`로 마운트되어 실행이 안 되면 `/dev/shm`에 복사해서 실행합니다 (아래 Linux 메모리 덤프 절 참고).
- 실행하면 도구 폴더에 `runtime/`이 생깁니다. 지우지 않아도 됩니다.

### 도구 해시 확인 (대상 PC에서 실행 전)

```
Windows:  certutil -hashfile WSCollector-GUI-windows-x86_64.exe SHA256
Linux:    sha256sum -c SHA256SUMS.txt
```

결과가 `SHA256SUMS.txt`의 값과 같아야 합니다. 수집할 때도 도구 실행 파일의 SHA-256이 수집 로그, 보고서에 자동으로 기록됩니다.

## GUI 사용 순서

1. **사건 정보**: 사건 번호, 증거 번호, 수집자, 저장 위치를 입력합니다.
   - 저장 위치는 "이 PC에 연결한 저장 매체"(폴더) 또는 "네트워크 수신 PC"(주소, 연결 코드).
   - 증거 파일은 `저장 폴더/사건 번호/증거 번호/` 아래에 만들어집니다. 저장 폴더가 없으면 수집을 시작할 때 만듭니다 (예: USB에 `Cases` 폴더가 없어도 `E:\Cases`, `/mnt/usb/Cases` 입력 가능). 단, Linux에서 새 폴더가 시스템 디스크, 루트(/), 메모리(tmpfs), 어느 디스크인지 모르는 위치에 생기게 되면 만들지 않고 안내합니다 - USB가 마운트되지 않은 채 `/mnt/usb/...`를 입력하면 대상 디스크에 써지는 사고를 막기 위해서입니다. 이 위치에 저장하려면 폴더를 직접 만들어 두세요. Windows는 매체가 없으면 드라이브 문자 자체가 없으므로 C:에도 새 폴더를 만들 수 있고, 시스템 디스크 경고만 합니다. 저장 폴더가 이 PC의 시스템 디스크(C:, 루트 /)이면 경고합니다. 오프라인 이미징에서는 그대로 써도 되지만, 라이브 수집이면 외부 매체를 고르세요.
2. **수집 항목**: 메모리, 디스크 전체, 볼륨, 파티션을 고릅니다.
   - 도구가 들어 있는 디스크도 이미징 대상이나 저장 위치로 쓸 수 있습니다 (수십 TB 디스크에 도구와 이미지를 함께 담는 경우).
   - 고를 수 없는 것은 **저장 폴더와 겹치는 대상**뿐입니다: 저장 폴더가 있는 디스크 전체, 저장 폴더가 있는 바로 그 파티션, 볼륨 (Linux는 LVM, LUKS 아래 장치와 루프 장치의 원본 파일이 있는 디스크까지). 같은 디스크의 다른 파티션은 고를 수 있습니다.
3. **수집 진행**: 메모리를 먼저, 디스크는 4MB 단위로 읽으며 MD5,SHA-1,SHA-256을 함께 계산하고, 끝나면 저장 파일을 캐시 없이 다시 읽어 검증합니다.
4. **결과**: 파일별 해시, 검증 결과, 대상 시스템에 남긴 흔적을 확인합니다. 보고서는 증거 폴더에 저장됩니다.

Linux GUI 실행: `sudo -E ./WSCollector-GUI-linux-x86_64` (`-E`: 화면 접속 정보 유지. Wayland에서 root 화면이 안 뜨면 TUI 사용)

## 터미널 화면 (TUI)

```
sudo ./WSCollector-CLI-linux-x86_64 tui        (Windows: 관리자 명령 프롬프트에서 WSCollector-CLI-windows-x86_64 tui)
```

GUI와 같은 4단계(사건 정보 → 수집 항목 → 진행 → 결과)를 키보드로 진행합니다. 화면 엔진이 필요 없어 오래된 배포판, 화면 없는 서버, SSH 접속에서 씁니다.

| 화면 | 키 |
|------|------|
| 사건 정보 | Tab,↑↓ 칸 이동, 글자 입력, 저장 위치 칸에서 ←(연결한 매체),→(네트워크) 또는 Space, 저장 폴더 칸에서 ←→로 연결된 매체 고르기(아래 목록, 고른 뒤 하위 폴더 이어서 입력), Enter 다음 |
| 수집 항목 | ↑↓ 이동, Space 선택, `f` 형식(E01/RAW), `c` 압축, `s` 나누기, `1`,`2`,`3` 해시, `v` 검증, `r` 새로고침, Enter 시작, Backspace 이전 |
| 진행 | `c` 중단 (확인 후) |
| 결과 | `n` 새 수집(같은 사건), `q` 끝내기 |

SSH 연결이 끊겨도 수집은 멈추지 않습니다. 화면 없이 끝까지 수집, 검증하고 로그, 요약, 보고서를 저장한 뒤 스스로 끝나며, 수집 로그에 "TUI 터미널 연결이 끊겼습니다"가 남습니다. 끝난 결과는 저장 폴더의 보고서로 확인하세요.

## CLI

파일 이름 규칙: `WSCollector-{GUI|CLI}-<OS>-<CPU>`.

### 언어 (Language)

CLI, TUI는 한국어와 영어를 모두 담고 있습니다 (파일 하나). 한글을 표시할 수 없는 서버 콘솔에서는 자동으로 영어가 나옵니다.

| 방법 | 예 |
|------|------|
| 옵션 | `--lang en` 또는 `--lang ko` (모든 명령에 붙일 수 있음) |
| 환경 변수 | `WSC_LANG=en` |
| 자동 (둘 다 없을 때) | Linux: 로캘이 한국어 UTF-8(`ko_KR.UTF-8`)이고 리눅스 콘솔(TERM=linux)이 아니면 한국어, 그 밖(`LANG=C`, `en_US.UTF-8` 등)은 영어 / Windows: 표시 언어가 한국어면 한국어 |

- 영어로 실행하면 그 수집의 수집 로그, 요약, 보고서도 영어로 저장됩니다. 사건 번호, 수집자 등 입력한 값은 그대로 들어갑니다.
- GUI는 늘 한국어입니다 (한글 글꼴이 들어 있어 어느 OS에서도 깨지지 않음).

```
sudo ./WSCollector-CLI-linux-x86_64 --lang en list
sudo WSC_LANG=en ./WSCollector-CLI-linux-x86_64 tui
```

Windows (관리자 명령 프롬프트):

```
WSCollector-CLI-windows-x86_64.exe list                     대상 시스템 정보, 디스크, 볼륨 목록
WSCollector-CLI-windows-x86_64.exe list --out E:\Cases      저장 매체를 표시해서 보기
WSCollector-CLI-windows-x86_64.exe list --json              JSON 출력
WSCollector-CLI-windows-x86_64.exe tui                      터미널 화면
```

Linux (root, ARM64는 `WSCollector-CLI-linux-aarch64`):

```
sudo ./WSCollector-CLI-linux-x86_64 list                    대상 시스템 정보, 메모리, 디스크 목록
sudo ./WSCollector-CLI-linux-x86_64 list --out /mnt/usb/Cases
sudo ./WSCollector-CLI-linux-x86_64 tui                     터미널 화면
```

이미징 (Windows, Linux 인자 같음):

```
WSCollector-CLI-windows-x86_64.exe image -s <대상> [-s <대상> ...] --out <저장 폴더> \
    --case 2026-형제-01187 --evidence EV-003 --examiner 홍길동 \
    [--org 소속] [--notes 메모] [--format e01|raw] [--compress best|fast|none] \
    [--split 0|2G|4G] [--hash md5,sha1,sha256] [--no-verify] [--abort-on-error] [--yes]
```

### 실행 예제

먼저 `list`로 대상 이름을 확인한 뒤 `image`를 실행합니다. 실행하면 수집 계획을 보여 주고 `시작할까요? [y/N]`을 묻습니다 (`-y`를 붙이면 묻지 않음). 증거는 `--out 폴더\사건 번호\증거 번호\`에 저장됩니다.

**Windows** (관리자 명령 프롬프트, USB가 `E:`일 때)

```
:: 1. 디스크, 볼륨 목록 확인 (저장 매체 표시)
E:\WSCollector\WSCollector-CLI-windows-x86_64.exe list --out E:\Cases

:: 2. 디스크 1 전체를 E01로 (기본: 최대 압축, 나누지 않음, MD5,SHA-1,SHA-256, 검증)
E:\WSCollector\WSCollector-CLI-windows-x86_64.exe image -s disk1 --out E:\Cases --case 2026-형제-01187 --evidence EV-003 --examiner 홍길동

:: 3. C: 볼륨만 (BitLocker가 풀린 상태의 내용) + 현장 메모
E:\WSCollector\WSCollector-CLI-windows-x86_64.exe image -s C: --out E:\Cases --case 2026-형제-01187 --evidence EV-004 --examiner 홍길동 --notes "사무실 책상 위 노트북, 전원 켜진 상태"

:: 4. 디스크 두 개를 한 번에, 묻지 않고 바로 시작
E:\WSCollector\WSCollector-CLI-windows-x86_64.exe image -s disk0 -s disk2 --out E:\Cases --case 2026-형제-01187 --evidence EV-005 --examiner 홍길동 -y

:: 5. RAW(dd)로 2GB씩 나눠서
E:\WSCollector\WSCollector-CLI-windows-x86_64.exe image -s disk1 --out E:\Cases --case 2026-형제-01187 --evidence EV-006 --examiner 홍길동 --format raw --split 2G

:: 6. CPU가 느린 PC: 빠른 압축
E:\WSCollector\WSCollector-CLI-windows-x86_64.exe image -s disk1 --out E:\Cases --case 2026-형제-01187 --evidence EV-007 --examiner 홍길동 --compress fast
```

**Linux** (USB가 `/mnt/usb`에 마운트됐을 때)

```
# 1. 목록 확인 (첫 줄 memory에 메모리 수집 가능 여부와 이유)
sudo /mnt/usb/WSCollector/WSCollector-CLI-linux-x86_64 list --out /mnt/usb/Cases

# 2. 메모리 + 디스크 전체 (메모리를 먼저 받음)
sudo /mnt/usb/WSCollector/WSCollector-CLI-linux-x86_64 image -s memory -s /dev/sda --out /mnt/usb/Cases --case 2026-형제-01187 --evidence EV-010 --examiner 홍길동

# 3. 메모리만
sudo /mnt/usb/WSCollector/WSCollector-CLI-linux-x86_64 image -s memory --out /mnt/usb/Cases --case 2026-형제-01187 --evidence EV-011 --examiner 홍길동

# 4. 암호화(LUKS), LVM이 풀린 루트 볼륨과 그 물리 디스크를 둘 다
sudo /mnt/usb/WSCollector/WSCollector-CLI-linux-x86_64 image -s /dev/mapper/ubuntu--vg-ubuntu--lv -s /dev/nvme0n1 --out /mnt/usb/Cases --case 2026-형제-01187 --evidence EV-012 --examiner 홍길동

# 5. 화면 없는 서버에서 메뉴로 (TUI)
sudo /mnt/usb/WSCollector/WSCollector-CLI-linux-x86_64 tui
```

명령 끝에 이런 결과가 나옵니다 (stdout):

```
[완료] /dev/sda (Samsung SSD 870, SATA, S/N S5Y1NX0R123456)
    EV-010_web01_20261106_sda.E01  38.2 GB
    원본 500 GB → E01 38.2 GB (7.6%)
    MD5      0701b00f852577d97288f7749d2fb236
    SHA-1    b6e439cc970c6f65b922274965064ab9e4834234
    SHA-256  afaab93dae283f56052028936d1dad006d31d2077b05ff7670835908467f38cd
    검증     일치
보고서: /mnt/usb/Cases/2026-형제-01187/EV-010/EV-010_web01_20261106_report.html
```

네트워크로 보내는 예제는 아래 "네트워크 전송" 절에 있습니다.

저장 형식 (기본: **E01, 최대 압축, 나누지 않음**)

- **E01** (EnCase 6 방식): 압축해서 `….E01` 파일 하나로 저장합니다. 2GB가 넘어도 파일 하나이며 FTK Imager, Autopsy, X-Ways, EnCase, libewf(ewfverify, ewfmount)에서 읽힙니다. 사건 번호, 증거 번호, 수집자, 메모, 대상 모델, 시리얼이 E01 안에 들어가고, MD5,SHA-1도 E01 안에 기록됩니다 (해시를 SHA-256만 골라도 E01용으로 함께 계산).
  - `--compress best`(기본)는 파일이 가장 작고, CPU가 약한 서버에서는 디스크 읽기보다 느릴 수 있습니다. 느리면 `fast`를 쓰세요.
  - `--split 2G`처럼 주면 `.E01, .E02 …`로 나눕니다. 저장 매체가 **FAT32면 나누지 않음이어도 약 3.9GB마다 자동으로 나눕니다** (FAT32는 파일 하나 4GB 한계).
  - 검증은 저장한 E01을 다시 읽어 풀고, 원본 해시와 E01에 적힌 해시를 모두 비교합니다. 중단된 E01은 이름 끝에 `.incomplete`가 붙습니다.
  - 원본 크기가 섹터 배수가 아니면(파일 이미징 등) E01을 만들 수 없어 RAW를 쓰라고 안내합니다.
- **RAW** (`--format raw`): `.dd` 파일 하나, `--split`을 주면 `.001, .002 …`. FAT32면 2GB로 자동 분할합니다.
- 메모리는 형식과 상관없이 항상 **LiME**입니다 (Volatility가 바로 읽음).
- 저장 공간 검사는 압축 전 크기를 기준으로 합니다 (압축률을 미리 알 수 없어 보수적으로).

대상 표기

| OS | 메모리 | 디스크 전체 | 파티션, 볼륨 |
|------|------|------|------|
| Windows | `memory` | `disk0`, `0`, `\\.\PhysicalDrive0` | `C:` (볼륨, BitLocker 해제된 내용), `disk0p3` (문자 없는 파티션) |
| Linux | `memory` | `sda`, `/dev/sda`, `nvme0n1`, `vda`, `xvda` | `/dev/sda1`, `nvme0n1p2`, `/dev/mapper/vg-root` (LVM, LUKS 해제된 내용), `md0` |

Linux에서는 물리 디스크를 이미징하면 원본 그대로(암호화된 상태)를, `/dev/mapper/...` 볼륨을 이미징하면 LVM, LUKS가 풀린 내용을 얻습니다. 필요하면 둘 다 지정하세요.

종료 코드: 0 완료, 1 오류, 2 잘못된 사용, 3 검증 불일치, 일부 실패, 4 완료했지만 읽기 오류 섹터 있음, 130 중단 (Ctrl+C 한 번: 정리 후 중단, 두 번: 즉시 종료)

## Linux에서 실행

```
sudo ./WSCollector-CLI-linux-x86_64 list
sudo ./WSCollector-CLI-linux-x86_64 image -s memory -s /dev/sda --out /mnt/usb/Cases --case ... --evidence ... --examiner ...
```

- root 권한이 필요합니다 (`sudo`).
- 원본은 `O_DIRECT`로 읽어 서버의 페이지 캐시(메모리)를 덮어쓰지 않습니다.
- USB가 실행 금지(`noexec`)로 마운트되어 바로 실행되지 않으면, 디스크가 아닌 메모리(`/dev/shm`)에 복사해서 실행하세요. 서버 디스크에는 쓰지 않습니다.
  ```
  cp /mnt/usb/WSCollector/WSCollector-CLI-linux-x86_64 /dev/shm/wsc && chmod +x /dev/shm/wsc
  sudo /dev/shm/wsc list
  # 끝나면: rm /dev/shm/wsc
  ```
- CPU 종류 확인: `uname -m` (x86_64 → `-x86_64`, aarch64 → `-aarch64`)

### 메모리 덤프 (Linux)

- AVML과 같은 원본을 같은 순서로 시도합니다: `/dev/crash`(RHEL 계열) → `/proc/kcore` → `/dev/mem`. 커널 모듈, 드라이버를 올리지 않습니다.
- 실제 메모리 위치는 `/proc/iomem`의 "System RAM" 영역에서 가져옵니다.
- 결과는 **LiME 형식** `…_Memory.lime` 파일 하나입니다 (Volatility 3에서 바로 분석). 저장 매체가 FAT32일 때만 나눠 저장하며, 그때는 `cat …_Memory.001 …_Memory.002 > memory.lime`으로 합쳐서 분석하세요.
- 디스크와 함께 고르면 메모리를 먼저 받습니다 (메모리 내용은 계속 바뀌기 때문).
- 수집할 수 없는 경우: 커널 lockdown(Secure Boot 시스템에서 흔함), `CONFIG_PROC_KCORE`가 없는 커널, 컨테이너 안, root가 아님. `list`의 memory 줄, GUI, TUI 메모리 항목에 이유가 그대로 나옵니다 (lockdown이면 그 상태도 표시).
- CXL, PMEM을 일반 메모리로 쓰는 시스템의 `System RAM (kmem)` 영역도 수집합니다.
- 읽을 수 없는 물리 페이지는 그 페이지(4KB)만 0으로 채우고 수집 로그에 물리 주소를 남깁니다.

### 메모리 덤프 (Windows)

- **WinPmem** 드라이버(`drivers/winpmem_x64.sys`, Velocidex WinPmem 4.1, Microsoft 하드웨어 호환성 서명)를 쓰며, 도구 폴더(USB)에 있는 파일을 그대로 적재합니다. 대상 디스크에 복사하지 않습니다.
- 순서: 커널 서비스 `WSCollectorPmem` 생성, 시작 → `\\.\pmem`을 PTE 모드로 읽음(공식 WinPmem 도구와 같은 방식) → **메모리 작업이 끝나면 바로**(실패, 중단 포함) 서비스를 중지, 삭제합니다. 디스크 이미징 중에는 드라이버가 남아 있지 않습니다.
- 메모리 영역은 드라이버가 알려 주는 물리 메모리 범위를 씁니다. 수집 전 화면의 크기는 레지스트리(`HARDWARE\RESOURCEMAP\System Resources\Physical Memory`)에서 읽습니다 (이때는 드라이버를 적재하지 않음).
- 결과는 Linux와 같은 **LiME 형식** `…_Memory.lime` (Volatility 3에서 바로 분석). 영역 사이 구멍(장치 메모리 등)은 담지 않습니다.
- 남는 흔적: 서비스 레지스트리 키(만들었다가 지움), Windows 시스템 이벤트 로그의 서비스 설치 기록(7045), 드라이버 적재 기록. 만든 서비스, 삭제 결과, 드라이버 SHA-256은 보고서 "대상 시스템에 남긴 흔적"에 남습니다.
- 서비스를 지우지 못하면 로그와 보고서에 경고와 함께 정리 명령을 남깁니다: 관리자 명령 프롬프트에서 `sc stop WSCollectorPmem` 후 `sc delete WSCollectorPmem`.
- 다른 도구(Velociraptor 등)가 이미 WinPmem을 적재해 둔 경우 그 장치를 그대로 쓰고, 남의 드라이버는 내리지 않습니다.
- 수집할 수 없는 경우: 관리자 권한이 아님, `drivers/winpmem_x64.sys`가 없음, x64가 아님, 드라이버 차단(메모리 무결성, 취약 드라이버 차단 목록, EDR, 오류 1275), 서명 거부(오류 577). 이유는 `list`의 memory 줄과 GUI, TUI 메모리 항목, 수집 로그에 나옵니다.

## 네트워크 전송 (USB를 꽂을 수 없는 서버)

이미지를 대상 시스템에 쓰지 않고 분석 PC로 바로 보냅니다. 설치는 필요 없습니다. GUI, TUI, CLI를 섞어 써도 됩니다 (예: Linux 서버 CLI → 분석 PC GUI).

**GUI**

- 받기: 왼쪽 아래 **네트워크 수신** → 저장 폴더, 포트 → **수신 시작**. 화면에 연결 코드와 이 PC 주소가 크게 나옵니다. 받는 동안 진행률, 속도, 파일, 검증 상황이 보이고, 끝나면 저장 폴더와 보고서를 바로 열 수 있습니다.
- 보내기: **사건 정보** 화면의 저장 위치에서 **네트워크 수신 PC**를 고르고 수신 PC 주소와 연결 코드를 입력합니다.

**CLI**

```
[분석 PC]   WSCollector-CLI-windows-x86_64.exe receive --out D:\Cases [--resume-wait 15]
            → 연결 코드와 이 PC 주소가 표시됩니다 (예: 4KQ7M-2XD9P, 192.168.0.10:7700)

[대상 서버] sudo /dev/shm/wsc image -s memory -s /dev/sda --send 192.168.0.10 \
                --case 2026-형제-01187 --evidence EV-003 --examiner 홍길동
            수신 PC 화면의 연결 코드: 4KQ7M-2XD9P      ← 실행하면 물어봄
```

- **이어받기**: 전송 중(검증 중 포함) 연결이 끊기면 보내는 쪽이 같은 코드로 3초마다 다시 접속해(최대 10분) 수신 측이 받은 지점부터 이어서 보냅니다. 수신 측은 세션을 유지한 채 15분 동안 기다립니다(`--resume-wait`). 원본을 다시 읽지 않고 보내는 쪽이 보관한 최근 데이터(32MB)에서 다시 보내므로 해시가 끊기지 않습니다. 끊김, 이어받기 시각은 수집 로그에 남습니다. 수신 프로그램이 이미 꺼진 경우(접속 거부)는 약 10초 만에 포기하고, 다시 연결을 기다리는 중에도 중단(Ctrl+C, 중단 버튼)이 바로 먹습니다. 검증 중 끊기면 처음부터 다시 검증하며 최대 3번까지 합니다.
- 연결 코드는 실행 후 입력하세요. `--code`로 명령행에 넣을 수도 있지만, 그러면 대상 서버의 프로세스 목록(`ps`)과 셸 기록에 남습니다. 자동화할 때는 환경 변수 `WSC_CODE`를 쓰세요.
- 대상 서버가 분석 PC로 접속합니다. 서버 방화벽은 들어오는 연결을 막는 경우가 많고 나가는 연결은 대부분 허용되기 때문입니다.
- 수신기는 기본으로 모든 네트워크(`0.0.0.0:7700`)에서 기다립니다. 수집망만 쓰려면 `--listen 192.168.0.10:7700`처럼 주소를 정하세요. GUI는 수신 설정의 **받을 네트워크**에서 이 PC 주소를 고릅니다.
- 분석 PC 방화벽에서 TCP 7700 포트로 들어오는 연결을 허용해야 합니다.
  - Windows: 처음 실행할 때 허용 창이 뜨면 허용. 또는 관리자 명령 프롬프트에서 `netsh advfirewall firewall add rule name="WSCollector 7700" dir=in action=allow protocol=TCP localport=7700` (수집 후 `... delete rule name="WSCollector 7700"`)
  - Linux: `sudo ufw allow 7700/tcp` 또는 `sudo firewall-cmd --add-port=7700/tcp`
- 연결 코드로 키를 합의(SPAKE2)하고 모든 데이터를 암호화(ChaCha20-Poly1305)합니다. 코드를 모르는 쪽은 연결할 수 없고, 통신을 엿봐도 코드나 내용을 알 수 없습니다. 코드가 틀린 접속이 오면 다음 접속까지 기다리는 시간을 늘려(최대 10초) 무차별 대입을 막습니다.
- 이미지, 로그, 요약, 보고서는 모두 분석 PC의 `저장 폴더/사건 번호/증거 번호/`에 저장됩니다. 대상 서버 디스크에는 아무것도 쓰지 않습니다.
- 검증은 분석 PC가 저장한 파일을 캐시 없이 다시 읽어 해시를 계산하고, 대상 쪽이 원본에서 읽은 해시와 비교합니다. 파일을 닫을 때 보낸 크기와 저장된 크기도 대조합니다.
- 이어받지 못하고 끝나면 받다 만 파일은 이름 뒤에 `.incomplete`가 붙습니다.
- 속도는 회선이 상한입니다 (1Gbps ≈ 초당 110MB, 1TB 약 2시간 30분).
- 수신 측은 보내는 쪽이 알린 크기보다 많이 받으면 중단합니다 (분석 PC 저장 매체 보호).
- 수신은 세션 하나를 받으면 끝납니다. 다음 증거는 수신을 다시 시작하세요.

## 결과물

| 파일 | 내용 |
|------|------|
| `증거번호_호스트_날짜_Disk1.E01` (Linux: `_sda.E01`) | E01 이미지 (기본). 나누면 `.E02 …` |
| `증거번호_호스트_날짜_Disk1.dd`, `.001, .002 …` | RAW 이미지 (`--format raw`) |
| `…_Memory.lime` | 메모리 (LiME 형식, Windows, Linux) |
| `…_collection.log` | 수집 로그 (시각, 도구 해시, 매체 정보, 오류 섹터 위치, 네트워크 끊김, 이어받기) |
| `…_summary.json` | 기계 판독용 요약 (해시, 검증, 흔적 기록) |
| `…_report.html` | 보고서 (브라우저로 열어 인쇄, PDF 저장) |

## 흔적 최소화 설계

- 원본은 읽기 전용, 캐시 없이(Windows NO_BUFFERING, Linux O_DIRECT) 읽어 대상 시스템 메모리의 파일 캐시를 덮어쓰지 않습니다.
- 모든 출력, 로그, 임시 파일은 외부 매체 또는 네트워크 수신 PC에 씁니다.
- 화면 엔진 데이터도 실행 위치의 `runtime/`으로 돌립니다: Windows는 WebView2 데이터, TEMP, Linux는 WebKitGTK 데이터, 캐시, 설정(XDG), TMPDIR, HOME, 그래픽 드라이버 셰이더 캐시.
- 남는 흔적(Windows: Prefetch, Amcache, USB 연결 기록, WebView2 프로세스, 메모리 덤프 시 WinPmem 서비스 설치 이벤트 / Linux: 셸 기록, sudo, USB 연결 로그 / 네트워크 연결 기록)은 보고서의 "대상 시스템에 남긴 흔적"에 기록합니다.

## 개발

`CLAUDE.md`의 "실행 방법"을 참고하세요.

- Windows GUI 점검: `WS_DEBUG_PORT=9333 WS_TEST_MAX_BYTES=256M npm run tauri dev` 후 `node tools/ui-check.mjs 9333 shot out.png`
- Linux CLI 빌드 (Windows에서 교차 빌드, `.cargo/config.toml`이 rust-lld 링커 지정):
  `cargo build -p ws-cli --release --target x86_64-unknown-linux-musl` / `--target aarch64-unknown-linux-musl`
- Linux GUI 배포 빌드 (WSL Ubuntu 22.04에서): `cargo build --release -p ws-gui --features custom-protocol`
- Linux GUI 점검 (WSL root): `VITE_WS_BRIDGE=9334 npm run build` → `bash tools/linux-gui-build-test.sh` → `bash tools/linux-gui-test.sh start <실행 파일>` → `node tools/ui-check.mjs bridge:9335 ...`
- TUI 점검: `bash tools/tui-check.sh start <CLI>` → `keys, key, shot`
- Linux 시험 장치 (WSL root): `sh tools/linux-test-setup.sh` 로 GPT 루프 디스크와 읽기 오류 장치(dm-error), 끝나면 `sh tools/linux-test-setup.sh clean`
