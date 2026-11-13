//! 수집 세션: GUI와 CLI가 똑같이 이 함수를 호출합니다.
//!
//! 1. 사건 정보, 저장 폴더, 대상 검사
//! 2. `저장 폴더/사건 번호/증거 번호` 생성 (이미 파일이 있으면 중단: 증거 섞임 방지)
//! 3. collection.log 기록 시작 (도구 해시, 호스트, 매체 정보)
//! 4. 작업을 순서대로 실행 (실패한 작업이 있어도 나머지는 계속)
//! 5. 흔적 기록, `_summary.json`, `_report.html` 저장

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::case::CaseInfo;
use crate::hash::{digest_file, HashAlg};
use crate::e01::{Compression, E01Meta};
use crate::imaging::{run_image_to, ImageFormat, ImageResult, ImageSpec, Observer, Progress};
use crate::model::VolumeInfo;
use crate::net::{Hello, RemoteStore};
use crate::store::{EvidenceStore, LocalStore};
use crate::inventory::{check_output_dir, disks_with_roles};
use crate::logbook::{LogBook, LogEntry, LogLevel};
use crate::model::{DiskInfo, DiskRole, HostInfo, ProcInfo};
use crate::platform;
use crate::target::Target;
use crate::tr;
use crate::util::{fmt_ts, format_bytes, now_local};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectRequest {
    pub case: CaseInfo,
    pub out_base: PathBuf,
    pub targets: Vec<Target>,
    /// RAW: 분할 크기, E01: 조각 크기. 0이면 나누지 않음 (E01은 FAT32 저장 매체면 자동으로 나눔)
    pub split_bytes: u64,
    /// 디스크 저장 형식 (기본 E01). 메모리는 항상 LiME(RAW)
    #[serde(default = "default_format")]
    pub format: ImageFormat,
    #[serde(default)]
    pub compression: Compression,
    pub algs: Vec<HashAlg>,
    pub verify: bool,
    pub zero_fill_errors: bool,
    /// "GUI" 또는 "CLI" (로그, 보고서 기록용)
    pub interface: String,
    #[serde(default)]
    pub max_bytes: Option<u64>,
    /// 네트워크 전송: 있으면 `out_base` 대신 수신 PC에 저장
    #[serde(default)]
    pub remote: Option<RemoteTarget>,
}

fn default_format() -> ImageFormat {
    ImageFormat::E01
}

/// FAT32에 실제로 쓸 분할 크기 (RAW 2GB, E01 4000MiB). 사용자 값이 FAT32 한계 안이면 그대로
fn fat_split(format: ImageFormat, split: u64) -> u64 {
    match format {
        ImageFormat::Raw if split == 0 || split > u32::MAX as u64 => FAT_RAW_SPLIT,
        ImageFormat::E01 if split == 0 || split > crate::e01::FAT32_SEGMENT => crate::e01::FAT32_SEGMENT,
        _ => split,
    }
}

/// 저장 공간 검사에 쓰는 분할 크기: FAT32면 엔진이 알아서 나누므로(위) 그 값으로 검사
fn space_split(req: &CollectRequest) -> u64 {
    fat_split(req.format, req.split_bytes)
}

/// RAW 메모리 덤프를 FAT32에 저장할 때 쓰는 분할 크기
const FAT_RAW_SPLIT: u64 = 2 << 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTarget {
    /// "192.168.0.10:7700" (포트 생략 시 7700)
    pub addr: String,
    pub code: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Done,
    Failed,
    Cancelled,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOutcome {
    pub index: usize,
    pub target: Target,
    pub title: String,
    pub description: String,
    pub status: JobStatus,
    pub result: Option<ImageResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Footprint {
    pub tool_path: String,
    pub tool_sha256: String,
    pub tool_media: String,
    pub output_media: String,
    /// 저장 위치가 이 PC의 시스템 디스크 (라이브 수집이면 대상 디스크에 쓴 것)
    #[serde(default)]
    pub output_on_system_disk: bool,
    pub processes: Vec<ProcInfo>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Completed,
    Partial,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectSummary {
    pub tool_version: String,
    pub interface: String,
    pub case: CaseInfo,
    pub host: HostInfo,
    pub evidence_dir: PathBuf,
    pub file_prefix: String,
    pub started: String,
    pub finished: String,
    pub status: SessionStatus,
    pub jobs: Vec<JobOutcome>,
    pub footprint: Footprint,
    pub log_file: String,
    pub report_file: String,
    pub summary_file: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CollectEvent {
    #[serde(rename_all = "camelCase")]
    Prepared { evidence_dir: String, jobs: Vec<JobPlan> },
    #[serde(rename_all = "camelCase")]
    JobStarted { index: usize },
    #[serde(rename_all = "camelCase")]
    Progress { index: usize, progress: Progress },
    #[serde(rename_all = "camelCase")]
    JobFinished { index: usize, outcome: JobOutcome },
    Log(LogEntry),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobPlan {
    pub title: String,
    pub description: String,
    #[serde(with = "crate::model::u64_str")]
    pub size: u64,
    pub file_base: String,
}

pub trait CollectObserver {
    fn event(&mut self, e: CollectEvent);
    /// 화면 쪽에서 생긴 일(예: SSH 끊김)을 수집 로그에 경고로 남기려면 돌려줌. 진행 알림 때마다 확인
    fn take_notes(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// 대상 하나를 사람이 읽는 제목, 설명, 크기로
fn describe(t: &Target, disks: &[DiskInfo]) -> Result<(String, String, u64)> {
    Ok(match t {
        Target::Disk { number, .. } => {
            let d = disks.iter().find(|d| d.number == *number).ok_or_else(|| anyhow!(tr!(format!("Disk {number}을(를) 찾지 못했습니다"), format!("Disk {number} not found"))))?;
            (format!("Disk {}", d.number), format!("{}, S/N {}", d.model, d.serial), d.size)
        }
        Target::Partition { disk, number, .. } => {
            let d = disks.iter().find(|d| d.number == *disk).ok_or_else(|| anyhow!(tr!(format!("Disk {disk}을(를) 찾지 못했습니다"), format!("Disk {disk} not found"))))?;
            let p = d
                .partitions
                .iter()
                .find(|p| p.number == *number)
                .ok_or_else(|| anyhow!(tr!(format!("Disk {disk} 파티션 {number}이(가) 없습니다"), format!("Disk {disk} has no partition {number}"))))?;
            (
                p.display_name(),
                tr!(
                    format!("Disk {}, 파티션 {}, 오프셋 {}", d.number, p.number, p.offset),
                    format!("Disk {}, partition {}, offset {}", d.number, p.number, p.offset)
                ),
                p.size,
            )
        }
        Target::Volume { letter } => {
            let (d, p) = disks
                .iter()
                .find_map(|d| d.partitions.iter().find(|p| p.letter.as_deref() == Some(letter.as_str())).map(|p| (d, p)))
                .ok_or_else(|| anyhow!(tr!(format!("{letter} 볼륨을 찾지 못했습니다"), format!("Volume {letter} not found"))))?;
            (p.display_name(), format!("Disk {}, {} {}", d.number, p.fs, d.model), p.size)
        }
        Target::Device { path, .. } => match locate_device(path, disks) {
            Some((d, None)) => (d.path.clone(), format!("{}, {}{}", d.model, d.bus, if d.serial.is_empty() { String::new() } else { format!(", S/N {}", d.serial) }), d.size),
            Some((d, Some(p))) => (
                p.device.clone().unwrap_or_else(|| path.clone()),
                format!("{} {} {}{}{}", d.path, tr!("파티션", "partition"), p.number, if p.fs.is_empty() { String::new() } else { format!(", {}", p.fs) }, p.letter.as_ref().map(|m| format!(", {m}")).unwrap_or_default()),
                p.size,
            ),
            None => {
                return Err(anyhow!(tr!(
                    format!("{path} 장치를 찾지 못했습니다. list 명령으로 장치 이름을 확인하세요"),
                    format!("Device {path} not found. Check device names with the list command")
                )))
            }
        },
        Target::Memory => {
            let m = crate::memory::memory_probe()
                .map_err(|e| anyhow!(tr!(format!("이 시스템에서는 메모리를 수집할 수 없습니다: {e}"), format!("Memory cannot be acquired on this system: {e}"))))?;
            (
                tr!("메모리", "Memory").into(),
                tr!(
                    format!("물리 메모리 {}, 원본 {}, 영역 {}개, LiME 형식", format_bytes(m.bytes), m.source, m.ranges),
                    format!("Physical memory {}, source {}, {} range(s), LiME format", format_bytes(m.bytes), m.source, m.ranges)
                ),
                m.image_bytes,
            )
        }
        Target::File { path: p } => {
            let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            (p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(), p.display().to_string(), len)
        }
    })
}

/// E01 머리말용: (모델, 시리얼, 물리 디스크 전체인지)
fn target_identity(t: &Target, disks: &[DiskInfo]) -> (String, String, bool) {
    let of = |d: &DiskInfo, whole: bool| (d.model.clone(), d.serial.clone(), whole);
    match t {
        Target::Disk { number, .. } => disks.iter().find(|d| d.number == *number).map(|d| of(d, true)),
        Target::Partition { disk, .. } => disks.iter().find(|d| d.number == *disk).map(|d| of(d, false)),
        Target::Volume { letter } => disks
            .iter()
            .find(|d| d.partitions.iter().any(|p| p.letter.as_deref() == Some(letter.as_str())))
            .map(|d| of(d, false)),
        Target::Device { path, .. } => locate_device(path, disks).map(|(d, p)| of(d, p.is_none())),
        Target::File { .. } | Target::Memory => None,
    }
    .unwrap_or_default()
}

/// 장치 경로가 목록의 어느 디스크(또는 그 파티션)인지 찾습니다. 심볼릭 링크(/dev/mapper, by-id)도 풀어서 비교
pub fn locate_device<'a>(path: &str, disks: &'a [DiskInfo]) -> Option<(&'a DiskInfo, Option<&'a crate::model::PartitionInfo>)> {
    let canon = |p: &str| std::fs::canonicalize(p).map(|x| x.display().to_string()).unwrap_or_else(|_| p.to_string());
    let want = canon(path);
    for d in disks {
        if canon(&d.path) == want {
            return Some((d, None));
        }
        for p in &d.partitions {
            if p.device.as_deref().map(canon).as_deref() == Some(want.as_str()) {
                return Some((d, Some(p)));
            }
        }
    }
    None
}

/// 대상 검사: 시리얼 확인, 그리고 이미징 대상과 저장 위치가 겹치는지.
/// 도구 디스크, 저장 디스크라는 이유만으로는 막지 않음 (수십 TB 디스크에 도구와 이미지를 함께 담는 경우).
/// 막는 것은 저장 폴더가 있는 디스크 전체, 또는 저장 폴더가 있는 바로 그 파티션, 볼륨(LVM, LUKS 아래 장치 포함)
fn check_target(t: &Target, disks: &[DiskInfo], out: Option<&std::path::Path>) -> Result<()> {
    let disk_no = match t {
        Target::Device { path, .. } => Some(locate_device(path, disks).map(|(d, _)| d.number).ok_or_else(|| anyhow!(tr!(format!("{path} 장치를 찾지 못했습니다"), format!("Device {path} not found"))))?),
        Target::Disk { number, .. } => Some(*number),
        Target::Partition { disk, .. } => Some(*disk),
        Target::Volume { letter } => disks
            .iter()
            .find(|d| d.partitions.iter().any(|p| p.letter.as_deref() == Some(letter.as_str())))
            .map(|d| d.number),
        Target::File { .. } | Target::Memory => None,
    };
    // 목록을 본 뒤 장치를 다시 꽂아 번호가 바뀌면 다른 디스크를 읽게 되므로 시리얼로 확인
    let expect = match t {
        Target::Disk { serial, .. } | Target::Partition { serial, .. } | Target::Device { serial, .. } => serial.as_deref(),
        _ => None,
    };
    if let (Some(n), Some(want)) = (disk_no, expect) {
        match disks.iter().find(|d| d.number == n) {
            Some(d) if d.serial == want => {}
            Some(d) => {
                return Err(anyhow!(tr!(
                    format!("Disk {n}이(가) 바뀌었습니다 (예상 S/N {want}, 현재 {}). 디스크 목록을 새로고침한 뒤 다시 고르세요", d.serial),
                    format!("Disk {n} has changed (expected S/N {want}, now {}). Refresh the disk list and select again", d.serial)
                )))
            }
            None => return Err(anyhow!(tr!(format!("Disk {n}을(를) 찾지 못했습니다. 디스크 목록을 새로고침하세요"), format!("Disk {n} not found. Refresh the disk list")))),
        }
    }
    if let Some(out) = out {
        if platform::overlaps_output(t, disks, out) {
            let (title, _, _) = describe(t, disks).unwrap_or_default();
            return Err(anyhow!(tr!(
                format!(
                    "{title}에 저장 폴더({})가 들어 있어 이미징할 수 없습니다 (이미징하는 동안 이미지 파일이 대상 안에 써짐). 저장 폴더를 다른 디스크나 파티션으로 바꾸세요",
                    out.display()
                ),
                format!(
                    "Cannot image {title}: it contains the output folder ({}), so image files would be written into the target. Choose an output folder on another disk or partition",
                    out.display()
                )
            )));
        }
    }
    Ok(())
}

struct JobObserver<'a> {
    index: usize,
    book: &'a LogBook,
    sink: &'a mut dyn CollectObserver,
}

impl Observer for JobObserver<'_> {
    fn progress(&mut self, p: &Progress) {
        self.sink.event(CollectEvent::Progress { index: self.index, progress: p.clone() });
        for n in self.sink.take_notes() {
            let e = self.book.write(LogLevel::Warn, &n);
            self.sink.event(CollectEvent::Log(e));
        }
    }
    fn log(&mut self, level: LogLevel, msg: &str) {
        let e = self.book.write(level, msg);
        self.sink.event(CollectEvent::Log(e));
    }
}

fn log(book: &LogBook, sink: &mut dyn CollectObserver, level: LogLevel, msg: &str) {
    let e = book.write(level, msg);
    sink.event(CollectEvent::Log(e));
}

/// 저장 매체 여유 공간과 파일시스템 제한 검사 (로컬 저장, 네트워크 수신 측 공용)
pub fn check_space(v: &VolumeInfo, need: u64, split_bytes: u64) -> Result<()> {
    if v.free < need {
        return Err(anyhow!(tr!(
            format!("저장 공간이 부족합니다: 필요 {}, 여유 {}", format_bytes(need), format_bytes(v.free)),
            format!("Not enough free space: {} needed, {} free", format_bytes(need), format_bytes(v.free))
        )));
    }
    // FAT32는 파일 하나가 4GB-1바이트를 넘을 수 없음 → 몇 시간 뒤 실패하지 않게 미리 막음
    let fat = v.fs.eq_ignore_ascii_case("FAT32") || v.fs.eq_ignore_ascii_case("vfat") || v.fs.eq_ignore_ascii_case("msdos");
    if fat && (split_bytes == 0 || split_bytes > u32::MAX as u64) {
        return Err(anyhow!(tr!(
            "저장 매체가 FAT32라서 4GB 넘는 파일을 만들 수 없습니다. 파일 분할을 2GB로 고르거나 E01 형식을 쓰세요",
            "The output media is FAT32 and cannot hold files over 4 GB. Choose 2 GB split or use the E01 format"
        )));
    }
    Ok(())
}

/// 네트워크 수신 측 저장 폴더 검사 (CLI receive, GUI 수신 공용).
/// 돌려주는 값: (매체 설명, 파일시스템, 여유 공간, 경고)
pub fn receiver_space(base: &std::path::Path, need: u64, split: u64) -> std::result::Result<(String, String, u64, Option<String>), String> {
    let disks = disks_with_roles(Some(base)).unwrap_or_default();
    let oc = check_output_dir(base, &disks);
    if !oc.ok {
        return Err(oc.message);
    }
    let v = oc.volume.clone().ok_or(tr!("저장 매체 정보를 읽지 못했습니다", "Could not read output media information"))?;
    check_space(&v, need, split).map_err(|e| e.to_string())?;
    Ok((oc.message.clone(), v.fs.clone(), v.free, oc.warning.clone()))
}

/// 이미징에 필요한 저장 공간 (모든 대상 크기 합)
fn needed_bytes(req: &CollectRequest, disks: &[DiskInfo]) -> Result<u64> {
    let mut need = 0u64;
    for t in &req.targets {
        let size = describe(t, disks)?.2;
        need += req.max_bytes.map_or(size, |m| m.min(size));
    }
    Ok(need)
}

/// 미리 검사만 합니다 (GUI 시작 버튼, CLI 확인 화면용).
/// 돌려주는 값: (디스크 목록, 로컬 증거 폴더). 네트워크 전송이면 증거 폴더는 수신 PC가 정하므로 빈 경로
pub fn preflight(req: &CollectRequest) -> Result<(Vec<DiskInfo>, PathBuf)> {
    req.case.validate().map_err(|e| anyhow!(e.to_string()))?;
    if req.targets.is_empty() {
        return Err(anyhow!(tr!("이미징할 대상을 하나 이상 고르세요", "Select at least one target to image")));
    }
    if req.algs.is_empty() {
        return Err(anyhow!(tr!("해시 알고리즘을 하나 이상 고르세요", "Select at least one hash algorithm")));
    }
    let needs_disks = req.targets.iter().any(|t| !matches!(t, Target::File { .. } | Target::Memory));
    let out = if req.remote.is_some() { None } else { Some(req.out_base.as_path()) };
    let disks = if needs_disks { disks_with_roles(out)? } else { disks_with_roles(out).unwrap_or_default() };
    for t in &req.targets {
        check_target(t, &disks, out)?;
    }
    if let Some(r) = &req.remote {
        crate::net::normalize_code(&r.code)?;
        if r.addr.trim().is_empty() {
            return Err(anyhow!(tr!("수신 PC 주소를 입력하세요", "Enter the receiving PC address")));
        }
        needed_bytes(req, &disks)?;
        return Ok((disks, PathBuf::new()));
    }
    let oc = check_output_dir(&req.out_base, &disks);
    if !oc.ok {
        return Err(anyhow!(oc.message));
    }
    let need = needed_bytes(req, &disks)?;
    if let Some(v) = &oc.volume {
        check_space(v, need, space_split(req))?;
    }
    let dir = req.case.evidence_dir(&req.out_base);
    if dir.exists() && std::fs::read_dir(&dir).map(|mut r| r.next().is_some()).unwrap_or(false) {
        return Err(anyhow!(tr!(
            format!("이미 파일이 있는 증거 폴더입니다: {}. 증거 번호를 바꾸세요", dir.display()),
            format!("The evidence folder already contains files: {}. Use a different evidence number", dir.display())
        )));
    }
    Ok((disks, dir))
}

pub fn run_collection(req: &CollectRequest, cancel: &Arc<AtomicBool>, sink: &mut dyn CollectObserver) -> Result<CollectSummary> {
    // 오류로 중간에 빠져나가거나 패닉해도 적재한 메모리 드라이버(Windows WinPmem)가 남지 않게
    struct DriverGuard;
    impl Drop for DriverGuard {
        fn drop(&mut self) {
            let _ = crate::memory::release_driver();
        }
    }
    let _driver_guard = DriverGuard;
    // 이전 세션이 중간에 끝나 남은 흔적 문장은 버림 (이번 세션 것만 보고서에)
    let _ = crate::memory::take_footprint();
    let (disks, dir) = preflight(req)?;
    let started = now_local();
    let host = platform::host_info();
    let prefix = req.case.file_prefix(&host.hostname, &started.format("%Y%m%d").to_string());

    let need = needed_bytes(req, &disks)?;

    // 저장소 준비: 로컬 증거 폴더 또는 네트워크 수신 PC
    let mut remote_store: Option<Arc<RemoteStore>> = None;
    let (store, accept): (Arc<dyn EvidenceStore>, Option<crate::net::Accept>) = match &req.remote {
        None => {
            std::fs::create_dir_all(&dir).map_err(|e| anyhow!(tr!(format!("증거 폴더를 만들지 못했습니다 ({}): {e}", dir.display()), format!("Could not create the evidence folder ({}): {e}", dir.display()))))?;
            (Arc::new(LocalStore::new(dir.clone())), None)
        }
        Some(r) => {
            let hello = Hello {
                protocol: crate::net::PROTOCOL,
                tool_version: crate::VERSION.into(),
                case: req.case.clone(),
                host: host.hostname.clone(),
                interface: req.interface.clone(),
                need_bytes: need,
                split_bytes: space_split(req),
            };
            let (s, a) = RemoteStore::connect(&r.addr, &r.code, &hello)?;
            s.set_cancel(cancel.clone());
            let s = Arc::new(s);
            remote_store = Some(s.clone());
            (s, Some(a))
        }
    };
    let dir = match &accept {
        Some(a) => PathBuf::from(&a.evidence_dir),
        None => dir,
    };
    let book = LogBook::memory();
    let log_name = format!("{prefix}_collection.log");
    store
        .create(&log_name)
        .and_then(|f| book.attach(f))
        .map_err(|e| anyhow!(tr!(format!("수집 로그를 만들지 못했습니다: {e}"), format!("Could not create the collection log: {e}"))))?;

    // 메모리는 디스크보다 먼저 (디스크를 읽는 동안 메모리 내용이 바뀌므로)
    let mut targets = req.targets.clone();
    targets.sort_by_key(|t| !matches!(t, Target::Memory));
    // 메모리는 분할하지 않음 (LiME 파일 하나로 바로 분석). FAT32 저장 매체면 어쩔 수 없이 분할
    let out_fs = match &accept {
        Some(a) => a.fs.clone(),
        None => platform::volume_for_path(&req.out_base).map(|v| v.fs).unwrap_or_default(),
    };
    let fat = matches!(out_fs.to_ascii_uppercase().as_str(), "FAT32" | "VFAT" | "MSDOS");

    // 계획 알림
    let mut plans = Vec::new();
    for t in &targets {
        let (title, description, size) = describe(t, &disks)?;
        let size = req.max_bytes.map_or(size, |m| m.min(size));
        plans.push(JobPlan { title, description, size, file_base: format!("{prefix}_{}", t.tag()) });
    }
    sink.event(CollectEvent::Prepared { evidence_dir: dir.display().to_string(), jobs: plans.clone() });

    // 머리말
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_hash = digest_file(&exe, HashAlg::Sha256).unwrap_or_else(|e| tr!(format!("계산 실패: {e}"), format!("calculation failed: {e}")));
    let media = |role: DiskRole| {
        disks
            .iter()
            .find(|d| d.has_role(role))
            .map(|d| {
                // Linux는 장치 경로(/dev/sda), Windows는 디스크 번호로 표시
                let id = if d.name.is_empty() { format!("Disk {}", d.number) } else { d.path.clone() };
                format!("{id}, {}, S/N {}, {}", d.model, if d.serial.is_empty() { "-" } else { &d.serial }, d.bus)
            })
            .unwrap_or_else(|| tr!("확인 안 됨", "unknown").into())
    };
    let tool_media = media(DiskRole::Tool);
    let output_media = match (&accept, &req.remote) {
        (Some(a), Some(r)) => tr!(
            format!("네트워크 수신 PC {} ({}), {}", a.receiver_host, r.addr, a.media),
            format!("Network receiving PC {} ({}), {}", a.receiver_host, r.addr, a.media)
        ),
        _ => media(DiskRole::Output),
    };
    let output_on_system_disk = accept.is_none() && disks.iter().any(|d| d.has_role(DiskRole::Output) && d.has_role(DiskRole::System));
    let c = &req.case;
    let org = if c.organization.is_empty() { String::new() } else { format!(" ({})", c.organization) };
    let fmt_label = match req.format {
        ImageFormat::E01 => format!("E01 ({})", req.compression.label()),
        ImageFormat::Raw => "RAW".into(),
    };
    let split_label = match (req.split_bytes, fat) {
        (n, true) if fat_split(req.format, n) != n => tr!(
            format!("{} → FAT32라 {} 단위로 나눔", if n == 0 { "없음".to_string() } else { format_bytes(n) }, format_bytes(fat_split(req.format, n))),
            format!("{} → split into {} (FAT32)", if n == 0 { "none".to_string() } else { format_bytes(n) }, format_bytes(fat_split(req.format, n)))
        ),
        (0, _) => tr!("없음", "none").into(),
        (n, _) => format_bytes(n),
    };
    let algs_label = req.algs.iter().map(|a| a.label()).collect::<Vec<_>>().join("/");
    let header: Vec<String> = tr!(
        vec![
            format!("WS Collector v{} ({}) 수집 시작", crate::VERSION, req.interface),
            format!("사건 번호 {}, 증거 번호 {}, 수집자 {}{org}", c.case_number, c.evidence_number, c.examiner),
            format!("대상 시스템: {} / {} {} ({}) / {}", host.hostname, host.os_name, host.os_version, host.build_label(), host.cpu),
            format!("대상 시스템 시각: {} / 시간대 {} / 부팅 {}", host.local_time, host.timezone, host.boot_time),
            format!("관리자 권한: {}", if platform::is_elevated() { "예" } else { "아니오" }),
            format!("도구 실행 파일: {} (SHA-256 {})", exe.display(), exe_hash),
            format!("도구 매체: {tool_media}"),
            format!("저장 매체: {output_media}"),
            format!("저장 폴더: {}{}", dir.display(), if accept.is_some() { " (수신 PC)" } else { "" }),
            format!(
                "옵션: 디스크 형식 {fmt_label}, {} {split_label}, 해시 {algs_label}, 검증 {}, 읽기 오류 {}",
                if req.format == ImageFormat::E01 { "조각" } else { "분할" },
                if req.verify { "함" } else { "안 함" },
                if req.zero_fill_errors { "0으로 채우고 계속" } else { "중단" }
            ),
        ],
        vec![
            format!("WS Collector v{} ({}) collection started", crate::VERSION, req.interface),
            format!("Case number {}, evidence number {}, examiner {}{org}", c.case_number, c.evidence_number, c.examiner),
            format!("Target system: {} / {} {} ({}) / {}", host.hostname, host.os_name, host.os_version, host.build_label(), host.cpu),
            format!("Target system time: {} / time zone {} / boot {}", host.local_time, host.timezone, host.boot_time),
            format!("Administrator privileges: {}", if platform::is_elevated() { "yes" } else { "no" }),
            format!("Tool executable: {} (SHA-256 {})", exe.display(), exe_hash),
            format!("Tool media: {tool_media}"),
            format!("Output media: {output_media}"),
            format!("Evidence folder: {}{}", dir.display(), if accept.is_some() { " (receiving PC)" } else { "" }),
            format!(
                "Options: disk format {fmt_label}, {} {split_label}, hash {algs_label}, verification {}, read errors {}",
                if req.format == ImageFormat::E01 { "segment" } else { "split" },
                if req.verify { "on" } else { "off" },
                if req.zero_fill_errors { "zero-fill and continue" } else { "stop" }
            ),
        ]
    );
    for line in header {
        log(&book, sink, LogLevel::Info, &line);
    }
    if !c.notes.trim().is_empty() {
        log(&book, sink, LogLevel::Info, &tr!(format!("현장 메모: {}", c.notes.trim()), format!("Notes: {}", c.notes.trim())));
    }
    if let Some(a) = &accept {
        log(
            &book,
            sink,
            LogLevel::Info,
            &tr!(
                format!("네트워크 전송: 수신 PC가 본 이 시스템 주소 {}, 암호화 SPAKE2 + ChaCha20-Poly1305", a.peer),
                format!("Network transfer: this system's address as seen by the receiving PC {}, encryption SPAKE2 + ChaCha20-Poly1305", a.peer)
            ),
        );
        if let Some(w) = &a.warning {
            log(&book, sink, LogLevel::Warn, &tr!(format!("수신 PC 저장 위치: {w}"), format!("Receiving PC output location: {w}")));
        }
    }
    if output_on_system_disk {
        log(
            &book,
            sink,
            LogLevel::Warn,
            tr!(
                "저장 위치가 이 PC의 시스템 디스크입니다. 라이브 수집이면 증거 파일만큼 대상 디스크에 기록됩니다",
                "The output location is on this PC's system disk. In live collection, evidence files are written to the target disk"
            ),
        );
    }
    if req.max_bytes.is_some() {
        log(&book, sink, LogLevel::Warn, tr!("시험 모드: 원본의 앞부분만 수집합니다 (--max-bytes)", "Test mode: acquiring only the beginning of the source (--max-bytes)"));
    }

    // 작업 실행
    let mut jobs = Vec::new();
    for (i, t) in targets.iter().enumerate() {
        let plan = &plans[i];
        let is_mem = matches!(t, Target::Memory);
        if cancel.load(Ordering::Relaxed) {
            jobs.push(JobOutcome { index: i, target: t.clone(), title: plan.title.clone(), description: plan.description.clone(), status: JobStatus::Skipped, result: None, error: None });
            continue;
        }
        sink.event(CollectEvent::JobStarted { index: i });
        let what = if is_mem { tr!("메모리 수집", "Memory acquisition") } else { tr!("디스크 이미징", "Disk imaging") };
        log(&book, sink, LogLevel::Info, &format!("[{}/{}] {what}: {} ({})", i + 1, targets.len(), plan.title, plan.description));
        // 메모리는 LiME 그대로 (Volatility가 바로 읽음). FAT32면 어쩔 수 없이 분할
        let format = if is_mem { ImageFormat::Raw } else { req.format };
        let split_bytes = match (fat, is_mem) {
            (true, _) => fat_split(format, req.split_bytes),
            (false, true) => 0,
            (false, false) => req.split_bytes,
        };
        let (model, serial, physical) = target_identity(t, &disks);
        let spec = ImageSpec {
            out_dir: dir.clone(),
            base_name: plan.file_base.clone(),
            split_bytes,
            algs: req.algs.clone(),
            verify: req.verify,
            zero_fill_errors: req.zero_fill_errors,
            max_bytes: req.max_bytes,
            ext: is_mem.then(|| "lime".to_string()),
            format,
            compression: req.compression,
            meta: E01Meta {
                case_number: c.case_number.clone(),
                evidence_number: c.evidence_number.clone(),
                examiner: c.examiner.clone(),
                notes: c.notes.clone(),
                description: format!("{} ({})", plan.title, plan.description),
                model,
                serial,
                os: format!("{} {}", host.os_name, host.os_version),
                physical,
            },
        };
        let opened: Result<Box<dyn crate::source::Source>> = if is_mem {
            crate::memory::open_memory().map(|(src, info)| {
                let line = match &info.driver {
                    Some(d) => tr!(
                        format!("메모리 원본: {} (영역 {}개, {}), 드라이버 {d}", info.source, info.ranges, format_bytes(info.bytes)),
                        format!("Memory source: {} ({} range(s), {}), driver {d}", info.source, info.ranges, format_bytes(info.bytes))
                    ),
                    None => tr!(
                        format!("메모리 원본: {} (영역 {}개, {}), 커널 모듈, 드라이버는 적재하지 않음", info.source, info.ranges, format_bytes(info.bytes)),
                        format!("Memory source: {} ({} range(s), {}), no kernel module or driver loaded", info.source, info.ranges, format_bytes(info.bytes))
                    ),
                };
                log(&book, sink, LogLevel::Info, &line);
                for n in &info.notes {
                    let level = if n.starts_with(crate::memory::COVERAGE_WARN) { LogLevel::Warn } else { LogLevel::Info };
                    log(&book, sink, level, &tr!(format!("메모리 원본 확인: {n}"), format!("Memory source check: {n}")));
                }
                Box::new(src) as Box<dyn crate::source::Source>
            })
        } else {
            platform::open_target(t, &disks)
        };
        let outcome = match opened {
            Err(e) => Err(e),
            Ok(mut src) => {
                log(&book, sink, LogLevel::Info, tr!("원본을 읽기 전용으로 열었습니다", "Source opened read-only"));
                let mut ob = JobObserver { index: i, book: &book, sink: &mut *sink };
                run_image_to(src.as_mut(), &spec, store.clone(), cancel, &mut ob)
            }
        };
        // 메모리 원본(장치 핸들)은 위에서 닫혔음 → 적재한 드라이버를 바로 내림 (디스크 이미징 중에 남겨 두지 않음)
        if is_mem {
            for (warn, line) in crate::memory::release_driver() {
                log(&book, sink, if warn { LogLevel::Warn } else { LogLevel::Info }, &line);
            }
        }
        let out = match outcome {
            Ok(r) => {
                let status = if r.cancelled { JobStatus::Cancelled } else { JobStatus::Done };
                JobOutcome { index: i, target: t.clone(), title: plan.title.clone(), description: plan.description.clone(), status, result: Some(r), error: None }
            }
            Err(e) => {
                log(&book, sink, LogLevel::Error, &tr!(format!("작업 실패: {e:#}"), format!("Job failed: {e:#}")));
                JobOutcome { index: i, target: t.clone(), title: plan.title.clone(), description: plan.description.clone(), status: JobStatus::Failed, result: None, error: Some(format!("{e:#}")) }
            }
        };
        sink.event(CollectEvent::JobFinished { index: i, outcome: out.clone() });
        jobs.push(out);
    }

    // 이제부터는 마무리 기록: 중단 요청이 있었어도 끊기면 다시 연결해 로그, 요약, 보고서를 남김
    if let Some(r) = &remote_store {
        r.clear_cancel();
    }
    // 화면 쪽 기록(SSH 끊김 등)과 네트워크 끊김, 이어받기 기록 (시각 포함)
    for n in sink.take_notes() {
        log(&book, sink, LogLevel::Warn, &n);
    }
    for n in store.notes() {
        log(&book, sink, LogLevel::Warn, &n);
    }

    // 흔적 기록. 혹시 남은 드라이버(작업 중 오류 등)는 여기서 한 번 더 내림
    for (warn, line) in crate::memory::release_driver() {
        log(&book, sink, if warn { LogLevel::Warn } else { LogLevel::Info }, &line);
    }
    let driver_notes = crate::memory::take_footprint();
    let processes = platform::process_tree(std::process::id());
    let footprint = Footprint {
        tool_path: exe.display().to_string(),
        tool_sha256: exe_hash,
        tool_media,
        output_media,
        output_on_system_disk,
        processes,
        notes: {
            let mut n = vec![if output_on_system_disk {
                tr!(
                    format!("대상 디스크 쓰기: 있음 (증거 파일, 로그를 이 PC의 시스템 디스크 {}에 저장)", req.out_base.display()),
                    format!("Writes to target disk: yes (evidence files and logs saved to this PC's system disk {})", req.out_base.display())
                )
            } else if accept.is_some() {
                tr!(
                    "대상 디스크 쓰기: 0건 (증거 파일, 로그를 모두 네트워크로 수신 PC에 저장하도록 설계됨, 측정값 아님)",
                    "Writes to target disk: 0 (by design, all evidence files and logs are sent over the network to the receiving PC; not measured)"
                )
                .into()
            } else {
                tr!(
                    "대상 디스크 쓰기: 0건 (모든 출력, 임시 파일을 외부 매체에 기록하도록 설계됨, 측정값 아님)",
                    "Writes to target disk: 0 (by design, all output and temporary files are written to external media; not measured)"
                )
                .into()
            }];
            if let (Some(a), Some(r)) = (&accept, &req.remote) {
                n.push(tr!(
                    format!("네트워크 연결 기록: 이 시스템({}) → 수신 PC {} (방화벽, conntrack, EDR 로그에 남을 수 있음)", a.peer, r.addr),
                    format!("Network connection record: this system ({}) → receiving PC {} (may appear in firewall, conntrack or EDR logs)", a.peer, r.addr)
                ));
            }
            n.extend(driver_notes);
            n.extend(platform::footprint_notes());
            n
        },
    };
    let status = if cancel.load(Ordering::Relaxed) {
        SessionStatus::Cancelled
    } else if jobs.iter().all(|j| j.status == JobStatus::Done && j.result.as_ref().is_some_and(|r| r.verify.as_ref().is_none_or(|v| v.matched))) {
        SessionStatus::Completed
    } else {
        SessionStatus::Partial
    };
    log(
        &book,
        sink,
        if status == SessionStatus::Completed { LogLevel::Info } else { LogLevel::Warn },
        &{
            let procs = footprint.processes.iter().map(|p| format!("{} {}", p.name, p.pid)).collect::<Vec<_>>().join(", ");
            let np = footprint.processes.len();
            tr!(
                format!(
                    "수집 {}: 프로세스 {np}개 기록 ({procs})",
                    match status {
                        SessionStatus::Completed => "완료",
                        SessionStatus::Partial => "일부 실패",
                        SessionStatus::Cancelled => "중단",
                    }
                ),
                format!(
                    "Collection {}: {np} process(es) recorded ({procs})",
                    match status {
                        SessionStatus::Completed => "completed",
                        SessionStatus::Partial => "partially failed",
                        SessionStatus::Cancelled => "cancelled",
                    }
                )
            )
        },
    );

    let summary_name = format!("{prefix}_summary.json");
    let report_name = format!("{prefix}_report.html");
    let summary = CollectSummary {
        tool_version: crate::VERSION.into(),
        interface: req.interface.clone(),
        case: req.case.clone(),
        host,
        evidence_dir: dir.clone(),
        file_prefix: prefix,
        started: fmt_ts(&started),
        finished: fmt_ts(&now_local()),
        status,
        jobs,
        footprint,
        log_file: log_name.clone(),
        report_file: report_name.clone(),
        summary_file: summary_name.clone(),
    };
    // 요약, 보고서 저장 실패가 이미 끝난 이미징 결과를 버리게 하지 않음 (로그에 남기고 결과는 돌려줌)
    let json = serde_json::to_string_pretty(&summary).unwrap_or_default();
    let mut saved = true;
    if let Err(e) = write_new(store.as_ref(), &summary_name, json.as_bytes()) {
        saved = false;
        log(&book, sink, LogLevel::Error, &format!("{e:#}"));
    }
    let report = crate::report::render(&summary, &book.entries());
    if let Err(e) = write_new(store.as_ref(), &report_name, report.as_bytes()) {
        saved = false;
        log(&book, sink, LogLevel::Error, &format!("{e:#}"));
    }
    if saved {
        log(&book, sink, LogLevel::Info, &tr!(format!("보고서 저장: {report_name}, {summary_name}"), format!("Report saved: {report_name}, {summary_name}")));
    }
    if let Err(e) = book.close() {
        sink.event(CollectEvent::Log(LogEntry { time: fmt_ts(&now_local()), level: LogLevel::Error, message: tr!(format!("수집 로그 저장 실패: {e}"), format!("Failed to save the collection log: {e}")) }));
    }
    // 수신 PC로 보내는 상태 값: 수신 측이 "완료"와 비교하므로 언어와 상관없이 고정된 값으로 보냄 (보내는 쪽, 받는 쪽 언어가 다를 수 있음)
    let status_text = match summary.status {
        SessionStatus::Completed => "완료",
        SessionStatus::Partial => "일부 실패",
        SessionStatus::Cancelled => "중단",
    };
    if let Err(e) = store.finish_session(status_text) {
        sink.event(CollectEvent::Log(LogEntry { time: fmt_ts(&now_local()), level: LogLevel::Warn, message: tr!(format!("수신 PC에 종료를 알리지 못했습니다: {e:#}"), format!("Could not notify the receiving PC of session end: {e:#}")) }));
    }
    Ok(summary)
}

fn write_new(store: &dyn EvidenceStore, name: &str, data: &[u8]) -> Result<()> {
    let fail = |e: std::io::Error| anyhow!(tr!(format!("{name} 저장 실패: {e}"), format!("Failed to save {name}: {e}")));
    let mut f = store.create(name).map_err(fail)?;
    f.write_all(data).map_err(fail)?;
    f.finish().map_err(fail)?;
    Ok(())
}
