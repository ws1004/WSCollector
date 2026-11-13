//! Windows 구현.
//!
//! 흔적 원칙:
//! - 디스크 정보 조회는 접근 권한 0(쿼리 전용)으로 엽니다. 읽기 핸들조차 만들지 않습니다.
//! - 레지스트리는 RegGetValueW로 읽기만 합니다.
//! - IOCTL 결과는 고정 레이아웃을 직접 파싱합니다 (WSImager device.rs 방식 확장).

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};
use windows::Win32::System::Diagnostics::Debug::{SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
use windows::Win32::System::SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::System::Time::{GetDynamicTimeZoneInformation, DYNAMIC_TIME_ZONE_INFORMATION};

use crate::model::{DiskInfo, HostInfo, PartitionInfo, ProcInfo, VolumeInfo};
use crate::source::{open_device_readonly, FileSource, Source};
use crate::target::Target;

// -- IOCTL 코드 (winioctl.h) ---------------------------------------------
const IOCTL_DISK_GET_DRIVE_GEOMETRY: u32 = 0x0007_0000;
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
const IOCTL_DISK_GET_DRIVE_LAYOUT_EX: u32 = 0x0007_0050;
const IOCTL_DISK_GET_LENGTH_INFO: u32 = 0x0007_405C;
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
const FSCTL_ALLOW_EXTENDED_DASD_IO: u32 = 0x0009_0083;

const MAX_DISKS: u32 = 64;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end]).trim().to_string()
}

/// 빈 카드 리더 등에서 "디스크 없음" 오류 창이 뜨지 않게 합니다.
fn quiet_errors() {
    // SAFETY: 프로세스 오류 모드 설정만 바꿈
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX);
    }
}

/// 쿼리 전용 핸들 (접근 권한 0)
fn open_query(path: &str) -> io::Result<File> {
    std::fs::OpenOptions::new().access_mode(0).share_mode(0x1 | 0x2).open(path)
}

fn ioctl(f: &File, code: u32, input: &[u8], out_size: usize) -> io::Result<Vec<u8>> {
    let mut out = vec![0u8; out_size];
    let mut ret = 0u32;
    // SAFETY: 버퍼 포인터와 길이가 일치하고 핸들은 유효함
    unsafe {
        DeviceIoControl(
            HANDLE(f.as_raw_handle()),
            code,
            if input.is_empty() { None } else { Some(input.as_ptr() as *const c_void) },
            input.len() as u32,
            Some(out.as_mut_ptr() as *mut c_void),
            out.len() as u32,
            Some(&mut ret),
            None,
        )
    }
    .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
    out.truncate(ret as usize);
    Ok(out)
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map_or(0, |s| u32::from_le_bytes(s.try_into().unwrap()))
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 8).map_or(0, |s| u64::from_le_bytes(s.try_into().unwrap()))
}
fn i64_at(b: &[u8], o: usize) -> i64 {
    u64_at(b, o) as i64
}

fn guid_at(b: &[u8], o: usize) -> String {
    let d1 = u32_at(b, o);
    let d2 = u16::from_le_bytes([b[o + 4], b[o + 5]]);
    let d3 = u16::from_le_bytes([b[o + 6], b[o + 7]]);
    let d4 = &b[o + 8..o + 16];
    format!(
        "{d1:08X}-{d2:04X}-{d3:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]
    )
}

fn ascii_at(b: &[u8], off: u32) -> String {
    let o = off as usize;
    if o == 0 || o >= b.len() {
        return String::new();
    }
    let end = b[o..].iter().position(|&c| c == 0).map_or(b.len(), |p| o + p);
    String::from_utf8_lossy(&b[o..end]).trim().to_string()
}

// -- 레지스트리 (읽기 전용) ------------------------------------------------
fn reg_str(subkey: &str, value: &str) -> Option<String> {
    let k = wide(subkey);
    let v = wide(value);
    let mut buf = vec![0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: 버퍼 크기를 바이트 단위로 전달
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(k.as_ptr()),
            PCWSTR(v.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    };
    if r.is_ok() {
        Some(from_wide(&buf)).filter(|s| !s.is_empty())
    } else {
        None
    }
}

fn reg_dword(subkey: &str, value: &str) -> Option<u32> {
    let k = wide(subkey);
    let v = wide(value);
    let mut d: u32 = 0;
    let mut size = 4u32;
    // SAFETY: 4바이트 출력
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(k.as_ptr()),
            PCWSTR(v.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut d as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    r.is_ok().then_some(d)
}

// -- 호스트 정보 -----------------------------------------------------------
pub fn host_info() -> HostInfo {
    const NT: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    const BIOS: &str = r"HARDWARE\DESCRIPTION\System\BIOS";
    let build = reg_str(NT, "CurrentBuild").unwrap_or_default();
    let ubr = reg_dword(NT, "UBR");
    let mut os_name = reg_str(NT, "ProductName").unwrap_or_else(|| "Windows".into());
    // Windows 11도 ProductName이 "Windows 10 …"으로 남아 있음 → 빌드 번호로 보정
    if build.parse::<u32>().unwrap_or(0) >= 22000 {
        os_name = os_name.replacen("Windows 10", "Windows 11", 1);
    }

    let mut mem = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: dwLength 설정 후 호출
    let ram = if unsafe { GlobalMemoryStatusEx(&mut mem) }.is_ok() { mem.ullTotalPhys } else { 0 };

    // SAFETY: 인자 없음
    let uptime_ms = unsafe { GetTickCount64() };
    let now = chrono::Local::now();
    let boot = now - chrono::Duration::milliseconds(uptime_ms as i64);

    let mut tz = DYNAMIC_TIME_ZONE_INFORMATION::default();
    // SAFETY: 구조체 포인터 전달
    unsafe { GetDynamicTimeZoneInformation(&mut tz) };
    let tz_name = from_wide(&tz.TimeZoneKeyName);

    HostInfo {
        hostname: std::env::var("COMPUTERNAME").unwrap_or_default(),
        os_name,
        os_version: reg_str(NT, "DisplayVersion").unwrap_or_default(),
        os_build: match ubr {
            Some(u) => format!("{build}.{u}"),
            None => build,
        },
        arch: std::env::consts::ARCH.into(),
        cpu: reg_str(r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", "ProcessorNameString").unwrap_or_default(),
        ram_bytes: ram,
        manufacturer: reg_str(BIOS, "SystemManufacturer").unwrap_or_default(),
        model: reg_str(BIOS, "SystemProductName").unwrap_or_default(),
        boot_time: boot.format("%Y-%m-%d %H:%M:%S").to_string(),
        local_time: now.format("%Y-%m-%d %H:%M:%S %:z").to_string(),
        timezone: if tz_name.is_empty() { now.format("UTC%:z").to_string() } else { format!("{tz_name} (UTC{})", now.format("%:z")) },
    }
}

pub fn is_elevated() -> bool {
    // SAFETY: 토큰 핸들은 사용 후 닫음
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut te = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut te as *mut _ as *mut c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && te.TokenIsElevated != 0
    }
}

/// 시스템 드라이브 문자 ("C:")
pub fn system_letter() -> Option<String> {
    std::env::var("SystemDrive").ok().map(|s| s.to_ascii_uppercase())
}

/// 경로가 저장되는 디스크 번호들
pub fn disks_for_path(p: &Path, _disks: &[DiskInfo]) -> Vec<u32> {
    volume_for_path(p).and_then(|v| v.disk_number).into_iter().collect()
}

/// 시스템(Windows) 디스크 번호들
pub fn system_disks(disks: &[DiskInfo]) -> Vec<u32> {
    let Some(l) = system_letter() else { return Vec::new() };
    disks.iter().filter(|d| d.partitions.iter().any(|p| p.letter.as_deref() == Some(l.as_str()))).map(|d| d.number).collect()
}

/// 이 OS가 도구 실행, 매체 연결로 남기는 기록 (보고서 흔적 기록용)
pub fn footprint_notes() -> Vec<String> {
    vec![
        crate::tr!(
            "Windows가 남기는 기록: Prefetch, Amcache, ShimCache, BAM (프로그램 실행)",
            "Artifacts left by Windows: Prefetch, Amcache, ShimCache, BAM (program execution)"
        )
        .into(),
        crate::tr!(
            "Windows가 남기는 기록: USBSTOR, MountedDevices, setupapi.dev.log, Partition/Diagnostic 이벤트 (USB 연결)",
            "Artifacts left by Windows: USBSTOR, MountedDevices, setupapi.dev.log, Partition/Diagnostic events (USB connection)"
        )
        .into(),
    ]
}

/// 이미징 대상이 저장 폴더가 있는 곳과 겹치는지 (디스크 전체, 같은 볼륨, 같은 파티션)
pub fn overlaps_output(t: &Target, disks: &[DiskInfo], out: &Path) -> bool {
    let Some(v) = volume_for_path(out) else { return false };
    match t {
        Target::Disk { number, .. } => v.disk_number == Some(*number),
        Target::Volume { letter } => letter.eq_ignore_ascii_case(&v.letter),
        Target::Partition { disk, number, .. } => {
            v.disk_number == Some(*disk)
                && disks
                    .iter()
                    .find(|d| d.number == *disk)
                    .and_then(|d| d.partitions.iter().find(|p| p.number == *number))
                    .is_some_and(|p| p.offset == v.offset || p.letter.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(&v.letter)))
        }
        Target::Device { .. } | Target::File { .. } | Target::Memory => false,
    }
}

/// 권한 안내 문구
pub const ADMIN_HINT: super::Bilingual = super::Bilingual {
    ko: "관리자 권한이 필요합니다. 관리자 권한 명령 프롬프트에서 다시 실행하세요.",
    en: "Administrator privileges are required. Run again from an elevated command prompt.",
};

// -- 디스크 ----------------------------------------------------------------
fn bus_name(t: u32) -> &'static str {
    match t {
        1 => "SCSI",
        2 => "ATAPI",
        3 => "ATA",
        4 => "IEEE 1394",
        6 => "Fibre",
        7 => "USB",
        8 => "RAID",
        9 => "iSCSI",
        10 => "SAS",
        11 => "SATA",
        12 => "SD",
        13 => "MMC",
        14 => "Virtual",
        15 => "File Backed Virtual",
        16 => "Storage Spaces",
        17 => "NVMe",
        18 => "SCM",
        19 => "UFS",
        _ => "Unknown",
    }
}

fn gpt_kind(guid: &str) -> &'static str {
    match guid {
        "C12A7328-F81F-11D2-BA4B-00A0C93EC93B" => crate::tr!("EFI 시스템", "EFI System"),
        "E3C9E316-0B5C-4DB8-817D-F92DF00215AE" => crate::tr!("Microsoft 예약", "Microsoft Reserved"),
        "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7" => crate::tr!("기본 데이터", "Basic Data"),
        "DE94BBA4-06D1-4D40-A16A-BFD50179D6AC" => crate::tr!("복구", "Recovery"),
        "5808C8AA-7E8F-42E0-85D2-E1E90434CFB3" => crate::tr!("LDM 메타데이터", "LDM Metadata"),
        "AF9B60A0-1431-4F62-BC68-3311714A69AD" => crate::tr!("LDM 데이터", "LDM Data"),
        "E75CAF8F-F680-4CEE-AFA3-B001E56EFC2D" => crate::tr!("저장소 공간", "Storage Spaces"),
        "0FC63DAF-8483-4772-8E79-3D69D8477DE4" => crate::tr!("Linux 파일시스템", "Linux Filesystem"),
        "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F" => crate::tr!("Linux 스왑", "Linux Swap"),
        "E6D6D379-F507-44C2-A23C-238F2A3DF928" => "Linux LVM",
        "48465300-0000-11AA-AA11-00306543ECAC" => "Apple HFS+",
        "7C3457EF-0000-11AA-AA11-00306543ECAC" => "Apple APFS",
        _ => "",
    }
}

fn mbr_kind(t: u8) -> &'static str {
    match t {
        0x07 => "NTFS/exFAT",
        0x0B | 0x0C => "FAT32",
        0x01 | 0x04 | 0x06 | 0x0E => "FAT",
        0x17 => crate::tr!("숨김 NTFS", "Hidden NTFS"),
        0x27 => crate::tr!("복구", "Recovery"),
        0x82 => crate::tr!("Linux 스왑", "Linux Swap"),
        0x83 => "Linux",
        0x8E => "Linux LVM",
        0xEE => crate::tr!("GPT 보호", "GPT Protective"),
        0xEF => crate::tr!("EFI 시스템", "EFI System"),
        _ => "",
    }
}

struct RawPartition {
    number: u32,
    offset: u64,
    size: u64,
    kind: String,
    name: String,
}

/// DRIVE_LAYOUT_INFORMATION_EX: 헤더 48바이트 + PARTITION_INFORMATION_EX(144바이트) 배열
fn parse_layout(b: &[u8]) -> (String, Vec<RawPartition>) {
    let style = match u32_at(b, 0) {
        0 => "MBR",
        1 => "GPT",
        _ => "RAW",
    };
    let count = u32_at(b, 4) as usize;
    let mut parts = Vec::new();
    for i in 0..count {
        let o = 48 + i * 144;
        if o + 144 > b.len() {
            break;
        }
        let pstyle = u32_at(b, o);
        let offset = i64_at(b, o + 8).max(0) as u64;
        let size = i64_at(b, o + 16).max(0) as u64;
        let number = u32_at(b, o + 24);
        if size == 0 || number == 0 {
            continue;
        }
        let (kind, name) = if pstyle == 1 {
            let g = guid_at(b, o + 32);
            let nm: Vec<u16> = (0..36).map(|k| u16::from_le_bytes([b[o + 72 + k * 2], b[o + 73 + k * 2]])).collect();
            let k = gpt_kind(&g);
            (if k.is_empty() { g } else { k.to_string() }, from_wide(&nm))
        } else {
            let t = b[o + 32];
            // 확장 파티션 컨테이너와 빈 항목은 건너뜀
            if t == 0 || t == 0x05 || t == 0x0F {
                continue;
            }
            let k = mbr_kind(t);
            (if k.is_empty() { crate::tr!(format!("유형 0x{t:02X}"), format!("Type 0x{t:02X}")) } else { k.to_string() }, String::new())
        };
        parts.push(RawPartition { number, offset, size, kind, name });
    }
    parts.sort_by_key(|p| p.offset);
    (style.to_string(), parts)
}

struct RawDisk {
    number: u32,
    size: u64,
    sector: u32,
    model: String,
    serial: String,
    bus: String,
    removable: bool,
    style: String,
    parts: Vec<RawPartition>,
}

fn query_disk(n: u32) -> Option<RawDisk> {
    let path = format!(r"\\.\PhysicalDrive{n}");
    let f = open_query(&path).ok()?;
    let geo = ioctl(&f, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], 256).ok()?;
    let sector = u32_at(&geo, 20);
    let size = i64_at(&geo, 24).max(0) as u64;

    // STORAGE_PROPERTY_QUERY { PropertyId=0(Device), QueryType=0(Standard) }
    let q = [0u8; 12];
    let (model, serial, bus, removable) = match ioctl(&f, IOCTL_STORAGE_QUERY_PROPERTY, &q, 4096) {
        Ok(d) if d.len() >= 36 => {
            let vendor = ascii_at(&d, u32_at(&d, 12));
            let product = ascii_at(&d, u32_at(&d, 16));
            let model = if vendor.is_empty() || product.starts_with(&vendor) { product } else { format!("{vendor} {product}") };
            (model, ascii_at(&d, u32_at(&d, 24)).trim_end_matches(['.', ' ']).to_string(), bus_name(u32_at(&d, 28)).to_string(), d[10] != 0)
        }
        _ => (String::new(), String::new(), "Unknown".into(), false),
    };

    // 파티션이 많으면 버퍼 부족(122) → 키워서 다시 시도
    let mut layout = Err(io::Error::from_raw_os_error(122));
    for n in [128usize, 1024] {
        layout = ioctl(&f, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, &[], 48 + 144 * n);
        if !matches!(&layout, Err(e) if e.raw_os_error() == Some(122)) {
            break;
        }
    }
    let (style, parts) = match layout {
        Ok(b) => parse_layout(&b),
        Err(_) => ("RAW".into(), Vec::new()),
    };

    Some(RawDisk { number: n, size, sector: if sector == 0 { 512 } else { sector }, model, serial, bus, removable, style, parts })
}

/// 드라이브 문자별 볼륨 정보
fn volumes() -> Vec<VolumeInfo> {
    quiet_errors();
    // SAFETY: 인자 없음
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = format!("{}:", (b'A' + i as u8) as char);
        if let Some(v) = volume_info(&letter) {
            out.push(v);
        }
    }
    out
}

fn volume_info(letter: &str) -> Option<VolumeInfo> {
    let root = wide(&format!("{letter}\\"));
    // SAFETY: NUL 종료 문자열
    let dt = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
    let drive_type = match dt {
        2 => "removable",
        3 => "fixed",
        4 => "remote",
        5 => "cdrom",
        6 => "ramdisk",
        _ => return None,
    };
    let mut label = [0u16; 261];
    let mut fs = [0u16; 261];
    // SAFETY: 버퍼 슬라이스 전달
    let _ = unsafe {
        GetVolumeInformationW(PCWSTR(root.as_ptr()), Some(&mut label), None, None, None, Some(&mut fs))
    };
    let (mut total, mut free) = (0u64, 0u64);
    // SAFETY: 출력 포인터
    let _ = unsafe { GetDiskFreeSpaceExW(PCWSTR(root.as_ptr()), None, Some(&mut total), Some(&mut free)) };

    let (mut disk_number, mut offset, mut size) = (None, 0u64, total);
    if drive_type != "remote" && drive_type != "cdrom" {
        if let Ok(f) = open_query(&format!(r"\\.\{letter}")) {
            // VOLUME_DISK_EXTENTS: NumberOfDiskExtents(4) + 패딩(4) + DISK_EXTENT{DiskNumber(4)+패딩(4), StartingOffset(8), ExtentLength(8)}
            if let Ok(b) = ioctl(&f, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], 8 + 24 * 8) {
                if u32_at(&b, 0) >= 1 {
                    disk_number = Some(u32_at(&b, 8));
                    offset = i64_at(&b, 16).max(0) as u64;
                    size = i64_at(&b, 24).max(0) as u64;
                }
            }
        }
    }
    Some(VolumeInfo {
        letter: letter.to_string(),
        label: from_wide(&label),
        fs: from_wide(&fs),
        size,
        free,
        disk_number,
        offset,
        drive_type: drive_type.into(),
    })
}

/// 경로가 속한 볼륨
pub fn volume_for_path(p: &Path) -> Option<VolumeInfo> {
    let s = p.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        quiet_errors();
        volume_info(&s[..2].to_ascii_uppercase())
    } else {
        None
    }
}

/// 물리 디스크 목록 (역할은 inventory에서 붙임)
pub fn list_disks() -> Result<Vec<DiskInfo>> {
    quiet_errors();
    let vols = volumes();
    let mut disks = Vec::new();
    for n in 0..MAX_DISKS {
        let Some(d) = query_disk(n) else { continue };
        let partitions = d
            .parts
            .iter()
            .map(|p| {
                let v = vols.iter().find(|v| v.disk_number == Some(d.number) && v.offset == p.offset);
                PartitionInfo {
                    number: p.number,
                    offset: p.offset,
                    size: p.size,
                    kind: p.kind.clone(),
                    name: p.name.clone(),
                    letter: v.map(|v| v.letter.clone()),
                    device: None,
                    label: v.map(|v| v.label.clone()).unwrap_or_default(),
                    fs: v.map(|v| v.fs.clone()).unwrap_or_default(),
                    free: v.map(|v| v.free),
                }
            })
            .collect();
        disks.push(DiskInfo {
            number: d.number,
            name: String::new(),
            path: format!(r"\\.\PhysicalDrive{}", d.number),
            model: if d.model.is_empty() { format!("PhysicalDrive{}", d.number) } else { d.model },
            serial: d.serial,
            bus: d.bus,
            removable: d.removable,
            size: d.size,
            sector_size: d.sector,
            partition_style: d.style,
            partitions,
            roles: Vec::new(),
            mounts: Vec::new(),
            fs: String::new(),
            backing: Vec::new(),
        });
    }
    if disks.is_empty() {
        return Err(anyhow!(crate::tr!(
            "물리 디스크를 찾지 못했습니다. 관리자 권한으로 실행했는지 확인하세요",
            "No physical disks found. Make sure you are running with administrator privileges"
        )));
    }
    Ok(disks)
}

/// 이미징 원본 열기 (읽기 전용)
pub fn open_target(t: &Target, disks: &[DiskInfo]) -> Result<Box<dyn Source>> {
    match t {
        Target::Disk { number, .. } => {
            let d = disks
                .iter()
                .find(|d| d.number == *number)
                .ok_or_else(|| anyhow!(crate::tr!(format!("Disk {number}을(를) 찾지 못했습니다"), format!("Disk {number} not found"))))?;
            let f = open_device_readonly(&d.path).with_context(|| open_failed(&d.path))?;
            let len = length_of(&f).unwrap_or(d.size);
            Ok(Box::new(FileSource::new(f, 0, len, d.sector_size)))
        }
        Target::Partition { disk, number, .. } => {
            let d = disks
                .iter()
                .find(|d| d.number == *disk)
                .ok_or_else(|| anyhow!(crate::tr!(format!("Disk {disk}을(를) 찾지 못했습니다"), format!("Disk {disk} not found"))))?;
            let p = d.partitions.iter().find(|p| p.number == *number).ok_or_else(|| {
                anyhow!(crate::tr!(format!("Disk {disk}의 파티션 {number}을(를) 찾지 못했습니다"), format!("Partition {number} on Disk {disk} not found")))
            })?;
            if p.offset.checked_add(p.size).is_none_or(|end| end > d.size) {
                return Err(anyhow!(crate::tr!(
                    format!("Disk {disk} 파티션 {number}의 범위가 디스크 크기를 넘습니다. 파티션 대신 디스크 전체를 이미징하세요"),
                    format!("Partition {number} on Disk {disk} extends beyond the disk size. Image the whole disk instead")
                )));
            }
            let ss = d.sector_size as u64;
            if p.offset % ss != 0 || p.size % ss != 0 {
                return Err(anyhow!(crate::tr!(
                    format!("Disk {disk} 파티션 {number}이(가) 섹터 경계에 맞지 않습니다. 디스크 전체를 이미징하세요"),
                    format!("Partition {number} on Disk {disk} is not sector-aligned. Image the whole disk instead")
                )));
            }
            let f = open_device_readonly(&d.path).with_context(|| open_failed(&d.path))?;
            Ok(Box::new(FileSource::new(f, p.offset, p.size, d.sector_size)))
        }
        Target::Volume { letter } => {
            let path = format!(r"\\.\{letter}");
            let f = open_device_readonly(&path).with_context(|| open_failed(&path))?;
            // 파일시스템 범위 밖(볼륨 끝 부분)까지 읽기 허용. 읽기 동작에만 영향
            let _ = ioctl(&f, FSCTL_ALLOW_EXTENDED_DASD_IO, &[], 0);
            let len = length_of(&f).context(crate::tr!("볼륨 크기를 알 수 없습니다", "Cannot determine the volume size"))?;
            // 섹터 크기: 볼륨이 속한 디스크 값 우선 (4Kn 디스크에서 512로 잘못 잡으면 정렬 오류)
            let disk_sector = disks
                .iter()
                .find(|d| d.partitions.iter().any(|p| p.letter.as_deref() == Some(letter.as_str())))
                .map(|d| d.sector_size);
            let sector = disk_sector
                .or_else(|| ioctl(&f, IOCTL_DISK_GET_DRIVE_GEOMETRY, &[], 24).ok().map(|g| u32_at(&g, 20)))
                .filter(|&s| s >= 512)
                .ok_or_else(|| anyhow!(crate::tr!(format!("{letter} 볼륨의 섹터 크기를 알 수 없습니다"), format!("Cannot determine the sector size of volume {letter}"))))?;
            Ok(Box::new(FileSource::new(f, 0, len, sector)))
        }
        Target::Device { path, .. } => {
            let f = open_device_readonly(path).with_context(|| open_failed(path))?;
            let len = length_of(&f).context(crate::tr!("장치 크기를 알 수 없습니다", "Cannot determine the device size"))?;
            Ok(Box::new(FileSource::new(f, 0, len, 512)))
        }
        Target::File { path: p } => Ok(Box::new(FileSource::open_file(p)?)),
        Target::Memory => Err(anyhow!(crate::tr!("Windows 메모리 수집은 아직 지원하지 않습니다", "Memory acquisition is not yet supported on Windows"))),
    }
}

fn open_failed(path: &str) -> String {
    crate::tr!(format!("{path} 열기 실패"), format!("Failed to open {path}"))
}

fn length_of(f: &File) -> Option<u64> {
    ioctl(f, IOCTL_DISK_GET_LENGTH_INFO, &[], 8).ok().map(|b| u64_at(&b, 0)).filter(|&n| n > 0)
}

// -- 프로세스 (흔적 기록) ---------------------------------------------------
/// `root`와 그 자손 프로세스 (WebView2 프로세스 포함)
pub fn process_tree(root: u32) -> Vec<ProcInfo> {
    let mut all = Vec::new();
    // SAFETY: 스냅샷 핸들은 사용 후 닫음
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return Vec::new() };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                all.push(ProcInfo { pid: e.th32ProcessID, parent_pid: e.th32ParentProcessID, name: from_wide(&e.szExeFile) });
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    // 부모 PID는 재사용될 수 있으므로, 부모보다 늦게 생성된 프로세스만 자식으로 인정합니다
    let mut out: Vec<(ProcInfo, u64)> = all.iter().filter(|p| p.pid == root).map(|p| (p.clone(), created_at(p.pid).unwrap_or(0))).collect();
    let mut i = 0;
    while i < out.len() {
        let (pid, t_parent) = (out[i].0.pid, out[i].1);
        for c in all.iter().filter(|p| p.parent_pid == pid && p.pid != pid) {
            if let Some(t) = created_at(c.pid) {
                if t >= t_parent && !out.iter().any(|(o, _)| o.pid == c.pid) {
                    out.push((c.clone(), t));
                }
            }
        }
        i += 1;
    }
    out.into_iter().map(|(p, _)| p).collect()
}

/// 프로세스 생성 시각 (FILETIME, 100ns 단위)
fn created_at(pid: u32) -> Option<u64> {
    // SAFETY: 핸들은 사용 후 닫음
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| ((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64)
    }
}

/// 저장 위치가 쓰는 디스크를 확인할 수 있는지 (Linux 전용 판별. 여기서는 늘 확인된 것으로 봄)
pub fn output_devices_known(_p: &std::path::Path) -> bool {
    true
}
