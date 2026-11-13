//! 터미널 화면 (`WSCollector-CLI tui`).
//!
//! GUI와 같은 4단계(사건 정보 → 수집 항목 → 진행 → 결과)를 터미널에서 진행합니다.
//! 화면 엔진이 필요 없어 오래된 배포판, 화면 없는 서버, SSH 접속에서도 쓸 수 있습니다.
//! 수집은 GUI, CLI와 같은 엔진(`run_collection`)으로 하므로 결과물이 같습니다.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

/// 화면 스레드가 아닌 곳에서 난 마지막 패닉 (내용과 위치)
static LAST_PANIC: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
/// SSH 끊김 등으로 터미널을 잃음 (SIGHUP)
static HANGUP: AtomicBool = AtomicBool::new(false);
/// 터미널 끊김을 수집 로그에 남겼는지
static HANGUP_NOTED: AtomicBool = AtomicBool::new(false);
/// 수집 중인지 (터미널이 끊겼을 때 바로 끝내도 되는지 판단)
static COLLECTING: AtomicBool = AtomicBool::new(false);
/// 터미널이 끊겼을 때 쓸 종료 코드 (화면 스레드가 늘 갱신)
static IDLE_EXIT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(129);

/// 터미널 감시: 끊기면(SIGHUP 또는 tcgetattr 실패)
/// - 표준 입력을 아무것도 오지 않는 파이프로 바꿈. 끊긴 터미널에서 crossterm이 read()=0을 쉬지 않고
///   반복하며(CPU 100%) 돌아오지 않는 문제를 막음 (파이프에서는 읽기가 그냥 기다림)
/// - 수집 중이 아니면 바로 끝냄. 수집 중이면 수집 스레드가 기록을 남기고 끝낼 때 프로세스를 끝냄
#[cfg(unix)]
fn spawn_terminal_watch() {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: 파이프 두 끝을 만들고 쓰기 끝은 프로세스가 끝날 때까지 열어 둠 (읽기가 EOF 없이 기다리게)
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    let _ = std::thread::Builder::new().name("tty-watch".into()).spawn(move || {
        let mut redirected = false;
        loop {
            std::thread::sleep(Duration::from_millis(200));
            // SIGHUP만으로는 끊김으로 보지 않음 (kill -HUP 등 오탐). 터미널이 실제로 죽었을 때만
            if terminal_alive() {
                continue;
            }
            HANGUP.store(true, Ordering::SeqCst);
            if !redirected {
                // SAFETY: 유효한 파이프 읽기 끝을 표준 입력 자리에 복제
                unsafe {
                    libc::dup2(fds[0], libc::STDIN_FILENO);
                }
                redirected = true;
            }
            if !COLLECTING.load(Ordering::SeqCst) {
                std::process::exit(IDLE_EXIT.load(Ordering::SeqCst));
            }
        }
    });
}

#[cfg(not(unix))]
fn spawn_terminal_watch() {}

/// 터미널이 살아 있는지: 끊긴(hang up) 터미널은 tcgetattr가 실패함.
/// 이벤트 읽기, 그리기 오류만으로는 못 잡는 경우가 있어(끊긴 뒤 읽기가 쉬지 않고 돌아옴) 직접 확인
fn terminal_alive() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: 표준 출력 fd의 터미널 속성을 읽기만 함
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            libc::tcgetattr(libc::STDOUT_FILENO, &mut t) == 0
        }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(unix)]
extern "C" fn on_hangup(_: libc::c_int) {
    // 신호 처리기 안에서는 원자 변수만 바꿈
    HANGUP.store(true, Ordering::SeqCst);
}

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use ws_core::case::CaseInfo;
use ws_core::collect::{preflight, run_collection, CollectEvent, CollectObserver, CollectRequest, CollectSummary, JobStatus, RemoteTarget, SessionStatus};
use ws_core::hash::HashAlg;
use ws_core::e01::Compression;
use ws_core::imaging::{ImageFormat, Phase, Progress};
use ws_core::inventory::{check_output_dir, disks_with_roles};
use ws_core::logbook::LogLevel;
use ws_core::memory::MemoryInfo;
use ws_core::model::{DiskInfo, DiskRole, HostInfo};
use ws_core::platform;
use ws_core::tr;
use ws_core::target::Target;
use ws_core::util::{format_bytes, format_duration, format_rate};

const ACCENT: Color = Color::Rgb(0, 120, 212);

/// 표시 폭 기준 왼쪽 정렬 (한글은 한 글자가 두 칸)
fn pad(s: &str, width: usize) -> String {
    let w = unicode_width::UnicodeWidthStr::width(s);
    format!("{s}{}", " ".repeat(width.saturating_sub(w)))
}

/// 개발 빌드 전용 시험 모드: 원본 앞부분만 읽기 (GUI와 같은 WS_TEST_MAX_BYTES)
fn test_max_bytes() -> Option<u64> {
    #[cfg(debug_assertions)]
    {
        std::env::var("WS_TEST_MAX_BYTES").ok().and_then(|v| ws_core::util::parse_size(&v))
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Case,
    Select,
    Run,
    Done,
}

/// 사건 정보 입력칸
#[derive(Clone, Copy, PartialEq, Eq)]
enum F {
    Case,
    Evidence,
    Examiner,
    Org,
    Notes,
    Dest,
    Out,
    Addr,
    Code,
}

struct Item {
    label: String,
    detail: String,
    size: u64,
    target: Target,
    /// 고를 수 없는 이유
    locked: Option<String>,
    indent: bool,
}

/// 저장 폴더로 고를 수 있는 연결된 매체 (마운트된 파티션, 볼륨)
struct Media {
    /// 저장 폴더 칸에 채울 경로 (Linux 마운트 위치, Windows "E:\")
    path: String,
    /// 장치, 파일시스템, 공간, 역할 설명
    detail: String,
    /// 대상 시스템 디스크 (라이브 수집 저장 비권장)
    system: bool,
}

/// 연결된 매체 목록: 외장, 이동식 먼저, 시스템 디스크 마지막. 읽기 전용, 부팅, 스왑은 뺌
fn list_media() -> Result<Vec<Media>, String> {
    let disks = disks_with_roles(None).map_err(|e| format!("{e:#}"))?;
    let skip_fs = |fs: &str| matches!(fs.to_ascii_lowercase().as_str(), "iso9660" | "squashfs" | "udf" | "swap" | "erofs");
    let mut out: Vec<(u8, Media)> = Vec::new();
    for d in &disks {
        let system = d.has_role(DiskRole::System);
        let rank = if system { 2 } else if d.removable || d.external() { 0 } else { 1 };
        let mut tags = vec![d.kind_label().to_string()];
        if !d.bus.is_empty() {
            tags.insert(0, d.bus.clone());
        }
        if system {
            tags.push(tr!("시스템", "System").into());
        }
        if d.has_role(DiskRole::Tool) {
            tags.push(tr!("수집 도구", "Tool").into());
        }
        let tags = tags.join(", ");
        let space = |size: u64, free: Option<u64>| match free {
            Some(f) => tr!(format!("{} 남음 / {}", format_bytes(f), format_bytes(size)), format!("{} free / {}", format_bytes(f), format_bytes(size))),
            None => format_bytes(size),
        };
        for p in &d.partitions {
            let Some(point) = p.letter.as_ref().filter(|l| !l.is_empty()) else { continue };
            if skip_fs(&p.fs) || (!cfg!(windows) && (point == "/boot" || point.starts_with("/boot/"))) {
                continue;
            }
            let path = if cfg!(windows) { format!("{}\\", point.trim_end_matches('\\')) } else { point.clone() };
            let dev = if cfg!(windows) { if p.label.is_empty() { tr!(format!("디스크 {}", d.number), format!("Disk {}", d.number)) } else { p.label.clone() } } else { p.device.clone().unwrap_or_default() };
            let detail = format!("{dev}  {}  {}  [{tags}]", if p.fs.is_empty() { "?" } else { &p.fs }, space(p.size, p.free));
            out.push((rank, Media { path, detail, system }));
        }
        // Linux: 장치 전체가 바로 마운트된 경우 (LVM, LUKS 볼륨, 루프 등). 같은 장치가 여러 곳에 마운트되면 가장 짧은 경로 하나만
        if !skip_fs(&d.fs) {
            if let Some(point) = d.mounts.iter().filter(|p| *p != "/boot" && !p.starts_with("/boot/")).min_by_key(|p| p.len()) {
                let vol = platform::volume_for_path(std::path::Path::new(point));
                let (size, free) = vol.map(|v| (v.size, Some(v.free))).unwrap_or((d.size, None));
                let detail = format!("{}  {}  {}  [{tags}]", d.path, if d.fs.is_empty() { "?" } else { &d.fs }, space(size, free));
                out.push((rank, Media { path: point.clone(), detail, system }));
            }
        }
    }
    // 같은 경로가 두 번 나오지 않게 (여러 장치로 된 파일시스템)
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)));
    let mut seen = std::collections::HashSet::new();
    Ok(out.into_iter().map(|(_, m)| m).filter(|m| seen.insert(m.path.clone())).collect())
}

enum Msg {
    Event(CollectEvent),
    Finished(Result<CollectSummary, String>),
}

struct JobView {
    title: String,
    size: u64,
    status: &'static str,
    progress: Option<Progress>,
}

struct App {
    step: Step,
    host: HostInfo,
    elevated: bool,
    memory: Option<MemoryInfo>,
    /// 메모리를 수집할 수 없는 이유 (Linux)
    memory_error: Option<String>,
    // 사건 정보
    case_number: String,
    evidence: String,
    examiner: String,
    org: String,
    notes: String,
    net: bool,
    out: String,
    addr: String,
    code: String,
    field: F,
    /// 저장 폴더 아래에 보여 줄 연결된 매체
    media: Vec<Media>,
    /// 매체 목록을 읽지 못한 이유 (권한 부족 등. "매체 없음"과 구분)
    media_error: Option<String>,
    // 수집 항목
    items: Vec<Item>,
    picked: Vec<bool>,
    cursor: ListState,
    split: u64,
    format: ImageFormat,
    compression: Compression,
    algs: [bool; 3],
    verify: bool,
    free: Option<u64>,
    // 진행
    cancel: Arc<AtomicBool>,
    rx: Option<Receiver<Msg>>,
    jobs: Vec<JobView>,
    logs: Vec<(LogLevel, String)>,
    evidence_dir: String,
    confirm_cancel: bool,
    // 결과
    summary: Option<CollectSummary>,
    error: Option<String>,
    status: String,
    quit: bool,
}

impl App {
    fn new() -> Self {
        // Linux, Windows 모두 (Windows는 드라이버를 적재하지 않고 파일, 영역만 확인)
        let probe = Some(ws_core::memory::memory_probe());
        App {
            step: Step::Case,
            host: platform::host_info(),
            elevated: platform::is_elevated(),
            memory: probe.as_ref().and_then(|p| p.as_ref().ok().cloned()),
            memory_error: probe.and_then(|p| p.err()),
            case_number: String::new(),
            evidence: String::new(),
            examiner: String::new(),
            org: String::new(),
            notes: String::new(),
            net: false,
            out: String::new(),
            addr: String::new(),
            code: String::new(),
            field: F::Case,
            media: Vec::new(),
            media_error: None,
            items: Vec::new(),
            picked: Vec::new(),
            cursor: ListState::default(),
            split: 0,
            format: ImageFormat::E01,
            compression: Compression::Best,
            algs: [true, true, true],
            verify: true,
            free: None,
            cancel: Arc::new(AtomicBool::new(false)),
            rx: None,
            jobs: Vec::new(),
            logs: Vec::new(),
            evidence_dir: String::new(),
            confirm_cancel: false,
            summary: None,
            error: None,
            status: tr!("Tab, ↑↓ 칸 이동, 글자 입력, Enter 다음 단계, Esc 끝내기", "Tab/↑↓ move field, type to enter, Enter next step, Esc quit").into(),
            quit: false,
        }
    }

    fn fields(&self) -> Vec<F> {
        let mut v = vec![F::Case, F::Evidence, F::Examiner, F::Org, F::Notes, F::Dest];
        if self.net {
            v.extend([F::Addr, F::Code]);
        } else {
            v.push(F::Out);
        }
        v
    }

    fn value_mut(&mut self, f: F) -> Option<&mut String> {
        Some(match f {
            F::Case => &mut self.case_number,
            F::Evidence => &mut self.evidence,
            F::Examiner => &mut self.examiner,
            F::Org => &mut self.org,
            F::Notes => &mut self.notes,
            F::Out => &mut self.out,
            F::Addr => &mut self.addr,
            F::Code => &mut self.code,
            F::Dest => return None,
        })
    }

    fn case_info(&self) -> CaseInfo {
        CaseInfo {
            case_number: self.case_number.trim().into(),
            evidence_number: self.evidence.trim().into(),
            examiner: self.examiner.trim().into(),
            organization: self.org.trim().into(),
            notes: self.notes.trim().into(),
        }
    }

    // -- 사건 정보 → 수집 항목 --
    fn submit_case(&mut self) {
        if let Err(e) = self.case_info().validate() {
            self.status = tr!(format!("확인: {e}"), format!("Check: {e}"));
            return;
        }
        let out_path = PathBuf::from(self.out.trim());
        if self.net {
            if self.addr.trim().is_empty() {
                self.status = tr!("확인: 수신 PC 주소를 입력하세요", "Check: enter the receiving PC address").into();
                return;
            }
            if let Err(e) = ws_core::net::normalize_code(&self.code) {
                self.status = tr!(format!("확인: {e}"), format!("Check: {e}"));
                return;
            }
            self.free = None;
        } else {
            let disks = disks_with_roles(Some(&out_path)).unwrap_or_default();
            let oc = check_output_dir(&out_path, &disks);
            if !oc.ok {
                self.status = tr!(format!("확인: {}", oc.message), format!("Check: {}", oc.message));
                return;
            }
            self.free = oc.volume.as_ref().map(|v| v.free);
            if let Some(w) = oc.warning {
                self.logs.push((LogLevel::Warn, tr!(format!("저장 위치: {w}"), format!("Destination: {w}"))));
            }
        }
        self.load_items();
        self.step = Step::Select;
        self.status = tr!(
            "↑↓ 이동, Space 선택, f 형식, c 압축, s 나누기, 1,2,3 해시, v 검증, Enter 시작, Backspace 이전",
            "↑↓ move, Space select, f format, c compression, s split, 1/2/3 hash, v verify, Enter start, Backspace back"
        )
        .into();
    }

    fn load_items(&mut self) {
        let out = (!self.net).then(|| PathBuf::from(self.out.trim()));
        let disks: Vec<DiskInfo> = disks_with_roles(out.as_deref()).unwrap_or_default();
        let mut items = Vec::new();
        if let Some(m) = &self.memory {
            items.push(Item {
                label: tr!("메모리", "Memory").into(),
                detail: tr!(format!("원본 {}, LiME 형식, 디스크보다 먼저", m.source), format!("Source {}, LiME format, before disks", m.source)),
                size: m.image_bytes,
                target: Target::Memory,
                locked: None,
                indent: false,
            });
        } else if let Some(e) = &self.memory_error {
            // 고를 수 없게 보여 주고 이유를 그대로 표시 (조용히 빠지면 왜 없는지 알 수 없음)
            items.push(Item { label: tr!("메모리", "Memory").into(), detail: String::new(), size: 0, target: Target::Memory, locked: Some(tr!(format!("수집 불가: {e}"), format!("Unavailable: {e}"))), indent: false });
        }
        let linux = !cfg!(windows);
        for d in &disks {
            let t = if linux { Target::Device { path: d.path.clone(), serial: (!d.serial.is_empty()).then(|| d.serial.clone()) } } else { Target::Disk { number: d.number, serial: (!d.serial.is_empty()).then(|| d.serial.clone()) } };
            let locked = out.as_ref().filter(|o| platform::overlaps_output(&t, &disks, o)).map(|_| tr!("저장 폴더가 있는 디스크", "Disk holds the output folder").to_string());
            let mut tags = vec![d.bus.clone(), d.kind_label().to_string()];
            if d.has_role(DiskRole::System) {
                tags.push(tr!("시스템", "System").into());
            }
            if d.has_role(DiskRole::Tool) {
                tags.push(tr!("수집 도구", "Tool").into());
            }
            items.push(Item {
                label: if linux { d.path.clone() } else { format!("Disk {}", d.number) },
                detail: format!("{}  [{}]", d.model, tags.join(", ")),
                size: d.size,
                target: t,
                locked,
                indent: false,
            });
            for p in &d.partitions {
                let t = if linux {
                    match &p.device {
                        Some(dev) => Target::Device { path: dev.clone(), serial: None },
                        None => continue,
                    }
                } else if let Some(l) = &p.letter {
                    Target::Volume { letter: l.clone() }
                } else {
                    Target::Partition { disk: d.number, number: p.number, serial: None }
                };
                let locked = out.as_ref().filter(|o| platform::overlaps_output(&t, &disks, o)).map(|_| tr!("저장 폴더가 있는 파티션", "Partition holds the output folder").to_string());
                let name = if linux { p.device.clone().unwrap_or_default() } else { p.display_name() };
                let mut detail = p.display_name();
                if !p.fs.is_empty() {
                    detail.push_str(&format!(" ({})", p.fs));
                }
                items.push(Item { label: name, detail, size: p.size, target: t, locked, indent: true });
            }
        }
        self.picked = vec![false; items.len()];
        self.items = items;
        self.cursor.select(Some(0));
    }

    fn need(&self) -> u64 {
        self.items.iter().zip(&self.picked).filter(|(_, p)| **p).map(|(i, _)| i.size).sum()
    }

    fn request(&self) -> CollectRequest {
        let algs: Vec<HashAlg> = HashAlg::ALL.iter().zip(self.algs).filter(|(_, on)| *on).map(|(a, _)| *a).collect();
        CollectRequest {
            case: self.case_info(),
            out_base: if self.net { PathBuf::new() } else { PathBuf::from(self.out.trim()) },
            targets: self.items.iter().zip(&self.picked).filter(|(_, p)| **p).map(|(i, _)| i.target.clone()).collect(),
            split_bytes: self.split,
            format: self.format,
            compression: self.compression,
            algs,
            verify: self.verify,
            zero_fill_errors: true,
            interface: "TUI".into(),
            max_bytes: test_max_bytes(),
            remote: self.net.then(|| RemoteTarget { addr: self.addr.trim().into(), code: self.code.trim().into() }),
        }
    }

    fn start(&mut self) {
        let req = self.request();
        if req.targets.is_empty() {
            self.status = tr!("확인: 수집할 항목을 Space로 고르세요", "Check: select targets with Space").into();
            return;
        }
        if req.algs.is_empty() {
            self.status = tr!("확인: 해시를 하나 이상 켜세요 (1,2,3)", "Check: enable at least one hash (1/2/3)").into();
            return;
        }
        if let Err(e) = preflight(&req) {
            self.status = tr!(format!("확인: {e:#}"), format!("Check: {e:#}"));
            return;
        }
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        self.rx = Some(rx);
        self.jobs.clear();
        self.logs.clear();
        self.summary = None;
        self.error = None;
        COLLECTING.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            struct Fwd(Sender<Msg>);
            impl CollectObserver for Fwd {
                fn event(&mut self, e: CollectEvent) {
                    let _ = self.0.send(Msg::Event(e));
                }
                fn take_notes(&mut self) -> Vec<String> {
                    // 화면 스레드에 기대지 않고 수집 스레드가 직접 확인 (화면 스레드가 끊긴 터미널 읽기에 묶여 있을 수 있음)
                    if !terminal_alive() && !HANGUP_NOTED.swap(true, Ordering::SeqCst) {
                        HANGUP.store(true, Ordering::SeqCst);
                        return vec![tr!(
                            "TUI 터미널 연결이 끊겼습니다 (SSH 끊김 등). 화면 없이 수집을 끝까지 계속합니다",
                            "TUI terminal disconnected (e.g. SSH drop). Collection continues to completion without a screen"
                        )
                        .into()];
                    }
                    Vec::new()
                }
            }
            let mut obs = Fwd(tx.clone());
            // 수집 중 패닉해도 화면이 "수집 중"에 멈추지 않게 결과를 반드시 보냄
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_collection(&req, &cancel, &mut obs)));
            let r = match r {
                Ok(r) => r.map_err(|e| format!("{e:#}")),
                Err(_) => {
                    let what = LAST_PANIC.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default();
                    Err(tr!(
                        format!("내부 오류로 수집이 중단되었습니다 ({what}). 이미 저장된 파일과 로그를 확인하세요"),
                        format!("Collection aborted by an internal error ({what}). Check the files and log already saved")
                    ))
                }
            };
            let code = match &r {
                Ok(s) if s.status == SessionStatus::Completed => 0,
                Ok(_) => 3,
                Err(_) => 1,
            };
            // 감시 스레드가 끝낼 때 쓸 종료 코드를 먼저 정해 둠 (화면 스레드가 아직 결과를 못 받았어도 맞는 코드)
            IDLE_EXIT.store(code, Ordering::SeqCst);
            // 터미널이 끊긴 채 끝났으면 화면 스레드를 기다리지 않고 여기서 끝냄 (로그, 요약, 보고서는 이미 저장됨)
            if !terminal_alive() {
                std::process::exit(code);
            }
            COLLECTING.store(false, Ordering::SeqCst);
            let _ = tx.send(Msg::Finished(r));
        });
        self.step = Step::Run;
        self.status = tr!("수집 중. c 중단", "Collecting. c cancel").into();
    }

    fn pump(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut done = None;
        while let Ok(m) = rx.try_recv() {
            match m {
                Msg::Event(CollectEvent::Prepared { evidence_dir, jobs }) => {
                    self.evidence_dir = evidence_dir;
                    self.jobs = jobs.into_iter().map(|j| JobView { title: j.title, size: j.size, status: tr!("대기", "Queued"), progress: None }).collect();
                }
                Msg::Event(CollectEvent::JobStarted { index }) => {
                    if let Some(j) = self.jobs.get_mut(index) {
                        j.status = tr!("진행", "Running");
                    }
                }
                Msg::Event(CollectEvent::Progress { index, progress }) => {
                    if let Some(j) = self.jobs.get_mut(index) {
                        j.progress = Some(progress);
                    }
                }
                Msg::Event(CollectEvent::JobFinished { index, outcome }) => {
                    if let Some(j) = self.jobs.get_mut(index) {
                        j.status = match outcome.status {
                            JobStatus::Done => tr!("완료", "Done"),
                            JobStatus::Failed => tr!("실패", "Failed"),
                            JobStatus::Cancelled => tr!("중단", "Cancelled"),
                            JobStatus::Skipped => tr!("건너뜀", "Skipped"),
                        };
                    }
                }
                Msg::Event(CollectEvent::Log(l)) => {
                    self.logs.push((l.level, format!("{} {}", l.time.get(11..19).unwrap_or(&l.time), l.message)));
                    if self.logs.len() > 2000 {
                        self.logs.drain(..500);
                    }
                }
                Msg::Finished(r) => done = Some(r),
            }
        }
        if let Some(r) = done {
            self.rx = None;
            self.confirm_cancel = false;
            match r {
                Ok(s) => self.summary = Some(s),
                Err(e) => self.error = Some(e),
            }
            self.step = Step::Done;
            self.status = tr!("n 새 수집 (같은 사건, 증거 번호만 새로), q 끝내기", "n new collection (same case, new evidence number), q quit").into();
        }
    }

    fn key(&mut self, k: KeyEvent) {
        if k.kind != KeyEventKind::Press {
            return;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            if self.step == Step::Run {
                self.confirm_cancel = true;
            } else {
                self.quit = true;
            }
            return;
        }
        // Ctrl, Alt 조합(Ctrl+S, Alt+F 등)은 글자 입력, 단축키로 쓰지 않음. Ctrl+Alt는 AltGr 글자라 그대로 둠
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        if matches!(k.code, KeyCode::Char(_)) && (ctrl ^ alt) {
            return;
        }
        match self.step {
            Step::Case => self.key_case(k),
            Step::Select => self.key_select(k),
            Step::Run => self.key_run(k),
            Step::Done => self.key_done(k),
        }
    }

    fn key_case(&mut self, k: KeyEvent) {
        let fields = self.fields();
        let pos = fields.iter().position(|f| *f == self.field).unwrap_or(0);
        let before = self.field;
        match k.code {
            KeyCode::Esc => self.quit = true,
            KeyCode::Tab | KeyCode::Down => self.field = fields[(pos + 1) % fields.len()],
            KeyCode::BackTab | KeyCode::Up => self.field = fields[(pos + fields.len() - 1) % fields.len()],
            KeyCode::Enter => self.submit_case(),
            // 항목 순서는 고정 (매체 → 네트워크), ◉만 움직임
            KeyCode::Left if self.field == F::Dest => self.net = false,
            KeyCode::Right if self.field == F::Dest => self.net = true,
            KeyCode::Char(' ') if self.field == F::Dest => self.net = !self.net,
            KeyCode::Left | KeyCode::Right if self.field == F::Out && !self.media.is_empty() => {
                let n = self.media.len();
                // 처음 고를 때는 ←→ 모두 맨 앞(외장 매체)부터. ←로 바로 맨 끝 시스템 디스크가 채워지지 않게
                let next = match (self.media_index(), k.code == KeyCode::Right) {
                    (None, _) => 0,
                    (Some(i), true) => (i + 1) % n,
                    (Some(i), false) => (i + n - 1) % n,
                };
                self.out = self.media[next].path.clone();
                // 시스템 디스크는 오프라인 이미징(분석 PC 저장)에서 쓰므로 막지 않고 상태줄에 경고
                self.status = if self.media[next].system {
                    tr!(
                        "주의: 대상 시스템 디스크입니다. 라이브 수집이면 외부 매체를 고르세요 (오프라인 이미징에서만 사용)",
                        "Caution: this is the target system disk. For live collection pick external media (offline imaging only)"
                    )
                    .into()
                } else {
                    tr!("Tab, ↑↓ 칸 이동, 글자 입력, Enter 다음 단계, Esc 끝내기", "Tab/↑↓ move field, type to enter, Enter next step, Esc quit").into()
                };
            }
            KeyCode::Backspace => {
                if let Some(v) = self.value_mut(self.field) {
                    v.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(v) = self.value_mut(self.field) {
                    if v.chars().count() < 200 {
                        v.push(c);
                    }
                }
            }
            _ => {}
        }
        // 저장 폴더 칸에 들어올 때마다 매체 목록을 새로 읽음 (USB를 꽂은 뒤에도 반영)
        if self.field == F::Out && before != F::Out {
            self.refresh_media();
        }
    }

    fn refresh_media(&mut self) {
        match list_media() {
            Ok(m) => {
                self.media = m;
                self.media_error = None;
            }
            Err(e) => {
                self.media.clear();
                self.media_error = Some(e);
            }
        }
    }

    /// 저장 폴더 칸 값이 속한 매체 (가장 긴 경로가 맞는 것)
    fn media_index(&self) -> Option<usize> {
        let out = self.out.trim();
        if out.is_empty() {
            return None;
        }
        let sep = if cfg!(windows) { '\\' } else { '/' };
        let norm = |s: &str| if cfg!(windows) { s.to_ascii_lowercase().replace('/', "\\") } else { s.to_string() };
        let out = norm(out);
        self.media
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                let p = norm(&m.path);
                let base = p.trim_end_matches(sep);
                // "/" 는 모든 절대 경로의 조상 (더 긴 마운트 위치가 맞으면 그쪽이 이김)
                if base.is_empty() {
                    return out.starts_with(sep);
                }
                out == base || out == p || out.starts_with(&format!("{base}{sep}"))
            })
            .max_by_key(|(_, m)| m.path.len())
            .map(|(i, _)| i)
    }

    fn key_select(&mut self, k: KeyEvent) {
        let n = self.items.len();
        let cur = self.cursor.selected().unwrap_or(0);
        match k.code {
            KeyCode::Esc | KeyCode::Backspace => {
                self.step = Step::Case;
                self.status = tr!("Tab, ↑↓ 칸 이동, 글자 입력, Enter 다음 단계, Esc 끝내기", "Tab/↑↓ move field, type to enter, Enter next step, Esc quit").into();
            }
            KeyCode::Down if n > 0 => self.cursor.select(Some((cur + 1) % n)),
            KeyCode::Up if n > 0 => self.cursor.select(Some((cur + n - 1) % n)),
            KeyCode::Char(' ') if n > 0 => {
                if let Some(why) = &self.items[cur].locked {
                    self.status = tr!(format!("고를 수 없음: {why} (이미징하는 동안 이미지가 대상 안에 써짐)"), format!("Cannot select: {why} (the image would be written onto the target)"));
                } else {
                    self.picked[cur] = !self.picked[cur];
                }
            }
            KeyCode::Char('s') => {
                self.split = match self.split {
                    0 => 2 << 30,
                    x if x == 2 << 30 => 4 << 30,
                    _ => 0,
                }
            }
            KeyCode::Char('f') => {
                self.format = match self.format {
                    ImageFormat::E01 => ImageFormat::Raw,
                    ImageFormat::Raw => ImageFormat::E01,
                }
            }
            KeyCode::Char('c') if self.format == ImageFormat::E01 => {
                self.compression = match self.compression {
                    Compression::Best => Compression::Fast,
                    Compression::Fast => Compression::None,
                    Compression::None => Compression::Best,
                }
            }
            KeyCode::Char('1') => self.algs[0] = !self.algs[0],
            KeyCode::Char('2') => self.algs[1] = !self.algs[1],
            KeyCode::Char('3') => self.algs[2] = !self.algs[2],
            KeyCode::Char('v') => self.verify = !self.verify,
            KeyCode::Char('r') => self.load_items(),
            KeyCode::Enter => self.start(),
            _ => {}
        }
    }

    fn key_run(&mut self, k: KeyEvent) {
        if self.confirm_cancel {
            match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.cancel.store(true, Ordering::SeqCst);
                    self.confirm_cancel = false;
                    self.status = tr!("중단 요청을 보냈습니다. 현재 작업을 정리하는 중…", "Cancel requested. Finishing the current job…").into();
                }
                _ => self.confirm_cancel = false,
            }
            return;
        }
        if matches!(k.code, KeyCode::Char('c') | KeyCode::Esc) {
            self.confirm_cancel = true;
        }
    }

    fn key_done(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('n') => {
                self.evidence.clear();
                self.notes.clear();
                self.code.clear();
                self.field = F::Evidence;
                self.step = Step::Case;
                self.status = tr!("증거 번호를 입력하세요. Enter 다음 단계", "Enter the evidence number. Enter next step").into();
            }
            _ => {}
        }
    }

    // -- 그리기 --
    fn draw(&mut self, f: &mut Frame) {
        let [top, body, bottom] = Layout::vertical([Constraint::Length(3), Constraint::Min(5), Constraint::Length(2)]).areas(f.area());
        let steps = [(tr!("사건 정보", "Case"), Step::Case), (tr!("수집 항목", "Targets"), Step::Select), (tr!("진행", "Progress"), Step::Run), (tr!("결과", "Result"), Step::Done)];
        let mut spans = vec![Span::styled(" WS Collector ", Style::new().fg(Color::White).bg(ACCENT).bold()), Span::raw("  ")];
        for (i, (name, st)) in steps.iter().enumerate() {
            let style = if *st == self.step { Style::new().fg(ACCENT).bold().underlined() } else { Style::new().fg(Color::DarkGray) };
            spans.push(Span::styled(format!("{}. {name}", i + 1), style));
            spans.push(Span::raw("   "));
        }
        let host = format!(
            " {}  {} {}  {}  {} {}  {}",
            self.host.hostname,
            self.host.os_name,
            self.host.os_version,
            self.host.build_label(),
            tr!("메모리", "RAM"),
            format_bytes(self.host.ram_bytes),
            if self.elevated { tr!("root/관리자 권한", "root/administrator") } else { tr!("일반 권한 (디스크를 읽으려면 sudo)", "standard user (use sudo to read disks)") }
        );
        let mut head = vec![Line::from(spans), Line::from(host).fg(Color::Gray)];
        if let Some(m) = test_max_bytes() {
            head.push(Line::from(tr!(format!(" 개발 빌드 시험 모드: 원본마다 앞부분 {}만 읽습니다", format_bytes(m)), format!(" Dev build test mode: reading only the first {} of each source", format_bytes(m)))).fg(Color::Yellow));
        }
        f.render_widget(Paragraph::new(head), top);
        match self.step {
            Step::Case => self.draw_case(f, body),
            Step::Select => self.draw_select(f, body),
            Step::Run => self.draw_run(f, body),
            Step::Done => self.draw_done(f, body),
        }
        let status = if self.confirm_cancel {
            tr!("수집을 중단할까요? 지금까지 읽은 부분은 증거로 쓸 수 없습니다. y 중단 / 다른 키 계속", "Cancel collection? Data read so far cannot be used as evidence. y cancel / any other key continue").to_string()
        } else {
            self.status.clone()
        };
        let style = if status.starts_with(tr!("확인", "Check")) || status.starts_with(tr!("주의", "Caution")) || self.confirm_cancel { Style::new().fg(Color::Yellow).bold() } else { Style::new().fg(Color::Gray) };
        f.render_widget(Paragraph::new(Line::from(Span::styled(status, style))).block(Block::new().borders(Borders::TOP)), bottom);
    }

    fn draw_case(&self, f: &mut Frame, area: Rect) {
        let row = |label: &str, value: &str, field: F, hint: &str| -> Line<'static> {
            let on = self.field == field;
            let v = if value.is_empty() { Span::styled(hint.to_string(), Style::new().fg(Color::DarkGray)) } else { Span::raw(value.to_string()) };
            let mut spans = vec![Span::styled(format!("{} {}", if on { "▶" } else { " " }, pad(label, tr!(14, 17))), if on { Style::new().fg(ACCENT).bold() } else { Style::new() }), v];
            if on {
                spans.push(Span::styled("▏", Style::new().fg(ACCENT)));
            }
            Line::from(spans)
        };
        let example = if cfg!(windows) { tr!("예: E:\\Cases", "e.g. E:\\Cases") } else { tr!("예: /mnt/usb/Cases", "e.g. /mnt/usb/Cases") };
        let mut lines = vec![
            Line::from(tr!("보고서와 증거 파일 이름에 들어갈 정보입니다.", "This information goes into the report and evidence file names.")).fg(Color::Gray),
            Line::raw(""),
            row(tr!("사건 번호", "Case number"), &self.case_number, F::Case, tr!("예: 2026-형제-01187", "e.g. 2026-CR-01187")),
            row(tr!("증거 번호", "Evidence number"), &self.evidence, F::Evidence, tr!("예: EV-003", "e.g. EV-003")),
            row(tr!("수집자", "Examiner"), &self.examiner, F::Examiner, tr!("예: 홍길동", "e.g. John Doe")),
            row(tr!("소속", "Organization"), &self.org, F::Org, tr!("(선택)", "(optional)")),
            row(tr!("현장 메모", "Notes"), &self.notes, F::Notes, tr!("(선택)", "(optional)")),
            Line::raw(""),
            row(
                tr!("저장 위치", "Destination"),
                &{
                    // 항목 자리는 고정, ◉만 움직임
                    let (a, b) = if self.net { ("○", "◉") } else { ("◉", "○") };
                    tr!(format!("{a} 이 시스템에 연결한 매체   {b} 네트워크 수신 PC"), format!("{a} Media attached to this system   {b} Network receiving PC"))
                },
                F::Dest,
                "",
            ),
        ];
        if self.net {
            lines.push(row(tr!("수신 PC 주소", "Receiver address"), &self.addr, F::Addr, tr!("예: 192.168.0.10", "e.g. 192.168.0.10")));
            lines.push(row(tr!("연결 코드", "Connection code"), &self.code, F::Code, tr!("수신 PC 화면의 10자리 코드", "10-character code shown on the receiving PC")));
            lines.push(Line::raw(""));
            lines.push(Line::from(tr!("이미지, 로그, 보고서를 모두 수신 PC에 저장합니다. 이 시스템 디스크에는 쓰지 않습니다.", "Images, logs and reports are all saved on the receiving PC. Nothing is written to this system's disks.")).fg(Color::Gray));
        } else {
            lines.push(row(tr!("저장 폴더", "Output folder"), &self.out, F::Out, example));
            let sep = if cfg!(windows) { '\\' } else { '/' };
            let base = self.out.trim().trim_end_matches(['/', '\\']);
            lines.push(Line::raw(""));
            lines.push(
                Line::from(format!(
                    "{}{}{sep}{}{sep}{}",
                    tr!("  저장될 경로: ", "  Save path: "),
                    // "/" 를 고르면 base 는 빈 값이지만 입력은 있음 → "/사건/증거"
                    if self.out.trim().is_empty() { tr!("<저장 폴더>", "<output folder>") } else { base },
                    if self.case_number.trim().is_empty() { tr!("<사건 번호>", "<case number>") } else { self.case_number.trim() },
                    if self.evidence.trim().is_empty() { tr!("<증거 번호>", "<evidence number>") } else { self.evidence.trim() }
                ))
                .fg(ACCENT),
            );
            lines.push(Line::raw(""));
            let on = self.field == F::Out;
            lines.push(Line::from(vec![
                Span::styled(tr!("  연결된 매체", "  Attached media"), Style::new().bold()),
                Span::styled(
                    if on {
                        tr!("  ←→ 고르기, 하위 폴더는 이어서 입력", "  ←→ pick, then type a subfolder if needed")
                    } else {
                        tr!("  저장 폴더 칸에서 ←→로 고르기", "  Pick with ←→ in the output folder field")
                    },
                    Style::new().fg(Color::Gray),
                ),
            ]));
            if let Some(e) = &self.media_error {
                lines.push(Line::from(tr!(format!("    매체 목록을 읽지 못했습니다: {e}"), format!("    Could not read the media list: {e}"))).fg(Color::Yellow));
            } else if self.media.is_empty() {
                lines.push(Line::from(tr!("    마운트된 매체가 없습니다. 매체를 마운트한 뒤 저장 폴더 칸으로 다시 오면 새로 읽습니다", "    No mounted media. Mount it, then come back to the output folder field to refresh")).fg(Color::DarkGray));
            }
            let cur = self.media_index();
            let w = self.media.iter().map(|m| unicode_width::UnicodeWidthStr::width(m.path.as_str())).max().unwrap_or(0).clamp(4, 28);
            for (i, m) in self.media.iter().enumerate() {
                let sel = cur == Some(i);
                let mark = if sel { "  ▸ " } else { "    " };
                let path_style = if sel { Style::new().fg(ACCENT).bold() } else if m.system { Style::new().fg(Color::DarkGray) } else { Style::new() };
                let mut spans = vec![Span::styled(mark, Style::new().fg(ACCENT)), Span::styled(pad(&m.path, w), path_style), Span::raw("  "), Span::styled(m.detail.clone(), Style::new().fg(Color::Gray))];
                if m.system {
                    // 라이브 수집에서는 대상 시스템 디스크에 저장하지 않도록 눈에 띄게
                    spans.push(Span::styled(tr!("  라이브 수집 비권장", "  not for live collection"), Style::new().fg(Color::Yellow)));
                }
                lines.push(Line::from(spans));
            }
        }
        f.render_widget(Paragraph::new(lines).block(Block::bordered().title(tr!(" 사건 정보 ", " Case "))).wrap(Wrap { trim: false }), area);
    }

    fn draw_select(&mut self, f: &mut Frame, area: Rect) {
        let [list_area, opt_area] = Layout::vertical([Constraint::Min(5), Constraint::Length(5)]).areas(area);
        let items: Vec<ListItem> = self
            .items
            .iter()
            .zip(&self.picked)
            .map(|(it, on)| {
                let mark = if it.locked.is_some() { "[–]" } else if *on { "[x]" } else { "[ ]" };
                let mut style = Style::new();
                if it.locked.is_some() {
                    style = style.fg(Color::DarkGray);
                } else if *on {
                    style = style.fg(ACCENT).bold();
                }
                let indent = if it.indent { "    " } else { "" };
                let lock = it.locked.as_ref().map(|w| format!("  ({w})")).unwrap_or_default();
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{indent}{mark} {}", pad(&it.label, 22)), style),
                    Span::styled(format!("{:>10}  ", format_bytes(it.size)), style),
                    Span::styled(format!("{}{lock}", it.detail), if it.locked.is_some() { Style::new().fg(Color::DarkGray) } else { Style::new().fg(Color::Gray) }),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(Block::bordered().title(tr!(" 수집 항목 (Space 선택) ", " Targets (Space to select) ")))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶");
        f.render_stateful_widget(list, list_area, &mut self.cursor);
        let need = self.need();
        let split = if self.split == 0 { tr!("안 함", "none").to_string() } else { format_bytes(self.split) };
        let format = match self.format {
            ImageFormat::E01 => format!("E01 {} (f, c)", self.compression.label()),
            ImageFormat::Raw => "RAW .dd (f)".to_string(),
        };
        let hashes: Vec<&str> = ["MD5", "SHA-1", "SHA-256"].iter().zip(self.algs).filter(|(_, on)| *on).map(|(n, _)| *n).collect();
        let space = match (self.net, self.free) {
            (true, _) => tr!(format!("보낼 크기 {} (수신 PC가 여유 공간 확인)", format_bytes(need)), format!("To send {} (receiving PC checks free space)", format_bytes(need))),
            (false, Some(free)) => {
                let short = if need > free { tr!("  (부족)", "  (insufficient)") } else { "" };
                tr!(format!("필요 {} / 여유 {}{short}", format_bytes(need), format_bytes(free)), format!("Required {} / Free {}{short}", format_bytes(need), format_bytes(free)))
            }
            (false, None) => tr!(format!("필요 {}", format_bytes(need)), format!("Required {}", format_bytes(need))),
        };
        let lines = vec![
            Line::from(tr!(
                format!("형식 {format}, 메모리는 LiME    나누기 {split} (s)    해시 {} (1,2,3)    검증 {} (v)", hashes.join("/"), if self.verify { "함" } else { "안 함" }),
                format!("Format {format}, Memory LiME    Split {split} (s)    Hash {} (1/2/3)    Verify {} (v)", hashes.join("/"), if self.verify { "on" } else { "off" })
            )),
            Line::from(space),
            Line::from(if self.net {
                tr!(format!("저장: 네트워크 수신 PC {}", self.addr.trim()), format!("Save to: receiving PC {}", self.addr.trim()))
            } else {
                tr!(format!("저장: {}", self.out.trim()), format!("Save to: {}", self.out.trim()))
            }).fg(Color::Gray),
        ];
        f.render_widget(Paragraph::new(lines).block(Block::bordered().title(tr!(" 옵션 ", " Options "))), opt_area);
    }

    fn draw_run(&self, f: &mut Frame, area: Rect) {
        let n = self.jobs.len().max(1) as u16;
        let [jobs_area, log_area] = Layout::vertical([Constraint::Length(n * 3 + 2), Constraint::Min(3)]).areas(area);
        let block = Block::bordered().title(tr!(format!(" 진행  {} ", self.evidence_dir), format!(" Progress  {} ", self.evidence_dir)));
        let inner = block.inner(jobs_area);
        f.render_widget(block, jobs_area);
        let rows = Layout::vertical(self.jobs.iter().map(|_| Constraint::Length(3))).split(inner);
        for (j, r) in self.jobs.iter().zip(rows.iter()) {
            let (ratio, label) = match &j.progress {
                Some(p) => {
                    let ratio = if p.total > 0 { p.done as f64 / p.total as f64 } else { 0.0 };
                    let phase = if p.phase == Phase::Reading { tr!("읽는 중", "Reading") } else { tr!("검증 중", "Verifying") };
                    let bad = if p.bad_sectors > 0 { tr!(format!("  오류 섹터 {}", p.bad_sectors), format!("  bad sectors {}", p.bad_sectors)) } else { String::new() };
                    (ratio, format!("{phase} {:.1}%  {} / {}  {}  {} {}{bad}", ratio * 100.0, format_bytes(p.done), format_bytes(p.total), format_rate(p.rate_bps), tr!("남은", "ETA"), format_duration(p.eta_secs)))
                }
                None => (if j.status == tr!("완료", "Done") { 1.0 } else { 0.0 }, j.status.to_string()),
            };
            let [t, g] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(*r);
            f.render_widget(Paragraph::new(Line::from(vec![Span::styled(format!("[{}] ", j.status), Style::new().fg(ACCENT).bold()), Span::raw(format!("{} ({})", j.title, format_bytes(j.size)))])), t);
            f.render_widget(Gauge::default().gauge_style(Style::new().fg(ACCENT).bg(Color::Black)).ratio(ratio.clamp(0.0, 1.0)).label(label), g);
        }
        self.draw_log(f, log_area, tr!(" 수집 로그 ", " Collection log "));
    }

    fn draw_log(&self, f: &mut Frame, area: Rect, title: &str) {
        let h = area.height.saturating_sub(2) as usize;
        let lines: Vec<Line> = self
            .logs
            .iter()
            .rev()
            .take(h)
            .rev()
            .map(|(lv, s)| match lv {
                LogLevel::Info => Line::raw(s.clone()),
                LogLevel::Warn => Line::from(s.clone()).fg(Color::Yellow),
                LogLevel::Error => Line::from(s.clone()).fg(Color::Red),
            })
            .collect();
        f.render_widget(Paragraph::new(lines).block(Block::bordered().title(title.to_string())), area);
    }

    fn draw_done(&self, f: &mut Frame, area: Rect) {
        let mut lines = Vec::new();
        if let Some(e) = &self.error {
            lines.push(Line::from(tr!(format!("수집이 끝나지 못했습니다: {e}"), format!("Collection did not complete: {e}"))).fg(Color::Red).bold());
        }
        if let Some(s) = &self.summary {
            let (msg, color) = match s.status {
                SessionStatus::Completed => (tr!("수집을 마쳤습니다.", "Collection complete."), Color::Green),
                SessionStatus::Partial => (tr!("일부 작업이 끝나지 않았습니다.", "Some jobs did not complete."), Color::Yellow),
                SessionStatus::Cancelled => (tr!("수집을 중단했습니다.", "Collection cancelled."), Color::Yellow),
            };
            lines.push(Line::from(msg).fg(color).bold());
            lines.push(Line::raw(""));
            for j in &s.jobs {
                lines.push(Line::from(format!("{}  ({})", j.title, j.description)).bold());
                if j.status == JobStatus::Cancelled {
                    lines.push(Line::from(tr!("    중단됨: 아래 해시는 읽은 부분까지의 값이라 증거로 쓸 수 없습니다", "    Cancelled: hashes below cover only the data read and cannot be used as evidence")).fg(Color::Yellow));
                }
                if let Some(r) = &j.result {
                    for (alg, h) in &r.hashes {
                        lines.push(Line::raw(format!("    {:<8} {h}", alg.label())));
                    }
                    let v = match &r.verify {
                        Some(v) if v.matched => tr!("검증 일치", "Verification matched").to_string(),
                        Some(v) => v.error.clone().map(|e| tr!(format!("검증 실패: {e}"), format!("Verification failed: {e}"))).unwrap_or_else(|| tr!("검증 불일치", "Verification mismatch").into()),
                        None => tr!("검증 안 함", "Not verified").into(),
                    };
                    lines.push(Line::from(format!("    {v}{}", if r.bad_sectors > 0 { tr!(format!(", 읽기 오류 섹터 {}개 0으로 채움", r.bad_sectors), format!(", {} bad sectors zero-filled", r.bad_sectors)) } else { String::new() })).fg(if r.verify.as_ref().is_some_and(|v| v.matched) { Color::Green } else { Color::Yellow }));
                    let first = r.files.first().map(|f| f.name.clone()).unwrap_or_default();
                    lines.push(Line::raw(tr!(format!("    파일 {}개: {first}", r.files.len()), format!("    {} file(s): {first}", r.files.len()))).fg(Color::Gray));
                }
                if let Some(e) = &j.error {
                    lines.push(Line::from(tr!(format!("    오류: {e}"), format!("    Error: {e}"))).fg(Color::Red));
                }
            }
            lines.push(Line::raw(""));
            let sep = if s.evidence_dir.display().to_string().contains('\\') { "\\" } else { "/" };
            lines.push(Line::from(format!("{}{}{sep}{}", tr!("보고서: ", "Report: "), s.evidence_dir.display(), s.report_file)).fg(ACCENT));
        }
        f.render_widget(Paragraph::new(lines).block(Block::bordered().title(tr!(" 결과 ", " Result "))).wrap(Wrap { trim: false }), area);
    }
}

/// 터미널 화면 실행. 종료 코드를 돌려줌
pub fn run() -> i32 {
    if !std::io::IsTerminal::is_terminal(&io::stdout()) {
        eprintln!("{}", tr!("오류: 터미널에서 실행하세요 (출력이 터미널이 아님)", "error: run this in a terminal (output is not a terminal)"));
        return 2;
    }
    if enable_raw_mode().is_err() {
        eprintln!("{}", tr!("오류: 터미널을 화면 모드로 바꿀 수 없습니다", "error: cannot switch the terminal to screen mode"));
        return 1;
    }
    let mut out = io::stdout();
    let _ = execute!(out, EnterAlternateScreen);
    // 패닉이 나도 터미널을 원래대로 되돌림
    let hook = std::panic::take_hook();
    let main_id = std::thread::current().id();
    std::panic::set_hook(Box::new(move |p| {
        // 화면 스레드가 죽을 때만 터미널을 되돌림 (수집 스레드 패닉은 결과로 받아 화면에 표시)
        if std::thread::current().id() == main_id {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            hook(p);
        } else {
            // 다른 스레드 패닉은 화면을 깨지 않게 출력하지 않고, 내용, 위치를 남겨 결과 화면 오류에 넣음
            *LAST_PANIC.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.to_string());
        }
    }));
    // SSH가 끊기면(SIGHUP) 끝내지 않고 화면 없이 수집을 끝까지 마침 (로그, 요약, 보고서 정상 저장)
    #[cfg(unix)]
    // SAFETY: 원자 변수만 바꾸는 처리기 등록
    unsafe {
        // nohup 등으로 이미 무시하게 되어 있으면 그대로 둠
        let prev = libc::signal(libc::SIGHUP, on_hangup as extern "C" fn(libc::c_int) as libc::sighandler_t);
        if prev == libc::SIG_IGN {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }
    }
    let exit_code = |app: &App| match &app.summary {
        Some(s) if s.status == SessionStatus::Completed => 0,
        Some(_) => 3,
        None if app.error.is_some() => 1,
        // 수집 없이 끝남: 사용자가 끝냈으면 0, 터미널이 끊겨 끝났으면 129 (SIGHUP 관례)
        None if !terminal_alive() => 129,
        None => 0,
    };
    let mut lost_terminal = false;
    spawn_terminal_watch();
    let code = match Terminal::new(CrosstermBackend::new(io::stdout())) {
        Ok(mut term) => {
            let mut app = App::new();
            app.refresh_media();
            loop {
                app.pump();
                // 수집 결과를 화면이 받은 뒤(rx 없음)에만 갱신: 받기 전이면 수집 스레드가 기록한 값을 둠
                if app.rx.is_none() && !COLLECTING.load(Ordering::SeqCst) && (app.summary.is_some() || app.error.is_some()) {
                    IDLE_EXIT.store(exit_code(&app), Ordering::SeqCst);
                }
                // 끊김 판정은 터미널 생존 확인으로만 (그리기, 읽기 오류 한 번으로 화면을 영영 멈추지 않게)
                if !lost_terminal && !terminal_alive() {
                    lost_terminal = true;
                    HANGUP.store(true, Ordering::SeqCst);
                }
                if !lost_terminal {
                    let _ = term.draw(|f| app.draw(f));
                }
                if lost_terminal {
                    if app.rx.is_none() {
                        // 수집 중이 아니면 바로 끝냄
                        break exit_code(&app);
                    }
                    // 수집 스레드가 기록을 남기고 끝나면서 프로세스를 끝냄. 그동안 터미널을 건드리지 않고 쉼
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
                if app.quit && app.rx.is_none() {
                    break exit_code(&app);
                }
                match event::poll(Duration::from_millis(200)) {
                    Ok(true) => match event::read() {
                        Ok(Event::Key(k)) => {
                            app.key(k);
                            // 수집 중에는 끝낼 수 없음 (중단 후 결과 화면에서 끝냄)
                            if app.quit && app.rx.is_some() {
                                app.quit = false;
                                app.confirm_cancel = true;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    },
                    Ok(false) => {}
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        }
        Err(_) => 1,
    };
    // 터미널이 살아 있으면 늘 원래대로 되돌림
    if terminal_alive() {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
    code
}
