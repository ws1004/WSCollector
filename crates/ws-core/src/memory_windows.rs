//! Windows 물리 메모리 수집 (WinPmem 드라이버).
//!
//! 흔적 원칙 (CLAUDE.md 주의사항 5):
//! - 드라이버 파일은 도구 매체(USB)의 `drivers\winpmem_x64.sys`를 그대로 적재합니다. 대상 디스크에 복사하지 않습니다.
//! - 커널 서비스를 만들어 시작하고, 메모리 작업이 끝나면(실패, 중단 포함) 장치를 닫고 서비스를 중지, 삭제합니다.
//! - 만든 서비스, 삭제 결과, 드라이버 해시는 흔적 기록(보고서)에 남깁니다.
//! - 다른 도구가 이미 `\\.\pmem`을 적재해 두었으면 그것을 쓰고, 내리지 않습니다.
//!   (그 전에 이전 실행에서 남은 우리 서비스가 있으면 먼저 정리해서, 우리 것을 남의 것으로 오인하지 않음)
//!
//! 읽기는 WinPmem 공식 도구(go-winpmem)와 같습니다: PTE 모드만, 물리 주소 = 장치 위치, 읽기 실패는 페이지 단위 0 채우기(엔진).

use std::ffi::c_void;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_ANY};
use windows::Win32::System::Services::{
    CloseServiceHandle, ControlService, CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW, QueryServiceStatus, StartServiceW, SC_HANDLE,
    SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS, SERVICE_CONTROL_STOP, SERVICE_DEMAND_START, SERVICE_ERROR_NORMAL, SERVICE_KERNEL_DRIVER, SERVICE_STATUS,
    SERVICE_STOPPED,
};
use windows::Win32::System::IO::DeviceIoControl;

use super::{identity_ranges, lime_len, parse_pmem_info, parse_resource_list, LimeSource, MemoryInfo};
use crate::tr;

/// 만들 커널 서비스 이름 (WinPmem 기본 "winpmem"과 겹치지 않게)
const SERVICE: &str = "WSCollectorPmem";
const DEVICE: &str = r"\\.\pmem";
const DRIVER_FILE: &str = "winpmem_x64.sys";
/// CTL_CODE(0x22, 0x101/0x103, METHOD_NEITHER, FILE_READ_DATA|FILE_WRITE_DATA)
const IOCTL_SET_MODE: u32 = 0x0022_C407;
const IOCTL_GET_INFO: u32 = 0x0022_C40F;
/// 공식 도구가 쓰는 유일한 모드 ("가장 안정적")
const PMEM_MODE_PTE: u32 = 2;
const REG_KEY: &str = r"HARDWARE\RESOURCEMAP\System Resources\Physical Memory";

/// 이 실행에서 적재한 드라이버 (작업이 끝나면 release_driver가 내림)
#[derive(Clone, Copy)]
struct Loaded {
    /// 우리가 만든 서비스면 true (이미 있던 \\.\pmem을 빌려 쓴 경우 false: 내리지 않음)
    ours: bool,
}

static LOADED: Mutex<Option<Loaded>> = Mutex::new(None);
/// 보고서 흔적 기록에 넣을 문장 (서비스 생성, 삭제, 드라이버 해시)
static FOOTPRINT: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn note(s: String) {
    FOOTPRINT.lock().unwrap_or_else(|e| e.into_inner()).push(s);
}

fn set_loaded(v: Option<Loaded>) {
    *LOADED.lock().unwrap_or_else(|e| e.into_inner()) = v;
}

/// windows::core::Error → Win32 오류 번호 (HRESULT_FROM_WIN32 형태일 때만)
fn win32(e: &windows::core::Error) -> u32 {
    let h = e.code().0 as u32;
    if h & 0xFFFF_0000 == 0x8007_0000 {
        h & 0xFFFF
    } else {
        h
    }
}

/// 드라이버 파일 위치: 실행 파일 옆 `drivers\winpmem_x64.sys` (없으면 실행 파일 바로 옆)
fn driver_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let cands = [dir.join("drivers").join(DRIVER_FILE), dir.join(DRIVER_FILE)];
    cands.iter().find(|p| p.is_file()).cloned().ok_or_else(|| {
        tr!(
            format!("WinPmem 드라이버 파일이 없습니다: {} (도구 폴더의 drivers 폴더에 {DRIVER_FILE}를 두세요)", cands[0].display()),
            format!("WinPmem driver file not found: {} (put {DRIVER_FILE} in the drivers folder next to the tool)", cands[0].display())
        )
    })
}

/// 레지스트리의 물리 메모리 영역 (드라이버 없이)
fn registry_runs() -> Result<Vec<(u64, u64)>, String> {
    let k = wide(REG_KEY);
    let v = wide(".Translated");
    let err = |code: u32| tr!(format!("레지스트리에서 물리 메모리 영역을 읽지 못했습니다 (오류 {code})"), format!("Could not read physical memory ranges from the registry (error {code})"));
    let mut size = 0u32;
    // SAFETY: 크기만 물어봄
    let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, PCWSTR(k.as_ptr()), PCWSTR(v.as_ptr()), RRF_RT_ANY, None, None, Some(&mut size)) };
    if r.is_err() || size == 0 {
        return Err(err(r.0));
    }
    let mut buf = vec![0u8; size as usize + 64];
    let mut size = buf.len() as u32;
    // SAFETY: 버퍼 크기를 바이트 단위로 전달
    let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, PCWSTR(k.as_ptr()), PCWSTR(v.as_ptr()), RRF_RT_ANY, None, Some(buf.as_mut_ptr() as *mut c_void), Some(&mut size)) };
    if r.is_err() {
        return Err(err(r.0));
    }
    buf.truncate(size as usize);
    let runs = parse_resource_list(&buf);
    if runs.is_empty() {
        return Err(tr!("레지스트리의 물리 메모리 영역이 비어 있습니다", "The registry lists no physical memory ranges").into());
    }
    Ok(runs)
}

fn total(runs: &[(u64, u64)]) -> u64 {
    runs.iter().map(|(s, e)| e - s).sum()
}

fn info_from(runs: &[(u64, u64)], driver: String, notes: Vec<String>) -> MemoryInfo {
    let ranges = identity_ranges(runs);
    MemoryInfo { bytes: total(runs), image_bytes: lime_len(&ranges), source: format!("WinPmem ({DEVICE})"), ranges: ranges.len(), notes, driver: Some(driver) }
}

/// 수집 전 확인 (드라이버는 적재하지 않음): 권한, CPU, 드라이버 파일, 영역
pub fn memory_probe() -> Result<MemoryInfo, String> {
    if !cfg!(target_arch = "x86_64") {
        return Err(tr!("Windows 메모리 수집은 x64에서만 지원합니다", "Windows memory acquisition is supported on x64 only").into());
    }
    if !crate::platform::is_elevated() {
        return Err(tr!("관리자 권한이 필요합니다", "Administrator privileges are required").into());
    }
    let driver = driver_path()?;
    let runs = registry_runs()?;
    let n = tr!(
        format!("수집할 때 WinPmem 드라이버를 도구 매체에서 적재하고 끝나면 서비스를 삭제합니다 ({})", driver.display()),
        format!("The WinPmem driver is loaded from the tool media during acquisition and its service is deleted afterwards ({})", driver.display())
    );
    Ok(info_from(&runs, driver.display().to_string(), vec![n]))
}

// -- 서비스 ----------------------------------------------------------------

struct Sc(SC_HANDLE);
impl Drop for Sc {
    fn drop(&mut self) {
        // SAFETY: 유효한 서비스 핸들
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

fn scm() -> Result<Sc, String> {
    // SAFETY: 로컬 서비스 관리자 열기
    let h = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS) }
        .map_err(|e| tr!(format!("서비스 관리자를 열지 못했습니다: {e}"), format!("Could not open the service control manager: {e}")))?;
    Ok(Sc(h))
}

fn open_service(m: &Sc) -> Option<Sc> {
    let n = wide(SERVICE);
    // SAFETY: 유효한 관리자 핸들과 이름
    unsafe { OpenServiceW(m.0, PCWSTR(n.as_ptr()), SERVICE_ALL_ACCESS) }.ok().map(Sc)
}

/// 서비스 중지 → 멈출 때까지 최대 10초 기다림 → 삭제. (멈췄는지, 삭제 결과)
fn stop_and_delete(s: &Sc) -> (bool, Result<(), String>) {
    let mut st = SERVICE_STATUS::default();
    // SAFETY: 유효한 서비스 핸들과 상태 구조체. 이미 멈춘 서비스면 실패하므로 아래에서 상태를 다시 봄
    let _ = unsafe { ControlService(s.0, SERVICE_CONTROL_STOP, &mut st) };
    let t0 = std::time::Instant::now();
    loop {
        // SAFETY: 위와 같음
        if unsafe { QueryServiceStatus(s.0, &mut st) }.is_err() || st.dwCurrentState == SERVICE_STOPPED || t0.elapsed() > std::time::Duration::from_secs(10) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let stopped = st.dwCurrentState == SERVICE_STOPPED;
    // SAFETY: 유효한 서비스 핸들. 멈추지 않았어도 삭제 표시는 됨 (재부팅 때 지워짐)
    let del = unsafe { DeleteService(s.0) }.map_err(|e| e.to_string());
    (stopped, del)
}

/// 이전 실행이 강제 종료돼 남은 우리 서비스를 정리. 정리할 것이 없으면 None
fn cleanup_stale() -> Option<(bool, String)> {
    let m = scm().ok()?;
    let old = open_service(&m)?;
    let (stopped, del) = stop_and_delete(&old);
    drop(old);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let line = match (stopped, del) {
        (true, Ok(())) => (false, tr!(format!("이전 실행에서 남은 서비스 {SERVICE}를 내리고 지웠습니다"), format!("Unloaded and removed the service {SERVICE} left by a previous run"))),
        (false, Ok(())) => (
            true,
            tr!(
                format!("이전 실행에서 남은 서비스 {SERVICE}를 삭제 표시했지만 드라이버가 멈추지 않았습니다 (재부팅하면 지워짐)"),
                format!("Marked the service {SERVICE} left by a previous run for deletion, but the driver did not stop (removed on reboot)")
            ),
        ),
        (_, Err(e)) => (true, tr!(format!("이전 실행에서 남은 서비스 {SERVICE}를 지우지 못했습니다: {e}"), format!("Could not remove the service {SERVICE} left by a previous run: {e}"))),
    };
    note(line.1.clone());
    Some(line)
}

/// 드라이버 적재: 서비스 생성 → 시작. 실패하면 만든 서비스를 지우고, 정리 결과를 오류 문장에 합침
fn load(driver: &Path) -> Result<()> {
    let m = scm().map_err(|e| anyhow!(e))?;
    // NT 경로로 넘김: 경로에 공백이 있어도 해석이 흔들리지 않게
    let p = driver.display().to_string();
    let nt = if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
        format!(r"\??\UNC\{rest}")
    } else if let Some(rest) = p.strip_prefix(r"\\?\") {
        format!(r"\??\{rest}")
    } else if let Some(rest) = p.strip_prefix(r"\\") {
        // 네트워크 공유에서 실행: \\server\share\... → \??\UNC\server\share\...
        format!(r"\??\UNC\{rest}")
    } else {
        format!(r"\??\{p}")
    };
    let (n, bin) = (wide(SERVICE), wide(&nt));
    // SAFETY: 유효한 관리자 핸들, 널 끝 문자열
    let s = unsafe {
        CreateServiceW(
            m.0,
            PCWSTR(n.as_ptr()),
            PCWSTR(n.as_ptr()),
            SERVICE_ALL_ACCESS,
            SERVICE_KERNEL_DRIVER,
            SERVICE_DEMAND_START,
            SERVICE_ERROR_NORMAL,
            PCWSTR(bin.as_ptr()),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        )
    }
    .map_err(|e| {
        let hint = if win32(&e) == 1072 {
            tr!(" (같은 이름의 서비스가 삭제 대기 중입니다. 재부팅 후 다시 시도하세요)", " (a service with the same name is pending deletion; reboot and try again)")
        } else {
            ""
        };
        anyhow!(tr!(format!("드라이버 서비스를 만들지 못했습니다: {e}{hint}"), format!("Could not create the driver service: {e}{hint}")))
    })?;
    let s = Sc(s);
    note(tr!(
        format!("커널 서비스 생성: {SERVICE} (HKLM\\SYSTEM\\CurrentControlSet\\Services\\{SERVICE}). Windows 시스템 이벤트 로그에 서비스 설치(7045) 기록이 남습니다"),
        format!("Kernel service created: {SERVICE} (HKLM\\SYSTEM\\CurrentControlSet\\Services\\{SERVICE}). The Windows System event log records a service install (7045)")
    ));
    // SAFETY: 유효한 서비스 핸들
    if let Err(e) = unsafe { StartServiceW(s.0, None) } {
        let code = win32(&e);
        let why = match code {
            577 => tr!("드라이버 서명이 거부되었습니다 (서명 정책, Secure Boot)", "Driver signature was rejected (signing policy / Secure Boot)").to_string(),
            1275 => tr!("Windows가 이 드라이버를 차단했습니다 (메모리 무결성, 취약 드라이버 차단 목록, EDR)", "Windows blocked this driver (memory integrity / vulnerable driver blocklist / EDR)").to_string(),
            _ => e.to_string(),
        };
        let (_, del) = stop_and_delete(&s);
        let cleanup = match &del {
            Ok(()) => tr!(format!("서비스 {SERVICE}는 삭제했습니다"), format!("service {SERVICE} was deleted")),
            Err(d) => tr!(format!("서비스 {SERVICE} 삭제 실패: {d} (관리자 명령 프롬프트에서 sc delete {SERVICE})"), format!("service {SERVICE} could not be deleted: {d} (run sc delete {SERVICE} as administrator)")),
        };
        note(tr!(format!("드라이버 시작 실패 후 정리: {cleanup}"), format!("Cleanup after the driver failed to start: {cleanup}")));
        return Err(anyhow!(tr!(format!("WinPmem 드라이버를 시작하지 못했습니다 (오류 {code}): {why}. {cleanup}"), format!("Could not start the WinPmem driver (error {code}): {why}. {cleanup}"))));
    }
    Ok(())
}

fn open_device() -> std::io::Result<std::fs::File> {
    // IOCTL 권한(읽기, 쓰기 데이터) 때문에 쓰기 권한으로 엶. 메모리에 쓰는 기능(WRITE_ENABLE)은 쓰지 않음
    std::fs::OpenOptions::new().read(true).write(true).share_mode(0x1 | 0x2).open(DEVICE)
}

/// 장치가 없다는 오류 (파일, 경로 없음)인지. 그 밖(접근 거부 등)은 적재로 넘어가지 않고 그대로 알림
fn device_missing(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(2) | Some(3))
}

fn ioctl(f: &std::fs::File, code: u32, input: &[u8], out: &mut [u8]) -> windows::core::Result<u32> {
    let mut ret = 0u32;
    // SAFETY: 유효한 장치 핸들과 버퍼
    unsafe {
        DeviceIoControl(
            HANDLE(f.as_raw_handle()),
            code,
            if input.is_empty() { None } else { Some(input.as_ptr() as *const c_void) },
            input.len() as u32,
            if out.is_empty() { None } else { Some(out.as_mut_ptr() as *mut c_void) },
            out.len() as u32,
            Some(&mut ret),
            None,
        )
    }?;
    Ok(ret)
}

/// 드라이버 영역과 레지스트리 영역 중 쓸 것. 드라이버 값(MmGetPhysicalMemoryRanges)이 기준이지만,
/// 구조체 배치가 달라 엉뚱한 값을 읽었을 수도 있으므로 레지스트리와 크게 다르면 레지스트리를 씀
fn choose_runs(drv: Option<Vec<(u64, u64)>>, reg: &Result<Vec<(u64, u64)>, String>, notes: &mut Vec<String>) -> Result<Vec<(u64, u64)>, String> {
    match (drv, reg) {
        (Some(d), Ok(r)) => {
            if &d == r {
                return Ok(d);
            }
            // 두 목록 모두 normalize_runs로 정렬, 합침 → 마지막 끝 주소가 최대
            let (td, tr_) = (total(&d), total(r));
            let reg_end = r.last().map(|x| x.1).unwrap_or(0);
            // 합계가 2% 안이고 레지스트리 최대 주소를 넘지 않으면 드라이버 쪽 (핫플러그 등으로 조금 다를 수 있음)
            if td.abs_diff(tr_).saturating_mul(50) <= tr_ && d.last().map(|x| x.1).unwrap_or(0) <= reg_end {
                let msg = tr!(
                    format!("드라이버 메모리 영역({}개, {td}바이트)이 레지스트리({}개, {tr_}바이트)와 조금 달라 드라이버 쪽을 씁니다", d.len(), r.len()),
                    format!("Memory ranges from the driver ({}, {td} bytes) differ slightly from the registry ({}, {tr_} bytes); using the driver's", d.len(), r.len())
                );
                // 드라이버 쪽이 작으면 빠진 영역이 있을 수 있으므로 경고로
                notes.push(if td < tr_ { format!("{} {msg}", super::COVERAGE_WARN) } else { msg });
                Ok(d)
            } else {
                notes.push(format!(
                    "{} {}",
                    super::COVERAGE_WARN,
                    tr!(
                        format!("드라이버 메모리 영역이 레지스트리와 크게 달라(합계 {td} / {tr_}바이트) 레지스트리 영역을 씁니다"),
                        format!("Memory ranges from the driver differ greatly from the registry (total {td} / {tr_} bytes); using the registry's")
                    )
                ));
                Ok(r.clone())
            }
        }
        (Some(d), Err(e)) => {
            notes.push(tr!(format!("레지스트리 영역을 읽지 못해({e}) 드라이버 영역을 대조 없이 씁니다"), format!("Could not read registry ranges ({e}); using the driver's ranges without cross-checking")));
            Ok(d)
        }
        (None, Ok(r)) => {
            notes.push(tr!("드라이버에서 메모리 영역을 받지 못해 레지스트리 영역을 씁니다", "Could not get memory ranges from the driver; using the registry's").into());
            Ok(r.clone())
        }
        (None, Err(e)) => Err(e.clone()),
    }
}

/// 실패 경로: 장치를 닫고 드라이버를 내린 뒤, 정리 결과를 오류 문장에 붙임
fn fail_with(file: Option<std::fs::File>, e: anyhow::Error) -> anyhow::Error {
    drop(file);
    let lines: Vec<String> = release_driver().into_iter().map(|(_, l)| l).collect();
    if lines.is_empty() {
        e
    } else {
        anyhow!("{e:#}. {}", lines.join(". "))
    }
}

/// 드라이버를 적재하고(필요하면) `\\.\pmem`을 LiME 원본으로 엶. 끝나면 반드시 `release_driver`
pub fn open_memory() -> Result<(LimeSource, MemoryInfo)> {
    if !cfg!(target_arch = "x86_64") {
        return Err(anyhow!(tr!("Windows 메모리 수집은 x64에서만 지원합니다", "Windows memory acquisition is supported on x64 only")));
    }
    if !crate::platform::is_elevated() {
        return Err(anyhow!(tr!("관리자 권한이 필요합니다", "Administrator privileges are required")));
    }
    let driver = driver_path().map_err(|e| anyhow!(e))?;
    let mut notes = Vec::new();
    // 이전 실행에서 남은 우리 서비스부터 정리 (남아 있으면 \\.\pmem이 열려 남의 것으로 오인하게 됨)
    let mut stale_stuck = false;
    if let Some((warn, line)) = cleanup_stale() {
        stale_stuck = warn;
        notes.push(line);
    }
    let reg = registry_runs();
    let (file, driver_label) = match open_device() {
        // 남은 우리 드라이버를 내리지 못함: 삭제 표시는 되어 있으므로 그대로 쓰고(재부팅 때 지워짐) 그렇게 기록
        Ok(f) if stale_stuck => {
            let n = tr!(
                format!("이전 실행에서 남은 WS Collector 드라이버({DEVICE})를 내리지 못해 그대로 씁니다. 서비스는 삭제 표시되어 재부팅하면 지워집니다"),
                format!("Could not unload the WS Collector driver ({DEVICE}) left by a previous run; using it as is. Its service is marked for deletion and will be removed on reboot")
            );
            note(n.clone());
            notes.push(n);
            set_loaded(Some(Loaded { ours: false }));
            (f, tr!("이전 실행에서 남은 WS Collector WinPmem (재부팅 때 지워짐)", "WS Collector WinPmem left by a previous run (removed on reboot)").to_string())
        }
        // 다른 도구가 적재해 둔 드라이버: 그대로 쓰고 내리지 않음
        Ok(f) => {
            let n = tr!(
                format!("{DEVICE} 장치가 이미 있습니다 (다른 도구가 적재한 WinPmem). 그대로 쓰고 내리지 않습니다. 이 장치의 읽기 모드를 PTE로 바꿉니다"),
                format!("{DEVICE} already exists (WinPmem loaded by another tool). Using it as is without unloading; its read mode is set to PTE")
            );
            note(n.clone());
            notes.push(n);
            set_loaded(Some(Loaded { ours: false }));
            (f, tr!("다른 도구가 적재한 WinPmem", "WinPmem loaded by another tool").to_string())
        }
        Err(e) if !device_missing(&e) => {
            return Err(anyhow!(tr!(format!("{DEVICE} 장치를 확인하지 못했습니다: {e}"), format!("Could not check {DEVICE}: {e}"))));
        }
        Err(_) => {
            load(&driver)?;
            set_loaded(Some(Loaded { ours: true }));
            // 적재한 뒤 해시 (보고서에는 실제로 적재에 성공한 파일만)
            let hash = crate::hash::digest_file(&driver, crate::hash::HashAlg::Sha256).unwrap_or_else(|e| tr!(format!("계산 실패: {e}"), format!("calculation failed: {e}")));
            note(tr!(format!("적재한 드라이버: {} (SHA-256 {hash})", driver.display()), format!("Driver loaded: {} (SHA-256 {hash})", driver.display())));
            match open_device() {
                Ok(f) => (f, driver.display().to_string()),
                Err(e) => return Err(fail_with(None, anyhow!(tr!(format!("드라이버는 시작했지만 {DEVICE}를 열지 못했습니다: {e}"), format!("The driver started but {DEVICE} could not be opened: {e}"))))),
            }
        }
    };
    // 공식 도구와 같은 PTE 모드만
    if let Err(e) = ioctl(&file, IOCTL_SET_MODE, &PMEM_MODE_PTE.to_le_bytes(), &mut []) {
        return Err(fail_with(Some(file), anyhow!(tr!(format!("WinPmem 읽기 모드(PTE)를 설정하지 못했습니다: {e}"), format!("Could not set the WinPmem read mode (PTE): {e}")))));
    }
    let mut out = vec![0u8; 64 * 1024];
    let drv = ioctl(&file, IOCTL_GET_INFO, &[], &mut out).ok().and_then(|_| parse_pmem_info(&out));
    let runs = match choose_runs(drv, &reg, &mut notes) {
        Ok(r) => r,
        Err(e) => return Err(fail_with(Some(file), anyhow!(e))),
    };
    notes.push(tr!("WinPmem 읽기 모드: PTE", "WinPmem read mode: PTE").into());
    let info = info_from(&runs, driver_label, notes);
    Ok((LimeSource::new(file, identity_ranges(&runs)), info))
}

/// 메모리 작업 뒤 드라이버 내리기 (장치 핸들을 먼저 닫은 뒤 부름). 여러 번 불러도 됨:
/// 서비스를 열지 못해 시도조차 못 했으면 상태를 되돌려 다음 호출(세션 마무리, Drop 안전장치)에서 다시 시도.
/// 돌려주는 값: (경고인지, 로그 문장)
pub fn release_driver() -> Vec<(bool, String)> {
    let mut guard = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    let Some(l) = *guard else { return Vec::new() };
    if !l.ours {
        *guard = None;
        return Vec::new();
    }
    let m = match scm() {
        Ok(m) => m,
        // 관리자를 못 열면 시도조차 못 함: 상태를 남겨 다음 호출에서 다시
        Err(e) => {
            let line = (true, tr!(format!("서비스 {SERVICE}를 내리지 못했습니다: {e}. 나중에 다시 시도합니다"), format!("Could not unload the service {SERVICE}: {e}. Will retry later")));
            note(line.1.clone());
            return vec![line];
        }
    };
    let Some(s) = open_service(&m) else {
        // 서비스가 이미 없음 (밖에서 지움 등): 할 일이 없으므로 상태를 지움
        *guard = None;
        let line = (false, tr!(format!("서비스 {SERVICE}가 이미 없습니다 (정리할 것 없음)"), format!("Service {SERVICE} no longer exists (nothing to clean up)")));
        note(line.1.clone());
        return vec![line];
    };
    let attempt: Result<(bool, Result<(), String>), String> = Ok(stop_and_delete(&s));
    let line = match attempt {
        Ok((true, Ok(()))) => {
            *guard = None;
            (false, tr!(format!("WinPmem 드라이버를 내리고 서비스 {SERVICE}를 삭제했습니다"), format!("Unloaded the WinPmem driver and deleted the service {SERVICE}")))
        }
        Ok((false, Ok(()))) => {
            // 삭제 표시는 됨: 더 할 수 있는 것이 없음 (재부팅 때 지워짐)
            *guard = None;
            (
                true,
                tr!(
                    format!("서비스 {SERVICE}를 삭제 표시했지만 드라이버가 멈추지 않았습니다. 재부팅하면 지워집니다"),
                    format!("Service {SERVICE} was marked for deletion but the driver did not stop. It will be removed on reboot")
                ),
            )
        }
        // 서비스는 열었지만 삭제 실패: 다시 시도해도 같을 가능성이 크지만 상태는 남겨 둠
        Ok((_, Err(e))) | Err(e) => (
            true,
            tr!(
                format!("서비스 {SERVICE}를 삭제하지 못했습니다: {e}. 관리자 명령 프롬프트에서 sc stop {SERVICE} 후 sc delete {SERVICE}를 실행하세요"),
                format!("Could not delete the service {SERVICE}: {e}. Run sc stop {SERVICE} then sc delete {SERVICE} as administrator")
            ),
        ),
    };
    note(line.1.clone());
    vec![line]
}

/// 보고서 흔적 기록용 문장을 꺼냄 (수집 세션 끝에서 한 번). 같은 문장이 여러 번 쌓였으면 하나만
pub fn take_footprint() -> Vec<String> {
    let mut v = std::mem::take(&mut *FOOTPRINT.lock().unwrap_or_else(|e| e.into_inner()));
    let mut seen = std::collections::HashSet::new();
    v.retain(|s| seen.insert(s.clone()));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    /// 이 PC 레지스트리의 영역 합계 = Windows가 아는 물리 메모리 (해석 배치가 맞는지 실제 데이터로 확인)
    #[test]
    fn registry_runs_match_installed_memory() {
        let runs = registry_runs().expect("레지스트리 영역");
        let t = total(&runs);
        let mut ms = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        // SAFETY: 크기를 채운 구조체
        unsafe { GlobalMemoryStatusEx(&mut ms) }.expect("GlobalMemoryStatusEx");
        let phys = ms.ullTotalPhys;
        // ullTotalPhys는 OS가 쓰는 메모리(일부 예약 제외)라 영역 합계와 몇 % 안에서 같아야 함
        let diff = t.abs_diff(phys) as f64 / phys as f64;
        assert!(diff < 0.05, "영역 합계 {t} vs 물리 메모리 {phys} ({:.2}% 차이), 영역 {runs:x?}", diff * 100.0);
        assert!(runs.windows(2).all(|w| w[0].1 < w[1].0), "{runs:x?}");
    }

    #[test]
    fn choose_runs_prefers_driver_only_when_close() {
        let reg: Result<Vec<(u64, u64)>, String> = Ok(vec![(0x1000, 0x9f000), (0x100000, 0x4000_0000)]);
        let mut n = Vec::new();
        // 같으면 그대로
        assert_eq!(choose_runs(Some(reg.clone().unwrap()), &reg, &mut n).unwrap(), reg.clone().unwrap());
        // 조금 다름 (한 페이지): 드라이버
        let d = vec![(0x1000, 0x9f000), (0x100000, 0x3fff_f000)];
        assert_eq!(choose_runs(Some(d.clone()), &reg, &mut n).unwrap(), d);
        // 크게 다름 (배치 어긋남 등): 레지스트리 + 경고
        let bad = vec![(0x1000, 0x2000)];
        assert_eq!(choose_runs(Some(bad), &reg, &mut n).unwrap(), reg.clone().unwrap());
        assert!(n.last().unwrap().starts_with(super::super::COVERAGE_WARN));
        // 레지스트리 범위를 넘는 주소: 레지스트리
        let beyond = vec![(0x1000, 0x9f000), (0x100000, 0x4000_0000), (0x10_0000_0000, 0x10_0000_1000)];
        assert_eq!(choose_runs(Some(beyond), &reg, &mut n).unwrap(), reg.clone().unwrap());
        // 드라이버 없음: 레지스트리
        assert_eq!(choose_runs(None, &reg, &mut n).unwrap(), reg.unwrap());
    }
}
