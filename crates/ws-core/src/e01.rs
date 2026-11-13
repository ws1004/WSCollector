//! E01(EWF, EnCase 6 방식) 쓰기, 검증.
//!
//! 형식 근거: libewf 문서 "Expert Witness Compression Format (EWF)" (github.com/libyal/libewf),
//! WSImager `ewf.rs`의 구조 상수. 바이트 배치는 아래 함수 주석에 적었습니다.
//!
//! 설계:
//! - **앞에서부터 차례로만 씀** (이미 쓴 위치로 되돌아가지 않음). 그래서 로컬 폴더와 네트워크 수신 PC
//!   (`EvidenceStore`)에 똑같이 저장됩니다. sectors 섹션 크기를 미리 알아야 하므로 청크를
//!   묶음(최대 2048개 = 64MB) 단위로 모아 압축한 뒤 sectors → table → table2를 한 번에 씁니다.
//! - 묶음마다 table의 base_offset을 그 묶음 데이터 시작으로 둡니다 (EnCase 6 방식).
//!   항목 오프셋(31비트)이 묶음 안에서만 쓰이므로 **파일 하나가 2GB를 넘어도 됩니다** (조각 0 = 나누지 않음).
//! - 압축은 여러 스레드가 4MB 블록 단위로 나눠 하고, 쓰는 쪽이 순서대로 다시 모읍니다.
//! - 끝: error2(읽기 오류 섹터) → digest(MD5,SHA-1) → hash(MD5) → done.
//!   중단, 실패로 끝 섹션이 없으면 "끝나지 않은 E01"이고, 검증은 이를 실패로 봅니다.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use flate2::write::ZlibEncoder;
use serde::{Deserialize, Serialize};

use crate::hash::{Digests, HashAlg, HashPipeline};
use crate::imaging::{BadRange, OutputFile};
use crate::logbook::LogLevel;
use crate::source::{drop_cache, is_alignment_error, open_for_verify, read_aligned, AlignedBuf};
use crate::store::{check_file_name, EvidenceStore, StoreFile, VerifyLog, VerifyProgress};

// --- 형식 상수 -------------------------------------------------------------

/// "EVF" 09 0D 0A FF 00
const EVF_SIGNATURE: [u8; 8] = [0x45, 0x56, 0x46, 0x09, 0x0d, 0x0a, 0xff, 0x00];
const FILE_HEADER_SIZE: u64 = 13;
const DESC: u64 = 76;
const VOLUME_SIZE: usize = 1052;
const TABLE_HDR: u64 = 24;
const ENTRY_COMPRESSED: u32 = 0x8000_0000;
const ENTRY_OFFSET_MASK: u32 = 0x7FFF_FFFF;

/// 청크 크기 (EnCase 기본 64섹터 × 512B. 4Kn 디스크는 8섹터 × 4096B)
pub const CHUNK_BYTES: usize = 32 * 1024;
/// sectors+table 묶음 하나의 최대 청크 수 (압축 안 될 때 약 64MB를 메모리에 모음)
const GROUP_CHUNKS: usize = 2048;
/// FAT32 저장 매체일 때 자동으로 쓰는 조각 크기 (파일 하나 4GB-1 한계보다 충분히 작게)
pub const FAT32_SEGMENT: u64 = 4000 << 20;
/// 사용자가 지정할 수 있는 가장 작은 조각
pub const MIN_SEGMENT: u64 = 16 << 20;
/// 조각 이름 한계: E01..E99 + EAA..YZZ (libewf 문서 기준)
const MAX_SEGMENTS: u32 = 99 + 676 * 21;
/// 조각 끝에 남겨 둘 자리 (next 또는 error2,digest, hash, done)
const TAIL_RESERVE: u64 = 4096;
/// 검증 시 한 sectors 섹션을 메모리에 올리는 상한 (우리가 쓰는 묶음은 약 66MB 이하)
const MAX_SECTORS_SECTION: u64 = 512 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    None,
    Fast,
    #[default]
    Best,
}

impl Compression {
    fn zlib_level(self) -> u32 {
        match self {
            Compression::None => 0,
            Compression::Fast => 1,
            Compression::Best => 9,
        }
    }
    /// volume 섹션의 압축 단계 (0 없음, 1 빠름, 2 최대)
    fn volume_code(self) -> u8 {
        match self {
            Compression::None => 0,
            Compression::Fast => 1,
            Compression::Best => 2,
        }
    }
    /// header 섹션의 r 값
    fn header_code(self) -> &'static str {
        match self {
            Compression::None => "n",
            Compression::Fast => "f",
            Compression::Best => "b",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Compression::None => crate::tr!("압축 안 함", "No compression"),
            Compression::Fast => crate::tr!("빠른 압축", "Fast compression"),
            Compression::Best => crate::tr!("최대 압축", "Best compression"),
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "0" | "no" => Some(Compression::None),
            "fast" | "1" => Some(Compression::Fast),
            "best" | "9" | "max" => Some(Compression::Best),
            _ => None,
        }
    }
}

/// E01 머리말에 들어가는 정보
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct E01Meta {
    pub case_number: String,
    pub evidence_number: String,
    pub examiner: String,
    pub notes: String,
    /// 대상 설명 (예: "/dev/sda, Samsung SSD, S/N …")
    pub description: String,
    pub model: String,
    pub serial: String,
    /// 수집한 시스템의 OS
    pub os: String,
    /// 물리 디스크 전체면 true (파티션, 볼륨, 파일이면 false)
    pub physical: bool,
}

/// 읽기가 끝난 뒤 쓰는 쪽에 넘기는 마무리 정보
pub struct Trailer {
    pub md5: Option<[u8; 16]>,
    pub sha1: Option<[u8; 20]>,
    pub bad_ranges: Vec<BadRange>,
}

/// 첫 조각 이름은 `.E01`, 다음은 `.E02` … `.E99`, `.EAA` … `.YZZ`
pub fn segment_name(base: &str, n: u32) -> Option<String> {
    if n == 0 || n > MAX_SEGMENTS {
        return None;
    }
    if n <= 99 {
        return Some(format!("{base}.E{n:02}"));
    }
    let i = n - 100;
    let a = (b'E' + (i / 676) as u8) as char;
    let b = (b'A' + ((i / 26) % 26) as u8) as char;
    let c = (b'A' + (i % 26) as u8) as char;
    Some(format!("{base}.{a}{b}{c}"))
}

// --- 체크섬, 기본 구조 ------------------------------------------------------

/// Adler-32 (초기값 1, zlib과 같음)
pub fn adler32(data: &[u8]) -> u32 {
    const M: u32 = 65521;
    // 5552바이트마다 나머지 연산 (zlib NMAX): u32 넘침 없이 빠르게
    let (mut a, mut b) = (1u32, 0u32);
    for block in data.chunks(5552) {
        for &x in block {
            a += x as u32;
            b += a;
        }
        a %= M;
        b %= M;
    }
    (b << 16) | a
}

/// 섹션 디스크립터 (76B): 종류[16] | 다음 섹션 위치[8] | 섹션 크기[8] | 0[40] | Adler-32(앞 72B)[4]
fn descriptor(kind: &[u8], next: u64, size: u64) -> [u8; 76] {
    let mut d = [0u8; 76];
    d[..kind.len().min(15)].copy_from_slice(&kind[..kind.len().min(15)]);
    d[16..24].copy_from_slice(&next.to_le_bytes());
    d[24..32].copy_from_slice(&size.to_le_bytes());
    let c = adler32(&d[..72]);
    d[72..76].copy_from_slice(&c.to_le_bytes());
    d
}

/// 파일 머리 (13B): 서명[8] | 1 | 조각 번호[2] | 0 0
fn file_header(segment: u16) -> [u8; 13] {
    let mut h = [0u8; 13];
    h[..8].copy_from_slice(&EVF_SIGNATURE);
    h[8] = 1;
    h[9..11].copy_from_slice(&segment.to_le_bytes());
    h
}

fn zlib(data: &[u8], level: u32) -> io::Result<Vec<u8>> {
    let mut e = ZlibEncoder::new(Vec::with_capacity(data.len() / 2 + 64), flate2::Compression::new(level));
    e.write_all(data)?;
    e.finish()
}

fn unzlib(data: &[u8], limit: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(limit);
    // 한계보다 1바이트 더 읽어 봐서 넘치면 오류 (압축 폭탄, 손상 방지)
    flate2::read::ZlibDecoder::new(data).take(limit as u64 + 1).read_to_end(&mut out)?;
    if out.len() > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, crate::tr!("청크가 너무 크게 풀립니다", "Chunk decompresses to more than the allowed size")));
    }
    Ok(out)
}

/// 머리말 값에 탭, 줄바꿈이 들어가면 칸이 어긋나므로 공백으로
fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string()
}

fn utf16le_bom(s: &str) -> Vec<u8> {
    let mut v = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

// --- 쓰기 ------------------------------------------------------------------

struct Chunk {
    /// 압축이면 zlib 스트림, 아니면 원본 + Adler-32(4B)
    data: Vec<u8>,
    compressed: bool,
}

fn compress_block(block: &[u8], c: Compression) -> io::Result<Vec<Chunk>> {
    let mut out = Vec::with_capacity(block.len().div_ceil(CHUNK_BYTES));
    for raw in block.chunks(CHUNK_BYTES) {
        if c != Compression::None {
            let z = zlib(raw, c.zlib_level())?;
            if z.len() < raw.len() {
                out.push(Chunk { data: z, compressed: true });
                continue;
            }
        }
        let mut d = Vec::with_capacity(raw.len() + 4);
        d.extend_from_slice(raw);
        d.extend_from_slice(&adler32(raw).to_le_bytes());
        out.push(Chunk { data: d, compressed: false });
    }
    Ok(out)
}

struct Seg {
    f: Box<dyn StoreFile>,
    name: String,
    pos: u64,
    groups: u32,
}

/// 분배 스레드가 쓰기보다 `window` 블록 이상 앞서지 않게 하는 문
struct Gate {
    /// (쓴 블록 수, 중단)
    state: Mutex<(u64, bool)>,
    cv: Condvar,
}

impl Gate {
    /// seq번 블록을 보내도 되면 true, 중단이면 false
    fn wait_room(&self, seq: u64, window: u64) -> bool {
        let mut g = self.state.lock().unwrap_or_else(|p| p.into_inner());
        while !g.1 && seq >= g.0 + window {
            g = self.cv.wait(g).unwrap_or_else(|p| p.into_inner());
        }
        !g.1
    }
    fn advance(&self, written: u64) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).0 = written;
        self.cv.notify_all();
    }
    fn abort(&self) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).1 = true;
        self.cv.notify_all();
    }
}

pub struct E01Writer {
    store: Arc<dyn EvidenceStore>,
    base: String,
    /// 0이면 나누지 않음
    segment_limit: u64,
    compression: Compression,
    meta: E01Meta,
    sector: u32,
    /// 원본(미디어) 전체 크기
    media_bytes: u64,
    guid: [u8; 16],
    acquired: chrono::DateTime<chrono::Local>,
    cur: Option<Seg>,
    seg_no: u32,
    files: Vec<OutputFile>,
    group_chunks: usize,
}

/// 쓰는 쪽에 보내는 내용: 원본 데이터 조각, 그리고 끝에 마무리 정보
pub enum WriterMsg {
    Data(Arc<[u8]>),
    Finish(Trailer),
}

impl E01Writer {
    pub fn new(
        store: Arc<dyn EvidenceStore>,
        base: String,
        segment_limit: u64,
        compression: Compression,
        meta: E01Meta,
        sector: u32,
        media_bytes: u64,
    ) -> Result<Self> {
        if !(512..=CHUNK_BYTES as u32).contains(&sector) || !sector.is_power_of_two() {
            bail!("{}", crate::tr!(format!("E01로 저장할 수 없는 섹터 크기입니다: {sector}"), format!("Sector size cannot be stored as E01: {sector}")));
        }
        let segment_limit = if segment_limit == 0 { 0 } else { segment_limit.max(MIN_SEGMENT) };
        // 조각이 작으면 묶음도 작게 (묶음 하나가 조각의 1/4을 넘지 않게)
        let group_chunks = if segment_limit == 0 { GROUP_CHUNKS } else { ((segment_limit / 4) as usize / CHUNK_BYTES).clamp(16, GROUP_CHUNKS) };
        let mut guid = [0u8; 16];
        if getrandom::getrandom(&mut guid).is_err() {
            // 난수를 못 얻으면 시각, 크기로 (0이 아닌 식별자)
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(1);
            guid.copy_from_slice(&(t ^ ((media_bytes as u128) << 64)).to_le_bytes());
        }
        Ok(Self {
            store,
            base,
            segment_limit,
            compression,
            meta,
            sector,
            media_bytes,
            guid,
            acquired: chrono::Local::now(),
            cur: None,
            seg_no: 0,
            files: Vec::new(),
            group_chunks,
        })
    }

    fn sectors_per_chunk(&self) -> u32 {
        CHUNK_BYTES as u32 / self.sector
    }

    /// volume / data 섹션 내용 (1052B)
    fn volume(&self) -> Vec<u8> {
        let mut v = vec![0u8; VOLUME_SIZE];
        let chunks = self.media_bytes.div_ceil(CHUNK_BYTES as u64);
        let sectors = self.media_bytes.div_ceil(self.sector as u64);
        v[0] = 0x01; // 고정 디스크 (0x0e는 논리 증거 파일(L01) 뜻이라 쓰지 않음)
        v[4..8].copy_from_slice(&(chunks.min(u32::MAX as u64) as u32).to_le_bytes());
        v[8..12].copy_from_slice(&self.sectors_per_chunk().to_le_bytes());
        v[12..16].copy_from_slice(&self.sector.to_le_bytes());
        v[16..24].copy_from_slice(&sectors.to_le_bytes());
        v[36] = if self.meta.physical { 0x03 } else { 0x01 }; // 이미지 + (물리 장치)
        v[52] = self.compression.volume_code();
        v[56..60].copy_from_slice(&self.sectors_per_chunk().to_le_bytes()); // 오류 단위
        v[64..80].copy_from_slice(&self.guid);
        let c = adler32(&v[..1048]);
        v[1048..1052].copy_from_slice(&c.to_le_bytes());
        v
    }

    /// EnCase 6 header2 (UTF-16LE). 날짜는 유닉스 시각
    fn header2_text(&self) -> String {
        let m = &self.meta;
        let ts = self.acquired.timestamp().to_string();
        format!(
            "3\nmain\na\tc\tn\te\tt\tmd\tsn\tav\tov\tm\tu\tp\tdc\n{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t0\t\n\nsrce\n0\t1\np\tn\tid\tev\ttb\tlo\tpo\tah\tgu\taq\n0\t0\n\t\t\t\t\t-1\t-1\t\t\t\n\nsub\n0\t1\np\tn\tid\tnu\tco\tgu\n0\t0\n\t\t\t\t1\t\n\n",
            clean(&m.description),
            clean(&m.case_number),
            clean(&m.evidence_number),
            clean(&m.examiner),
            clean(&m.notes),
            clean(&m.model),
            clean(&m.serial),
            clean(&format!("WS Collector {}", crate::VERSION)),
            clean(&m.os),
            ts,
            ts,
        )
    }

    /// EnCase 4 형식 header (ASCII). 날짜는 "연 월 일 시 분 초" (현지 시각)
    fn header_text(&self) -> String {
        let m = &self.meta;
        let d = self.acquired.format("%Y %-m %-d %-H %-M %-S").to_string();
        // ASCII 섹션이라 한글 등은 header2에만 온전히 남음 (여기서는 '?'로)
        let a = |s: &str| clean(s).chars().map(|c| if c.is_ascii() { c } else { '?' }).collect::<String>();
        format!(
            "1\nmain\nc\tn\ta\te\tt\tav\tov\tm\tu\tp\tr\n{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t0\t{}\n\n",
            a(&m.case_number),
            a(&m.evidence_number),
            a(&m.description),
            a(&m.examiner),
            a(&m.notes),
            a(&format!("WS Collector {}", crate::VERSION)),
            a(&m.os),
            d,
            d,
            self.compression.header_code(),
        )
    }

    fn write_section(seg: &mut Seg, kind: &[u8], payload: &[u8]) -> io::Result<()> {
        let size = DESC + payload.len() as u64;
        seg.f.write_all(&descriptor(kind, seg.pos + size, size))?;
        seg.f.write_all(payload)?;
        seg.pos += size;
        Ok(())
    }

    fn open_segment(&mut self) -> io::Result<()> {
        let n = self.seg_no + 1;
        let name = segment_name(&self.base, n).ok_or_else(|| io::Error::other(crate::tr!(format!("E01 조각 수 한계({MAX_SEGMENTS}개)를 넘었습니다. 조각 크기를 늘리세요"), format!("Exceeded the E01 segment limit ({MAX_SEGMENTS}). Increase the segment size"))))?;
        let f = self.store.create(&name)?;
        let mut seg = Seg { f, name, pos: 0, groups: 0 };
        seg.f.write_all(&file_header(n.min(u16::MAX as u32) as u16))?;
        seg.pos = FILE_HEADER_SIZE;
        if n == 1 {
            let h2 = zlib(&utf16le_bom(&self.header2_text()), 9)?;
            Self::write_section(&mut seg, b"header2", &h2)?;
            Self::write_section(&mut seg, b"header2", &h2)?;
            let h = zlib(self.header_text().as_bytes(), 9)?;
            Self::write_section(&mut seg, b"header", &h)?;
            Self::write_section(&mut seg, b"volume", &self.volume())?;
        } else {
            // 두 번째 조각부터는 volume 사본인 data 섹션으로 시작
            Self::write_section(&mut seg, b"data", &self.volume())?;
        }
        self.seg_no = n;
        self.cur = Some(seg);
        Ok(())
    }

    /// 지금 조각을 닫습니다. `last_kind`: 다음 조각이 있으면 "next", 마지막이면 "done", 중단이면 None
    fn close_segment(&mut self, last_kind: Option<&[u8]>) -> io::Result<()> {
        if let Some(mut seg) = self.cur.take() {
            if let Some(k) = last_kind {
                // 마지막 섹션은 자기 자신을 가리킴
                seg.f.write_all(&descriptor(k, seg.pos, DESC))?;
                seg.pos += DESC;
            }
            let (name, size) = (seg.name, seg.pos);
            seg.f.finish()?;
            self.files.push(OutputFile { name, size });
        }
        Ok(())
    }

    /// 끝 섹션 없이 닫고, 저장소가 지원하면(로컬 폴더) 모든 조각 이름에 `.incomplete`를 붙여
    /// 끝나지 않은 E01이 완전한 증거 파일처럼 보이지 않게 함
    fn abandon(&mut self) {
        let _ = self.close_segment(None);
        for f in &mut self.files {
            if let Ok(Some(n)) = self.store.mark_incomplete(&f.name) {
                f.name = n;
            }
        }
    }

    fn table_payload(entries: &[u32], base: u64) -> Vec<u8> {
        let mut t = Vec::with_capacity(TABLE_HDR as usize + entries.len() * 4 + 4);
        let mut h = [0u8; 24];
        h[0..4].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        h[8..16].copy_from_slice(&base.to_le_bytes());
        let c = adler32(&h[..20]);
        h[20..24].copy_from_slice(&c.to_le_bytes());
        t.extend_from_slice(&h);
        let start = t.len();
        for e in entries {
            t.extend_from_slice(&e.to_le_bytes());
        }
        let c = adler32(&t[start..]);
        t.extend_from_slice(&c.to_le_bytes());
        t
    }

    /// 청크 묶음 하나를 sectors + table + table2로 씁니다
    fn write_group(&mut self, chunks: &[Chunk]) -> io::Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let data_len: u64 = chunks.iter().map(|c| c.data.len() as u64).sum();
        let table_len = TABLE_HDR + chunks.len() as u64 * 4 + 4;
        let group_size = DESC + data_len + 2 * (DESC + table_len);
        if self.cur.is_none() {
            self.open_segment()?;
        }
        if self.segment_limit > 0 {
            let seg = self.cur.as_ref().unwrap();
            if seg.groups > 0 && seg.pos + group_size + TAIL_RESERVE > self.segment_limit {
                self.close_segment(Some(b"next"))?;
                self.open_segment()?;
            }
        }
        let seg = self.cur.as_mut().unwrap();
        let base = seg.pos + DESC;
        let mut entries = Vec::with_capacity(chunks.len());
        let mut data = Vec::with_capacity(data_len as usize);
        for c in chunks {
            let rel = data.len() as u64;
            if rel > ENTRY_OFFSET_MASK as u64 {
                return Err(io::Error::other(crate::tr!("E01 table 오프셋 한계를 넘었습니다 (내부 오류)", "Exceeded the E01 table offset limit (internal error)")));
            }
            entries.push(rel as u32 | if c.compressed { ENTRY_COMPRESSED } else { 0 });
            data.extend_from_slice(&c.data);
        }
        Self::write_section(seg, b"sectors", &data)?;
        drop(data);
        let t = Self::table_payload(&entries, base);
        Self::write_section(seg, b"table", &t)?;
        Self::write_section(seg, b"table2", &t)?;
        seg.groups += 1;
        Ok(())
    }

    fn write_trailer(&mut self, t: &Trailer) -> io::Result<()> {
        if self.cur.is_none() {
            self.open_segment()?;
        }
        let seg = self.cur.as_mut().unwrap();
        // error2: 개수[4] | 0[512] | Adler-32(앞 516B)[4] | (시작 섹터[4], 섹터 수[4])… | Adler-32(항목)[4]
        // 시작 섹터가 32비트라 2TB(512B 섹터) 넘는 위치의 오류는 넣지 못함 (수집 로그, 보고서에는 모두 남음)
        let entries: Vec<&BadRange> = t.bad_ranges.iter().filter(|r| r.start_sector <= u32::MAX as u64).collect();
        if !entries.is_empty() {
            let mut p = vec![0u8; 520];
            p[0..4].copy_from_slice(&(entries.len() as u32).to_le_bytes());
            let c = adler32(&p[..516]);
            p[516..520].copy_from_slice(&c.to_le_bytes());
            let start = p.len();
            for r in &entries {
                p.extend_from_slice(&(r.start_sector as u32).to_le_bytes());
                p.extend_from_slice(&(r.count.min(u32::MAX as u64) as u32).to_le_bytes());
            }
            let c = adler32(&p[start..]);
            p.extend_from_slice(&c.to_le_bytes());
            Self::write_section(seg, b"error2", &p)?;
        }
        let md5 = t.md5.unwrap_or([0; 16]);
        // digest: MD5[16] | SHA-1[20] | 0[40] | Adler-32(앞 76B)
        let mut d = [0u8; 80];
        d[0..16].copy_from_slice(&md5);
        d[16..36].copy_from_slice(&t.sha1.unwrap_or([0; 20]));
        let c = adler32(&d[..76]);
        d[76..80].copy_from_slice(&c.to_le_bytes());
        Self::write_section(seg, b"digest", &d)?;
        // hash: MD5[16] | 0[16] | Adler-32(앞 32B)
        let mut h = [0u8; 36];
        h[0..16].copy_from_slice(&md5);
        let c = adler32(&h[..32]);
        h[32..36].copy_from_slice(&c.to_le_bytes());
        Self::write_section(seg, b"hash", &h)?;
        Ok(())
    }

    /// 원본 데이터를 받아 압축, 저장합니다. 끝에 `Finish`가 오지 않으면(중단, 실패) 끝 섹션 없이 닫습니다.
    pub fn run(mut self, rx: Receiver<WriterMsg>) -> io::Result<Vec<OutputFile>> {
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).saturating_sub(1).clamp(1, 8);
        let compression = self.compression;
        let group_chunks = self.group_chunks;
        // 분배 스레드가 쓰기보다 너무 앞서 가지 않게 (압축이 빨리 끝난 뒤 블록들이 메모리에 쌓이는 것 방지)
        let window = (workers * 3) as u64;
        let gate = Gate { state: Mutex::new((0, false)), cv: Condvar::new() };
        let (result, trailer) = std::thread::scope(|s| {
            let (jtx, jrx) = sync_channel::<(u64, Arc<[u8]>)>(workers * 2);
            // 압축 스레드들만 받는 쪽을 가짐: 모두 끝나면 받는 쪽이 사라져 보내는 쪽(분배 스레드)이 깨어남
            let jrx = Arc::new(Mutex::new(jrx));
            let (ctx, crx) = sync_channel::<(u64, io::Result<Vec<Chunk>>)>(workers * 2);
            for _ in 0..workers {
                let jrx = jrx.clone();
                let ctx = ctx.clone();
                s.spawn(move || loop {
                    let job = match jrx.lock() {
                        Ok(r) => r.recv(),
                        Err(_) => return,
                    };
                    let Ok((seq, block)) = job else { return };
                    if ctx.send((seq, compress_block(&block, compression))).is_err() {
                        return;
                    }
                });
            }
            drop(jrx);
            drop(ctx);
            let gate = &gate;
            let dispatcher = s.spawn(move || {
                let mut seq = 0u64;
                let mut trailer = None;
                // 쓰기 오류(중단 표시)면 받는 쪽(rx)을 바로 버려 읽기 쪽의 다음 보내기가 실패하게 함
                for m in rx {
                    match m {
                        WriterMsg::Data(b) => {
                            if !gate.wait_room(seq, window) || jtx.send((seq, b)).is_err() {
                                break;
                            }
                            seq += 1;
                        }
                        WriterMsg::Finish(t) => trailer = Some(t),
                    }
                }
                trailer
            });

            let mut pending: BTreeMap<u64, Vec<Chunk>> = BTreeMap::new();
            let mut next = 0u64;
            let mut group: Vec<Chunk> = Vec::with_capacity(group_chunks);
            let mut res: io::Result<()> = Ok(());
            'recv: for (seq, chunks) in crx.iter() {
                match chunks {
                    Ok(c) => {
                        pending.insert(seq, c);
                    }
                    Err(e) => {
                        res = Err(e);
                        break;
                    }
                }
                while let Some(v) = pending.remove(&next) {
                    next += 1;
                    for c in v {
                        group.push(c);
                        if group.len() >= group_chunks {
                            if let Err(e) = self.write_group(&group) {
                                res = Err(e);
                                break 'recv;
                            }
                            group.clear();
                        }
                    }
                    gate.advance(next);
                }
            }
            if res.is_err() {
                gate.abort();
            }
            // 받는 쪽을 닫아 압축 스레드 → 분배 스레드 → 읽기 쪽 순서로 멈추게 함
            drop(crx);
            if res.is_ok() && !pending.is_empty() {
                res = Err(io::Error::other(crate::tr!("압축된 블록 순서가 맞지 않습니다 (내부 오류)", "Compressed block order mismatch (internal error)")));
            }
            if res.is_ok() {
                res = self.write_group(&group);
            }
            let trailer = match dispatcher.join() {
                Ok(t) => t,
                Err(_) => {
                    if res.is_ok() {
                        res = Err(io::Error::other(crate::tr!("E01 분배 스레드가 비정상 종료했습니다", "E01 dispatch thread terminated abnormally")));
                    }
                    None
                }
            };
            (res, trailer)
        });
        let finished = match (result, trailer) {
            (Err(e), _) => Err(e),
            (Ok(()), Some(t)) => self.write_trailer(&t).and_then(|_| self.close_segment(Some(b"done"))).map(|_| true),
            // 중단, 읽기 실패: 끝 섹션 없이 닫음
            (Ok(()), None) => Ok(false),
        };
        match finished {
            Ok(true) => {}
            Ok(false) => self.abandon(),
            Err(e) => {
                // 실패한 조각도 목록에 남겨 결과, 보고서에 보이게 함
                self.abandon();
                return Err(e);
            }
        }
        Ok(self.files)
    }
}

// --- 검증 (저장된 E01을 풀어 해시 다시 계산) -------------------------------

/// 앞으로만 읽는 캐시 없는 파일 읽기 (정렬된 4MB 단위)
struct Forward<'a> {
    path: &'a Path,
    file: File,
    cached: bool,
    buf: AlignedBuf,
    /// buf[0]의 파일 위치
    start: u64,
    len: usize,
    pos: u64,
    size: u64,
}

impl<'a> Forward<'a> {
    fn open(path: &'a Path, size: u64, warn: &mut dyn FnMut(&str)) -> Result<Self> {
        let (file, cached) = match open_for_verify(path) {
            Ok(f) => (f, false),
            Err(_) => {
                warn(crate::tr!(
                    "저장 매체가 캐시 없는 읽기를 지원하지 않아 일반 읽기로 검증합니다 (캐시를 비운 뒤 읽음)",
                    "Output media does not support uncached reads; verifying with normal reads (after flushing the cache)"
                ));
                let f = File::open(path).with_context(|| crate::tr!(format!("검증용 열기 실패: {}", path.display()), format!("Failed to open for verification: {}", path.display())))?;
                drop_cache(&f, 0, 0);
                (f, true)
            }
        };
        Ok(Self { path, file, cached, buf: AlignedBuf::new(crate::imaging::CHUNK), start: 0, len: 0, pos: 0, size })
    }

    fn fill(&mut self, warn: &mut dyn FnMut(&str)) -> Result<()> {
        let at = self.pos / AlignedBuf::ALIGN as u64 * AlignedBuf::ALIGN as u64;
        loop {
            match read_aligned(&self.file, at, &mut self.buf[..]) {
                Ok(n) => {
                    self.start = at;
                    self.len = n;
                    return Ok(());
                }
                Err(e) if is_alignment_error(&e) && !self.cached => {
                    warn(crate::tr!(
                        "캐시 없는 읽기가 거부되어 일반 읽기로 검증을 계속합니다 (캐시를 비운 뒤 읽음)",
                        "Uncached read was rejected; continuing verification with normal reads (after flushing the cache)"
                    ));
                    self.file = File::open(self.path)?;
                    drop_cache(&self.file, 0, 0);
                    self.cached = true;
                }
                Err(e) => return Err(anyhow!("{}", crate::tr!(format!("검증 중 읽기 실패 ({}): {e}", self.path.display()), format!("Read failed during verification ({}): {e}", self.path.display())))),
            }
        }
    }

    fn skip_to(&mut self, off: u64) -> Result<()> {
        if off < self.pos {
            bail!("{}", crate::tr!(format!("E01 섹션 순서가 앞으로 되돌아갑니다 ({} → {})", self.pos, off), format!("E01 section order goes backwards ({} -> {})", self.pos, off)));
        }
        if off > self.size {
            bail!("{}", crate::tr!(format!("E01 섹션 위치가 파일 끝을 넘습니다 ({off})"), format!("E01 section offset is past the end of file ({off})")));
        }
        self.pos = off;
        Ok(())
    }

    fn read(&mut self, out: &mut [u8], warn: &mut dyn FnMut(&str)) -> Result<()> {
        if self.pos + out.len() as u64 > self.size {
            bail!("{}", crate::tr!(format!("E01 파일이 예상보다 짧습니다: {}", self.path.display()), format!("E01 file is shorter than expected: {}", self.path.display())));
        }
        let mut done = 0;
        while done < out.len() {
            if self.pos < self.start || self.pos >= self.start + self.len as u64 {
                self.fill(warn)?;
                if self.len == 0 || self.pos >= self.start + self.len as u64 {
                    bail!("{}", crate::tr!(format!("E01 파일이 예상보다 짧습니다: {}", self.path.display()), format!("E01 file is shorter than expected: {}", self.path.display())));
                }
            }
            let i = (self.pos - self.start) as usize;
            let n = (self.len - i).min(out.len() - done);
            out[done..done + n].copy_from_slice(&self.buf[i..i + n]);
            done += n;
            self.pos += n as u64;
        }
        Ok(())
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 저장된 E01 조각들을 순서대로 읽어, 풀어낸 원본 데이터의 해시를 계산합니다. 중단되면 `Ok(None)`.
/// E01 안에 적힌 MD5,SHA-1이 다시 계산한 값과 다르면 오류입니다.
pub fn verify_e01(
    dir: &Path,
    files: &[OutputFile],
    algs: &[HashAlg],
    cancel: &AtomicBool,
    progress: VerifyProgress,
    log: VerifyLog,
) -> Result<Option<(Digests, u64)>> {
    if files.is_empty() {
        bail!(crate::tr!("검증할 E01 파일이 없습니다", "No E01 files to verify"));
    }
    let pipe = HashPipeline::new(algs);
    let mut warned = false;
    let mut warn = |m: &str| {
        if !warned {
            log(LogLevel::Warn, m);
            warned = true;
        }
    };
    let mut done: u64 = 0;
    let mut total: u64 = 0;
    let mut chunk_size: usize = 0;
    let mut last = Instant::now();
    let mut saw_done = false;
    let mut embedded_md5: Option<[u8; 16]> = None;
    let mut embedded_sha1: Option<[u8; 20]> = None;

    for (idx, of) in files.iter().enumerate() {
        check_file_name(&of.name)?;
        let path = dir.join(&of.name);
        let real = std::fs::metadata(&path).with_context(|| crate::tr!(format!("검증용 파일 없음: {}", of.name), format!("File for verification not found: {}", of.name)))?.len();
        if real != of.size {
            bail!("{}", crate::tr!(format!("파일 크기가 기록과 다릅니다: {} (기록 {} 바이트, 실제 {} 바이트)", of.name, of.size, real), format!("File size differs from the record: {} (recorded {} bytes, actual {} bytes)", of.name, of.size, real)));
        }
        let mut r = Forward::open(&path, real, &mut warn)?;
        let mut fh = [0u8; 13];
        r.read(&mut fh, &mut warn)?;
        if fh[..8] != EVF_SIGNATURE {
            bail!("{}", crate::tr!(format!("E01 파일이 아닙니다: {}", of.name), format!("Not an E01 file: {}", of.name)));
        }
        let seg = u16::from_le_bytes([fh[9], fh[10]]) as usize;
        if seg != (idx + 1).min(u16::MAX as usize) {
            bail!("{}", crate::tr!(format!("E01 조각 순서가 맞지 않습니다: {} (조각 번호 {seg}, 예상 {})", of.name, idx + 1), format!("E01 segment order mismatch: {} (segment number {seg}, expected {})", of.name, idx + 1)));
        }
        let mut pos = FILE_HEADER_SIZE;
        // (데이터 시작 위치, 데이터) - 바로 뒤의 table이 이 데이터를 가리킴
        let mut sectors: Option<(u64, Vec<u8>)> = None;
        let end_kind = loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            r.skip_to(pos)?;
            let mut d = [0u8; 76];
            r.read(&mut d, &mut warn)?;
            if adler32(&d[..72]).to_le_bytes() != d[72..76] {
                bail!("{}", crate::tr!(format!("E01 섹션 머리가 손상되었습니다: {} ({pos} 위치)", of.name), format!("E01 section header is corrupted: {} (at offset {pos})", of.name)));
            }
            let kind = String::from_utf8_lossy(&d[..16]).trim_end_matches('\0').to_string();
            let next = u64::from_le_bytes(d[16..24].try_into().unwrap());
            let size = u64::from_le_bytes(d[24..32].try_into().unwrap());
            // 섹션은 파일 안에 있어야 하고, 우리가 쓰는 E01은 다음 섹션이 바로 뒤에 붙음 (조작, 손상 감지, 큰 할당 방지)
            if size < DESC || pos.checked_add(size).is_none_or(|e| e > real) {
                bail!("{}", crate::tr!(format!("E01 섹션 크기가 잘못되었습니다: {} ({kind}, {pos} 위치)", of.name), format!("E01 section size is invalid: {} ({kind}, at offset {pos})", of.name)));
            }
            if kind != "next" && kind != "done" && next != pos + size {
                bail!("{}", crate::tr!(format!("E01 섹션 연결이 잘못되었습니다: {} ({kind}, {pos} 위치)", of.name), format!("E01 section chain is invalid: {} ({kind}, at offset {pos})", of.name)));
            }
            let body = size - DESC;
            match kind.as_str() {
                "next" | "done" => {
                    if next != pos {
                        bail!("{}", crate::tr!(format!("E01 마지막 섹션이 잘못되었습니다: {}", of.name), format!("E01 last section is invalid: {}", of.name)));
                    }
                    // 마지막 섹션 뒤에 덧붙은 데이터가 있으면 손상, 조작
                    if pos + DESC != real {
                        bail!("{}", crate::tr!(format!("E01 마지막 섹션 뒤에 데이터가 더 있습니다: {}", of.name), format!("E01 has extra data after the last section: {}", of.name)));
                    }
                    break kind;
                }
                "volume" | "data" => {
                    let mut v = vec![0u8; VOLUME_SIZE];
                    if body < VOLUME_SIZE as u64 {
                        bail!("{}", crate::tr!(format!("E01 volume 섹션이 짧습니다: {}", of.name), format!("E01 volume section is too short: {}", of.name)));
                    }
                    r.read(&mut v, &mut warn)?;
                    if adler32(&v[..1048]).to_le_bytes() != v[1048..1052] {
                        bail!("{}", crate::tr!(format!("E01 volume 섹션이 손상되었습니다: {}", of.name), format!("E01 volume section is corrupted: {}", of.name)));
                    }
                    let spc = u32::from_le_bytes(v[8..12].try_into().unwrap()) as usize;
                    let bps = u32::from_le_bytes(v[12..16].try_into().unwrap()) as usize;
                    let n = u64::from_le_bytes(v[16..24].try_into().unwrap());
                    let cs = spc.checked_mul(bps).filter(|c| *c > 0 && *c <= 1 << 24).ok_or_else(|| anyhow!(crate::tr!("E01 청크 크기가 잘못되었습니다", "E01 chunk size is invalid")))?;
                    let t = n.checked_mul(bps as u64).ok_or_else(|| anyhow!(crate::tr!("E01 섹터 수가 잘못되었습니다", "E01 sector count is invalid")))?;
                    if chunk_size == 0 {
                        chunk_size = cs;
                        total = t;
                    } else if chunk_size != cs || total != t {
                        bail!("{}", crate::tr!(format!("E01 조각마다 volume 정보가 다릅니다: {}", of.name), format!("E01 volume information differs between segments: {}", of.name)));
                    }
                }
                "sectors" => {
                    if body > MAX_SECTORS_SECTION {
                        bail!("{}", crate::tr!(format!("E01 sectors 섹션이 너무 큽니다 ({} 바이트). WS Collector가 만든 E01만 검증할 수 있습니다", body), format!("E01 sectors section is too large ({} bytes). Only E01 files created by WS Collector can be verified", body)));
                    }
                    let mut v = vec![0u8; body as usize];
                    r.read(&mut v, &mut warn)?;
                    sectors = Some((pos + DESC, v));
                }
                "table" => {
                    if chunk_size == 0 {
                        bail!("{}", crate::tr!(format!("E01 volume 섹션보다 table이 먼저 나옵니다: {}", of.name), format!("E01 table appears before the volume section: {}", of.name)));
                    }
                    let Some((data_start, data)) = sectors.take() else {
                        bail!("{}", crate::tr!(format!("E01 table 앞에 sectors 섹션이 없습니다: {}", of.name), format!("E01 has no sectors section before the table: {}", of.name)));
                    };
                    let mut h = [0u8; 24];
                    r.read(&mut h, &mut warn)?;
                    if adler32(&h[..20]).to_le_bytes() != h[20..24] {
                        bail!("{}", crate::tr!(format!("E01 table 머리가 손상되었습니다: {}", of.name), format!("E01 table header is corrupted: {}", of.name)));
                    }
                    let count = u32::from_le_bytes(h[0..4].try_into().unwrap()) as usize;
                    let base = u64::from_le_bytes(h[8..16].try_into().unwrap());
                    if (count as u64) * 4 + TABLE_HDR + 4 > body || count == 0 || count > 1 << 20 {
                        bail!("{}", crate::tr!(format!("E01 table 항목 수가 잘못되었습니다: {}", of.name), format!("E01 table entry count is invalid: {}", of.name)));
                    }
                    let mut raw = vec![0u8; count * 4 + 4];
                    r.read(&mut raw, &mut warn)?;
                    if adler32(&raw[..count * 4]).to_le_bytes() != raw[count * 4..] {
                        bail!("{}", crate::tr!(format!("E01 table 항목이 손상되었습니다: {}", of.name), format!("E01 table entries are corrupted: {}", of.name)));
                    }
                    let entries: Vec<u32> = raw[..count * 4].chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
                    let data_end = data_start + data.len() as u64;
                    for (i, e) in entries.iter().enumerate() {
                        if cancel.load(Ordering::Relaxed) {
                            return Ok(None);
                        }
                        let off = base + (e & ENTRY_OFFSET_MASK) as u64;
                        let end = entries.get(i + 1).map(|n| base + (n & ENTRY_OFFSET_MASK) as u64).unwrap_or(data_end);
                        if off < data_start || end > data_end || end <= off {
                            bail!("{}", crate::tr!(format!("E01 청크 위치가 sectors 섹션 밖입니다: {} (청크 {i})", of.name), format!("E01 chunk offset is outside the sectors section: {} (chunk {i})", of.name)));
                        }
                        let bytes = &data[(off - data_start) as usize..(end - data_start) as usize];
                        let plain = if e & ENTRY_COMPRESSED != 0 {
                            unzlib(bytes, chunk_size).map_err(|x| anyhow!("{}", crate::tr!(format!("E01 청크를 풀 수 없습니다: {} (청크 {i}): {x}", of.name), format!("Cannot decompress E01 chunk: {} (chunk {i}): {x}", of.name))))?
                        } else {
                            if bytes.len() < 5 || bytes.len() - 4 > chunk_size {
                                bail!("{}", crate::tr!(format!("E01 청크 크기가 잘못되었습니다: {} (청크 {i})", of.name), format!("E01 chunk size is invalid: {} (chunk {i})", of.name)));
                            }
                            let (p, c) = bytes.split_at(bytes.len() - 4);
                            if adler32(p).to_le_bytes() != c {
                                bail!("{}", crate::tr!(format!("E01 청크 체크섬이 맞지 않습니다: {} (청크 {i})", of.name), format!("E01 chunk checksum mismatch: {} (chunk {i})", of.name)));
                            }
                            p.to_vec()
                        };
                        // 마지막 청크 외에는 꽉 차 있어야 함
                        let n = (plain.len() as u64).min(total.saturating_sub(done)) as usize;
                        if n < plain.len() || (n < chunk_size && done + (n as u64) < total) {
                            bail!("{}", crate::tr!(format!("E01 청크 길이가 원본 크기와 맞지 않습니다: {} (청크 {i})", of.name), format!("E01 chunk length does not match the source size: {} (chunk {i})", of.name)));
                        }
                        pipe.feed(&Arc::from(&plain[..n]));
                        done += n as u64;
                        if last.elapsed().as_millis() >= 200 {
                            progress(done, total);
                            last = Instant::now();
                        }
                    }
                }
                "digest" => {
                    let mut v = [0u8; 80];
                    r.read(&mut v, &mut warn)?;
                    if adler32(&v[..76]).to_le_bytes() == v[76..80] {
                        if v[..16].iter().any(|b| *b != 0) {
                            embedded_md5 = Some(v[..16].try_into().unwrap());
                        }
                        if v[16..36].iter().any(|b| *b != 0) {
                            embedded_sha1 = Some(v[16..36].try_into().unwrap());
                        }
                    } else {
                        bail!("{}", crate::tr!(format!("E01 digest 섹션이 손상되었습니다: {}", of.name), format!("E01 digest section is corrupted: {}", of.name)));
                    }
                }
                "hash" => {
                    let mut v = [0u8; 36];
                    r.read(&mut v, &mut warn)?;
                    if adler32(&v[..32]).to_le_bytes() != v[32..36] {
                        bail!("{}", crate::tr!(format!("E01 hash 섹션이 손상되었습니다: {}", of.name), format!("E01 hash section is corrupted: {}", of.name)));
                    }
                    if v[..16].iter().any(|b| *b != 0) {
                        let m: [u8; 16] = v[..16].try_into().unwrap();
                        // digest와 hash 섹션의 MD5가 다르면 손상, 조작
                        if embedded_md5.is_some_and(|d| d != m) {
                            bail!("{}", crate::tr!(format!("E01의 digest와 hash 섹션 MD5가 다릅니다: {}", of.name), format!("E01 digest and hash section MD5 values differ: {}", of.name)));
                        }
                        embedded_md5 = Some(m);
                    }
                }
                // header, header2,table2,error2 등은 건너뜀
                _ => {}
            }
            if next <= pos || next > real {
                bail!("{}", crate::tr!(format!("E01 섹션 연결이 잘못되었습니다: {} ({kind}, {pos} → {next})", of.name), format!("E01 section chain is invalid: {} ({kind}, {pos} -> {next})", of.name)));
            }
            pos = next;
        };
        let last_file = idx + 1 == files.len();
        match (end_kind.as_str(), last_file) {
            ("done", true) => saw_done = true,
            ("next", false) => {}
            ("done", false) => bail!("{}", crate::tr!(format!("E01이 {}에서 끝나는데 뒤에 조각이 더 있습니다", of.name), format!("E01 ends at {} but more segments follow", of.name))),
            _ => bail!("{}", crate::tr!(format!("E01 조각이 끝나지 않았습니다: {} (다음 조각 파일이 없음)", of.name), format!("Incomplete E01 segments: {} (next segment file is missing)", of.name))),
        }
    }
    if !saw_done {
        bail!(crate::tr!("E01 끝 표시가 없습니다", "E01 end marker is missing"));
    }
    progress(done, total);
    if done != total {
        bail!("{}", crate::tr!(format!("E01에서 풀어낸 크기({done})가 원본 크기({total})와 다릅니다"), format!("Size decompressed from E01 ({done}) differs from the source size ({total})")));
    }
    let digests = pipe.finish()?;
    // E01 안에 적힌 해시와 대조 (FTK, EnCase가 보여 주는 값)
    // WS Collector가 만든 E01에는 늘 MD5가 들어 있음: 없으면 손상, 조작으로 봄
    if embedded_md5.is_none() {
        bail!(crate::tr!("E01에 MD5(digest, hash 섹션)가 없습니다", "E01 has no MD5 (digest/hash section)"));
    }
    if let (Some(m), Some(calc)) = (embedded_md5, digests.get(&HashAlg::Md5)) {
        if hex(&m) != *calc {
            bail!("{}", crate::tr!(format!("E01에 적힌 MD5({})가 다시 계산한 값({calc})과 다릅니다", hex(&m)), format!("MD5 stored in E01 ({}) differs from the recomputed value ({calc})", hex(&m))));
        }
    }
    if let (Some(s), Some(calc)) = (embedded_sha1, digests.get(&HashAlg::Sha1)) {
        if hex(&s) != *calc {
            bail!("{}", crate::tr!(format!("E01에 적힌 SHA-1({})이 다시 계산한 값({calc})과 다릅니다", hex(&s)), format!("SHA-1 stored in E01 ({}) differs from the recomputed value ({calc})", hex(&s))));
        }
    }
    Ok(Some((digests, done)))
}

/// 16진수 해시 → 바이트 (E01 digest 섹션용)
pub fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}
