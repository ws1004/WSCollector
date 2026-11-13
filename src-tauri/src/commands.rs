//! 화면에서 호출하는 커맨드. 수집 로직은 모두 ws-core에 있고, 여기서는 연결만 합니다.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use ws_core::collect::{preflight, run_collection, CollectEvent, CollectObserver, CollectRequest, CollectSummary};
use ws_core::inventory::{check_output_dir, disks_with_roles, OutputCheck};
use ws_core::model::{DiskInfo, DiskRole, HostInfo};
use ws_core::net::{serve, ReceiveControl, ReceiveOptions, ReceiveOutcome, RecvEvent, STOPPED_MSG};
use ws_core::platform;

#[derive(Default)]
pub struct Session {
    /// 진행 중인 수집의 중단 플래그. Some이면 수집 중 (동시 시작 방지 슬롯)
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// 진행 중인 네트워크 수신. Some이면 수신 중 (수집과 동시에 하지 않음)
    receive: Mutex<Option<Arc<ReceiveControl>>>,
    /// 탐색기로 열 수 있는 유일한 위치: 마지막 수집의 증거 폴더
    evidence_dir: Mutex<Option<PathBuf>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    id: &'static str,
    title: &'static str,
    detail: String,
    /// pass | warn | fail
    status: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    version: &'static str,
    /// "windows" | "linux" (화면이 경로 형식, 대상 지정 방식을 고름)
    os: &'static str,
    host: HostInfo,
    /// 메모리 수집 가능 여부와 원본 (Linux). 수집할 수 없으면 null
    memory: Option<ws_core::memory::MemoryInfo>,
    /// 메모리를 수집할 수 없는 이유 (Linux). 화면에 그대로 보여 줌
    memory_error: Option<String>,
    /// 이 PC의 IPv4 주소들 (네트워크 수신 "받을 네트워크" 선택)
    local_ips: Vec<String>,
    elevated: bool,
    exe_dir: String,
    tool_media: Option<String>,
    checks: Vec<Check>,
    /// 개발 빌드 시험 모드: 디스크 앞부분만 읽는 한도 (바이트, 문자열)
    test_max_bytes: Option<String>,
}

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

/// 모든 디스크를 조회하므로 메인 스레드를 막지 않게 백그라운드에서 실행
#[tauri::command]
pub async fn get_environment() -> Result<Environment, String> {
    tauri::async_runtime::spawn_blocking(environment).await.map_err(|e| e.to_string())
}

fn environment() -> Environment {
    let host = platform::host_info();
    let elevated = platform::is_elevated();
    let exe_dir = crate::runtime::base_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let disks = disks_with_roles(None).unwrap_or_default();
    let tool = disks.iter().find(|d| d.has_role(DiskRole::Tool));
    let tool_on_system = tool.is_some_and(|d| d.has_role(DiskRole::System));
    let tool_media = tool.map(|d| format!("{}, {}", d.model, d.kind_label()));

    let rt = crate::runtime::base_dir().map(|p| p.join("runtime"));
    #[cfg(windows)]
    let (wv, tmp) = (std::env::var("WEBVIEW2_USER_DATA_FOLDER").unwrap_or_default(), std::env::var("TEMP").unwrap_or_default());
    #[cfg(not(windows))]
    let (wv, tmp) = (std::env::var("XDG_DATA_HOME").unwrap_or_default(), std::env::var("TMPDIR").unwrap_or_default());
    let fixed = std::env::var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER").is_ok();
    let engine = if cfg!(windows) { if fixed { "WebView2 휴대용 런타임" } else { "WebView2 시스템 런타임" } } else { "WebKitGTK" };
    let isolated = |p: &str| rt.as_ref().is_some_and(|r| !p.is_empty() && PathBuf::from(p).starts_with(r));

    let mut checks = vec![
        Check {
            id: "admin",
            title: if cfg!(windows) { "관리자 권한" } else { "root 권한" },
            detail: if elevated {
                "디스크와 메모리에 직접 접근할 수 있습니다".into()
            } else if cfg!(windows) {
                "일반 사용자 권한입니다. 디스크를 읽을 수 없습니다".into()
            } else {
                "일반 사용자 권한입니다. 디스크, 메모리를 읽으려면 sudo로 실행하세요".into()
            },
            status: if elevated { "pass" } else { "fail" },
        },
        Check {
            id: "exe",
            title: "실행 위치",
            detail: format!("{}{}", exe_dir, tool_media.as_ref().map(|m| format!(" ({m})")).unwrap_or_default()),
            status: if tool_on_system { "warn" } else { "pass" },
        },
        Check {
            id: "webview",
            title: "화면 엔진 격리",
            detail: if isolated(&wv) {
                format!("{engine}, 데이터, 캐시 폴더를 실행 위치에 둠")
            } else {
                format!("{engine} 데이터 폴더가 대상 PC의 사용자 폴더에 생깁니다")
            },
            status: if isolated(&wv) { "pass" } else { "warn" },
        },
        Check {
            id: "temp",
            title: "임시 폴더",
            detail: tmp.clone(),
            status: if isolated(&tmp) { "pass" } else { "warn" },
        },
    ];
    if tool_on_system {
        checks[1].detail = format!("{} 대상 PC의 시스템 디스크에서 실행 중입니다", checks[1].detail);
    }
    let memory = ws_core::memory::memory_probe();
    Environment {
        version: ws_core::VERSION,
        os: if cfg!(windows) { "windows" } else { "linux" },
        host,
        memory: memory.as_ref().ok().cloned(),
        memory_error: memory.err(),
        local_ips: ws_core::net::local_ips(),
        elevated,
        exe_dir,
        tool_media,
        checks,
        test_max_bytes: test_max_bytes().map(|v| v.to_string()),
    }
}

#[tauri::command]
pub async fn list_disks(out_dir: Option<String>) -> Result<Vec<DiskInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        disks_with_roles(out_dir.as_deref().filter(|s| !s.is_empty()).map(std::path::Path::new)).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn check_output(path: String) -> Result<OutputCheck, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = PathBuf::from(path.trim());
        let disks = disks_with_roles(Some(&p)).unwrap_or_default();
        check_output_dir(&p, &disks)
    })
    .await
    .map_err(|e| e.to_string())
}

/// 수집 진행 이벤트를 화면으로 보냅니다 ("collect" 이벤트)
struct Emitting(AppHandle);
impl CollectObserver for Emitting {
    fn event(&mut self, e: CollectEvent) {
        let _ = self.0.emit("collect", &e);
    }
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Finish {
    Done { summary: CollectSummary },
    Failed { message: String },
}

/// 검사 후 백그라운드에서 수집을 시작합니다. 진행은 "collect", 끝은 "collect-finish" 이벤트로 알립니다.
#[tauri::command]
pub async fn start_collection(app: AppHandle, session: State<'_, Session>, mut request: CollectRequest) -> Result<(), String> {
    request.interface = "GUI".into();
    // 개발 빌드 전용 시험 옵션: 디스크 앞부분만 읽기 (릴리스에서는 항상 None)
    request.max_bytes = test_max_bytes();

    // 슬롯을 먼저 잡아서 두 번 눌러도 하나만 실행 (검사 중에도 잡혀 있음).
    // 수집, 수신 슬롯은 항상 같은 순서(cancel → receive)로 함께 잡아, 검사와 설정 사이에 다른 쪽이 끼어들지 못하게 함
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut cur = session.cancel.lock().unwrap();
        let rx = session.receive.lock().unwrap();
        if rx.is_some() {
            return Err("네트워크 수신 중에는 수집할 수 없습니다. 수신을 먼저 끝내세요".into());
        }
        if cur.is_some() {
            return Err("이미 수집이 진행 중입니다".into());
        }
        *cur = Some(cancel.clone());
    }
    let release = |s: &Session| *s.cancel.lock().unwrap() = None;

    let req2 = request.clone();
    let checked = tauri::async_runtime::spawn_blocking(move || preflight(&req2).map(|(_, dir)| dir)).await;
    let dir = match checked {
        Ok(Ok(dir)) => dir,
        Ok(Err(e)) => {
            release(&session);
            return Err(format!("{e:#}"));
        }
        Err(e) => {
            release(&session);
            return Err(e.to_string());
        }
    };
    // 네트워크 전송이면 증거 폴더가 이 PC에 없음 (빈 경로를 허용 목록에 넣으면 모든 경로가 열림)
    *session.evidence_dir.lock().unwrap() = (!dir.as_os_str().is_empty()).then_some(dir);

    let handle = app.clone();
    let spawned = std::thread::Builder::new().name("collect".into()).spawn(move || {
        let mut obs = Emitting(handle.clone());
        // 엔진에서 패닉이 나도 화면이 멈추지 않게 잡아서 실패로 알림
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_collection(&request, &cancel, &mut obs)));
        let finish = match r {
            Ok(Ok(summary)) => Finish::Done { summary },
            Ok(Err(e)) => Finish::Failed { message: format!("{e:#}") },
            Err(p) => {
                let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
                Finish::Failed { message: format!("내부 오류로 수집이 멈췄습니다: {msg}") }
            }
        };
        use tauri::Manager;
        if let Some(s) = handle.try_state::<Session>() {
            *s.cancel.lock().unwrap() = None;
        }
        let _ = handle.emit("collect-finish", &finish);
    });
    if let Err(e) = spawned {
        release(&session);
        return Err(e.to_string());
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_collection(session: State<'_, Session>) -> bool {
    match session.cancel.lock().unwrap().as_ref() {
        Some(c) => {
            c.store(true, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

/// 탐색기로 증거 폴더를 열거나 그 안의 파일을 선택해서 보여줍니다.
/// - 마지막 수집의 증거 폴더 안쪽 경로만 허용 (네트워크 경로, 임의 경로 차단)
/// - 탐색기 사용은 대상 PC에 ShellBags 등 기록을 남기므로 수집자가 직접 누를 때만 실행
#[tauri::command]
pub fn reveal_path(session: State<'_, Session>, path: String) -> Result<(), String> {
    let Some(root) = session.evidence_dir.lock().unwrap().clone() else {
        return Err("열 수 있는 증거 폴더가 없습니다".into());
    };
    let p = PathBuf::from(&path);
    if path.starts_with(r"\\") || !p.is_absolute() || !p.starts_with(&root) || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err("증거 폴더 밖의 경로는 열 수 없습니다".into());
    }
    if !p.exists() {
        return Err(format!("경로가 없습니다: {path}"));
    }
    open_in_file_manager(&p)
}

#[cfg(windows)]
fn open_in_file_manager(p: &std::path::Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("explorer.exe");
    if p.is_file() {
        // 경로에 쉼표, 공백이 있어도 깨지지 않게 직접 따옴표 처리
        cmd.raw_arg(format!("/select,\"{}\"", p.display()));
    } else {
        cmd.raw_arg(format!("\"{}\"", p.display()));
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Linux: 파일이면 그 파일을 기본 프로그램으로(보고서 → 브라우저), 폴더면 파일 관리자로 엶
#[cfg(not(windows))]
fn open_in_file_manager(p: &std::path::Path) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(p)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("xdg-open을 실행할 수 없습니다 ({e}). 경로: {}", p.display()))
}

// -- 네트워크 수신 ---------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum ReceiveFinish {
    Done { outcome: ReceiveOutcome },
    Stopped,
    Failed { message: String },
}

/// 이 PC를 수신 PC로: 연결 코드를 만들고 접속을 기다립니다.
/// 진행은 "receive" 이벤트(RecvEvent), 끝은 "receive-finish" 이벤트로 알립니다.
#[tauri::command]
pub async fn start_receive(app: AppHandle, session: State<'_, Session>, out_base: String, port: u16, bind: Option<String>) -> Result<(), String> {
    let base = PathBuf::from(out_base.trim());
    if port == 0 {
        return Err("포트 번호를 확인하세요 (1~65535)".into());
    }
    // 받을 네트워크: 비우면 모든 주소. 이 PC의 주소만 허용 (화면 목록에서 고름)
    let bind = match bind.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        None => "0.0.0.0".to_string(),
        Some(b) if tauri::async_runtime::spawn_blocking(ws_core::net::local_ips).await.unwrap_or_default().iter().any(|ip| ip == b) => b.to_string(),
        Some(b) => return Err(format!("이 PC의 주소가 아닙니다: {b} (주소가 바뀌었을 수 있음). '모든 네트워크'를 고르거나 프로그램을 다시 시작하세요")),
    };
    // 화면 검사와 별도로 백엔드에서도 확인: 상대 경로(실행 위치 기준으로 풀림), 네트워크 경로 금지
    if !base.is_absolute() || out_base.trim().starts_with(r"\\") {
        return Err("드라이브 문자로 시작하는 전체 경로를 입력하세요. 예: D:\\Cases".into());
    }
    let ctrl = Arc::new(ReceiveControl::default());
    {
        // start_collection과 같은 순서(cancel → receive)로 함께 잡음
        let col = session.cancel.lock().unwrap();
        let mut cur = session.receive.lock().unwrap();
        if col.is_some() {
            return Err("수집 중에는 수신할 수 없습니다".into());
        }
        if cur.is_some() {
            return Err("이미 수신 중입니다".into());
        }
        *cur = Some(ctrl.clone());
    }
    let release = |s: &Session| *s.receive.lock().unwrap() = None;
    let b2 = base.clone();
    let checked = tauri::async_runtime::spawn_blocking(move || {
        let disks = disks_with_roles(Some(&b2)).unwrap_or_default();
        check_output_dir(&b2, &disks)
    })
    .await;
    match checked {
        Ok(oc) if oc.ok => {}
        Ok(oc) => {
            release(&session);
            return Err(oc.message);
        }
        Err(e) => {
            release(&session);
            return Err(e.to_string());
        }
    }

    let handle = app.clone();
    let spawned = std::thread::Builder::new().name("receive".into()).spawn(move || {
        use tauri::Manager;
        let opts = ReceiveOptions { listen: format!("{bind}:{port}"), out_base: base, code: None, resume_wait: ws_core::net::DEFAULT_RESUME_WAIT };
        let h2 = handle.clone();
        let mut on_event = |e: RecvEvent| {
            // 받은 증거 폴더는 탐색기로 열 수 있게 허용 목록에 넣음
            if let RecvEvent::Accepted { evidence_dir, .. } = &e {
                if let Some(s) = h2.try_state::<Session>() {
                    *s.evidence_dir.lock().unwrap() = Some(PathBuf::from(evidence_dir));
                }
            }
            let _ = h2.emit("receive", &e);
        };
        let space = ws_core::collect::receiver_space;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(&opts, &space, &mut on_event, &ctrl)));
        let finish = match r {
            Ok(Ok(outcome)) => ReceiveFinish::Done { outcome },
            Ok(Err(e)) if ctrl.stopped() && e.to_string() == STOPPED_MSG => ReceiveFinish::Stopped,
            Ok(Err(e)) => ReceiveFinish::Failed { message: format!("{e:#}") },
            Err(_) => ReceiveFinish::Failed { message: "내부 오류로 수신이 멈췄습니다".into() },
        };
        if let Some(s) = handle.try_state::<Session>() {
            *s.receive.lock().unwrap() = None;
        }
        let _ = handle.emit("receive-finish", &finish);
    });
    if let Err(e) = spawned {
        release(&session);
        return Err(e.to_string());
    }
    Ok(())
}

#[tauri::command]
pub fn stop_receive(session: State<'_, Session>) -> bool {
    match session.receive.lock().unwrap().as_ref() {
        Some(c) => {
            c.stop();
            true
        }
        None => false,
    }
}

/// 수집, 수신 중인지 (창 닫기 막기용)
pub fn busy(app: &AppHandle) -> Option<&'static str> {
    use tauri::Manager;
    let s = app.try_state::<Session>()?;
    if s.cancel.lock().unwrap().is_some() {
        return Some("collect");
    }
    if s.receive.lock().unwrap().is_some() {
        return Some("receive");
    }
    None
}
