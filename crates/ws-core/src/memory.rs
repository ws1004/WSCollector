//! 물리 메모리 수집 (Linux, Windows).
//!
//! Windows: WinPmem 드라이버를 USB에서 적재해 `\\.\pmem`을 물리 주소 = 위치로 읽음 (memory_windows.rs).
//!
//! Linux: AVML과 같은 원본을 같은 순서로 시도합니다: `/dev/crash` → `/proc/kcore` → `/dev/mem`.
//! - 실제 메모리 위치는 `/proc/iomem`의 "System RAM" 영역 (root 권한 필요. 일반 사용자에게는 주소가 0으로 보임)
//! - `/proc/kcore`는 ELF 코어 파일이라, 각 영역의 물리 주소(p_paddr)로 파일 위치를 찾음 (p_paddr가 채워진 커널에서만)
//! - `/dev/crash`, `/dev/mem`은 물리 주소 = 파일 위치
//!
//! 결과는 LiME 형식입니다 (Volatility 등이 바로 읽음): 영역마다 32바이트 머리말 + 그 영역의 메모리.
//! 수집 엔진은 이것을 "LiME 형식으로 읽히는 원본"(`LimeSource`)으로 받아 디스크와 똑같이
//! 해시, 분할, 검증, 로그, 네트워크 전송을 합니다.

use std::io;

use serde::{Deserialize, Serialize};

/// LiME 머리말 크기
pub const LIME_HEADER: u64 = 32;
const LIME_MAGIC: u32 = 0x4C69_4D45;

/// 화면, 로그용 메모리 정보
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryInfo {
    /// 수집할 메모리 크기 (영역 합)
    #[serde(with = "crate::model::u64_str")]
    pub bytes: u64,
    /// LiME 파일 크기 (머리말 포함)
    #[serde(with = "crate::model::u64_str")]
    pub image_bytes: u64,
    /// 쓸 원본 ("/proc/kcore" 등)
    pub source: String,
    pub ranges: usize,
    /// 원본을 고른 과정 (안 된 원본과 이유)
    pub notes: Vec<String>,
    /// 적재하는 커널 드라이버 파일 (Windows WinPmem). Linux는 적재하지 않으므로 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
}

/// 읽을 영역: 물리 주소 [start, end), 원본 파일 위치
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub end: u64,
    pub file_off: u64,
}

/// LiME 머리말 (끝 주소는 포함 범위)
pub fn lime_header(r: &Range) -> [u8; 32] {
    let mut h = [0u8; 32];
    h[0..4].copy_from_slice(&LIME_MAGIC.to_le_bytes());
    h[4..8].copy_from_slice(&1u32.to_le_bytes());
    h[8..16].copy_from_slice(&r.start.to_le_bytes());
    h[16..24].copy_from_slice(&(r.end - 1).to_le_bytes());
    h
}

/// `/proc/iomem`의 RAM 영역 [start, end) (합치기 전, 나온 그대로)
/// - 맨 위 단계 "System RAM"(뒤에 붙은 말 포함)
/// - 장치 아래에 붙은 "System RAM (…)": CXL, PMEM을 일반 메모리로 쓰는 DAX kmem("System RAM (kmem)"),
///   virtio-mem 등 드라이버가 붙인 메모리. 들여쓰기 되어 나옴
pub fn parse_iomem_raw(text: &str) -> Vec<(u64, u64)> {
    text.lines()
        .filter_map(|l| {
            let top = !l.starts_with(' ');
            let (range, name) = l.split_once(" : ")?;
            let name = name.trim();
            if !((top && name.starts_with("System RAM")) || name.starts_with("System RAM (")) {
                return None;
            }
            let (a, b) = range.trim().split_once('-')?;
            let s = u64::from_str_radix(a, 16).ok()?;
            let e = u64::from_str_radix(b, 16).ok()?.checked_add(1)?;
            (e > s).then_some((s, e))
        })
        .collect()
}

/// RAM 영역을 정렬하고 겹치는 영역(이상한 펌웨어 표, kmem과 맨 위 영역 중복 등)은 합침
pub fn parse_iomem(text: &str) -> Vec<(u64, u64)> {
    let mut v = parse_iomem_raw(text);
    v.sort();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(v.len());
    for (s, e) in v {
        match out.last_mut() {
            Some(l) if s < l.1 => l.1 = l.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// kcore ELF의 PT_LOAD 구간: (물리 시작, 길이, 파일 위치). 물리 주소가 없는 구간(vmalloc 등)은 뺌
pub fn parse_kcore_loads(head: &[u8], read_at: &mut dyn FnMut(u64, &mut [u8]) -> io::Result<usize>) -> io::Result<Vec<(u64, u64, u64)>> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, crate::tr!(format!("kcore 형식 오류: {m}"), format!("Invalid kcore format: {m}")));
    if head.len() < 64 || &head[0..4] != b"\x7fELF" {
        return Err(bad(crate::tr!("ELF 아님", "not ELF")));
    }
    if head[4] != 2 || head[5] != 1 {
        return Err(bad(crate::tr!("64비트 리틀 엔디언 ELF가 아님", "not a 64-bit little-endian ELF")));
    }
    let u16_at = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u32_at = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let u64_at = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let phoff = u64_at(head, 0x20);
    let shoff = u64_at(head, 0x28);
    let phentsize = u16_at(head, 0x36) as usize;
    let mut phnum = u16_at(head, 0x38) as usize;
    if phentsize < 56 {
        return Err(bad(crate::tr!("프로그램 헤더 크기", "program header size")));
    }
    // 구간이 65535개 이상이면 실제 개수는 섹션 헤더 0번의 sh_info에 있음
    if phnum == 0xffff {
        let mut sh = [0u8; 64];
        read_at(shoff, &mut sh)?;
        phnum = u32_at(&sh, 44) as usize;
    }
    if phnum > 1 << 20 {
        return Err(bad(crate::tr!("프로그램 헤더 수", "program header count")));
    }
    let mut table = vec![0u8; phnum * phentsize];
    let mut got = 0;
    while got < table.len() {
        let n = read_at(phoff + got as u64, &mut table[got..])?;
        if n == 0 {
            return Err(bad(crate::tr!("프로그램 헤더가 잘림", "program headers truncated")));
        }
        got += n;
    }
    let mut out = Vec::new();
    for i in 0..phnum {
        let p = &table[i * phentsize..];
        if u32_at(p, 0) != 1 {
            continue; // PT_LOAD만
        }
        let off = u64_at(p, 8);
        let paddr = u64_at(p, 24);
        let size = u64_at(p, 32);
        if paddr == u64::MAX || size == 0 {
            continue;
        }
        out.push((paddr, size, off));
    }
    // 오래된 커널은 p_paddr를 채우지 않아 모두 0. 이때만 버림
    // (물리 주소 0에서 시작하는 RAM 구간은 정상이므로 하나라도 0이 아닌 값이 있으면 0도 그대로 씀)
    if out.iter().all(|(p, _, _)| *p == 0) {
        out.clear();
    }
    Ok(out)
}

/// p_paddr가 0인 구간은 "물리 주소 0에서 시작하는 RAM"일 때만 남김.
/// 커널에 따라 RAM이 아닌 구간(vmalloc 등)이 0으로 나올 수 있어, 크기가 주소 0 RAM 영역(페이지 올림)을 넘으면 버림
pub fn keep_zero_ram(ram: &[(u64, u64)], loads: Vec<(u64, u64, u64)>) -> Vec<(u64, u64, u64)> {
    let zero_len = ram.iter().find(|(s, _)| *s == 0).map(|(_, e)| (e + 4095) & !4095);
    loads.into_iter().filter(|(p, len, _)| *p != 0 || zero_len.is_some_and(|z| *len <= z)).collect()
}

/// RAM 영역과 kcore 구간의 겹치는 부분 → 읽을 영역
pub fn intersect(ram: &[(u64, u64)], loads: &[(u64, u64, u64)]) -> Vec<Range> {
    let mut out = Vec::new();
    for &(rs, re) in ram {
        for &(ps, len, off) in loads {
            let pe = ps.saturating_add(len);
            let s = rs.max(ps);
            let e = re.min(pe);
            if s < e {
                out.push(Range { start: s, end: e, file_off: off + (s - ps) });
            }
        }
    }
    out.sort_by_key(|r| r.start);
    // 같은 물리 영역이 kcore에 두 번 나오는 경우(커널 텍스트 등) 겹치는 부분 제거
    let mut merged: Vec<Range> = Vec::new();
    for r in out {
        match merged.last() {
            Some(l) if r.start < l.end => {
                if r.end > l.end {
                    let cut = l.end;
                    merged.push(Range { start: cut, end: r.end, file_off: r.file_off + (cut - r.start) });
                }
            }
            _ => merged.push(r),
        }
    }
    merged
}

/// 영역들을 LiME 파일로 읽히게 하는 원본
pub struct LimeSource {
    file: std::fs::File,
    ranges: Vec<Range>,
    /// 각 영역의 LiME 파일 내 시작 위치 (머리말 포함)
    starts: Vec<u64>,
    len: u64,
    /// 한 번에 한 물리 페이지만 읽음 (/dev/crash는 페이지를 넘는 읽기를 EINVAL로 거부. /dev/mem도 안전하게)
    page_reads: bool,
}

impl LimeSource {
    pub fn new(file: std::fs::File, ranges: Vec<Range>) -> Self {
        let mut starts = Vec::with_capacity(ranges.len());
        let mut pos = 0u64;
        for r in &ranges {
            starts.push(pos);
            pos += LIME_HEADER + (r.end - r.start);
        }
        Self { file, ranges, starts, len: pos, page_reads: false }
    }

    /// 물리 주소 = 파일 위치인 장치(/dev/crash, /dev/mem)용: 페이지 단위로 읽음
    pub fn with_page_reads(mut self) -> Self {
        self.page_reads = true;
        self
    }
}

/// 메모리 원본 확인 메모 중 경고(일부 RAM을 읽지 못함)의 머리말. 수집 로그에 경고로 남김
/// 언어와 상관없이 같은 ASCII 표시 (collect.rs가 starts_with로 확인)
pub const COVERAGE_WARN: &str = "[WARN]";

pub fn lime_len(ranges: &[Range]) -> u64 {
    ranges.iter().map(|r| LIME_HEADER + (r.end - r.start)).sum()
}

impl LimeSource {
    /// LiME 스트림 위치가 속한 영역 번호
    fn region(&self, pos: u64) -> usize {
        match self.starts.binary_search(&pos) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }
}

impl crate::source::Source for LimeSource {
    fn len(&self) -> u64 {
        self.len
    }
    /// 오류 단위 = 물리 페이지 (머리말 32B는 따로). 섹터 단위로 하면 머리말만큼 어긋나
    /// 오류 페이지 하나가 이웃 페이지까지 0으로 채우게 됨
    fn unit_at(&self, offset: u64) -> (u64, u64) {
        if offset >= self.len || self.ranges.is_empty() {
            return (offset, offset + 4096);
        }
        let i = self.region(offset);
        let (r, base) = (self.ranges[i], self.starts[i]);
        let rel = offset - base;
        if rel < LIME_HEADER {
            return (base, base + LIME_HEADER);
        }
        let phys = r.start + (rel - LIME_HEADER);
        let ps = (phys & !4095).max(r.start);
        let pe = ((phys & !4095) + 4096).min(r.end);
        (base + LIME_HEADER + (ps - r.start), base + LIME_HEADER + (pe - r.start))
    }
    /// 메모리에는 읽을 수 없는 구멍이 있을 수 있어 디스크(64MB)보다 길게 봐 줌.
    /// 그래도 1GB 넘게 하나도 못 읽으면 원본이 막힌 것(lockdown 등): 0만 담긴 이미지를 만들지 않게 중단
    fn dead_run_limit(&self) -> u32 {
        256
    }
    fn is_synthetic(&self, offset: u64) -> bool {
        if offset >= self.len || self.ranges.is_empty() {
            return false;
        }
        offset - self.starts[self.region(offset)] < LIME_HEADER
    }
    fn where_is(&self, offset: u64) -> Option<String> {
        if offset >= self.len || self.ranges.is_empty() {
            return None;
        }
        let i = self.region(offset);
        let rel = offset - self.starts[i];
        Some(if rel < LIME_HEADER {
            crate::tr!(format!("영역 {} 머리말", i + 1), format!("region {} header", i + 1))
        } else {
            crate::tr!(format!("물리 주소 {:#x}", self.ranges[i].start + (rel - LIME_HEADER)), format!("physical address {:#x}", self.ranges[i].start + (rel - LIME_HEADER)))
        })
    }
    fn sector_size(&self) -> u32 {
        // 읽기 오류 재시도 단위 = 페이지
        4096
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.len {
            return Ok(0);
        }
        // 32비트 환경에서 4GB 넘는 남은 길이가 잘리지 않게 u64로 비교
        let want = (buf.len() as u64).min(self.len - offset) as usize;
        let mut done = 0usize;
        while done < want {
            let pos = offset + done as u64;
            // pos가 속한 영역
            let i = self.region(pos);
            let r = self.ranges[i];
            let rel = pos - self.starts[i];
            if rel < LIME_HEADER {
                let h = lime_header(&r);
                let n = ((LIME_HEADER - rel) as usize).min(want - done);
                buf[done..done + n].copy_from_slice(&h[rel as usize..rel as usize + n]);
                done += n;
                continue;
            }
            let mrel = rel - LIME_HEADER;
            let n = (r.end - r.start - mrel).min((want - done) as u64) as usize;
            let phys = r.start + mrel;
            let res = if self.page_reads {
                read_pages_at(&self.file, r.file_off + mrel, &mut buf[done..done + n])
            } else {
                read_exact_at(&self.file, r.file_off + mrel, &mut buf[done..done + n])
            };
            // 메모리 읽기 오류(EINVAL, EFAULT, EIO 등)는 장치가 사라진 게 아니라 그 페이지를 못 읽은 것:
            // 치명 오류로 끝내지 않고 엔진이 페이지 단위로 다시 읽고 0으로 채우게 일반 오류로 바꿈
            res.map_err(|e| page_error(e, phys, n))?;
            done += n;
        }
        Ok(done)
    }
}

/// 메모리 읽기 오류 분류: 그 페이지만 못 읽는 오류(EINVAL, EFAULT, EIO, /dev/crash가 예약 영역에서 줌)는
/// 일반 오류로 바꿔 엔진이 페이지 단위로 0 채우게 함. 그 밖(ENODEV, EPERM, EBADF 등 원본 자체가 막힘)은
/// 원래 오류 번호를 그대로 두어 엔진이 즉시 중단하게 함
fn page_error(e: io::Error, phys: u64, n: usize) -> io::Error {
    #[cfg(unix)]
    let per_page = matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::EFAULT) | Some(libc::EIO)) || e.raw_os_error().is_none();
    // Windows(WinPmem): 장치가 사라지거나 막힌 오류(핸들 무효, 장치 없음, 연결 끊김, 준비 안 됨, 접근 거부)만 원래 번호를 두어
    // 엔진이 즉시 중단하게 함. 그 밖(87 등)은 그 페이지만 못 읽은 것으로 보고 0 채움 (공식 도구도 페이지 단위 0 채움)
    #[cfg(windows)]
    let per_page = !matches!(e.raw_os_error(), Some(5) | Some(6) | Some(21) | Some(55) | Some(433) | Some(1167));
    #[cfg(not(any(unix, windows)))]
    let per_page = true;
    if per_page {
        io::Error::other(crate::tr!(format!("메모리 읽기 실패 ({phys:#x}부터 {n}바이트 중): {e}"), format!("Memory read failed ({n} bytes from {phys:#x}): {e}")))
    } else {
        e
    }
}

/// 페이지 경계를 넘지 않게 나눠 읽음
fn read_pages_at(f: &std::fs::File, mut off: u64, mut buf: &mut [u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let n = ((4096 - off % 4096) as usize).min(buf.len());
        let (head, rest) = std::mem::take(&mut buf).split_at_mut(n);
        read_exact_at(f, off, head)?;
        off += n as u64;
        buf = rest;
    }
    Ok(())
}

fn read_exact_at(f: &std::fs::File, mut off: u64, mut buf: &mut [u8]) -> io::Result<()> {
    while !buf.is_empty() {
        #[cfg(unix)]
        let r = {
            use std::os::unix::fs::FileExt;
            f.read_at(buf, off)
        };
        #[cfg(windows)]
        let r = {
            use std::os::windows::fs::FileExt;
            f.seek_read(buf, off)
        };
        match r {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, crate::tr!("메모리 원본이 예상보다 짧습니다", "Memory source is shorter than expected"))),
            Ok(n) => {
                off += n as u64;
                buf = &mut buf[n..];
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// -- Windows: 물리 메모리 영역 해석 (어느 OS에서나 테스트되게 순수 함수) ---------

/// 영역 목록 정리: 정렬, 빈 영역 제거, 겹침 합치기
pub fn normalize_runs(mut v: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    v.retain(|(s, e)| e > s);
    v.sort();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(v.len());
    for (s, e) in v {
        match out.last_mut() {
            Some(l) if s <= l.1 => l.1 = l.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// 레지스트리 `HARDWARE\RESOURCEMAP\System Resources\Physical Memory\.Translated` (REG_RESOURCE_LIST, x64)
/// → RAM 영역 [start, end). 드라이버 없이 읽을 수 있어 수집 전 화면(크기 표시)에 씀.
/// CM_RESOURCE_LIST: Count(u32) + 전체 서술자[ InterfaceType(u32) BusNumber(u32) Version(u16) Revision(u16) Count(u32) + 부분 서술자 20바이트 × Count ]
/// 부분 서술자(x64, 4바이트 정렬): Type(u8) Share(u8) Flags(u16) Start(u64) Length(u32) … (공용체 16바이트)
pub fn parse_resource_list(b: &[u8]) -> Vec<(u64, u64)> {
    const MEMORY: u8 = 3; // CmResourceTypeMemory
    const MEMORY_LARGE: u8 = 7; // CmResourceTypeMemoryLarge
    let u16_at = |o: usize| b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()));
    let u64_at = |o: usize| b.get(o..o + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap()));
    let mut out = Vec::new();
    let Some(full) = u32_at(0) else { return out };
    let mut o = 4usize;
    for _ in 0..full.min(64) {
        let Some(count) = u32_at(o + 12) else { break };
        o += 16;
        for _ in 0..count.min(4096) {
            let (Some(ty), Some(flags), Some(start), Some(len)) = (b.get(o).copied(), u16_at(o + 2), u64_at(o + 4), u32_at(o + 12)) else {
                return normalize_runs(out);
            };
            let len = len as u64;
            let bytes = match ty {
                MEMORY => Some(len),
                // 큰 메모리: 길이를 플래그만큼 왼쪽으로 밀어 씀 (40,48,64비트 표현)
                MEMORY_LARGE if flags & 0x0200 != 0 => Some(len << 8),
                MEMORY_LARGE if flags & 0x0400 != 0 => Some(len << 16),
                MEMORY_LARGE if flags & 0x0800 != 0 => Some(len << 32),
                _ => None,
            };
            if let Some(n) = bytes {
                if let Some(end) = start.checked_add(n) {
                    out.push((start, end));
                }
            }
            o += 20;
        }
    }
    normalize_runs(out)
}

/// WinPmem 4.x `IOCTL_GET_INFO` 결과(WINPMEM_MEMORY_INFO, x64) → 영역 [start, end).
/// NumberOfRuns는 2608, Run[]은 2616부터 (BaseAddress u64, NumberOfBytes u64). 값이 이상하면 None
pub fn parse_pmem_info(b: &[u8]) -> Option<Vec<(u64, u64)>> {
    const RUNS_AT: usize = 8 * (4 + 64 + 4 + 0xfe);
    let n = u64::from_le_bytes(b.get(RUNS_AT..RUNS_AT + 8)?.try_into().ok()?);
    if n == 0 || n > 300 {
        return None;
    }
    let mut v = Vec::with_capacity(n as usize);
    for i in 0..n as usize {
        let o = RUNS_AT + 8 + i * 16;
        let base = u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?);
        let len = u64::from_le_bytes(b.get(o + 8..o + 16)?.try_into().ok()?);
        v.push((base, base.checked_add(len)?));
    }
    let v = normalize_runs(v);
    (!v.is_empty()).then_some(v)
}

/// 영역 → LiME 읽기 영역 (물리 주소 = 장치 위치)
pub fn identity_ranges(runs: &[(u64, u64)]) -> Vec<Range> {
    runs.iter().map(|&(s, e)| Range { start: s, end: e, file_off: s }).collect()
}

// -- OS별 원본 고르기 -----------------------------------------------------

#[cfg(target_os = "linux")]
pub use linux::{memory_probe, open_memory};

#[cfg(windows)]
#[path = "memory_windows.rs"]
mod windows_impl;
#[cfg(windows)]
pub use windows_impl::{memory_probe, open_memory, release_driver, take_footprint};

/// 드라이버를 쓰지 않는 OS: 내릴 것이 없음
#[cfg(not(windows))]
pub fn release_driver() -> Vec<(bool, String)> {
    Vec::new()
}
/// 드라이버를 쓰지 않는 OS: 흔적 기록 없음
#[cfg(not(windows))]
pub fn take_footprint() -> Vec<String> {
    Vec::new()
}

/// 메모리를 수집할 수 있으면 정보, 없으면 None (이유는 `memory_probe`)
pub fn memory_info() -> Option<MemoryInfo> {
    memory_probe().ok()
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn memory_probe() -> Result<MemoryInfo, String> {
    Err(crate::tr!("이 운영체제의 메모리 수집은 아직 지원하지 않습니다", "Memory acquisition is not yet supported on this operating system").into())
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn open_memory() -> anyhow::Result<(LimeSource, MemoryInfo)> {
    Err(anyhow::anyhow!(crate::tr!("이 운영체제의 메모리 수집은 아직 지원하지 않습니다", "Memory acquisition is not yet supported on this operating system")))
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use anyhow::{anyhow, Result};
    use std::fs::File;
    use std::os::unix::fs::FileExt;

    fn ram_ranges() -> Result<Vec<(u64, u64)>> {
        let text = std::fs::read_to_string("/proc/iomem").map_err(|e| anyhow!("{}", crate::tr!(format!("/proc/iomem을 읽을 수 없습니다: {e}"), format!("Cannot read /proc/iomem: {e}"))))?;
        // 권한이 모자라면(컨테이너 root에 CAP_SYS_ADMIN 없음 등) 주소가 모두 0으로 보임: 합치기 전에 확인
        let raw = parse_iomem_raw(&text);
        if raw.len() > 1 && raw.iter().all(|(s, _)| *s == 0) {
            return Err(anyhow!(crate::tr!("/proc/iomem 주소가 0으로 보입니다 (root 권한, CAP_SYS_ADMIN이 필요합니다. 컨테이너 안이면 호스트에서 실행하세요)", "/proc/iomem addresses appear as 0 (root privileges and CAP_SYS_ADMIN are required; if inside a container, run on the host)")));
        }
        let ram = parse_iomem(&text);
        if ram.is_empty() {
            return Err(anyhow!(crate::tr!("/proc/iomem에 System RAM 영역이 없습니다", "/proc/iomem has no System RAM regions")));
        }
        Ok(ram)
    }

    /// 물리 주소 = 파일 위치인 장치 (/dev/crash, /dev/mem). 1MB 위 첫 페이지를 읽어 볼 수 있어야 사용.
    /// 수집도 이 시험과 같이 한 페이지씩 읽음 (/dev/crash는 페이지를 넘는 읽기를 거부)
    fn try_phys_dev(path: &str, ram: &[(u64, u64)]) -> Result<(File, Vec<Range>)> {
        let f = File::open(path).map_err(|e| anyhow!("{path}: {e}"))?;
        let probe = ram.iter().find(|(s, e)| *e > 0x10_0000 && e - s >= 4096).map(|(s, _)| (*s).max(0x10_0000)).unwrap_or(ram[0].0);
        let mut b = [0u8; 4096];
        f.read_at(&mut b, probe).map_err(|e| anyhow!("{}", crate::tr!(format!("{path}: 1MB 위 메모리를 읽을 수 없음 ({e})"), format!("{path}: cannot read memory above 1 MB ({e})"))))?;
        Ok((f, ram.iter().map(|&(s, e)| Range { start: s, end: e, file_off: s }).collect()))
    }

    fn try_kcore(ram: &[(u64, u64)]) -> Result<(File, Vec<Range>)> {
        let f = File::open("/proc/kcore").map_err(|e| anyhow!("/proc/kcore: {e}"))?;
        let mut head = [0u8; 64];
        f.read_at(&mut head, 0).map_err(|e| anyhow!("/proc/kcore: {e}"))?;
        let mut rd = |off: u64, b: &mut [u8]| f.read_at(b, off);
        let loads = parse_kcore_loads(&head, &mut rd).map_err(|e| anyhow!("/proc/kcore: {e}"))?;
        if loads.is_empty() {
            return Err(anyhow!(crate::tr!("/proc/kcore: 물리 주소(p_paddr)가 있는 구간이 없음 (오래된 커널)", "/proc/kcore: no segments with a physical address (p_paddr) (old kernel)")));
        }
        let loads = keep_zero_ram(ram, loads);
        let ranges = intersect(ram, &loads);
        if ranges.is_empty() {
            return Err(anyhow!(crate::tr!("/proc/kcore: System RAM과 겹치는 구간이 없음", "/proc/kcore: no segments overlap System RAM")));
        }
        // 첫 영역 한 페이지를 실제로 읽어 봄 (lockdown 등으로 막혀 있으면 여기서 실패)
        let mut b = [0u8; 4096];
        let r0 = ranges[0];
        let n = ((r0.end - r0.start) as usize).min(4096);
        f.read_at(&mut b[..n], r0.file_off).map_err(|e| anyhow!("{}", crate::tr!(format!("/proc/kcore: 읽기 거부 ({e}). 커널 lockdown일 수 있음"), format!("/proc/kcore: read denied ({e}). The kernel may be in lockdown"))))?;
        Ok((f, ranges))
    }

    /// System RAM 중 읽을 영역에 들어가지 않은 양을 알림 (조용히 일부만 수집되지 않게)
    fn coverage(ram: &[(u64, u64)], ranges: &[Range], notes: &mut Vec<String>) {
        let total: u64 = ram.iter().map(|(s, e)| e - s).sum();
        let got: u64 = ranges.iter().map(|r| r.end - r.start).sum();
        // 몇 페이지(1MB 이하)는 커널이 kcore에서 늘 빼는 영역(펌웨어 예약 경계 등)이라 경고가 아닌 메모로
        if got < total && total - got <= 1 << 20 {
            let miss = crate::util::format_bytes(total - got);
            notes.push(crate::tr!(
                format!("System RAM 중 {miss}는 원본에 없어 이미지에 들어가지 않습니다 (커널이 내보내지 않는 몇 페이지)"),
                format!("{miss} of System RAM is not in the source and will not be in the image (a few pages the kernel does not export)")
            ));
        } else if got < total {
            let (all, have, pct, miss) = (
                crate::util::format_bytes(total),
                crate::util::format_bytes(got),
                got as f64 * 100.0 / total.max(1) as f64,
                crate::util::format_bytes(total - got),
            );
            notes.push(crate::tr!(
                format!("{COVERAGE_WARN} System RAM {all} 중 {have}({pct:.2}%)만 읽을 수 있습니다. 빠진 {miss}는 이미지에 들어가지 않습니다"),
                format!("{COVERAGE_WARN} Only {have} ({pct:.2}%) of {all} System RAM can be read. The missing {miss} will not be in the image")
            ));
        }
    }

    fn choose() -> Result<(File, Vec<Range>, String, Vec<String>)> {
        let (f, r, src, mut notes) = choose_source()?;
        let ram = ram_ranges()?;
        coverage(&ram, &r, &mut notes);
        Ok((f, r, src, notes))
    }

    fn choose_source() -> Result<(File, Vec<Range>, String, Vec<String>)> {
        let ram = ram_ranges()?;
        let mut notes = Vec::new();
        if std::path::Path::new("/dev/crash").exists() {
            match try_phys_dev("/dev/crash", &ram) {
                Ok((f, r)) => return Ok((f, r, "/dev/crash".into(), notes)),
                Err(e) => notes.push(e.to_string()),
            }
        } else {
            notes.push(crate::tr!("/dev/crash: 없음", "/dev/crash: not present").into());
        }
        match try_kcore(&ram) {
            Ok((f, r)) => return Ok((f, r, "/proc/kcore".into(), notes)),
            Err(e) => notes.push(e.to_string()),
        }
        match try_phys_dev("/dev/mem", &ram) {
            Ok((f, r)) => return Ok((f, r, "/dev/mem".into(), notes)),
            Err(e) => notes.push(e.to_string()),
        }
        let hint = match lockdown_mode().as_deref() {
            Some(m @ ("integrity" | "confidentiality")) => crate::tr!(
                format!(". 커널 lockdown({m}) 상태입니다 (보통 Secure Boot). 이 상태에서는 커널 모듈 없이 메모리를 읽을 수 없습니다: 서명된 수집 모듈을 쓰거나 Secure Boot를 끈 상태에서 수집하세요"),
                format!(". The kernel is in lockdown ({m}) mode (usually due to Secure Boot). Memory cannot be read without a kernel module in this state: use a signed acquisition module or acquire with Secure Boot disabled")
            ),
            _ => String::new(),
        };
        Err(anyhow!("{}", crate::tr!(format!("메모리를 읽을 수 있는 원본이 없습니다: {}{hint}", notes.join("; ")), format!("No readable memory source: {}{hint}", notes.join("; ")))))
    }

    /// 커널 lockdown 상태 (`/sys/kernel/security/lockdown`의 [대괄호] 값). 파일이 없으면 None
    fn lockdown_mode() -> Option<String> {
        let t = std::fs::read_to_string("/sys/kernel/security/lockdown").ok()?;
        parse_lockdown(&t)
    }

    /// 수집할 수 있으면 정보, 아니면 화면, 로그에 그대로 보여 줄 이유
    pub fn memory_probe() -> Result<MemoryInfo, String> {
        if !crate::platform::is_elevated() {
            return Err(crate::tr!("root 권한이 필요합니다 (sudo로 실행하세요)", "Root privileges are required (run with sudo)").into());
        }
        let (_, ranges, source, notes) = choose().map_err(|e| e.to_string())?;
        Ok(MemoryInfo {
            bytes: ranges.iter().map(|r| r.end - r.start).sum(),
            image_bytes: lime_len(&ranges),
            source,
            ranges: ranges.len(),
            notes,
            driver: None,
        })
    }

    pub fn open_memory() -> Result<(LimeSource, MemoryInfo)> {
        let (f, ranges, source, notes) = choose()?;
        let page_reads = source != "/proc/kcore";
        let info = MemoryInfo {
            bytes: ranges.iter().map(|r| r.end - r.start).sum(),
            image_bytes: lime_len(&ranges),
            source,
            ranges: ranges.len(),
            notes,
            driver: None,
        };
        let src = LimeSource::new(f, ranges);
        Ok((if page_reads { src.with_page_reads() } else { src }, info))
    }
}

/// "none [integrity] confidentiality" → "integrity"
pub fn parse_lockdown(t: &str) -> Option<String> {
    let a = t.find('[')?;
    let b = t[a..].find(']')? + a;
    Some(t[a + 1..b].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Source;

    /// CM_RESOURCE_LIST 만들기 (x64): 서술자 하나에 (종류, 플래그, 시작, 길이 필드)
    fn resource_list(parts: &[(u8, u16, u64, u32)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(1u32.to_le_bytes()); // 전체 서술자 1개
        b.extend(0u32.to_le_bytes()); // InterfaceType
        b.extend(0u32.to_le_bytes()); // BusNumber
        b.extend(1u16.to_le_bytes()); // Version
        b.extend(1u16.to_le_bytes()); // Revision
        b.extend((parts.len() as u32).to_le_bytes());
        for &(ty, flags, start, len) in parts {
            b.push(ty);
            b.push(1); // ShareDisposition
            b.extend(flags.to_le_bytes());
            b.extend(start.to_le_bytes());
            b.extend(len.to_le_bytes());
            b.extend([0u8; 4]); // 공용체 나머지 (x64 16바이트)
        }
        b
    }

    #[test]
    fn resource_list_memory_and_large() {
        let b = resource_list(&[
            (3, 0, 0x1000, 0x9e000),                // 일반 메모리 0x1000..0x9f000
            (7, 0x0200, 0x100000, 0x00ff_ff00 >> 8), // 큰 메모리 40: 길이 <<8
            (7, 0x0800, 0x1_0000_0000, 0x4),        // 큰 메모리 64: 4 <<32 = 16GB
            (2, 0, 0xdead, 0x10),                   // 메모리 아님 (인터럽트 등): 무시
        ]);
        let v = parse_resource_list(&b);
        assert_eq!(v, vec![(0x1000, 0x9f000), (0x100000, 0x100000 + 0x00ff_ff00), (0x1_0000_0000, 0x1_0000_0000 + (4u64 << 32))]);
        // 잘린 데이터는 읽은 데까지만 (패닉 없음)
        assert_eq!(parse_resource_list(&b[..40]), vec![(0x1000, 0x9f000)]);
        assert!(parse_resource_list(&[]).is_empty());
    }

    #[test]
    fn pmem_info_runs() {
        let at = 8 * (4 + 64 + 4 + 0xfe);
        let mut b = vec![0u8; at + 8 + 300 * 16];
        b[at..at + 8].copy_from_slice(&3u64.to_le_bytes());
        // 순서가 섞여 있고 둘은 붙어 있음 → 정렬, 합침
        for (i, (base, len)) in [(0x100000u64, 0x1000u64), (0x1000, 0x9e000), (0x101000, 0x1000)].iter().enumerate() {
            let o = at + 8 + i * 16;
            b[o..o + 8].copy_from_slice(&base.to_le_bytes());
            b[o + 8..o + 16].copy_from_slice(&len.to_le_bytes());
        }
        assert_eq!(parse_pmem_info(&b), Some(vec![(0x1000, 0x9f000), (0x100000, 0x102000)]));
        // 개수 0,300 초과, 잘림은 None (레지스트리로 대신)
        b[at..at + 8].copy_from_slice(&0u64.to_le_bytes());
        assert_eq!(parse_pmem_info(&b), None);
        b[at..at + 8].copy_from_slice(&301u64.to_le_bytes());
        assert_eq!(parse_pmem_info(&b), None);
        assert_eq!(parse_pmem_info(&b[..at]), None);
        let r = identity_ranges(&[(0x1000, 0x2000)]);
        assert_eq!(r, vec![Range { start: 0x1000, end: 0x2000, file_off: 0x1000 }]);
    }

    #[test]
    fn lockdown_value() {
        assert_eq!(parse_lockdown("none [integrity] confidentiality
").as_deref(), Some("integrity"));
        assert_eq!(parse_lockdown("[none] integrity confidentiality").as_deref(), Some("none"));
        assert_eq!(parse_lockdown("none"), None);
    }

    #[test]
    fn iomem_zero_addresses_detected_before_merge() {
        let t = "00000000-00000000 : System RAM\n00000000-00000000 : System RAM\n00000000-00000000 : System RAM\n";
        assert_eq!(parse_iomem_raw(t).len(), 3);
        assert_eq!(parse_iomem(t).len(), 1, "합치면 하나가 되어 0 주소 검사를 못 함");
        let t2 = "100000000-17fffffff : System RAM\n  180000000-1ffffffff : System RAM (virtio_mem)\n";
        // 이어지는 영역은 따로 둠 (겹치지 않으므로 중복 아님). virtio_mem 영역이 빠지지 않는지가 핵심
        assert_eq!(parse_iomem(t2), vec![(0x1_0000_0000, 0x1_8000_0000), (0x1_8000_0000, 0x2_0000_0000)]);
    }

    #[test]
    fn iomem_kmem_nested_and_merged() {
        let t = "00001000-0009efff : System RAM
100000000-17fffffff : System RAM
880000000-107fffffff : CXL Window 0
  880000000-107fffffff : region0
    880000000-107fffffff : dax0.0
      880000000-107fffffff : System RAM (kmem)
  880000000-880000fff : System RAM
";
        // 들여쓴 "System RAM"(맨 위 아님)은 빼고, kmem은 넣음
        assert_eq!(parse_iomem(t), vec![(0x1000, 0x9f000), (0x1_0000_0000, 0x1_8000_0000), (0x8_8000_0000, 0x10_8000_0000)]);
    }

    /// 오류 단위는 물리 페이지: 머리말(32B) 때문에 스트림 위치가 섹터와 어긋나도 이웃 페이지를 0으로 만들지 않음
    #[test]
    fn lime_error_unit_is_physical_page() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), vec![0u8; 0x10000]).unwrap();
        let ranges = vec![Range { start: 0x1000, end: 0x3000, file_off: 0x1000 }, Range { start: 0x8000, end: 0xa000, file_off: 0x8000 }];
        let s = LimeSource::new(std::fs::File::open(tmp.path()).unwrap(), ranges);
        assert_eq!(s.unit_at(0), (0, 32));
        assert_eq!(s.unit_at(32), (32, 32 + 4096));
        assert_eq!(s.unit_at(32 + 4096 + 5), (32 + 4096, 32 + 8192));
        let h2 = 32 + 0x2000;
        assert_eq!(s.unit_at(h2 + 3), (h2, h2 + 32));
        assert_eq!(s.unit_at(h2 + 32 + 4096), (h2 + 32 + 4096, h2 + 32 + 8192));
        assert_eq!(s.where_is(h2 + 32 + 4096).as_deref(), Some("물리 주소 0x9000"));
    }

    /// /dev/crash처럼 페이지 경계를 넘는 읽기를 EINVAL로 거부하는 장치도 페이지 단위로 읽어 끝까지 수집
    #[cfg(unix)]
    #[test]
    fn page_reads_split_at_page_boundaries() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data: Vec<u8> = (0..0x6000u32).map(|i| (i % 253) as u8).collect();
        std::fs::write(tmp.path(), &data).unwrap();
        let mut b = vec![0u8; 0x2800];
        read_pages_at(&std::fs::File::open(tmp.path()).unwrap(), 0x1800, &mut b).unwrap();
        assert_eq!(&b[..], &data[0x1800..0x4000]);
        // LimeSource 경로 (영역 2개, 페이지 읽기)
        let ranges = vec![Range { start: 0x1000, end: 0x3000, file_off: 0x1000 }, Range { start: 0x4000, end: 0x6000, file_off: 0x4000 }];
        let mut s = LimeSource::new(std::fs::File::open(tmp.path()).unwrap(), ranges).with_page_reads();
        let mut out = vec![0u8; s.len() as usize];
        let n = s.read_at(0, &mut out).unwrap();
        assert_eq!(n, out.len());
        assert_eq!(&out[32..32 + 0x2000], &data[0x1000..0x3000]);
    }

    /// 원본이 통째로 막힌 작은 메모리(1GB 미만)는 0만 담긴 "완료" 이미지가 되지 않고 실패
    #[test]
    fn all_pages_failing_is_an_error() {
        use crate::imaging::{run_image, ImageFormat, ImageSpec, Observer, Progress};
        use crate::logbook::LogLevel;
        use std::sync::atomic::AtomicBool;
        struct Q;
        impl Observer for Q {
            fn progress(&mut self, _: &Progress) {}
            fn log(&mut self, _: LogLevel, _: &str) {}
        }
        struct Dead(LimeSource);
        impl Source for Dead {
            fn len(&self) -> u64 {
                self.0.len()
            }
            fn sector_size(&self) -> u32 {
                4096
            }
            fn unit_at(&self, o: u64) -> (u64, u64) {
                self.0.unit_at(o)
            }
            fn is_synthetic(&self, o: u64) -> bool {
                self.0.is_synthetic(o)
            }
            fn dead_run_limit(&self) -> u32 {
                self.0.dead_run_limit()
            }
            fn read_at(&mut self, o: u64, b: &mut [u8]) -> io::Result<usize> {
                // 머리말만 있는 읽기는 성공, 메모리가 조금이라도 걸리면 실패
                if self.0.is_synthetic(o) && self.0.is_synthetic(o + b.len() as u64 - 1) {
                    return self.0.read_at(o, b);
                }
                Err(io::Error::other("EIO (시험)"))
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let mem = tmp.path().join("mem");
        std::fs::write(&mem, vec![1u8; 0x40000]).unwrap();
        let ranges = vec![Range { start: 0x1000, end: 0x20000, file_off: 0x1000 }, Range { start: 0x21000, end: 0x40000, file_off: 0x21000 }];
        let mut src = Dead(LimeSource::new(std::fs::File::open(&mem).unwrap(), ranges));
        let out = tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let spec = ImageSpec {
            out_dir: out,
            base_name: "m".into(),
            split_bytes: 0,
            algs: vec![crate::hash::HashAlg::Sha256],
            verify: true,
            zero_fill_errors: true,
            max_bytes: None,
            ext: Some("lime".into()),
            format: ImageFormat::Raw,
            compression: Default::default(),
            meta: Default::default(),
        };
        let e = run_image(&mut src, &spec, &AtomicBool::new(false), &mut Q).unwrap_err();
        assert!(e.to_string().contains("한 바이트도"), "{e}");
    }

    /// 원본 자체가 막힌 오류(ENODEV 등)는 페이지 오류로 바꾸지 않음 → 엔진이 즉시 중단
    #[cfg(unix)]
    #[test]
    fn fatal_memory_errors_keep_errno() {
        let e = page_error(io::Error::from_raw_os_error(libc::ENODEV), 0x1000, 4096);
        assert_eq!(e.raw_os_error(), Some(libc::ENODEV));
        let e = page_error(io::Error::from_raw_os_error(libc::EINVAL), 0x1000, 4096);
        assert_eq!(e.raw_os_error(), None);
    }

    /// 실제 이미징 경로: 물리 페이지 하나가 읽기 오류면 그 페이지만 0, 이웃 페이지는 그대로
    #[test]
    fn lime_bad_page_zero_fills_only_that_page() {
        use crate::imaging::{run_image, ImageFormat, ImageSpec, Observer, Progress};
        use crate::logbook::LogLevel;
        use std::sync::atomic::AtomicBool;
        struct Q(Vec<String>);
        impl Observer for Q {
            fn progress(&mut self, _: &Progress) {}
            fn log(&mut self, _: LogLevel, m: &str) {
                self.0.push(m.to_string());
            }
        }
        /// 물리 페이지 0x5000만 읽기 실패하는 메모리
        struct BadPage(LimeSource);
        impl Source for BadPage {
            fn len(&self) -> u64 {
                self.0.len()
            }
            fn sector_size(&self) -> u32 {
                4096
            }
            fn unit_at(&self, o: u64) -> (u64, u64) {
                self.0.unit_at(o)
            }
            fn where_is(&self, o: u64) -> Option<String> {
                self.0.where_is(o)
            }
            fn read_at(&mut self, o: u64, b: &mut [u8]) -> io::Result<usize> {
                // 스트림 [o, o+len)이 물리 0x5000..0x6000 (영역 0: 머리말 32 뒤 0x1000부터)과 겹치면 실패
                let (bs, be) = (32 + 0x4000u64, 32 + 0x5000u64);
                if o < be && o + b.len() as u64 > bs {
                    return Err(io::Error::other("EIO (시험)"));
                }
                self.0.read_at(o, b)
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let mem = tmp.path().join("mem");
        let data: Vec<u8> = (0..0x10000u32).map(|i| (i % 251) as u8 + 1).collect();
        std::fs::write(&mem, &data).unwrap();
        let ranges = vec![Range { start: 0x1000, end: 0x9000, file_off: 0x1000 }];
        let mut src = BadPage(LimeSource::new(std::fs::File::open(&mem).unwrap(), ranges));
        let out = tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let spec = ImageSpec {
            out_dir: out.clone(),
            base_name: "m".into(),
            split_bytes: 0,
            algs: vec![crate::hash::HashAlg::Sha256],
            verify: true,
            zero_fill_errors: true,
            max_bytes: None,
            ext: Some("lime".into()),
            format: ImageFormat::Raw,
            compression: Default::default(),
            meta: Default::default(),
        };
        let mut q = Q(Vec::new());
        let r = run_image(&mut src, &spec, &AtomicBool::new(false), &mut q).unwrap();
        assert_eq!(r.bad_sectors, 1, "{:?}", q.0);
        let img = std::fs::read(out.join("m.lime")).unwrap();
        let body = &img[32..];
        // 물리 0x1000..0x5000, 0x6000..0x9000은 원본 그대로, 0x5000..0x6000만 0
        assert_eq!(&body[..0x4000], &data[0x1000..0x5000]);
        assert!(body[0x4000..0x5000].iter().all(|b| *b == 0));
        assert_eq!(&body[0x5000..0x8000], &data[0x6000..0x9000]);
        assert!(q.0.iter().any(|m| m.contains("물리 주소 0x5000") && m.contains("페이지 1개")), "{:?}", q.0);
    }

    #[test]
    fn iomem_top_level_ram_only() {
        let t = "00000000-00000fff : Reserved\n00001000-0009efff : System RAM\n  00001000-00001fff : Kernel code\n00100000-bfffffff : System RAM\nfee00000-fee00fff : Local APIC\n";
        assert_eq!(parse_iomem(t), vec![(0x1000, 0x9f000), (0x10_0000, 0xc000_0000)]);
    }

    #[test]
    fn intersect_and_dedup() {
        let ram = vec![(0x1000, 0x9000), (0x10000, 0x20000)];
        let loads = vec![(0x0, 0x8000, 100), (0x10000, 0x10000, 5000), (0x11000, 0x1000, 9000)];
        let r = intersect(&ram, &loads);
        assert_eq!(r, vec![Range { start: 0x1000, end: 0x8000, file_off: 100 + 0x1000 }, Range { start: 0x10000, end: 0x20000, file_off: 5000 }]);
    }

    #[test]
    fn lime_stream_layout() {
        // 원본 파일: 0..64KB에 바이트 값 = 위치 % 251
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let data: Vec<u8> = (0..65536u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(tmp.path(), &data).unwrap();
        let ranges = vec![Range { start: 0x1000, end: 0x3000, file_off: 0x1000 }, Range { start: 0x8000, end: 0x9000, file_off: 0x8000 }];
        let mut s = LimeSource::new(std::fs::File::open(tmp.path()).unwrap(), ranges.clone());
        assert_eq!(s.len(), 32 + 0x2000 + 32 + 0x1000);
        let mut out = vec![0u8; s.len() as usize];
        // 일부러 엇갈린 크기로 나눠 읽어도 같은 결과
        let mut pos = 0;
        while pos < out.len() {
            let n = (out.len() - pos).min(777);
            let got = s.read_at(pos as u64, &mut out[pos..pos + n]).unwrap();
            pos += got;
        }
        assert_eq!(&out[0..4], &LIME_MAGIC.to_le_bytes());
        assert_eq!(u64::from_le_bytes(out[8..16].try_into().unwrap()), 0x1000);
        assert_eq!(u64::from_le_bytes(out[16..24].try_into().unwrap()), 0x2fff);
        assert_eq!(&out[32..32 + 0x2000], &data[0x1000..0x3000]);
        let h2 = 32 + 0x2000;
        assert_eq!(u64::from_le_bytes(out[h2 + 8..h2 + 16].try_into().unwrap()), 0x8000);
        assert_eq!(&out[h2 + 32..], &data[0x8000..0x9000]);
    }

    /// 실제 시스템 확인 (Linux root에서 `--ignored`로 실행):
    /// 커널이 /proc/iomem에 알려 준 "Kernel rodata" 물리 위치를, 수집과 같은 읽기 경로(LimeSource)로 읽으면
    /// /proc/version과 같은 커널 버전 문자열이 나와야 함 (물리 주소 → LiME 위치 매핑이 맞다는 증거)
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn live_kernel_banner_at_iomem_rodata() {
        let iomem = std::fs::read_to_string("/proc/iomem").unwrap();
        let line = iomem.lines().find(|l| l.contains("Kernel rodata")).expect("Kernel rodata 없음");
        let (range, _) = line.trim().split_once(" : ").unwrap();
        let (a, b) = range.split_once('-').unwrap();
        let (ps, pe) = (u64::from_str_radix(a, 16).unwrap(), u64::from_str_radix(b, 16).unwrap() + 1);
        let version = std::fs::read_to_string("/proc/version").unwrap();
        let banner = version.split(" (").next().unwrap().to_string(); // "Linux version 6.x..."
        let (mut src, info) = open_memory().unwrap();
        println!("원본 {}, 영역 {}개, rodata 물리 {ps:#x}-{pe:#x}", info.source, info.ranges);
        // 물리 주소 → LiME 스트림 위치
        let i = src.ranges.iter().position(|r| r.start <= ps && ps < r.end).expect("rodata가 수집 영역 밖");
        let lime_off = src.starts[i] + LIME_HEADER + (ps - src.ranges[i].start);
        let mut buf = vec![0u8; (pe - ps) as usize];
        let mut got = 0;
        while got < buf.len() {
            let n = src.read_at(lime_off + got as u64, &mut buf[got..]).unwrap();
            assert!(n > 0);
            got += n;
        }
        let found = buf.windows(banner.len()).any(|w| w == banner.as_bytes());
        assert!(found, "rodata에서 '{banner}'를 찾지 못함");
        println!("확인: 물리 {ps:#x} (LiME 위치 {lime_off:#x})에서 '{banner}' 발견");
    }

    #[test]
    fn zero_paddr_only_for_ram_at_zero() {
        let ram = [(0u64, 0x9fc00u64), (0x100000, 0x8000_0000)];
        let loads = vec![(0, 0x9f000, 0x1000), (0, 1 << 40, 0x9000_0000), (0x100000, 0x7ff0_0000, 0x200000)];
        let kept = keep_zero_ram(&ram, loads);
        // RAM 0 영역 크기의 0 구간은 남고, 거대한 0 구간(vmalloc 등)은 버림
        assert_eq!(kept, vec![(0, 0x9f000, 0x1000), (0x100000, 0x7ff0_0000, 0x200000)]);
        let r = intersect(&ram, &kept);
        assert_eq!(r[0], Range { start: 0, end: 0x9f000, file_off: 0x1000 });
        // 0에서 시작하는 RAM이 없으면 0 구간은 모두 버림
        assert!(keep_zero_ram(&[(0x100000, 0x200000)], vec![(0, 4096, 0)]).is_empty());
    }

    #[test]
    fn kcore_elf_parse() {
        // 최소 ELF64: 프로그램 헤더 2개 (PT_LOAD 물리 주소 있음 1개, vmalloc 1개)
        let mut f = vec![0u8; 64 + 56 * 2];
        f[0..4].copy_from_slice(b"\x7fELF");
        f[4] = 2;
        f[5] = 1;
        f[0x20..0x28].copy_from_slice(&64u64.to_le_bytes());
        f[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        f[0x38..0x3a].copy_from_slice(&2u16.to_le_bytes());
        let ph = |f: &mut Vec<u8>, i: usize, off: u64, paddr: u64, size: u64| {
            let b = 64 + i * 56;
            f[b..b + 4].copy_from_slice(&1u32.to_le_bytes());
            f[b + 8..b + 16].copy_from_slice(&off.to_le_bytes());
            f[b + 24..b + 32].copy_from_slice(&paddr.to_le_bytes());
            f[b + 32..b + 40].copy_from_slice(&size.to_le_bytes());
        };
        ph(&mut f, 0, 0x1000, 0x100000, 0x4000);
        ph(&mut f, 1, 0x9000, u64::MAX, 0x4000);
        let head = f[..64].to_vec();
        let mut rd = |off: u64, b: &mut [u8]| {
            let s = off as usize;
            let n = b.len().min(f.len().saturating_sub(s));
            b[..n].copy_from_slice(&f[s..s + n]);
            Ok(n)
        };
        assert_eq!(parse_kcore_loads(&head, &mut rd).unwrap(), vec![(0x100000, 0x4000, 0x1000)]);
    }
}
