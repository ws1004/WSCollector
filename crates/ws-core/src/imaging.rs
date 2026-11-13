//! 이미징 엔진 (RAW, E01).
//!
//! 흐름: 원본 읽기(현재 스레드) → [해시 스레드들] + [쓰기 스레드(E01이면 압축 스레드들)] → (선택) 저장 파일 다시 읽어 검증
//!
//! - 4MB 단위로 읽고, 실패하면 그 구간만 섹터 단위로 다시 읽습니다 (섹터당 최대 3회).
//!   끝까지 실패한 섹터는 0으로 채우고 위치를 기록합니다 (`zero_fill_errors = false`면 중단).
//! - 해시는 원본에서 읽은 데이터 기준입니다. 검증 단계에서 저장 파일을 캐시 없이 다시 읽어 비교합니다.
//! - 저장 파일은 `create_new`로 만들어 기존 파일을 절대 덮어쓰지 않습니다.
//! - RAW 분할: `이름.001`, `이름.002` …  분할 없음: `이름.dd`
//! - E01: `이름.E01` (조각을 나누면 `.E02` …). 형식은 `e01` 모듈

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::e01::{self, Compression, E01Meta, E01Writer, Trailer, WriterMsg};
use crate::hash::{Digests, HashAlg, HashPipeline};
use crate::logbook::LogLevel;
use crate::source::{AlignedBuf, Source};
use crate::store::{EvidenceStore, LocalStore, StoreFile};
use crate::util::{format_bytes, format_duration};

pub const CHUNK: usize = 4 << 20;
const SECTOR_RETRIES: u32 = 3;
const PROGRESS_EVERY: Duration = Duration::from_millis(200);
const MAX_BAD_RANGE_LOGS: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSpec {
    pub out_dir: PathBuf,
    /// 확장자 없는 파일 이름. 예: `EV-003_DESKTOP-7Q2LM4K_20261105_Disk0`
    pub base_name: String,
    /// 분할 크기 (바이트). 0이면 분할하지 않음
    pub split_bytes: u64,
    pub algs: Vec<HashAlg>,
    pub verify: bool,
    pub zero_fill_errors: bool,
    /// 시험용: 앞부분만 읽기
    #[serde(default)]
    pub max_bytes: Option<u64>,
    /// 분할하지 않을 때 확장자 (기본 "dd", 메모리는 "lime"). RAW만 해당
    #[serde(default)]
    pub ext: Option<String>,
    /// 저장 형식. 분할 크기(`split_bytes`)는 E01이면 조각 크기
    #[serde(default)]
    pub format: ImageFormat,
    #[serde(default)]
    pub compression: Compression,
    /// E01 머리말 (사건 정보, 대상 설명)
    #[serde(default)]
    pub meta: E01Meta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    #[default]
    Raw,
    E01,
}

impl ImageFormat {
    pub fn label(self) -> &'static str {
        match self {
            ImageFormat::Raw => "RAW",
            ImageFormat::E01 => "E01",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Reading,
    Verifying,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
    pub rate_bps: f64,
    pub eta_secs: u64,
    pub bad_sectors: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BadRange {
    pub start_sector: u64,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputFile {
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResult {
    pub hashes: Digests,
    pub matched: bool,
    pub secs: f64,
    /// 검증을 끝내지 못한 이유 (읽기 오류 등)
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageResult {
    pub files: Vec<OutputFile>,
    /// 원본에서 읽은 바이트 수 (E01이면 풀었을 때 크기)
    pub total_bytes: u64,
    /// 저장한 파일 크기 합 (RAW는 total_bytes와 같음, E01은 압축 후 크기)
    #[serde(default)]
    pub stored_bytes: u64,
    #[serde(default)]
    pub format: ImageFormat,
    pub sector_size: u32,
    pub hashes: Digests,
    pub verify: Option<VerifyResult>,
    pub bad_ranges: Vec<BadRange>,
    pub bad_sectors: u64,
    pub cancelled: bool,
    pub read_secs: f64,
}

/// 진행 상황과 로그를 받는 쪽 (GUI 이벤트, CLI 출력, 수집 로그 파일)
pub trait Observer {
    fn progress(&mut self, p: &Progress);
    fn log(&mut self, level: LogLevel, msg: &str);
}

/// 속도 계산: 최근 3초 구간의 평균
struct RateMeter {
    samples: VecDeque<(Instant, u64)>,
}

impl RateMeter {
    fn new() -> Self {
        Self { samples: VecDeque::new() }
    }
    fn push(&mut self, done: u64) -> f64 {
        let now = Instant::now();
        // 진행량이 줄었으면(네트워크 이어받기 후 검증을 처음부터 다시 하는 경우) 측정을 새로 시작
        if self.samples.back().is_some_and(|(_, d)| done < *d) {
            self.samples.clear();
        }
        self.samples.push_back((now, done));
        while self.samples.len() > 2 && now.duration_since(self.samples[0].0) > Duration::from_secs(3) {
            self.samples.pop_front();
        }
        let (t0, d0) = self.samples[0];
        let dt = now.duration_since(t0).as_secs_f64();
        if dt > 0.05 {
            (done - d0) as f64 / dt
        } else {
            0.0
        }
    }
}

fn eta(total: u64, done: u64, rate: f64) -> u64 {
    if rate > 1.0 {
        ((total.saturating_sub(done)) as f64 / rate) as u64
    } else {
        0
    }
}

/// 분할 저장 (로컬 폴더 또는 네트워크 저장소)
struct SplitWriter {
    store: Arc<dyn EvidenceStore>,
    base: String,
    ext: String,
    split: u64,
    cur: Option<(Box<dyn StoreFile>, String, u64)>,
    files: Vec<OutputFile>,
}

impl SplitWriter {
    fn name_for(&self, idx: usize) -> String {
        if self.split == 0 {
            format!("{}.{}", self.base, self.ext)
        } else {
            format!("{}.{:03}", self.base, idx + 1)
        }
    }

    fn open_next(&mut self) -> io::Result<()> {
        let name = self.name_for(self.files.len());
        let f = self.store.create(&name)?;
        self.cur = Some((f, name, 0));
        Ok(())
    }

    fn close_cur(&mut self) -> io::Result<()> {
        if let Some((f, name, size)) = self.cur.take() {
            f.finish()?;
            self.files.push(OutputFile { name, size });
        }
        Ok(())
    }

    fn write(&mut self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            if self.cur.is_none() {
                self.open_next()?;
            }
            let (f, _, size) = self.cur.as_mut().unwrap();
            let room = if self.split == 0 { u64::MAX } else { self.split - *size };
            let n = (data.len() as u64).min(room) as usize;
            f.write_all(&data[..n])?;
            *size += n as u64;
            data = &data[n..];
            if self.split != 0 && *size >= self.split {
                self.close_cur()?;
            }
        }
        Ok(())
    }

    fn run(mut self, rx: Receiver<WriterMsg>) -> io::Result<Vec<OutputFile>> {
        for m in rx {
            if let WriterMsg::Data(chunk) = m {
                self.write(&chunk)?;
            }
        }
        self.close_cur()?;
        Ok(self.files)
    }
}

fn round_up(v: usize, unit: usize) -> usize {
    v.div_ceil(unit) * unit
}

/// 장치가 사라졌거나 접근할 수 없음을 뜻하는 오류: 불량 섹터로 보지 않고 즉시 중단합니다.
/// (USB 분리 중에 남은 부분을 0으로 채운 "완료" 이미지가 만들어지는 것을 막음)
#[cfg(windows)]
fn is_fatal_device_error(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(2)      // ERROR_FILE_NOT_FOUND
            | Some(3)    // ERROR_PATH_NOT_FOUND
            | Some(5)    // ERROR_ACCESS_DENIED
            | Some(6)    // ERROR_INVALID_HANDLE
            | Some(21)   // ERROR_NOT_READY
            | Some(55)   // ERROR_DEV_NOT_EXIST
            | Some(87)   // ERROR_INVALID_PARAMETER (정렬 오류)
            | Some(1110) // ERROR_MEDIA_CHANGED
            | Some(1167) // ERROR_DEVICE_NOT_CONNECTED
            | Some(433)  // ERROR_NO_SUCH_DEVICE
    )
}

/// Linux: EIO(5)는 불량 섹터일 수 있으므로 재시도 대상. 장치 제거, 권한, 정렬 오류만 즉시 중단
#[cfg(unix)]
fn is_fatal_device_error(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EPERM)
            | Some(libc::ENOENT)
            | Some(libc::ENXIO)
            | Some(libc::EBADF)
            | Some(libc::EACCES)
            | Some(libc::ENODEV)
            | Some(libc::EINVAL) // 정렬 오류
            | Some(libc::ENOMEDIUM)
    )
}

#[cfg(not(any(windows, unix)))]
fn is_fatal_device_error(_e: &io::Error) -> bool {
    false
}


/// 원본을 RAW 이미지로 `spec.out_dir` 폴더에 저장합니다.
pub fn run_image(
    source: &mut dyn Source,
    spec: &ImageSpec,
    cancel: &AtomicBool,
    obs: &mut dyn Observer,
) -> Result<ImageResult> {
    if !spec.out_dir.is_dir() {
        return Err(anyhow!("{}", crate::tr!(format!("저장 폴더가 없습니다: {}", spec.out_dir.display()), format!("Output folder does not exist: {}", spec.out_dir.display()))));
    }
    let store: Arc<dyn EvidenceStore> = Arc::new(LocalStore::new(spec.out_dir.clone()));
    run_image_to(source, spec, store, cancel, obs)
}

/// 원본을 RAW 이미지로 저장소(로컬 폴더 또는 네트워크 수신 PC)에 저장합니다. `spec.out_dir`는 쓰지 않습니다.
pub fn run_image_to(
    source: &mut dyn Source,
    spec: &ImageSpec,
    store: Arc<dyn EvidenceStore>,
    cancel: &AtomicBool,
    obs: &mut dyn Observer,
) -> Result<ImageResult> {
    if spec.algs.is_empty() {
        return Err(anyhow!(crate::tr!("해시 알고리즘을 하나 이상 골라야 합니다", "Select at least one hash algorithm")));
    }
    let sector = source.sector_size() as usize;
    if sector < 512 || sector > AlignedBuf::ALIGN || !sector.is_power_of_two() {
        return Err(anyhow!("{}", crate::tr!(format!("지원하지 않는 섹터 크기입니다: {sector}"), format!("Unsupported sector size: {sector}"))));
    }
    let total = spec.max_bytes.map_or(source.len(), |m| m.min(source.len()));
    if total == 0 {
        return Err(anyhow!(crate::tr!("원본 크기가 0입니다", "Source size is 0")));
    }

    let is_e01 = spec.format == ImageFormat::E01;
    // E01은 섹터 단위로 크기를 적으므로 원본이 섹터 배수여야 함 (디스크, 파티션은 늘 배수).
    // 시험용 앞부분 읽기는 섹터 배수로 내림
    let total = if is_e01 && spec.max_bytes.is_some() { total / sector as u64 * sector as u64 } else { total };
    if is_e01 && (total == 0 || total % sector as u64 != 0) {
        return Err(anyhow!("{}", crate::tr!(format!("원본 크기({total} 바이트)가 섹터({sector}B) 배수가 아니라 E01로 저장할 수 없습니다. RAW 형식을 쓰세요"), format!("Source size ({total} bytes) is not a multiple of the sector size ({sector} B), so it cannot be stored as E01. Use RAW format"))));
    }
    // E01은 파일 안에 MD5,SHA-1을 적어야 하므로 고르지 않았어도 함께 계산
    let mut algs = spec.algs.clone();
    if is_e01 {
        for a in [HashAlg::Md5, HashAlg::Sha1] {
            if !algs.contains(&a) {
                algs.push(a);
            }
        }
        algs.sort();
    }

    obs.log(
        LogLevel::Info,
        &format!(
            "{}: {} ({} {}, {} {}B), {} {}{}, {} {}, {} {}",
            crate::tr!("이미징 시작", "Imaging started"),
            format_bytes(total),
            total,
            crate::tr!("바이트", "bytes"),
            crate::tr!("섹터", "sector"),
            sector,
            crate::tr!("형식", "format"),
            spec.format.label(),
            if is_e01 { format!(" ({})", spec.compression.label()) } else { String::new() },
            if is_e01 { crate::tr!("조각", "segment") } else { crate::tr!("분할", "split") },
            if spec.split_bytes == 0 { crate::tr!("없음", "none").to_string() } else { format_bytes(spec.split_bytes) },
            crate::tr!("해시", "hash"),
            algs.iter().map(|a| a.label()).collect::<Vec<_>>().join(", ")
        ),
    );

    let started = Instant::now();
    let hashes = HashPipeline::new(&algs);
    let (wtx, wrx) = sync_channel::<WriterMsg>(3);
    let writer_h = if is_e01 {
        let w = E01Writer::new(store.clone(), spec.base_name.clone(), spec.split_bytes, spec.compression, spec.meta.clone(), sector as u32, total)?;
        std::thread::Builder::new().name("image-writer".into()).spawn(move || w.run(wrx))
    } else {
        let w = SplitWriter {
            store: store.clone(),
            ext: spec.ext.clone().unwrap_or_else(|| "dd".into()),
            base: spec.base_name.clone(),
            split: spec.split_bytes,
            cur: None,
            files: Vec::new(),
        };
        std::thread::Builder::new().name("image-writer".into()).spawn(move || w.run(wrx))
    }
    .context(crate::tr!("쓰기 스레드 생성 실패", "Failed to create writer thread"))?;

    let mut buf = AlignedBuf::new(CHUNK);
    let mut off: u64 = 0;
    let mut bad_ranges: Vec<BadRange> = Vec::new();
    let mut bad_sectors: u64 = 0;
    let mut cancelled = false;
    let mut read_err: Option<anyhow::Error> = None;
    let mut dead_chunks: u32 = 0;
    // 원본에서 실제로 읽힌 데이터가 있었는지 (머리말 같은 합성 부분 제외)
    let mut data_ok = false;
    let mut meter = RateMeter::new();
    let mut last_emit = Instant::now() - PROGRESS_EVERY;

    'read: while off < total {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            obs.log(LogLevel::Warn, &crate::tr!(format!("수집자가 중단했습니다 ({} 지점)", format_bytes(off)), format!("Cancelled by examiner (at {})", format_bytes(off))));
            break;
        }
        let want = ((total - off) as usize).min(CHUNK);
        let req = round_up(want, sector).min(CHUNK);

        match source.read_at(off, &mut buf[..req]) {
            Ok(n) if n >= want => {
                dead_chunks = 0;
                data_ok = true;
            }
            Ok(n) => {
                read_err = Some(anyhow!("{}", crate::tr!(format!("원본이 예상보다 짧습니다: {} 지점에서 {} 바이트만 읽힘", off, n), format!("Source is shorter than expected: only {1} bytes read at offset {0}", off, n))));
                break;
            }
            Err(e) if is_fatal_device_error(&e) => {
                read_err = Some(anyhow!("{}", crate::tr!(format!("원본 장치에 접근할 수 없어 중단했습니다 ({} 지점): {e}", off), format!("Stopped: cannot access the source device (at offset {}): {e}", off))));
                break;
            }
            Err(e) => {
                // 이 구간만 원본이 알려 주는 단위(디스크: 섹터, 메모리: 물리 페이지)로 다시 읽기
                let end = off + want as u64;
                let mut units = 0usize;
                let mut failed_here = 0usize;
                let mut first_bad: Option<u64> = None;
                let mut p = off;
                while p < end {
                    if cancel.load(Ordering::Relaxed) {
                        cancelled = true;
                        obs.log(LogLevel::Warn, &crate::tr!(format!("수집자가 중단했습니다 ({} 지점, 오류 구간 재시도 중)", format_bytes(off)), format!("Cancelled by examiner (at {}, while retrying an error range)", format_bytes(off))));
                        break 'read;
                    }
                    let (_, ue) = source.unit_at(p);
                    // 단위 끝이 앞으로 가지 않는 원본 오류를 막고, 버퍼(청크) 안으로 자름.
                    // 디스크는 섹터 전체를 읽어야 하므로(캐시 없는 읽기 정렬) 끝을 want가 아닌 버퍼 크기로 자름
                    let ue = ue.max(p + 1).min(off + buf.len() as u64);
                    let need = (ue.min(end) - p) as usize;
                    let so = (p - off) as usize;
                    let eo = (ue - off) as usize;
                    let synthetic = source.is_synthetic(p);
                    if !synthetic {
                        units += 1;
                    }
                    let mut ok = false;
                    let mut last = None;
                    for _ in 0..SECTOR_RETRIES {
                        match source.read_at(p, &mut buf[so..eo]) {
                            Ok(n) if n >= need => {
                                ok = true;
                                break;
                            }
                            Ok(_) => last = Some(io::Error::new(io::ErrorKind::UnexpectedEof, crate::tr!("짧게 읽힘", "Short read"))),
                            Err(e2) if is_fatal_device_error(&e2) => {
                                read_err = Some(anyhow!("{}", crate::tr!(format!("원본 장치에 접근할 수 없어 중단했습니다 ({} 지점): {e2}", p), format!("Stopped: cannot access the source device (at offset {}): {e2}", p))));
                                break 'read;
                            }
                            Err(e2) => last = Some(e2),
                        }
                    }
                    let cur = p;
                    p = ue.min(end);
                    if ok {
                        if !synthetic {
                            data_ok = true;
                        }
                        continue;
                    }
                    let abs = cur / sector as u64;
                    if !spec.zero_fill_errors {
                        let at = source.where_is(cur).unwrap_or_else(|| crate::tr!(format!("섹터 {abs}"), format!("sector {abs}")));
                        let why = last.map(|x| x.to_string()).unwrap_or_default();
                        read_err = Some(anyhow!("{}", crate::tr!(format!("읽기 오류로 중단: {at} ({why})"), format!("Stopped on read error: {at} ({why})"))));
                        break 'read;
                    }
                    buf[so..so + need].fill(0);
                    first_bad.get_or_insert(cur);
                    bad_sectors += 1;
                    failed_here += 1;
                    match bad_ranges.last_mut() {
                        Some(r) if r.start_sector + r.count == abs => r.count += 1,
                        Some(r) if r.start_sector + r.count > abs => {} // 같은 섹터 안의 두 단위 (메모리 머리말 경계)
                        _ => bad_ranges.push(BadRange { start_sector: abs, count: 1 }),
                    }
                }
                let sectors = units;
                if sectors > 0 && failed_here == sectors {
                    dead_chunks += 1;
                    if dead_chunks >= source.dead_run_limit() {
                        let (n, at) = (source.dead_run_limit(), format_bytes(off));
                        read_err = Some(anyhow!(
                            "{}",
                            crate::tr!(
                                format!("연속 {n} 구간을 전혀 읽지 못해 중단했습니다 ({at} 지점). 장치 연결 상태를 확인하세요"),
                                format!("Stopped: {n} consecutive ranges could not be read at all (at {at}). Check the device connection")
                            )
                        ));
                        break;
                    }
                } else {
                    dead_chunks = 0;
                }
                if failed_here > 0 && bad_ranges.len() <= MAX_BAD_RANGE_LOGS {
                    let at = first_bad.and_then(|b| source.where_is(b)).unwrap_or_else(|| crate::tr!(format!("{off} 지점"), format!("offset {off}")));
                    let unit = if source.where_is(off).is_some() { crate::tr!("페이지", "page") } else { crate::tr!("섹터", "sector") };
                    obs.log(LogLevel::Warn, &crate::tr!(format!("읽기 오류: {at}에서 {unit} {failed_here}개를 0으로 채움 ({e})"), format!("Read error: {failed_here} {unit}(s) at {at} zero-filled ({e})")));
                }
            }
        }

        let data: Arc<[u8]> = Arc::from(&buf[..want]);
        hashes.feed(&data);
        if wtx.send(WriterMsg::Data(data)).is_err() {
            // 쓰기 스레드가 오류로 끝남 → join에서 원인 확인
            break;
        }
        off += want as u64;

        if last_emit.elapsed() >= PROGRESS_EVERY || off == total {
            let rate = meter.push(off);
            obs.progress(&Progress {
                phase: Phase::Reading,
                done: off,
                total,
                rate_bps: rate,
                eta_secs: eta(total, off, rate),
                bad_sectors,
            });
            last_emit = Instant::now();
        }
    }

    // 원본에서 실제로 읽은 데이터가 하나도 없음(모두 0으로 채움): 0만 담긴 이미지를 "완료"로 만들지 않음
    // (메모리 원본이 통째로 막힌 경우 등. 메모리가 작으면 연속 실패 한도에 닿기 전에 끝나므로 따로 확인)
    if read_err.is_none() && !cancelled && off >= total && !data_ok {
        read_err = Some(anyhow!(crate::tr!("원본에서 한 바이트도 읽지 못했습니다 (모든 구간이 읽기 오류). 0만 담긴 이미지는 증거로 쓸 수 없어 실패로 처리합니다", "Not a single byte could be read from the source (every range was a read error). An all-zero image is not usable as evidence, so this is treated as a failure")));
    }
    let digests = hashes.finish();
    // 끝까지 읽었을 때만 마무리 정보(해시, 오류 섹터)를 넘김: 없으면 E01은 끝 섹션 없이 닫힘(끝나지 않은 파일)
    if let (Ok(d), false, None) = (&digests, cancelled, &read_err) {
        let _ = wtx.send(WriterMsg::Finish(Trailer {
            md5: d.get(&HashAlg::Md5).and_then(|h| e01::unhex::<16>(h)),
            sha1: d.get(&HashAlg::Sha1).and_then(|h| e01::unhex::<20>(h)),
            bad_ranges: bad_ranges.clone(),
        }));
    }
    drop(wtx);
    let files = match writer_h.join() {
        Ok(Ok(f)) => f,
        Ok(Err(e)) => return Err(anyhow!("{}", crate::tr!(format!("저장 매체에 쓰기 실패: {e}"), format!("Write to output media failed: {e}")))),
        Err(_) => return Err(anyhow!(crate::tr!("쓰기 스레드가 비정상 종료했습니다", "Writer thread terminated abnormally"))),
    };
    let digests = digests?;
    if let Some(e) = read_err {
        return Err(e);
    }
    let read_secs = started.elapsed().as_secs_f64();

    if bad_sectors > 0 {
        obs.log(
            LogLevel::Warn,
            &crate::tr!(format!("읽기 오류: 섹터 {}개 ({}개 구간)를 0으로 채움", bad_sectors, bad_ranges.len()), format!("Read errors: {} sector(s) in {} range(s) zero-filled", bad_sectors, bad_ranges.len())),
        );
        if is_e01 && bad_ranges.iter().any(|r| r.start_sector > u32::MAX as u64) {
            obs.log(
                LogLevel::Warn,
                crate::tr!(
                    "E01 오류 목록(error2)은 섹터 번호가 32비트라 2^32번 섹터 뒤의 오류 위치는 E01 안에 적지 못했습니다 (이 로그와 보고서에는 모두 있음)",
                    "The E01 error list (error2) uses 32-bit sector numbers, so error locations beyond sector 2^32 could not be recorded in the E01 (all are in this log and the report)"
                ),
            );
        }
    }
    let written: u64 = files.iter().map(|f| f.size).sum();
    if !cancelled {
        obs.log(
            LogLevel::Info,
            &format!(
                "{}: {} ({}), {}{}",
                crate::tr!("읽기 완료", "Read complete"),
                format_bytes(off),
                format_duration(read_secs as u64),
                crate::tr!(format!("파일 {}개", files.len()), format!("{} file(s)", files.len())),
                if is_e01 && off > 0 {
                    crate::tr!(format!(", E01 저장 크기 {} (원본의 {:.1}%)", format_bytes(written), written as f64 * 100.0 / off as f64), format!(", E01 stored size {} ({:.1}% of source)", format_bytes(written), written as f64 * 100.0 / off as f64))
                } else {
                    String::new()
                }
            ),
        );
        for (alg, h) in &digests {
            obs.log(LogLevel::Info, &format!("{} {}", alg.label(), h));
        }
    }

    let mut result = ImageResult {
        files,
        total_bytes: off,
        stored_bytes: written,
        format: spec.format,
        sector_size: sector as u32,
        hashes: digests,
        verify: None,
        bad_ranges,
        bad_sectors,
        cancelled,
        read_secs,
    };

    if spec.verify && !cancelled {
        // 검증 실패가 이미징 결과(해시)를 버리게 하지 않음
        match verify_files(store.as_ref(), &result, &algs, cancel, obs) {
            Ok(Some(v)) => result.verify = Some(v),
            Ok(None) => result.cancelled = true,
            Err(e) => {
                let msg = format!("{e:#}");
                obs.log(LogLevel::Error, &crate::tr!(format!("검증을 끝내지 못했습니다: {msg}"), format!("Verification could not be completed: {msg}")));
                result.verify = Some(VerifyResult { hashes: Default::default(), matched: false, secs: 0.0, error: Some(msg) });
            }
        }
    }
    Ok(result)
}

/// 저장된 파일을 캐시 없이 다시 읽어 해시를 비교합니다. 중단되면 `Ok(None)`.
pub fn verify_files(
    store: &dyn EvidenceStore,
    result: &ImageResult,
    algs: &[HashAlg],
    cancel: &AtomicBool,
    obs: &mut dyn Observer,
) -> Result<Option<VerifyResult>> {
    obs.log(
        LogLevel::Info,
        if result.format == ImageFormat::E01 {
            crate::tr!(
                "검증 시작: 저장한 E01을 다시 읽어 풀고, 해시를 원본, E01에 적힌 값과 비교합니다",
                "Verification started: re-reading and decompressing the stored E01, comparing hashes with the source and the values stored in the E01"
            )
        } else {
            crate::tr!("검증 시작: 저장한 파일을 다시 읽어 해시를 비교합니다", "Verification started: re-reading the stored files and comparing hashes")
        },
    );
    let started = Instant::now();
    let total = result.total_bytes;
    let bad = result.bad_sectors;
    let mut meter = RateMeter::new();
    // 진행, 로그 콜백이 같은 Observer를 쓰므로 RefCell로 나눠 빌림
    let obs_cell = std::cell::RefCell::new(obs);
    let mut on_progress = |done: u64, _t: u64| {
        let rate = meter.push(done);
        obs_cell.borrow_mut().progress(&Progress {
            phase: Phase::Verifying,
            done,
            total,
            rate_bps: rate,
            eta_secs: eta(total, done, rate),
            bad_sectors: bad,
        });
    };
    let mut on_log = |level: LogLevel, msg: &str| obs_cell.borrow_mut().log(level, msg);
    let out = store.verify(&result.files, result.format, algs, cancel, &mut on_progress, &mut on_log)?;
    let obs = obs_cell.into_inner();
    let Some((hashes, done)) = out else {
        obs.log(LogLevel::Warn, crate::tr!("검증을 중단했습니다", "Verification cancelled"));
        return Ok(None);
    };
    let matched = done == total && algs.iter().all(|a| hashes.get(a).is_some() && hashes.get(a) == result.hashes.get(a));
    obs.log(
        if matched { LogLevel::Info } else { LogLevel::Error },
        if matched {
            crate::tr!("검증 일치: 저장 파일의 해시가 원본과 같습니다", "Verification matched: stored file hashes are identical to the source")
        } else {
            crate::tr!("검증 불일치: 저장 파일의 해시가 원본과 다릅니다", "Verification mismatch: stored file hashes differ from the source")
        },
    );
    Ok(Some(VerifyResult { hashes, matched, secs: started.elapsed().as_secs_f64(), error: None }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::HashPipeline;
    use std::path::Path;
    use std::collections::HashSet;

    struct Quiet;
    impl Observer for Quiet {
        fn progress(&mut self, _: &Progress) {}
        fn log(&mut self, _: LogLevel, _: &str) {}
    }

    /// 메모리 원본, 지정한 섹터는 항상 읽기 실패
    struct MemSource {
        data: Vec<u8>,
        bad: HashSet<u64>,
    }
    impl Source for MemSource {
        fn len(&self) -> u64 {
            self.data.len() as u64
        }
        fn sector_size(&self) -> u32 {
            512
        }
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
            let first = offset / 512;
            let last = (offset + buf.len() as u64).div_ceil(512);
            if (first..last).any(|s| self.bad.contains(&s)) {
                return Err(io::Error::other("CRC 오류 (시험)"));
            }
            let start = offset as usize;
            if start >= self.data.len() {
                return Ok(0);
            }
            let n = buf.len().min(self.data.len() - start);
            buf[..n].copy_from_slice(&self.data[start..start + n]);
            Ok(n)
        }
    }

    fn data(len: usize) -> Vec<u8> {
        let mut x: u32 = 0x1234_5678;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    fn digests_of(d: &[u8]) -> Digests {
        let p = HashPipeline::new(&HashAlg::ALL);
        p.feed(&Arc::from(d));
        p.finish().unwrap()
    }

    fn spec(dir: &Path, split: u64) -> ImageSpec {
        ImageSpec {
            out_dir: dir.to_path_buf(),
            base_name: "EV-T_HOST_20261105_Disk9".into(),
            split_bytes: split,
            algs: HashAlg::ALL.to_vec(),
            verify: true,
            zero_fill_errors: true,
            max_bytes: None,
            ext: None,
            format: ImageFormat::Raw,
            compression: Default::default(),
            meta: Default::default(),
        }
    }

    fn concat(dir: &Path, files: &[OutputFile]) -> Vec<u8> {
        files.iter().flat_map(|f| std::fs::read(dir.join(&f.name)).unwrap()).collect()
    }

    #[test]
    fn split_and_verify() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(10 * 1024 * 1024 + 4096);
        let mut src = MemSource { data: d.clone(), bad: HashSet::new() };
        let r = run_image(&mut src, &spec(tmp.path(), 3 << 20), &AtomicBool::new(false), &mut Quiet).unwrap();
        assert_eq!(r.files.len(), 4);
        assert_eq!(r.files[0].name, "EV-T_HOST_20261105_Disk9.001");
        assert_eq!(r.files[3].size, (1 << 20) + 4096);
        assert_eq!(r.total_bytes, d.len() as u64);
        assert_eq!(r.hashes, digests_of(&d));
        assert!(r.verify.as_ref().unwrap().matched);
        assert_eq!(concat(tmp.path(), &r.files), d);
    }

    #[test]
    fn single_file_without_split() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(5 * 1024 * 1024);
        let mut src = MemSource { data: d.clone(), bad: HashSet::new() };
        let r = run_image(&mut src, &spec(tmp.path(), 0), &AtomicBool::new(false), &mut Quiet).unwrap();
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].name, "EV-T_HOST_20261105_Disk9.dd");
    }

    #[test]
    fn bad_sectors_are_zero_filled_and_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(6 * 1024 * 1024);
        let bad: HashSet<u64> = [100u64, 101, 102, 9000].into_iter().collect();
        let mut src = MemSource { data: d.clone(), bad: bad.clone() };
        let r = run_image(&mut src, &spec(tmp.path(), 0), &AtomicBool::new(false), &mut Quiet).unwrap();
        assert_eq!(r.bad_sectors, 4);
        assert_eq!(
            r.bad_ranges,
            vec![BadRange { start_sector: 100, count: 3 }, BadRange { start_sector: 9000, count: 1 }]
        );
        let mut expect = d.clone();
        for s in &bad {
            let o = (*s as usize) * 512;
            expect[o..o + 512].fill(0);
        }
        assert_eq!(concat(tmp.path(), &r.files), expect);
        assert_eq!(r.hashes, digests_of(&expect));
        assert!(r.verify.unwrap().matched);
    }

    #[test]
    fn abort_on_error_policy() {
        let tmp = tempfile::tempdir().unwrap();
        let mut src = MemSource { data: data(2 << 20), bad: [7u64].into_iter().collect() };
        let mut s = spec(tmp.path(), 0);
        s.zero_fill_errors = false;
        let e = run_image(&mut src, &s, &AtomicBool::new(false), &mut Quiet).unwrap_err();
        assert!(e.to_string().contains("섹터 7"));
    }

    #[test]
    fn never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("EV-T_HOST_20261105_Disk9.dd"), b"old").unwrap();
        let mut src = MemSource { data: data(1 << 20), bad: HashSet::new() };
        assert!(run_image(&mut src, &spec(tmp.path(), 0), &AtomicBool::new(false), &mut Quiet).is_err());
        assert_eq!(std::fs::read(tmp.path().join("EV-T_HOST_20261105_Disk9.dd")).unwrap(), b"old");
    }

    #[test]
    fn cancel_stops_early() {
        let tmp = tempfile::tempdir().unwrap();
        let mut src = MemSource { data: data(20 << 20), bad: HashSet::new() };
        let r = run_image(&mut src, &spec(tmp.path(), 0), &AtomicBool::new(true), &mut Quiet).unwrap();
        assert!(r.cancelled);
        assert!(r.verify.is_none());
        assert_eq!(r.total_bytes, 0);
    }

    /// 장치가 사라진 오류(1167)는 0으로 채우지 않고 즉시 중단
    struct GoneSource;
    impl Source for GoneSource {
        fn len(&self) -> u64 {
            8 << 20
        }
        fn sector_size(&self) -> u32 {
            512
        }
        fn read_at(&mut self, offset: u64, _buf: &mut [u8]) -> io::Result<usize> {
            if offset >= (4 << 20) {
                // 장치 분리: Windows ERROR_DEVICE_NOT_CONNECTED, Linux ENODEV
                #[cfg(windows)]
                return Err(io::Error::from_raw_os_error(1167));
                #[cfg(unix)]
                return Err(io::Error::from_raw_os_error(libc::ENODEV));
            } else {
                Ok(_buf.len())
            }
        }
    }

    #[test]
    fn device_gone_aborts_instead_of_zero_fill() {
        let tmp = tempfile::tempdir().unwrap();
        let e = run_image(&mut GoneSource, &spec(tmp.path(), 0), &AtomicBool::new(false), &mut Quiet).unwrap_err();
        assert!(e.to_string().contains("접근할 수 없어"), "{e}");
    }

    /// 모든 섹터가 계속 실패하면 일정 구간 뒤 중단
    #[test]
    fn dead_device_stops_after_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let len = 80 << 20;
        let bad: HashSet<u64> = ((8u64 << 20) / 512..(len as u64) / 512).collect();
        let mut src = MemSource { data: data(len), bad };
        let e = run_image(&mut src, &spec(tmp.path(), 0), &AtomicBool::new(false), &mut Quiet).unwrap_err();
        assert!(e.to_string().contains("전혀 읽지 못해"), "{e}");
    }

    #[test]
    fn max_bytes_limits_read() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(9 << 20);
        let mut src = MemSource { data: d.clone(), bad: HashSet::new() };
        let mut s = spec(tmp.path(), 0);
        s.max_bytes = Some(5 << 20);
        let r = run_image(&mut src, &s, &AtomicBool::new(false), &mut Quiet).unwrap();
        assert_eq!(r.total_bytes, 5 << 20);
        assert_eq!(r.hashes, digests_of(&d[..5 << 20]));
    }

    // --- E01 ---------------------------------------------------------------

    /// 압축이 되는 부분(0)과 안 되는 부분(난수)이 섞인 원본
    fn mixed(len: usize) -> Vec<u8> {
        let mut d = data(len);
        for (i, b) in d.iter_mut().enumerate() {
            if (i / (1 << 20)) % 2 == 0 {
                *b = 0;
            }
        }
        d
    }

    fn e01_spec(dir: &Path, split: u64) -> ImageSpec {
        let mut s = spec(dir, split);
        s.format = ImageFormat::E01;
        s.meta.case_number = "2026-테스트".into();
        s.meta.evidence_number = "EV-T".into();
        s.meta.description = "탭\t줄바꿈\n있는 설명".into();
        s
    }

    fn verify_again(dir: &Path, r: &ImageResult) -> Result<Option<(Digests, u64)>> {
        crate::e01::verify_e01(dir, &r.files, &HashAlg::ALL, &AtomicBool::new(false), &mut |_, _| {}, &mut |_, _| {})
    }

    #[test]
    fn e01_single_file_with_bad_sectors() {
        let tmp = tempfile::tempdir().unwrap();
        let d = mixed(10 * 1024 * 1024 + 4096);
        let bad: HashSet<u64> = [100u64, 101, 5000].into_iter().collect();
        let mut src = MemSource { data: d.clone(), bad: bad.clone() };
        let r = run_image(&mut src, &e01_spec(tmp.path(), 0), &AtomicBool::new(false), &mut Quiet).unwrap();
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].name, "EV-T_HOST_20261105_Disk9.E01");
        assert_eq!(r.total_bytes, d.len() as u64);
        assert!(r.stored_bytes < d.len() as u64 * 6 / 10, "0 구간이 압축되어야 함 ({})", r.stored_bytes);
        let mut expect = d.clone();
        for s in &bad {
            let o = (*s as usize) * 512;
            expect[o..o + 512].fill(0);
        }
        assert_eq!(r.hashes, digests_of(&expect));
        assert!(r.verify.as_ref().unwrap().matched, "{:?}", r.verify);
        let (h, n) = verify_again(tmp.path(), &r).unwrap().unwrap();
        assert_eq!(n, d.len() as u64);
        assert_eq!(h, digests_of(&expect));
    }

    #[test]
    fn e01_segments_when_split() {
        let tmp = tempfile::tempdir().unwrap();
        // 압축 안 되는 40MB → 16MB 조각 3개 이상
        let d = data(40 << 20);
        let mut src = MemSource { data: d.clone(), bad: HashSet::new() };
        let r = run_image(&mut src, &e01_spec(tmp.path(), 16 << 20), &AtomicBool::new(false), &mut Quiet).unwrap();
        assert!(r.files.len() >= 3, "{:?}", r.files);
        assert_eq!(r.files[1].name, "EV-T_HOST_20261105_Disk9.E02");
        assert!(r.files.iter().all(|f| f.size <= 16 << 20), "{:?}", r.files);
        assert!(r.verify.as_ref().unwrap().matched);
        assert_eq!(r.hashes, digests_of(&d));
    }

    #[test]
    fn e01_no_compression_and_fast() {
        for c in [Compression::None, Compression::Fast] {
            let tmp = tempfile::tempdir().unwrap();
            let d = mixed(3 << 20);
            let mut src = MemSource { data: d.clone(), bad: HashSet::new() };
            let mut s = e01_spec(tmp.path(), 0);
            s.compression = c;
            let r = run_image(&mut src, &s, &AtomicBool::new(false), &mut Quiet).unwrap();
            assert!(r.verify.as_ref().unwrap().matched, "{c:?}");
        }
    }

    #[test]
    fn e01_cancelled_is_not_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(30 << 20);
        let cancel = AtomicBool::new(false);
        struct CancelAfter<'a>(&'a AtomicBool);
        impl Observer for CancelAfter<'_> {
            fn progress(&mut self, p: &Progress) {
                if p.done >= 8 << 20 {
                    self.0.store(true, Ordering::Relaxed);
                }
            }
            fn log(&mut self, _: LogLevel, _: &str) {}
        }
        // 진행 알림이 200ms 간격이라 느린 원본으로 중단 시점을 만듦
        struct Slow(MemSource);
        impl Source for Slow {
            fn len(&self) -> u64 {
                self.0.len()
            }
            fn sector_size(&self) -> u32 {
                512
            }
            fn read_at(&mut self, o: u64, b: &mut [u8]) -> io::Result<usize> {
                std::thread::sleep(Duration::from_millis(60));
                self.0.read_at(o, b)
            }
        }
        let mut src = Slow(MemSource { data: d, bad: HashSet::new() });
        let r = run_image(&mut src, &e01_spec(tmp.path(), 0), &cancel, &mut CancelAfter(&cancel)).unwrap();
        assert!(r.cancelled);
        assert_eq!(r.files.len(), 1);
        let e = verify_again(tmp.path(), &r).unwrap_err();
        assert!(e.to_string().contains("끝나지 않았") || e.to_string().contains("짧습니다"), "{e}");
    }

    #[test]
    fn e01_detects_tampering() {
        let tmp = tempfile::tempdir().unwrap();
        let d = data(2 << 20);
        let mut src = MemSource { data: d, bad: HashSet::new() };
        let mut s = e01_spec(tmp.path(), 0);
        s.verify = false;
        let r = run_image(&mut src, &s, &AtomicBool::new(false), &mut Quiet).unwrap();
        let p = tmp.path().join(&r.files[0].name);
        let mut b = std::fs::read(&p).unwrap();
        let mid = b.len() / 2;
        b[mid] ^= 0xFF;
        std::fs::write(&p, b).unwrap();
        assert!(verify_again(tmp.path(), &r).is_err());
    }

    #[test]
    fn e01_4k_sector_source() {
        struct Big(MemSource);
        impl Source for Big {
            fn len(&self) -> u64 {
                self.0.len()
            }
            fn sector_size(&self) -> u32 {
                4096
            }
            fn read_at(&mut self, o: u64, b: &mut [u8]) -> io::Result<usize> {
                self.0.read_at(o, b)
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let d = mixed(5 << 20);
        let mut src = Big(MemSource { data: d.clone(), bad: HashSet::new() });
        let r = run_image(&mut src, &e01_spec(tmp.path(), 0), &AtomicBool::new(false), &mut Quiet).unwrap();
        assert_eq!(r.sector_size, 4096);
        assert!(r.verify.as_ref().unwrap().matched);
        assert_eq!(r.hashes, digests_of(&d));
    }

    /// E01에는 MD5,SHA-1이 늘 들어가야 하므로 SHA-256만 골라도 함께 계산
    #[test]
    fn e01_always_has_md5_sha1() {
        let tmp = tempfile::tempdir().unwrap();
        let mut src = MemSource { data: data(1 << 20), bad: HashSet::new() };
        let mut s = e01_spec(tmp.path(), 0);
        s.algs = vec![HashAlg::Sha256];
        let r = run_image(&mut src, &s, &AtomicBool::new(false), &mut Quiet).unwrap();
        assert!(r.hashes.contains_key(&HashAlg::Md5) && r.hashes.contains_key(&HashAlg::Sha1));
        assert!(r.verify.unwrap().matched);
    }

    /// 섹터 배수가 아닌 원본은 E01로 저장하지 않음 (검증이 항상 실패하는 파일을 만들지 않게)
    #[test]
    fn e01_rejects_non_sector_size() {
        let tmp = tempfile::tempdir().unwrap();
        let mut src = MemSource { data: data((1 << 20) + 100), bad: HashSet::new() };
        let e = run_image(&mut src, &e01_spec(tmp.path(), 0), &AtomicBool::new(false), &mut Quiet).unwrap_err();
        assert!(e.to_string().contains("섹터"), "{e}");
        // 시험용 앞부분 읽기는 섹터 배수로 내려서 진행
        let mut s = e01_spec(tmp.path(), 0);
        s.max_bytes = Some((1 << 20) + 100);
        let r = run_image(&mut src, &s, &AtomicBool::new(false), &mut Quiet).unwrap();
        assert_eq!(r.total_bytes, 1 << 20);
        assert!(r.verify.unwrap().matched);
    }

    /// 중단된 E01은 이름에 .incomplete가 붙어 완전한 증거 파일처럼 보이지 않음
    #[test]
    fn e01_cancelled_files_marked_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        struct StopAfter<'a>(&'a AtomicBool, MemSource, u32);
        impl Source for StopAfter<'_> {
            fn len(&self) -> u64 {
                self.1.len()
            }
            fn sector_size(&self) -> u32 {
                512
            }
            fn read_at(&mut self, o: u64, b: &mut [u8]) -> io::Result<usize> {
                self.2 += 1;
                if self.2 == 3 {
                    self.0.store(true, Ordering::Relaxed);
                }
                self.1.read_at(o, b)
            }
        }
        let mut src = StopAfter(&cancel, MemSource { data: data(40 << 20), bad: HashSet::new() }, 0);
        let r = run_image(&mut src, &e01_spec(tmp.path(), 0), &cancel, &mut Quiet).unwrap();
        assert!(r.cancelled);
        assert!(r.files.iter().all(|f| f.name.ends_with(".E01.incomplete")), "{:?}", r.files);
        assert!(tmp.path().join(&r.files[0].name).exists());
        assert!(!tmp.path().join("EV-T_HOST_20261105_Disk9.E01").exists());
    }

    /// 저장 매체 쓰기가 실패하면 원본을 끝까지 읽지 않고 곧 멈춤 (불량 디스크를 몇 시간 더 읽지 않게)
    #[test]
    fn e01_write_failure_stops_reading_soon() {
        struct FullStore(PathBuf);
        struct FullFile(u64);
        impl StoreFile for FullFile {
            fn write_all(&mut self, d: &[u8]) -> io::Result<()> {
                self.0 += d.len() as u64;
                if self.0 > 8 << 20 {
                    return Err(io::Error::other("디스크 가득 참 (시험)"));
                }
                Ok(())
            }
            fn finish(self: Box<Self>) -> io::Result<()> {
                Ok(())
            }
        }
        impl EvidenceStore for FullStore {
            fn location(&self) -> String {
                self.0.display().to_string()
            }
            fn create(&self, _: &str) -> io::Result<Box<dyn StoreFile>> {
                Ok(Box::new(FullFile(0)))
            }
            fn verify(&self, _: &[OutputFile], _: ImageFormat, _: &[HashAlg], _: &AtomicBool, _: crate::store::VerifyProgress, _: crate::store::VerifyLog) -> Result<Option<(Digests, u64)>> {
                Ok(None)
            }
        }
        struct Counting(MemSource, u64);
        impl Source for Counting {
            fn len(&self) -> u64 {
                self.0.len()
            }
            fn sector_size(&self) -> u32 {
                512
            }
            fn read_at(&mut self, o: u64, b: &mut [u8]) -> io::Result<usize> {
                self.1 += b.len() as u64;
                self.0.read_at(o, b)
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        // 압축 안 되는 400MB: 8MB에서 쓰기 실패
        let mut src = Counting(MemSource { data: data(400 << 20), bad: HashSet::new() }, 0);
        let store: Arc<dyn EvidenceStore> = Arc::new(FullStore(tmp.path().to_path_buf()));
        let e = run_image_to(&mut src, &e01_spec(tmp.path(), 0), store, &AtomicBool::new(false), &mut Quiet).unwrap_err();
        assert!(e.to_string().contains("가득"), "{e}");
        assert!(src.1 < 200 << 20, "쓰기 실패 뒤에도 {}MB를 더 읽음", src.1 >> 20);
    }

    #[test]
    fn e01_segment_names() {
        use crate::e01::segment_name;
        assert_eq!(segment_name("a", 1).unwrap(), "a.E01");
        assert_eq!(segment_name("a", 99).unwrap(), "a.E99");
        assert_eq!(segment_name("a", 100).unwrap(), "a.EAA");
        assert_eq!(segment_name("a", 101).unwrap(), "a.EAB");
        assert_eq!(segment_name("a", 126).unwrap(), "a.EBA");
        assert_eq!(segment_name("a", 99 + 676 * 21).unwrap(), "a.YZZ");
        assert!(segment_name("a", 99 + 676 * 21 + 1).is_none());
    }

    /// 시험용 E01 파일을 남김 (libewf ewfverify 교차 확인용): WS_E01_OUT=폴더 cargo test e01_export -- --ignored
    #[test]
    #[ignore]
    fn e01_export_for_libewf() {
        let dir = PathBuf::from(std::env::var("WS_E01_OUT").expect("WS_E01_OUT"));
        let d = mixed(12 * 1024 * 1024 + 512);
        let bad: HashSet<u64> = [2048u64, 2049, 9000].into_iter().collect();
        let mut expect = d.clone();
        for s in &bad {
            let o = (*s as usize) * 512;
            expect[o..o + 512].fill(0);
        }
        std::fs::write(dir.join("expect.raw"), &expect).unwrap();
        for (name, split) in [("single", 0u64), ("split", 16 << 20)] {
            let sub = dir.join(name);
            std::fs::create_dir_all(&sub).unwrap();
            let mut src = MemSource { data: if name == "split" { data(40 << 20) } else { d.clone() }, bad: if name == "split" { HashSet::new() } else { bad.clone() } };
            let r = run_image(&mut src, &e01_spec(&sub, split), &AtomicBool::new(false), &mut Quiet).unwrap();
            std::fs::write(sub.join("hashes.txt"), format!("{:?}", r.hashes)).unwrap();
        }
    }
}
