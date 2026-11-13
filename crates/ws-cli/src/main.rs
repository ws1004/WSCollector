//! WS Collector CLI.
//!
//! GUI와 같은 엔진(ws-core)을 사용합니다. 화면 엔진(WebView2)을 띄우지 않으므로
//! 대상 시스템의 메모리와 프로세스 흔적이 GUI보다 적습니다.
//!
//! 종료 코드: 0 완료, 1 오류, 2 잘못된 사용, 3 검증 불일치, 일부 실패, 4 완료했지만 읽기 오류 섹터 있음, 130 중단
//!
//! 네트워크 전송: 수신 PC에서 `receive`, 대상 시스템에서 `image ... --send IP:7700 --code 코드`

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use ws_core::case::CaseInfo;
use ws_core::collect::{run_collection, CollectEvent, CollectObserver, CollectRequest, JobStatus, RemoteTarget, SessionStatus};
use ws_core::hash::HashAlg;
use ws_core::e01::Compression;
use ws_core::imaging::{ImageFormat, Phase};
use ws_core::inventory::disks_with_roles;
use ws_core::logbook::LogLevel;
use ws_core::model::DiskRole;
use ws_core::platform;
use ws_core::target::Target;
use ws_core::util::{format_bytes, format_duration, format_rate, parse_size};

mod tui;

// -- 도움말 예제 (--help 끝에 나옴). OS마다 실제 파일 이름, 대상 표기로 --

#[cfg(windows)]
fn ex_main() -> &'static str {
    ws_core::tr!(
        "예제 (관리자 명령 프롬프트, USB가 E:일 때):
  WSCollector-CLI-windows-x86_64.exe list --out E:\\Cases
  WSCollector-CLI-windows-x86_64.exe image -s disk1 --out E:\\Cases --case 2026-형제-01187 --evidence EV-003 --examiner 홍길동
  WSCollector-CLI-windows-x86_64.exe tui

명령마다 자세한 예제: WSCollector-CLI-windows-x86_64.exe image --help",
        "Examples (administrator command prompt, USB at E:):
  WSCollector-CLI-windows-x86_64.exe list --out E:\\Cases
  WSCollector-CLI-windows-x86_64.exe image -s disk1 --out E:\\Cases --case 2026-CASE-001 --evidence EV-003 --examiner \"John Doe\"
  WSCollector-CLI-windows-x86_64.exe tui

Detailed examples per command: WSCollector-CLI-windows-x86_64.exe image --help"
    )
}
#[cfg(not(windows))]
fn ex_main() -> &'static str {
    ws_core::tr!(
        "예제 (root, USB가 /mnt/usb일 때):
  sudo ./WSCollector-CLI-linux-x86_64 list --out /mnt/usb/Cases
  sudo ./WSCollector-CLI-linux-x86_64 image -s memory -s /dev/sda --out /mnt/usb/Cases --case 2026-형제-01187 --evidence EV-003 --examiner 홍길동
  sudo ./WSCollector-CLI-linux-x86_64 tui

명령마다 자세한 예제: ./WSCollector-CLI-linux-x86_64 image --help",
        "Examples (root, USB at /mnt/usb):
  sudo ./WSCollector-CLI-linux-x86_64 list --out /mnt/usb/Cases
  sudo ./WSCollector-CLI-linux-x86_64 image -s memory -s /dev/sda --out /mnt/usb/Cases --case 2026-CASE-001 --evidence EV-003 --examiner \"John Doe\"
  sudo ./WSCollector-CLI-linux-x86_64 tui

Detailed examples per command: ./WSCollector-CLI-linux-x86_64 image --help"
    )
}

#[cfg(windows)]
fn ex_image() -> &'static str {
    ws_core::tr!(
        "예제 (먼저 list로 대상 이름 확인. 증거는 --out\\사건 번호\\증거 번호\\ 에 저장):
  디스크 1 전체를 E01로 (기본: 최대 압축, 나누지 않음, MD5,SHA-1,SHA-256, 검증)
    WSCollector-CLI-windows-x86_64.exe image -s disk1 --out E:\\Cases --case 2026-형제-01187 --evidence EV-003 --examiner 홍길동
  메모리 먼저, 이어서 디스크 0 (메모리는 WinPmem 드라이버를 USB에서 적재, 끝나면 서비스 삭제)
    WSCollector-CLI-windows-x86_64.exe image -s memory -s disk0 --out E:\\Cases --case 2026-형제-01187 --evidence EV-008 --examiner 홍길동
  C: 볼륨만 (BitLocker가 풀린 내용) + 현장 메모
    WSCollector-CLI-windows-x86_64.exe image -s C: --out E:\\Cases --case 2026-형제-01187 --evidence EV-004 --examiner 홍길동 --notes \"전원 켜진 노트북\"
  디스크 두 개, 묻지 않고 바로 시작
    WSCollector-CLI-windows-x86_64.exe image -s disk0 -s disk2 --out E:\\Cases --case 2026-형제-01187 --evidence EV-005 --examiner 홍길동 -y
  RAW(dd)로 2GB씩 나눠서
    WSCollector-CLI-windows-x86_64.exe image -s disk1 --out E:\\Cases --case 2026-형제-01187 --evidence EV-006 --examiner 홍길동 --format raw --split 2G
  네트워크로 수신 PC에 보내기 (연결 코드는 실행하면 물어봄)
    WSCollector-CLI-windows-x86_64.exe image -s disk0 --send 192.168.0.10 --case 2026-형제-01187 --evidence EV-007 --examiner 홍길동

대상 표기: memory (물리 메모리), disk0 (디스크 전체), C: (볼륨), disk0p3 (문자 없는 파티션)",
        "Examples (run list first to find target names. Evidence is saved to --out\\<case number>\\<evidence number>\\):
  Whole disk 1 as E01 (default: best compression, no split, MD5/SHA-1/SHA-256, verification)
    WSCollector-CLI-windows-x86_64.exe image -s disk1 --out E:\\Cases --case 2026-CASE-001 --evidence EV-003 --examiner \"John Doe\"
  Memory first, then disk 0 (memory loads the WinPmem driver from the USB and deletes its service afterwards)
    WSCollector-CLI-windows-x86_64.exe image -s memory -s disk0 --out E:\\Cases --case 2026-CASE-001 --evidence EV-008 --examiner \"John Doe\"
  C: volume only (BitLocker-unlocked contents) + notes
    WSCollector-CLI-windows-x86_64.exe image -s C: --out E:\\Cases --case 2026-CASE-001 --evidence EV-004 --examiner \"John Doe\" --notes \"Laptop found powered on\"
  Two disks, start without confirmation
    WSCollector-CLI-windows-x86_64.exe image -s disk0 -s disk2 --out E:\\Cases --case 2026-CASE-001 --evidence EV-005 --examiner \"John Doe\" -y
  RAW (dd) split into 2 GB pieces
    WSCollector-CLI-windows-x86_64.exe image -s disk1 --out E:\\Cases --case 2026-CASE-001 --evidence EV-006 --examiner \"John Doe\" --format raw --split 2G
  Send over the network to a receiving PC (connection code is prompted at run time)
    WSCollector-CLI-windows-x86_64.exe image -s disk0 --send 192.168.0.10 --case 2026-CASE-001 --evidence EV-007 --examiner \"John Doe\"

Target notation: memory (physical memory), disk0 (whole disk), C: (volume), disk0p3 (partition without a drive letter)"
    )
}
#[cfg(not(windows))]
fn ex_image() -> &'static str {
    ws_core::tr!(
        "예제 (먼저 list로 대상 이름 확인. 증거는 --out/사건 번호/증거 번호/ 에 저장):
  메모리 + 디스크 전체 (메모리를 먼저 받음, 디스크는 E01 최대 압축)
    sudo ./WSCollector-CLI-linux-x86_64 image -s memory -s /dev/sda --out /mnt/usb/Cases --case 2026-형제-01187 --evidence EV-010 --examiner 홍길동
  메모리만 (LiME 형식)
    sudo ./WSCollector-CLI-linux-x86_64 image -s memory --out /mnt/usb/Cases --case 2026-형제-01187 --evidence EV-011 --examiner 홍길동
  LVM, LUKS가 풀린 볼륨과 그 물리 디스크를 둘 다
    sudo ./WSCollector-CLI-linux-x86_64 image -s /dev/mapper/vg-root -s /dev/nvme0n1 --out /mnt/usb/Cases --case 2026-형제-01187 --evidence EV-012 --examiner 홍길동
  CPU가 느린 서버: 빠른 압축, 묻지 않고 시작
    sudo ./WSCollector-CLI-linux-x86_64 image -s /dev/sda --out /mnt/usb/Cases --case 2026-형제-01187 --evidence EV-013 --examiner 홍길동 --compress fast -y
  USB를 꽂을 수 없을 때: 분석 PC로 보내기 (연결 코드는 실행하면 물어봄)
    sudo ./WSCollector-CLI-linux-x86_64 image -s memory -s /dev/sda --send 192.168.0.10 --case 2026-형제-01187 --evidence EV-014 --examiner 홍길동

대상 표기: memory, sda 또는 /dev/sda (디스크 전체), /dev/sda1, nvme0n1p2, /dev/mapper/vg-root, md0",
        "Examples (run list first to find target names. Evidence is saved to --out/<case number>/<evidence number>/):
  Memory + whole disk (memory is acquired first, disk as E01 with best compression)
    sudo ./WSCollector-CLI-linux-x86_64 image -s memory -s /dev/sda --out /mnt/usb/Cases --case 2026-CASE-001 --evidence EV-010 --examiner \"John Doe\"
  Memory only (LiME format)
    sudo ./WSCollector-CLI-linux-x86_64 image -s memory --out /mnt/usb/Cases --case 2026-CASE-001 --evidence EV-011 --examiner \"John Doe\"
  Both an unlocked LVM/LUKS volume and its physical disk
    sudo ./WSCollector-CLI-linux-x86_64 image -s /dev/mapper/vg-root -s /dev/nvme0n1 --out /mnt/usb/Cases --case 2026-CASE-001 --evidence EV-012 --examiner \"John Doe\"
  Server with a slow CPU: fast compression, start without confirmation
    sudo ./WSCollector-CLI-linux-x86_64 image -s /dev/sda --out /mnt/usb/Cases --case 2026-CASE-001 --evidence EV-013 --examiner \"John Doe\" --compress fast -y
  When USB cannot be attached: send to an analysis PC (connection code is prompted at run time)
    sudo ./WSCollector-CLI-linux-x86_64 image -s memory -s /dev/sda --send 192.168.0.10 --case 2026-CASE-001 --evidence EV-014 --examiner \"John Doe\"

Target notation: memory, sda or /dev/sda (whole disk), /dev/sda1, nvme0n1p2, /dev/mapper/vg-root, md0"
    )
}

#[cfg(windows)]
fn ex_receive() -> &'static str {
    ws_core::tr!(
        "예제 (분석 PC):
  WSCollector-CLI-windows-x86_64.exe receive --out D:\\Cases
  → 화면의 연결 코드(예: 4KQ7M-2XD9P)와 주소(예: 192.168.0.10:7700)를 대상 쪽에 알려 줌
  수집망 랜카드 하나로만 받기:
  WSCollector-CLI-windows-x86_64.exe receive --out D:\\Cases --listen 192.168.0.10:7700

방화벽에서 TCP 7700 들어오는 연결을 허용해야 합니다.",
        "Examples (analysis PC):
  WSCollector-CLI-windows-x86_64.exe receive --out D:\\Cases
  -> Give the displayed connection code (e.g. 4KQ7M-2XD9P) and address (e.g. 192.168.0.10:7700) to the target side
  Listen on the acquisition network adapter only:
  WSCollector-CLI-windows-x86_64.exe receive --out D:\\Cases --listen 192.168.0.10:7700

The firewall must allow inbound TCP 7700 connections."
    )
}
#[cfg(not(windows))]
fn ex_receive() -> &'static str {
    ws_core::tr!(
        "예제 (분석 PC):
  ./WSCollector-CLI-linux-x86_64 receive --out /data/Cases
  → 화면의 연결 코드(예: 4KQ7M-2XD9P)와 주소(예: 192.168.0.10:7700)를 대상 쪽에 알려 줌
  수집망 랜카드 하나로만 받기:
  ./WSCollector-CLI-linux-x86_64 receive --out /data/Cases --listen 192.168.0.10:7700

방화벽에서 TCP 7700 들어오는 연결을 허용해야 합니다 (예: sudo ufw allow 7700/tcp).",
        "Examples (analysis PC):
  ./WSCollector-CLI-linux-x86_64 receive --out /data/Cases
  -> Give the displayed connection code (e.g. 4KQ7M-2XD9P) and address (e.g. 192.168.0.10:7700) to the target side
  Listen on the acquisition network adapter only:
  ./WSCollector-CLI-linux-x86_64 receive --out /data/Cases --listen 192.168.0.10:7700

The firewall must allow inbound TCP 7700 connections (e.g. sudo ufw allow 7700/tcp)."
    )
}

#[derive(Parser)]
#[command(
    name = "wscollector-cli",
    version,
    about = ws_core::tr!("WS Collector: 라이브 증거 수집 도구 (CLI)", "WS Collector: live evidence acquisition tool (CLI)"),
    after_help = ex_main()
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    // 화면 언어 (실제 적용은 choose_lang에서 먼저 함)
    #[arg(
        long,
        global = true,
        value_name = "ko|en",
        help = ws_core::tr!(
            "화면 언어: ko | en (기본: 자동. 환경 변수 WSC_LANG도 됨)",
            "Output language: ko | en (default: auto; WSC_LANG environment variable also works)"
        )
    )]
    lang: Option<String>,
}

/// 화면 언어 정하기 (clap이 도움말을 만들기 전에): --lang 값 → WSC_LANG → 터미널 자동 판별
fn choose_lang() {
    let args: Vec<String> = std::env::args().collect();
    let from_arg = args.iter().enumerate().find_map(|(i, a)| {
        if let Some(v) = a.strip_prefix("--lang=") {
            Some(v.to_string())
        } else if a == "--lang" {
            args.get(i + 1).cloned()
        } else {
            None
        }
    });
    let chosen = from_arg
        .or_else(|| std::env::var("WSC_LANG").ok())
        .and_then(|v| ws_core::i18n::parse(&v))
        .unwrap_or_else(ws_core::i18n::detect_terminal);
    ws_core::i18n::set_lang(chosen);
}

#[derive(Subcommand)]
enum Command {
    // 대상 시스템 정보와 디스크, 볼륨 목록
    #[command(about = ws_core::tr!(
        "대상 시스템 정보와 연결된 디스크, 볼륨을 보여줍니다",
        "Show target system information and attached disks/volumes"
    ))]
    List {
        #[arg(long, help = ws_core::tr!(
            "저장 폴더를 주면 그 매체를 \"저장 위치\"로 표시합니다",
            "If an output folder is given, its media is marked as \"output location\""
        ))]
        out: Option<PathBuf>,
        #[arg(long, help = ws_core::tr!("JSON으로 출력", "Output as JSON"))]
        json: bool,
    },
    // 수집
    #[command(
        about = ws_core::tr!(
            "메모리, 디스크, 볼륨, 파티션을 수집합니다 (기본 E01, 메모리는 LiME. Windows 메모리는 도구 폴더 drivers\\winpmem_x64.sys 필요)",
            "Acquire memory, disks, volumes, and partitions (default E01; memory as LiME. Windows memory needs drivers\\winpmem_x64.sys in the tool folder)"
        ),
        after_help = ex_image()
    )]
    Image(ImageArgs),
    // 터미널 화면(TUI)
    #[command(about = ws_core::tr!(
        "터미널 화면으로 수집합니다 (화면 없는 서버, SSH, 오래된 배포판용. 메뉴로 사건 정보, 대상 선택)",
        "Acquire with a terminal UI (for headless servers, SSH, older distributions; select case info and targets from menus)"
    ))]
    Tui,
    // 수신 PC
    #[command(
        about = ws_core::tr!(
            "(수신 PC) 네트워크로 보내는 이미지를 받아 저장합니다. 연결 코드를 보여주고 접속을 기다립니다",
            "(Receiving PC) Receive and save images sent over the network. Shows a connection code and waits for a connection"
        ),
        after_help = ex_receive()
    )]
    Receive {
        #[arg(short, long, help = ws_core::tr!(
            "저장 폴더. 그 아래에 사건번호/증거번호 폴더가 만들어집니다",
            "Output folder. A <case number>/<evidence number> folder is created under it"
        ))]
        out: PathBuf,
        #[arg(long, default_value = "0.0.0.0:7700", help = ws_core::tr!(
            "대기 주소 (기본: 모든 네트워크, 포트 7700)",
            "Listen address (default: all networks, port 7700)"
        ))]
        listen: String,
        #[arg(long, help = ws_core::tr!(
            "연결 코드를 직접 정함 (생략하면 새로 만듦)",
            "Set the connection code manually (generated if omitted)"
        ))]
        code: Option<String>,
        #[arg(long, default_value_t = 15, help = ws_core::tr!(
            "전송 중 연결이 끊겼을 때 이어받기를 기다리는 시간 (분)",
            "Time to wait for resume when the connection drops during transfer (minutes)"
        ))]
        resume_wait: u64,
    },
}

#[derive(Args)]
struct ImageArgs {
    #[arg(short, long = "source", required = true, help = ws_core::tr!(
        "대상 (여러 번 지정 가능). Windows: disk0, C:, disk0p3 / Linux: sda, /dev/nvme0n1p2, /dev/mapper/vg-root",
        "Target (can be given multiple times). Windows: disk0, C:, disk0p3 / Linux: sda, /dev/nvme0n1p2, /dev/mapper/vg-root"
    ))]
    sources: Vec<String>,
    #[arg(short, long, required_unless_present = "send", conflicts_with = "send", help = ws_core::tr!(
        "저장 폴더. 그 아래에 사건번호/증거번호 폴더가 만들어집니다 (--send를 쓰면 생략)",
        "Output folder. A <case number>/<evidence number> folder is created under it (omit when using --send)"
    ))]
    out: Option<PathBuf>,
    #[arg(long, help = ws_core::tr!(
        "네트워크 전송: 수신 PC 주소 (예: 192.168.0.10, 192.168.0.10:7700). 대상 시스템에 아무것도 쓰지 않음",
        "Network transfer: receiving PC address (e.g. 192.168.0.10, 192.168.0.10:7700). Nothing is written to the target system"
    ))]
    send: Option<String>,
    #[arg(long, requires = "send", help = ws_core::tr!(
        "네트워크 전송: 수신 PC 화면에 나온 연결 코드. 생략하면 물어봄 (권장: 명령행에 쓰면 프로세스 목록, 셸 기록에 남음). 환경 변수 WSC_CODE로도 줄 수 있음",
        "Network transfer: connection code shown on the receiving PC. Prompted if omitted (recommended: on the command line it remains in the process list and shell history). Can also be given via the WSC_CODE environment variable"
    ))]
    code: Option<String>,
    #[arg(long = "case")]
    case_number: String,
    #[arg(long = "evidence")]
    evidence_number: String,
    #[arg(long)]
    examiner: String,
    #[arg(long = "org", default_value = "")]
    organization: String,
    #[arg(long, default_value = "")]
    notes: String,
    #[arg(long, default_value = "e01", help = ws_core::tr!(
        "디스크 저장 형식: e01 (기본, 압축) 또는 raw (dd). 메모리는 항상 LiME",
        "Disk image format: e01 (default, compressed) or raw (dd). Memory is always LiME"
    ))]
    format: String,
    #[arg(long, default_value = "best", help = ws_core::tr!(
        "E01 압축: best (기본, 최대), fast, none",
        "E01 compression: best (default, maximum), fast, none"
    ))]
    compress: String,
    #[arg(long, default_value = "0", help = ws_core::tr!(
        "나누는 크기 (예: 2G, 4G, 0 = 나누지 않음). E01이면 조각 크기. FAT32 저장 매체면 E01은 자동으로 나눔",
        "Split size (e.g. 2G, 4G, 0 = no split). For E01 this is the segment size. On FAT32 output media E01 is split automatically"
    ))]
    split: String,
    #[arg(long, default_value = "md5,sha1,sha256", help = ws_core::tr!(
        "해시 (쉼표로 구분: md5,sha1,sha256)",
        "Hashes (comma-separated: md5,sha1,sha256)"
    ))]
    hash: String,
    #[arg(long, help = ws_core::tr!(
        "이미징 후 검증(다시 읽기)을 하지 않습니다",
        "Skip verification (re-read) after imaging"
    ))]
    no_verify: bool,
    #[arg(long, help = ws_core::tr!(
        "읽기 오류가 나면 0으로 채우지 않고 중단합니다",
        "Abort on read errors instead of zero-filling"
    ))]
    abort_on_error: bool,
    #[arg(short = 'y', long, help = ws_core::tr!("확인 질문 없이 바로 시작", "Start immediately without confirmation"))]
    yes: bool,
    // 시험용 (숨김)
    #[arg(long, hide = true, help = ws_core::tr!(
        "시험용: 원본 앞부분만 수집 (예: 256M)",
        "Testing only: acquire only the beginning of the source (e.g. 256M)"
    ))]
    max_bytes: Option<String>,
}

fn main() {
    // `list | head`처럼 출력이 먼저 닫히면 다른 Linux 명령처럼 조용히 끝냄 (Rust 기본은 패닉 메시지).
    // 소켓 쓰기는 MSG_NOSIGNAL을 쓰므로 네트워크 전송에는 영향 없음
    #[cfg(unix)]
    // SAFETY: 시작 직후 신호 처리기를 기본값으로 되돌릴 뿐
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    choose_lang();
    let cli = Cli::parse();
    let code = match cli.command {
        Command::List { out, json } => cmd_list(out, json),
        Command::Image(a) => cmd_image(a),
        Command::Tui => {
            if !require_admin() {
                1
            } else {
                tui::run()
            }
        }
        Command::Receive { out, listen, code, resume_wait } => cmd_receive(out, listen, code, resume_wait),
    };
    std::process::exit(code);
}

fn fail(msg: impl std::fmt::Display) -> i32 {
    eprintln!("{}: {msg}", ws_core::tr!("오류", "Error"));
    1
}

fn require_admin() -> bool {
    if !platform::is_elevated() {
        eprintln!("{}: {}", ws_core::tr!("오류", "Error"), platform::ADMIN_HINT);
        return false;
    }
    true
}

fn cmd_list(out: Option<PathBuf>, json: bool) -> i32 {
    if !require_admin() {
        return 1;
    }
    let host = platform::host_info();
    let disks = match disks_with_roles(out.as_deref()) {
        Ok(d) => d,
        Err(e) => return fail(e),
    };
    if json {
        let v = serde_json::json!({ "host": host, "disks": disks });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return 0;
    }
    println!("{}  {} {} ({})", host.hostname, host.os_name, host.os_version, host.build_label());
    if ws_core::i18n::en() {
        println!("  {}  Memory {}  Boot {}  {}", host.cpu, format_bytes(host.ram_bytes), host.boot_time, host.timezone);
    } else {
        println!("  {}  메모리 {}  부팅 {}  {}", host.cpu, format_bytes(host.ram_bytes), host.boot_time, host.timezone);
    }
    println!();
    match ws_core::memory::memory_probe() {
        Ok(m) => println!(
            "{}",
            ws_core::tr!(
                format!("memory                 {:>10}  물리 메모리, 원본 {} (영역 {}개), LiME 형식으로 수집\n", format_bytes(m.bytes), m.source, m.ranges),
                format!("memory                 {:>10}  physical memory, source {} ({} ranges), acquired in LiME format\n", format_bytes(m.bytes), m.source, m.ranges)
            )
        ),
        Err(e) => println!("memory                 {}: {e}\n", ws_core::tr!("수집 불가", "Not acquirable")),
    }
    for d in &disks {
        let mut tags: Vec<&str> = vec![d.bus.as_str(), d.kind_label()];
        if d.has_role(DiskRole::System) {
            tags.push(ws_core::tr!("시스템", "System"));
        }
        if d.has_role(DiskRole::Tool) {
            tags.push(ws_core::tr!("수집 도구", "Tool media"));
        }
        if d.has_role(DiskRole::Output) {
            tags.push(ws_core::tr!(
                "저장 위치 (디스크 전체, 저장 파티션은 선택 불가)",
                "Output media (whole disk and output partition cannot be selected)"
            ));
        }
        if cfg!(windows) {
            println!("disk{:<3} {:>10}  {}  [{}]", d.number, format_bytes(d.size), d.model, tags.join(", "));
            println!("        {}  S/N {}  {} {} {}B", d.path, d.serial, d.partition_style, ws_core::tr!("섹터", "Sector"), d.sector_size);
        } else {
            // Linux: 대상 지정에 쓰는 장치 경로를 앞에 보여줌
            println!("{:<22} {:>10}  {}  [{}]", d.path, format_bytes(d.size), d.model, tags.join(", "));
            let mut extra = vec![format!("S/N {}", if d.serial.is_empty() { "-" } else { &d.serial })];
            extra.push(format!("{} {}", ws_core::tr!("파티션", "Partition"), d.partition_style));
            extra.push(format!("{} {}B", ws_core::tr!("섹터", "Sector"), d.sector_size));
            if !d.fs.is_empty() {
                extra.push(d.fs.clone());
            }
            if !d.mounts.is_empty() {
                extra.push(format!("{} {}", ws_core::tr!("마운트", "Mounted"), d.mounts.join(", ")));
            }
            if !d.backing.is_empty() {
                extra.push(format!("{} {}", ws_core::tr!("구성 장치", "Backing devices"), d.backing.join(", ")));
            }
            println!("    {}", extra.join("  "));
        }
        for p in &d.partitions {
            let id = match (&p.device, &p.letter) {
                (Some(dev), _) => format!("{dev:<18}"),
                (None, Some(l)) => format!("{l:<8}"),
                (None, None) => format!("disk{}p{:<3}", d.number, p.number),
            };
            let shown = match (&p.device, &p.letter) {
                (Some(_), Some(m)) => format!("{} {}", m, if p.label.is_empty() { p.kind.clone() } else { p.label.clone() }).trim().to_string(),
                _ => p.display_name(),
            };
            let free = p.free.map(|f| format!(", {} {}", ws_core::tr!("여유", "free"), format_bytes(f))).unwrap_or_default();
            println!(
                "   └ {} {:>10}  {}{}{}",
                id,
                format_bytes(p.size),
                shown,
                if p.fs.is_empty() { String::new() } else { format!(" ({})", p.fs) },
                free
            );
        }
        if d.external() && !d.locked() {
            println!(
                "   ! {}",
                ws_core::tr!(
                    "외장 디스크입니다. 원본 증거라면 쓰기 방지 장치를 거쳐 연결하세요.",
                    "External disk. If this is original evidence, connect it through a write blocker."
                )
            );
        }
        println!();
    }
    0
}

struct CliObserver {
    tty: bool,
    titles: Vec<String>,
    last_line_len: usize,
}

impl CliObserver {
    fn clear_line(&mut self) {
        if self.tty && self.last_line_len > 0 {
            eprint!("\r{}\r", " ".repeat(self.last_line_len));
            self.last_line_len = 0;
        }
    }
}

impl CollectObserver for CliObserver {
    fn event(&mut self, e: CollectEvent) {
        match e {
            CollectEvent::Prepared { evidence_dir, jobs } => {
                eprintln!("{}: {evidence_dir}", ws_core::tr!("저장 폴더", "Output folder"));
                self.titles = jobs.iter().map(|j| j.title.clone()).collect();
            }
            CollectEvent::Log(l) => {
                self.clear_line();
                let tag = match l.level {
                    LogLevel::Info => "  ",
                    LogLevel::Warn => "! ",
                    LogLevel::Error => "X ",
                };
                eprintln!("{} {}{}", &l.time[11..19], tag, l.message);
            }
            CollectEvent::Progress { index, progress: p } => {
                if !self.tty {
                    return;
                }
                let pct = if p.total > 0 { p.done as f64 * 100.0 / p.total as f64 } else { 0.0 };
                let line = format!(
                    "[{}/{} {}] {} {:5.1}%  {} / {}  {}  {} {}{}",
                    index + 1,
                    self.titles.len(),
                    self.titles.get(index).map(String::as_str).unwrap_or(""),
                    if p.phase == Phase::Reading { ws_core::tr!("읽는 중", "Reading") } else { ws_core::tr!("검증 중", "Verifying") },
                    pct,
                    format_bytes(p.done),
                    format_bytes(p.total),
                    format_rate(p.rate_bps),
                    ws_core::tr!("남은 시간", "ETA"),
                    format_duration(p.eta_secs),
                    if p.bad_sectors > 0 { format!("  {} {}", ws_core::tr!("오류 섹터", "Bad sectors"), p.bad_sectors) } else { String::new() }
                );
                let pad = self.last_line_len.saturating_sub(line.chars().count());
                eprint!("\r{line}{}", " ".repeat(pad));
                let _ = std::io::stderr().flush();
                self.last_line_len = line.chars().count();
            }
            CollectEvent::JobStarted { .. } | CollectEvent::JobFinished { .. } => self.clear_line(),
        }
    }
}

fn cmd_image(a: ImageArgs) -> i32 {
    if !require_admin() {
        return 1;
    }
    let mut targets = Vec::new();
    for s in &a.sources {
        match Target::parse(s) {
            Some(t) => targets.push(t),
            None => {
                eprintln!(
                    "{}",
                    ws_core::tr!(
                        format!("오류: 대상을 알 수 없습니다: {s} (예: disk0, C:, disk0p3)"),
                        format!("Error: unknown target: {s} (e.g. disk0, C:, disk0p3)")
                    )
                );
                return 2;
            }
        }
    }
    let Some(split) = parse_size(&a.split) else {
        eprintln!(
            "{}",
            ws_core::tr!(
                format!("오류: 분할 크기를 알 수 없습니다: {} (예: 2G, 0)", a.split),
                format!("Error: invalid split size: {} (e.g. 2G, 0)", a.split)
            )
        );
        return 2;
    };
    let format = match a.format.trim().to_ascii_lowercase().as_str() {
        "e01" | "ewf" => ImageFormat::E01,
        "raw" | "dd" => ImageFormat::Raw,
        other => {
            eprintln!(
                "{}",
                ws_core::tr!(
                    format!("오류: 지원하지 않는 형식입니다: {other} (e01, raw)"),
                    format!("Error: unsupported format: {other} (e01, raw)")
                )
            );
            return 2;
        }
    };
    let Some(compression) = Compression::parse(&a.compress) else {
        eprintln!(
            "{}",
            ws_core::tr!(
                format!("오류: 압축 단계를 알 수 없습니다: {} (best, fast, none)", a.compress),
                format!("Error: unknown compression level: {} (best, fast, none)", a.compress)
            )
        );
        return 2;
    };
    if format == ImageFormat::E01 && split != 0 && split < ws_core::e01::MIN_SEGMENT {
        let min = format_bytes(ws_core::e01::MIN_SEGMENT);
        eprintln!(
            "{}",
            ws_core::tr!(
                format!("오류: E01 조각 크기는 {min} 이상이어야 합니다 (0 = 나누지 않음)"),
                format!("Error: E01 segment size must be at least {min} (0 = no split)")
            )
        );
        return 2;
    }
    let mut algs = Vec::new();
    for h in a.hash.split(',') {
        match HashAlg::parse(h) {
            Some(x) if !algs.contains(&x) => algs.push(x),
            Some(_) => {}
            None => {
                eprintln!(
                    "{}",
                    ws_core::tr!(
                        format!("오류: 지원하지 않는 해시입니다: {h} (md5, sha1, sha256)"),
                        format!("Error: unsupported hash: {h} (md5, sha1, sha256)")
                    )
                );
                return 2;
            }
        }
    }
    let max_bytes = match a.max_bytes.as_deref().map(parse_size) {
        Some(None) => {
            eprintln!("{}", ws_core::tr!("오류: --max-bytes 값을 알 수 없습니다", "Error: invalid --max-bytes value"));
            return 2;
        }
        Some(Some(v)) => Some(v),
        None => None,
    };
    let req = CollectRequest {
        case: CaseInfo {
            case_number: a.case_number,
            evidence_number: a.evidence_number,
            examiner: a.examiner,
            organization: a.organization,
            notes: a.notes,
        },
        out_base: a.out.clone().unwrap_or_default(),
        targets,
        split_bytes: split,
        format,
        compression,
        algs,
        verify: !a.no_verify,
        zero_fill_errors: !a.abort_on_error,
        interface: "CLI".into(),
        max_bytes,
        remote: None,
    };
    let mut req = req;
    if let Some(addr) = &a.send {
        let code = match a.code.clone().or_else(|| std::env::var("WSC_CODE").ok().filter(|s| !s.trim().is_empty())) {
            Some(c) => c,
            None => {
                if !std::io::stdin().is_terminal() {
                    eprintln!(
                        "{}",
                        ws_core::tr!(
                            "오류: 연결 코드를 입력받을 수 없습니다. --code 또는 환경 변수 WSC_CODE로 주세요",
                            "Error: cannot prompt for the connection code. Provide it with --code or the WSC_CODE environment variable"
                        )
                    );
                    return 2;
                }
                eprint!("{}", ws_core::tr!("수신 PC 화면의 연결 코드: ", "Connection code shown on the receiving PC: "));
                let _ = std::io::stderr().flush();
                let mut s = String::new();
                let _ = std::io::stdin().read_line(&mut s);
                s.trim().to_string()
            }
        };
        req.remote = Some(RemoteTarget { addr: addr.clone(), code });
    }
    let req = req;

    // 미리 검사하고 계획을 보여줌
    let (disks, dir) = match ws_core::collect::preflight(&req) {
        Ok(v) => v,
        Err(e) => return fail(e),
    };
    eprintln!("{}", ws_core::tr!("수집 계획", "Acquisition plan"));
    for t in &req.targets {
        let label = match t {
            Target::Disk { number, .. } => disks.iter().find(|d| d.number == *number).map(|d| format!("disk{} {} ({})", d.number, d.model, format_bytes(d.size))),
            Target::Volume { letter } => Some(format!("{} {letter}", ws_core::tr!("볼륨", "Volume"))),
            Target::Partition { disk, number, .. } => Some(format!("disk{disk} {} {number}", ws_core::tr!("파티션", "partition"))),
            Target::Device { path, .. } => Some(match ws_core::collect::locate_device(path, &disks) {
                Some((d, None)) => format!("{} {} ({})", d.path, d.model, format_bytes(d.size)),
                Some((d, Some(p))) => format!(
                    "{} ({} {} {}, {})",
                    p.device.as_deref().unwrap_or(path),
                    d.model,
                    ws_core::tr!("파티션", "partition"),
                    p.number,
                    format_bytes(p.size)
                ),
                None => path.clone(),
            }),
            Target::File { path: p } => Some(p.display().to_string()),
            Target::Memory => Some(match ws_core::memory::memory_probe() {
                Ok(m) => ws_core::tr!(
                    format!("메모리 {} (원본 {}, LiME 형식, 디스크보다 먼저 수집)", format_bytes(m.bytes), m.source),
                    format!("Memory {} (source {}, LiME format, acquired before disks)", format_bytes(m.bytes), m.source)
                ),
                Err(e) => ws_core::tr!(format!("메모리 (수집 불가: {e})"), format!("Memory (not acquirable: {e})")),
            }),
        };
        eprintln!("  - {}", label.unwrap_or_default());
    }
    match &req.remote {
        Some(r) => eprintln!(
            "{}",
            ws_core::tr!(
                format!("  저장: 네트워크 수신 PC {} (이 시스템 디스크에는 쓰지 않음)", r.addr),
                format!("  Output: receiving PC over network {} (nothing is written to this system's disks)", r.addr)
            )
        ),
        None => eprintln!("  {}: {}", ws_core::tr!("저장", "Output"), dir.display()),
    }
    let fmt_label = match format {
        ImageFormat::E01 => format!("E01 ({})", compression.label()),
        ImageFormat::Raw => "RAW".into(),
    };
    let split_label = if split == 0 { ws_core::tr!("나누지 않음", "no split").into() } else { format_bytes(split) };
    let hash_label = req.algs.iter().map(|h| h.label()).collect::<Vec<_>>().join("/");
    if ws_core::i18n::en() {
        eprintln!(
            "  Format {}, {} {}, hash {}, verification {}",
            fmt_label,
            if format == ImageFormat::E01 { "segment" } else { "split" },
            split_label,
            hash_label,
            if req.verify { "on" } else { "off" }
        );
    } else {
        eprintln!(
            "  형식 {}, {} {}, 해시 {}, 검증 {}",
            fmt_label,
            if format == ImageFormat::E01 { "조각" } else { "분할" },
            split_label,
            hash_label,
            if req.verify { "함" } else { "안 함" }
        );
    }
    if !a.yes {
        if !std::io::stdin().is_terminal() {
            eprintln!("{}", ws_core::tr!("오류: 확인 없이 시작하려면 --yes를 붙이세요", "Error: add --yes to start without confirmation"));
            return 2;
        }
        eprint!("{}", ws_core::tr!("시작할까요? [y/N] ", "Start? [y/N] "));
        let _ = std::io::stderr().flush();
        let mut ans = String::new();
        let _ = std::io::stdin().read_line(&mut ans);
        if !matches!(ans.trim(), "y" | "Y" | "yes") {
            eprintln!("{}", ws_core::tr!("취소했습니다.", "Cancelled."));
            return 0;
        }
    }

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let c = cancel.clone();
        let _ = ctrlc::set_handler(move || {
            if c.swap(true, Ordering::SeqCst) {
                std::process::exit(130);
            }
            eprintln!(
                "\n{}",
                ws_core::tr!(
                    "중단 요청을 받았습니다. 현재 작업을 정리하고 멈춥니다 (한 번 더 누르면 즉시 종료)",
                    "Cancel requested. Cleaning up the current job and stopping (press again to exit immediately)"
                )
            );
        });
    }

    let mut obs = CliObserver { tty: std::io::stderr().is_terminal(), titles: Vec::new(), last_line_len: 0 };
    let summary = match run_collection(&req, &cancel, &mut obs) {
        Ok(s) => s,
        Err(e) => return fail(format!("{e:#}")),
    };
    obs.clear_line();

    // 결과 요약 (stdout: 파이프로 받기 좋게)
    println!();
    for j in &summary.jobs {
        let st = match j.status {
            JobStatus::Done => ws_core::tr!("완료", "Done"),
            JobStatus::Failed => ws_core::tr!("실패", "Failed"),
            JobStatus::Cancelled => ws_core::tr!("중단", "Cancelled"),
            JobStatus::Skipped => ws_core::tr!("건너뜀", "Skipped"),
        };
        println!("[{st}] {} ({})", j.title, j.description);
        if let Some(r) = &j.result {
            for f in &r.files {
                println!("    {}  {}", f.name, format_bytes(f.size));
            }
            if r.format == ImageFormat::E01 && r.total_bytes > 0 {
                println!(
                    "    {} {} → E01 {} ({:.1}%)",
                    ws_core::tr!("원본", "Source"),
                    format_bytes(r.total_bytes),
                    format_bytes(r.stored_bytes),
                    r.stored_bytes as f64 * 100.0 / r.total_bytes as f64
                );
            }
            for (alg, h) in &r.hashes {
                println!("    {:<8} {}", alg.label(), h);
            }
            // 영어는 열 맞춤용 공백 포함 ("Verify" 6자 + 공백)
            let vlabel = ws_core::tr!("검증    ", "Verify  ");
            match &r.verify {
                Some(v) if v.matched => println!("    {vlabel} {}", ws_core::tr!("일치", "matched")),
                Some(v) => println!(
                    "    {vlabel} {}",
                    v.error
                        .as_deref()
                        .map(|e| ws_core::tr!(format!("실패 ({e})"), format!("failed ({e})")))
                        .unwrap_or_else(|| ws_core::tr!("불일치", "mismatch").into())
                ),
                None => println!("    {vlabel} {}", ws_core::tr!("안 함", "skipped")),
            }
            if r.bad_sectors > 0 {
                println!(
                    "{}",
                    ws_core::tr!(
                        format!("    읽기 오류 섹터 {}개 (0으로 채움)", r.bad_sectors),
                        format!("    Bad sectors: {} (zero-filled)", r.bad_sectors)
                    )
                );
            }
        }
        if let Some(e) = &j.error {
            println!("    {}: {e}", ws_core::tr!("오류", "Error"));
        }
    }
    match &req.remote {
        Some(r) => {
            // 수신 PC의 경로 형식(Windows/Linux)에 맞춰 이어 붙임
            let d = summary.evidence_dir.display().to_string();
            let sep = if d.contains('\\') { '\\' } else { '/' };
            println!(
                "{}",
                ws_core::tr!(
                    format!("보고서: 수신 PC({}) {}{}{}", r.addr, d, sep, summary.report_file),
                    format!("Report: receiving PC ({}) {}{}{}", r.addr, d, sep, summary.report_file)
                )
            )
        }
        None => println!("{}: {}", ws_core::tr!("보고서", "Report"), summary.evidence_dir.join(&summary.report_file).display()),
    }
    let bad: u64 = summary.jobs.iter().filter_map(|j| j.result.as_ref()).map(|r| r.bad_sectors).sum();
    match summary.status {
        SessionStatus::Completed if bad > 0 => {
            eprintln!(
                "{}",
                ws_core::tr!(
                    format!("주의: 읽기 오류 섹터 {bad}개를 0으로 채웠습니다. 위치는 보고서에 있습니다."),
                    format!("Warning: {bad} bad sectors were zero-filled. Their locations are listed in the report.")
                )
            );
            4
        }
        SessionStatus::Completed => 0,
        SessionStatus::Partial => 3,
        SessionStatus::Cancelled => 130,
    }
}

/// 수신 PC: 연결 코드를 보여주고 이미지를 받아 저장
fn cmd_receive(out: PathBuf, listen: String, code: Option<String>, resume_wait: u64) -> i32 {
    use ws_core::net::{serve, ReceiveOptions, RecvEvent};
    // 저장 폴더 검사 (로컬 이미징과 같은 규칙). 수신 PC는 대상 시스템이 아니므로 권한은 필요 없음
    let disks = disks_with_roles(Some(&out)).unwrap_or_default();
    let oc = ws_core::inventory::check_output_dir(&out, &disks);
    if !oc.ok {
        return fail(&oc.message);
    }
    eprintln!("{}: {} ({})", ws_core::tr!("저장 위치", "Output location"), out.display(), oc.message);
    if let Some(w) = &oc.warning {
        eprintln!("{}: {w}", ws_core::tr!("주의", "Warning"));
    }
    let space = ws_core::collect::receiver_space;
    let opts = ReceiveOptions { listen, out_base: out, code, resume_wait: std::time::Duration::from_secs(resume_wait.max(1) * 60) };
    let tty = std::io::stderr().is_terminal();
    let mut line_len = 0usize;
    let clear = |n: &mut usize| {
        if tty && *n > 0 {
            eprint!("\r{}\r", " ".repeat(*n));
            *n = 0;
        }
    };
    let mut on_event = |e: RecvEvent| match e {
        RecvEvent::Listening { addrs, code } => {
            eprintln!();
            if ws_core::i18n::en() {
                eprintln!("  Connection code  {code}");
                for a in &addrs {
                    eprintln!("  This PC address  {a}");
                }
            } else {
                eprintln!("  연결 코드   {code}");
                for a in &addrs {
                    eprintln!("  이 PC 주소  {a}");
                }
            }
            eprintln!();
            eprintln!("{}", ws_core::tr!("대상 시스템에서 실행:", "Run on the target system:"));
            let addr = addrs.first().cloned().unwrap_or_else(|| ws_core::tr!("<이 PC 주소>", "<this PC address>").into());
            eprintln!(
                "{}",
                ws_core::tr!(
                    format!("  wscollector-cli image -s <대상> --send {addr} --case <사건번호> --evidence <증거번호> --examiner <수집자>"),
                    format!("  wscollector-cli image -s <target> --send {addr} --case <case number> --evidence <evidence number> --examiner <examiner>")
                )
            );
            eprintln!(
                "{}",
                ws_core::tr!(
                    "  (연결 코드는 실행 후 물어볼 때 입력. 명령행 --code는 대상 시스템의 프로세스 목록, 셸 기록에 남음)",
                    "  (Enter the connection code when prompted. --code on the command line remains in the target system's process list and shell history)"
                )
            );
            eprintln!();
            eprintln!("{}", ws_core::tr!("접속을 기다립니다... (Ctrl+C: 중지)", "Waiting for connection... (Ctrl+C: stop)"));
        }
        RecvEvent::Rejected { peer, reason } => {
            clear(&mut line_len);
            eprintln!("! {peer} {}: {reason}", ws_core::tr!("접속 거부", "connection rejected"));
        }
        RecvEvent::AuthFailed { peer, reason, count, delay_secs } => {
            clear(&mut line_len);
            eprintln!(
                "{}",
                ws_core::tr!(
                    format!("! {peer} 연결 코드 불일치 ({reason}). 누적 {count}번, 다음 접속까지 {delay_secs}초 대기"),
                    format!("! {peer} connection code mismatch ({reason}). {count} attempts so far, waiting {delay_secs}s before the next connection")
                )
            );
        }
        RecvEvent::FileIncomplete { name, size } => {
            clear(&mut line_len);
            eprintln!("! {}: {name} ({})", ws_core::tr!("끝까지 받지 못한 파일", "Incomplete file"), format_bytes(size));
        }
        RecvEvent::Accepted { peer, case, host, evidence_dir, need_bytes } => {
            if ws_core::i18n::en() {
                eprintln!("Connected: {peer} (host {host})");
                eprintln!("  Case {} / Evidence {} / Examiner {}", case.case_number, case.evidence_number, case.examiner);
                eprintln!("  Evidence folder {evidence_dir}, size to receive {}", format_bytes(need_bytes));
            } else {
                eprintln!("접속: {peer} (호스트 {host})");
                eprintln!("  사건 {} / 증거 {} / 수집자 {}", case.case_number, case.evidence_number, case.examiner);
                eprintln!("  저장 폴더 {evidence_dir}, 받을 크기 {}", format_bytes(need_bytes));
            }
        }
        RecvEvent::FileOpened { .. } => {}
        RecvEvent::FileClosed { name, size } => {
            clear(&mut line_len);
            eprintln!("  {} {name} ({})", ws_core::tr!("저장", "Saved"), format_bytes(size));
        }
        RecvEvent::Received { total, need, rate_bps } => {
            if tty {
                let pct = if need > 0 { total as f64 * 100.0 / need as f64 } else { 0.0 };
                let l = format!(
                    "  {} {:5.1}%  {} / {}  {}",
                    ws_core::tr!("받는 중", "Receiving"),
                    pct.min(100.0),
                    format_bytes(total),
                    format_bytes(need),
                    format_rate(rate_bps)
                );
                let pad = line_len.saturating_sub(l.chars().count());
                eprint!("\r{l}{}", " ".repeat(pad));
                line_len = l.chars().count();
            }
        }
        RecvEvent::Verifying { done, total } => {
            if tty {
                let pct = if total > 0 { done as f64 * 100.0 / total as f64 } else { 0.0 };
                let l = format!("  {} {:5.1}%  {} / {}", ws_core::tr!("검증 중", "Verifying"), pct, format_bytes(done), format_bytes(total));
                let pad = line_len.saturating_sub(l.chars().count());
                eprint!("\r{l}{}", " ".repeat(pad));
                line_len = l.chars().count();
            }
        }
        RecvEvent::VerifyDone => {
            clear(&mut line_len);
            eprintln!(
                "{}",
                ws_core::tr!(
                    "  검증용 다시 읽기 완료 (해시 비교 결과는 대상 시스템 화면과 보고서에 있음)",
                    "  Verification re-read complete (hash comparison results are on the target system's screen and in the report)"
                )
            );
        }
        RecvEvent::Finished { status } => {
            clear(&mut line_len);
            eprintln!("{}: {status}", ws_core::tr!("세션 종료", "Session ended"));
        }
        RecvEvent::Disconnected { reason, wait_secs } => {
            clear(&mut line_len);
            let mins = wait_secs / 60;
            eprintln!(
                "{}",
                ws_core::tr!(
                    format!("! 연결이 끊어졌습니다 ({reason}). 보내는 쪽이 다시 연결해 이어받기를 {mins}분 동안 기다립니다"),
                    format!("! Connection lost ({reason}). Waiting {mins} min for the sender to reconnect and resume")
                )
            );
        }
        RecvEvent::Resumed { peer, count } => {
            clear(&mut line_len);
            eprintln!(
                "{}",
                ws_core::tr!(format!("  다시 연결됨: {peer} (이어받기 {count}번째)"), format!("  Reconnected: {peer} (resume #{count})"))
            );
        }
    };
    // Ctrl+C 한 번: 정리 후 중지 (받다 만 파일은 .incomplete), 두 번: 즉시 종료
    let ctrl = Arc::new(ws_core::net::ReceiveControl::default());
    {
        let c = ctrl.clone();
        let _ = ctrlc::set_handler(move || {
            if c.stopped() {
                std::process::exit(130);
            }
            eprintln!("\n{}", ws_core::tr!("수신을 중지합니다 (한 번 더 누르면 즉시 종료)", "Stopping receive (press again to exit immediately)"));
            c.stop();
        });
    }
    match serve(&opts, &space, &mut on_event, &ctrl) {
        Ok(o) => {
            println!("{}: {}", ws_core::tr!("증거 폴더", "Evidence folder"), o.evidence_dir.display());
            for (n, s) in &o.files {
                println!("    {n}  {}", format_bytes(*s));
            }
            for (n, s) in &o.incomplete {
                println!("    {n}  {}  ({})", format_bytes(*s), ws_core::tr!("끝까지 받지 못함", "incomplete"));
            }
            // 보내는 쪽이 알려 준 세션 상태 (프로토콜 값). 보내는 쪽이 영어 화면이어도 완료로 봄
            if matches!(o.status.as_str(), "완료" | "Completed") && o.incomplete.is_empty() {
                0
            } else {
                3
            }
        }
        Err(e) => fail(format!("{e:#}")),
    }
}
